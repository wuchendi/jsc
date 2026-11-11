//! The page allocator and the sweep (design/runtime-memory.md §5.1, §5.2,
//! §5.6, §5.7).
//!
//! The heap is a run of 64 KiB pages from the first page boundary after the
//! static region up to the frontier `$__heap_ptr`. The static page table at
//! [`abi::GC_PAGE_TABLE`] codes every page: a small page of class
//! `1..=`[`abi::GC_CLASS_COUNT`] (a 16-byte header, then equal slots of the
//! class size, bump-allocated up to the header's `bump`), the head or a tail
//! page of a large span (requests above [`abi::GC_SMALL_MAX`]: a 16-byte
//! pre-header, then the block), or a page in the free-page pool.
//!
//! `__alloc_small(class)` takes a slot from the class free list (zeroing
//! it), else bumps the class's current page, else starts a page from
//! `__gc_take_pages`. Every block comes back zeroed, header included, so its
//! `gc` byte is `0` and a fresh block is never marked.
//!
//! `__gc_sweep` frees every unmarked slot (`gc != $__gc_epoch`) onto
//! rebuilt class lists, returns empty pages and unmarked large spans to the
//! pool and recomputes `$__gc_live`. It never allocates. Blocks in the static
//! region are on no heap page and are never swept. The collector that marks
//! and calls it lands in M2-T2 at [`emit_gc_budget`]'s hook.

use wasm_encoder::{
    BlockType, Function, FunctionSection, Instruction, MemArg, TypeSection, ValType,
};

use super::{
    FD_WRITE_IMPORT, GC_BUSY_GLOBAL, GC_DEBT_GLOBAL, GC_EPOCH_GLOBAL, GC_FREE_PAGES_GLOBAL,
    GC_LARGE_GLOBAL, GC_LIVE_GLOBAL, HEAP_PTR_GLOBAL, Helpers, LeafSet, PROC_EXIT_IMPORT, RAW_I32,
    Sink, abi, finish_builder, helper_builder, promise_engine,
};

/// Emit each listed instruction in order: `wasm!(function; LocalGet(0), ..)`.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

use ValType::I32;

/// The `(params, results)` of the allocator helpers, in function-index order.
pub(super) const TYPES: [(&[ValType], &[ValType]); 4] = [
    (&[I32], &[I32]),   // __alloc_small(class) -> base
    (&[I32], &[I32]),   // __gc_take_pages(k) -> base | 0
    (&[I32, I32], &[]), // __gc_release_pages(base, k)
    (&[], &[]),         // __gc_sweep()
];

/// Append the allocator helpers' types and function entries (after the
/// promise engine's).
pub(super) fn register(types: &mut TypeSection, functions: &mut FunctionSection) {
    for (params, results) in TYPES {
        let index = types.len();
        types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        functions.function(index);
    }
}

impl Helpers<'_> {
    /// The first allocator helper: they follow the promise engine.
    fn gc_base(self) -> u32 {
        self.promise_new() + u32::try_from(promise_engine::TYPES.len()).unwrap_or(0)
    }
    /// `__alloc_small(class: i32) -> i32`: a zeroed slot of small class
    /// `class` (`1..=GC_CLASS_COUNT`), the per-class entry of constant-size
    /// sites ([`emit_alloc_const`]) and of `__alloc` for small sizes.
    pub(super) fn alloc_small(self) -> u32 {
        self.gc_base()
    }
    /// `__gc_take_pages(k: i32) -> i32`: see [`emit_take_pages`].
    pub(super) fn gc_take_pages(self) -> u32 {
        self.gc_base() + 1
    }
    /// `__gc_release_pages(base: i32, k: i32)`: see [`emit_release_pages`].
    pub(super) fn gc_release_pages(self) -> u32 {
        self.gc_base() + 2
    }
    /// `__gc_sweep()`: see [`emit_sweep`].
    pub(super) fn gc_sweep(self) -> u32 {
        self.gc_base() + 3
    }
}

/// The helper call graph of the allocator, for the leaf analysis: the page
/// helpers and the sweep never reach `__alloc` (`__alloc_small` is an
/// allocation and stays a safepoint).
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    vec![
        (helpers.gc_take_pages(), vec![]),
        (helpers.gc_release_pages(), vec![]),
        (helpers.gc_sweep(), vec![helpers.gc_release_pages()]),
    ]
}

/// Every allocator helper body, in [`TYPES`] order.
pub(super) fn emit_helpers(helpers: Helpers<'_>, leaves: &LeafSet) -> Vec<Function> {
    vec![
        emit_alloc_small(helpers, leaves),
        emit_take_pages(leaves),
        emit_release_pages(leaves),
        emit_sweep(helpers, leaves),
    ]
}

/// The first heap address: `static_end` rounded up to a page boundary.
pub(super) fn heap_start(static_end: i32) -> i32 {
    let page = abi::GC_PAGE_SIZE.cast_signed();
    (static_end + page - 1) & -page
}

/// The class size table at [`abi::GC_CLASS_SIZES`]: `u32` slot sizes of
/// classes `1..=GC_CLASS_COUNT`.
pub(super) fn class_sizes() -> Vec<u8> {
    (1..=abi::GC_CLASS_COUNT)
        .flat_map(|class| abi::gc_class_size(class).to_le_bytes())
        .collect()
}

/// Push the base of a fresh zeroed block of the compile-time `size`: the
/// per-class entry `__alloc_small(class)` for a small size, else
/// `__alloc(size)`.
pub(super) fn emit_alloc_const(function: &mut impl Sink, helpers: Helpers<'_>, size: i32) {
    match abi::gc_size_class(size.cast_unsigned()) {
        0 => wasm!(function; I32Const(size), Call(helpers.alloc)),
        class => wasm!(function; I32Const(class.cast_signed()), Call(helpers.alloc_small())),
    }
}

/// Emit the allocation budget step: `$__gc_debt -= bytes` (pushed by
/// `bytes`); when the debt goes negative outside a collection, the budget is
/// renewed to `max(GC_MIN_BUDGET, $__gc_live)`.
///
/// `GC_COLLECT_HOOK`: the single point where M2-T2 inserts `call
/// $__gc_collect`, right before the renewal. Until then nothing collects.
pub(super) fn emit_gc_budget<S: Sink>(function: &mut S, bytes: impl FnOnce(&mut S)) {
    let min = abi::GC_MIN_BUDGET.cast_signed();
    wasm!(function; GlobalGet(GC_DEBT_GLOBAL));
    bytes(function);
    wasm!(function;
        I32Sub, GlobalSet(GC_DEBT_GLOBAL),
        GlobalGet(GC_DEBT_GLOBAL), I32Const(0), I32LtS, GlobalGet(GC_BUSY_GLOBAL), I32Eqz, I32And,
        If(BlockType::Empty),
        // GC_COLLECT_HOOK (M2-T2): `call $__gc_collect` goes here.
        GlobalGet(GC_LIVE_GLOBAL), I32Const(min), GlobalGet(GC_LIVE_GLOBAL), I32Const(min), I32GtS,
        Select, GlobalSet(GC_DEBT_GLOBAL),
        End,
    );
}

/// Emit the out-of-memory exit: the static [`abi::OOM_LINE`] on fd 2 through
/// its static iovec (allocating nothing), then `proc_exit(1)`.
pub(super) fn emit_oom_exit(function: &mut impl Sink) {
    wasm!(function;
        I32Const(2), I32Const(abi::OOM_IOVEC.cast_signed()), I32Const(1),
        I32Const(abi::WASI_NWRITTEN.cast_signed()), Call(FD_WRITE_IMPORT), Drop,
        I32Const(1), Call(PROC_EXIT_IMPORT), Unreachable,
    );
}

/// A 4-byte-aligned `u32` [`MemArg`] at `offset`.
fn m32(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 2,
        memory_index: 0,
    }
}

/// A byte [`MemArg`] at `offset`.
fn m8(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 0,
        memory_index: 0,
    }
}

/// `MemArg` of entry `c` (pushed as `c << 2`) of a static `u32[GC_CLASS_COUNT]`
/// array at `base`, whose class `1` sits at `base`.
fn class_entry(base: u32) -> MemArg {
    m32(base - 4)
}

const PAGE_SHIFT: i32 = 16;
const PAGE_SIZE: i32 = abi::GC_PAGE_SIZE.cast_signed();
const FREE_CODE: i32 = abi::GC_PAGE_FREE as i32;
const TAIL_CODE: i32 = abi::GC_PAGE_LARGE_TAIL as i32;
const TYPE_FREE: i32 = abi::TYPE_FREE as i32;
const PAGE_HEADER: i32 = abi::GC_PAGE_HEADER_SIZE.cast_signed();

/// Emit `__alloc_small(class: i32) -> i32` (locals: `ci = class << 2`, the
/// slot `size`, the free-list `head`, the current `page`, its `bump`). The
/// order of §5.6: budget, class free list (zeroing the slot), current page
/// bump, a fresh page (zero) from `__gc_take_pages`, else the out-of-memory
/// exit.
fn emit_alloc_small(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32; 5]);
    let (class, ci, size, head, page, bump) = (0, 1, 2, 3, 4, 5);
    wasm!(f;
        LocalGet(class), I32Const(2), I32Shl, LocalTee(ci),
        I32Load(class_entry(abi::GC_CLASS_SIZES)), LocalSet(size),
    );
    emit_gc_budget(&mut f, |f| wasm!(f; LocalGet(size)));
    wasm!(f;
        // Pop the class free list and zero the whole slot.
        LocalGet(ci), I32Load(class_entry(abi::GC_FREE_HEADS)), LocalTee(head),
        If(BlockType::Empty),
        LocalGet(ci), LocalGet(head), I32Load(m32(abi::GC_FREE_NEXT_OFFSET)),
        I32Store(class_entry(abi::GC_FREE_HEADS)),
        LocalGet(head), I32Const(0), LocalGet(size), MemoryFill(0),
        LocalGet(head), Return,
        End,
        // Bump the current page while the slot fits (`head` = its end).
        LocalGet(ci), I32Load(class_entry(abi::GC_CUR_PAGES)), LocalTee(page),
        If(BlockType::Empty),
        LocalGet(page), I32Load(m32(abi::GC_PAGE_BUMP_OFFSET)), LocalTee(bump),
        LocalGet(size), I32Add, LocalTee(head),
        LocalGet(page), I32Const(PAGE_SIZE), I32Add, I32LeU,
        If(BlockType::Empty),
        LocalGet(page), LocalGet(head), I32Store(m32(abi::GC_PAGE_BUMP_OFFSET)),
        LocalGet(bump), Return,
        End,
        End,
        // A fresh page becomes the class's current page.
        I32Const(1), Call(helpers.gc_take_pages()), LocalTee(page), I32Eqz,
        If(BlockType::Empty),
    );
    emit_oom_exit(&mut f);
    wasm!(f;
        End,
        LocalGet(page), I32Const(PAGE_SHIFT), I32ShrU, LocalGet(class),
        I32Store8(m8(abi::GC_PAGE_TABLE)),
        LocalGet(page), LocalGet(class), I32Store(m32(abi::GC_PAGE_CLASS_OFFSET)),
        LocalGet(page), LocalGet(page), I32Const(PAGE_HEADER), I32Add, LocalTee(bump),
        LocalGet(size), I32Add, I32Store(m32(abi::GC_PAGE_BUMP_OFFSET)),
        LocalGet(ci), LocalGet(page), I32Store(class_entry(abi::GC_CUR_PAGES)),
        LocalGet(bump), End,
    );
    finish_builder(f)
}

/// Emit `__gc_take_pages(k: i32) -> i32`, the page pool, usable without
/// `__alloc` (the mark stack takes its span here, §5.4).
///
/// Contract: returns the base of `k >= 1` contiguous zeroed pages, or `0`
/// when memory cannot supply them (it never collects, never reports and
/// never traps). Every returned page is coded [`abi::GC_PAGE_LARGE_TAIL`]
/// in the page table, so the sweep neither walks nor frees it; the caller
/// re-codes the pages it turns into a small page or a large-span head, and
/// hands them back with `__gc_release_pages`. The pool is searched first
/// (first fit over the page table, only when `$__gc_free_pages >= k`), then
/// the frontier advances, growing memory by `max(needed, memory.size)` pages
/// (doubling) or, failing that, exactly the needed pages.
fn emit_take_pages(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32; 5]);
    let (k, i, end, run, base, need) = (0, 1, 2, 3, 4, 5);
    let max_pages = i32::try_from(abi::MEMORY_MAX_PAGES).unwrap_or(i32::MAX);
    wasm!(f;
        Block(BlockType::Empty),
        GlobalGet(GC_FREE_PAGES_GLOBAL), LocalGet(k), I32LtU, BrIf(0),
        GlobalGet(HEAP_PTR_GLOBAL), I32Const(PAGE_SHIFT), I32ShrU, LocalSet(end),
        Loop(BlockType::Empty),
        LocalGet(i), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(i), I32Load8U(m8(abi::GC_PAGE_TABLE)), I32Const(FREE_CODE), I32Eq,
        If(BlockType::Empty),
        LocalGet(run), I32Const(1), I32Add, LocalTee(run), LocalGet(k), I32Eq,
        If(BlockType::Empty),
        // A run of `k` free pages ends at page `i`: take and zero it.
        LocalGet(i), I32Const(1), I32Add, LocalGet(k), I32Sub, LocalTee(i),
        I32Const(abi::GC_PAGE_TABLE.cast_signed()), I32Add, I32Const(TAIL_CODE), LocalGet(k),
        MemoryFill(0),
        GlobalGet(GC_FREE_PAGES_GLOBAL), LocalGet(k), I32Sub, GlobalSet(GC_FREE_PAGES_GLOBAL),
        LocalGet(i), I32Const(PAGE_SHIFT), I32Shl, LocalTee(base),
        I32Const(0), LocalGet(k), I32Const(PAGE_SHIFT), I32Shl, MemoryFill(0),
        LocalGet(base), Return,
        End,
        Else,
        I32Const(0), LocalSet(run),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i), Br(0),
        End,
        End,
        // The frontier: pages above it were never written, so they are zero.
        LocalGet(k), I32Const(max_pages), I32GtU,
        If(BlockType::Empty), I32Const(0), Return, End,
        GlobalGet(HEAP_PTR_GLOBAL), LocalTee(base), I32Const(PAGE_SHIFT), I32ShrU, LocalGet(k),
        I32Add, MemorySize(0), I32Sub, LocalTee(need), I32Const(0), I32GtS,
        If(BlockType::Empty),
        LocalGet(need), MemorySize(0), LocalGet(need), MemorySize(0), I32GtU, Select,
        MemoryGrow(0), I32Const(-1), I32Eq,
        If(BlockType::Empty),
        LocalGet(need), MemoryGrow(0), I32Const(-1), I32Eq,
        If(BlockType::Empty), I32Const(0), Return, End,
        End,
        End,
        LocalGet(base), LocalGet(k), I32Const(PAGE_SHIFT), I32Shl, I32Add,
        GlobalSet(HEAP_PTR_GLOBAL),
        LocalGet(base), I32Const(PAGE_SHIFT), I32ShrU, I32Const(abi::GC_PAGE_TABLE.cast_signed()),
        I32Add, I32Const(TAIL_CODE), LocalGet(k), MemoryFill(0),
        LocalGet(base), End,
    );
    finish_builder(f)
}

/// Emit `__gc_release_pages(base: i32, k: i32)`: return `k` pages from `base`
/// to the pool (coded [`abi::GC_PAGE_FREE`]; zeroed when taken again).
fn emit_release_pages(leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32, RAW_I32], &[]);
    let (base, k) = (0, 1);
    wasm!(f;
        LocalGet(base), I32Const(PAGE_SHIFT), I32ShrU, I32Const(abi::GC_PAGE_TABLE.cast_signed()),
        I32Add, I32Const(FREE_CODE), LocalGet(k), MemoryFill(0),
        GlobalGet(GC_FREE_PAGES_GLOBAL), LocalGet(k), I32Add, GlobalSet(GC_FREE_PAGES_GLOBAL),
        End,
    );
    finish_builder(f)
}

/// Emit `__gc_sweep()` (§5.6 "Sweep: eager"), run after marking: clear the
/// class free lists, then walk every small page from its first slot to
/// `bump`, keeping slots whose `gc` byte is `$__gc_epoch` and rewriting every
/// other slot (garbage or already free) as a [`abi::TYPE_FREE`] slot on a
/// page-local list. A page with no survivor goes back to the pool (and stops
/// being its class's current page); otherwise its `live` count is stored and
/// its list spliced onto the class list. Unmarked large spans are unlinked
/// and released. `$__gc_live` becomes the surviving slot and span bytes.
/// Allocates nothing.
fn emit_sweep(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[], &[RAW_I32; 14]);
    let (i, end, class, page, size, slot, limit, head, tail, live) = (0, 1, 2, 3, 4, 5, 6, 7, 8, 9);
    let (total, span, prev, next) = (10, 11, 12, 13);
    let gc = abi::HEADER_GC_OFFSET;
    let classes = abi::GC_CLASS_COUNT.cast_signed();
    wasm!(f;
        I32Const(abi::GC_FREE_HEADS.cast_signed()), I32Const(0), I32Const(4 * classes),
        MemoryFill(0),
        GlobalGet(HEAP_PTR_GLOBAL), I32Const(PAGE_SHIFT), I32ShrU, LocalSet(end),
        Block(BlockType::Empty),
        Loop(BlockType::Empty),
        LocalGet(i), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(i), I32Load8U(m8(abi::GC_PAGE_TABLE)), LocalTee(class),
        I32Const(1), I32Sub, I32Const(classes), I32LtU,
        If(BlockType::Empty),
        LocalGet(i), I32Const(PAGE_SHIFT), I32Shl, LocalTee(page), I32Const(PAGE_HEADER), I32Add,
        LocalSet(slot),
        LocalGet(page), I32Load(m32(abi::GC_PAGE_BUMP_OFFSET)), LocalSet(limit),
        LocalGet(class), I32Const(2), I32Shl, I32Load(class_entry(abi::GC_CLASS_SIZES)),
        LocalSet(size),
        I32Const(0), LocalSet(head), I32Const(0), LocalSet(tail), I32Const(0), LocalSet(live),
        Block(BlockType::Empty),
        Loop(BlockType::Empty),
        LocalGet(slot), LocalGet(limit), I32GeU, BrIf(1),
        LocalGet(slot), I32Load8U(m8(0)), I32Const(TYPE_FREE), I32Ne,
        LocalGet(slot), I32Load8U(m8(gc)), GlobalGet(GC_EPOCH_GLOBAL), I32Eq, I32And,
        If(BlockType::Empty),
        LocalGet(live), I32Const(1), I32Add, LocalSet(live),
        Else,
        LocalGet(slot), I32Const(TYPE_FREE), I32Store(m32(0)),
        LocalGet(slot), LocalGet(head), I32Store(m32(abi::GC_FREE_NEXT_OFFSET)),
        LocalGet(head), I32Eqz,
        If(BlockType::Empty), LocalGet(slot), LocalSet(tail), End,
        LocalGet(slot), LocalSet(head),
        End,
        LocalGet(slot), LocalGet(size), I32Add, LocalSet(slot), Br(0),
        End,
        End,
        LocalGet(live),
        If(BlockType::Empty),
        LocalGet(page), LocalGet(live), I32Store(m32(abi::GC_PAGE_LIVE_OFFSET)),
        LocalGet(head),
        If(BlockType::Empty),
        LocalGet(tail), LocalGet(class), I32Const(2), I32Shl,
        I32Load(class_entry(abi::GC_FREE_HEADS)), I32Store(m32(abi::GC_FREE_NEXT_OFFSET)),
        LocalGet(class), I32Const(2), I32Shl, LocalGet(head),
        I32Store(class_entry(abi::GC_FREE_HEADS)),
        End,
        LocalGet(total), LocalGet(live), LocalGet(size), I32Mul, I32Add, LocalSet(total),
        Else,
        LocalGet(page), I32Const(1), Call(helpers.gc_release_pages()),
        LocalGet(class), I32Const(2), I32Shl, I32Load(class_entry(abi::GC_CUR_PAGES)),
        LocalGet(page), I32Eq,
        If(BlockType::Empty),
        LocalGet(class), I32Const(2), I32Shl, I32Const(0), I32Store(class_entry(abi::GC_CUR_PAGES)),
        End,
        End,
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i), Br(0),
        End,
        End,
        // The large spans, unlinking and releasing the unmarked ones.
        GlobalGet(GC_LARGE_GLOBAL), LocalSet(span),
        Block(BlockType::Empty),
        Loop(BlockType::Empty),
        LocalGet(span), I32Eqz, BrIf(1),
        LocalGet(span), I32Load(m32(abi::GC_LARGE_NEXT_OFFSET)), LocalSet(next),
        LocalGet(span), I32Load8U(m8(abi::GC_LARGE_HEADER_SIZE + gc)), GlobalGet(GC_EPOCH_GLOBAL),
        I32Eq,
        If(BlockType::Empty),
        LocalGet(total), LocalGet(span), I32Load(m32(abi::GC_LARGE_PAGES_OFFSET)),
        I32Const(PAGE_SHIFT), I32Shl, I32Add, LocalSet(total),
        LocalGet(span), LocalSet(prev),
        Else,
        LocalGet(prev),
        If(BlockType::Empty),
        LocalGet(prev), LocalGet(next), I32Store(m32(abi::GC_LARGE_NEXT_OFFSET)),
        Else,
        LocalGet(next), GlobalSet(GC_LARGE_GLOBAL),
        End,
        LocalGet(span), LocalGet(span), I32Load(m32(abi::GC_LARGE_PAGES_OFFSET)),
        Call(helpers.gc_release_pages()),
        End,
        LocalGet(next), LocalSet(span), Br(0),
        End,
        End,
        LocalGet(total), GlobalSet(GC_LIVE_GLOBAL), End,
    );
    finish_builder(f)
}
