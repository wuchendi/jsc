//! The page allocator and the collector (design/runtime-memory.md §5.1,
//! §5.2, §5.4, §5.6, §5.7).
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
//! `__gc_collect` is a precise, non-moving, stop-the-world mark-sweep run
//! from the allocation budget step ([`emit_gc_budget`]) and once before an
//! out-of-memory exit. It bumps the epoch, marks from the roots
//! ([`emit_collect`]) through the mark stack (`__gc_visit` / `__gc_trace` /
//! `__gc_drain`), tombstones the unmarked heap strings of the weak intern
//! table, and sweeps. `__gc_sweep` frees every unmarked slot
//! (`gc != $__gc_epoch`) onto rebuilt class lists, returns empty pages and
//! unmarked large spans to the pool and recomputes `$__gc_live`. No
//! collector helper allocates or runs user code, and a collection never
//! nests (`$__gc_busy`).

use wasm_encoder::{
    BlockType, Function, FunctionSection, Instruction, MemArg, TypeSection, ValType,
};

use super::{
    CF_VALUE_GLOBAL, EXC_VALUE_GLOBAL, FD_WRITE_IMPORT, GC_BUSY_GLOBAL, GC_DEBT_GLOBAL,
    GC_EPOCH_GLOBAL, GC_FREE_PAGES_GLOBAL, GC_LARGE_GLOBAL, GC_LIVE_GLOBAL, GC_OVERFLOW_GLOBAL,
    GC_STACK_END_GLOBAL, GC_STACK_GLOBAL, GC_STACK_TOP_GLOBAL, HEAP_BASE, HEAP_PTR_GLOBAL, Helpers,
    INTERN_BASE_GLOBAL, INTERN_CAP_GLOBAL, JOB_HEAD_GLOBAL, JOB_TAIL_GLOBAL, LeafSet,
    MODULE_CELL_GLOBAL_BASE, NEW_TARGET_GLOBAL, OUT_BUF_GLOBAL, PATH_BUF_GLOBAL, PROC_EXIT_IMPORT,
    RAW_I32, RAW_I64, REFS_BUF_GLOBAL, REJECTED_HEAD_GLOBAL, SP_GLOBAL, SPREAD_ARGS_GLOBAL, Sink,
    THIS_GLOBAL, abi, finish_builder, helper_builder, promise_engine,
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

/// What the collector knows about the module being emitted.
#[derive(Clone, Copy)]
pub(super) struct GcLayout {
    /// The first heap address (a page boundary): addresses below it are the
    /// shadow stack and the static region, never swept.
    pub(super) heap_start: i32,
    /// The static root list `[count: u32][addr: u32 x count]` ([`static_roots`]).
    pub(super) roots: i32,
    /// The number of module-cell globals.
    pub(super) module_cells: u32,
    /// The test-only `gc_stress` option: budget `0`, so every allocation
    /// collects.
    pub(super) stress: bool,
}

/// The `(params, results)` of the allocator and collector helpers, in
/// function-index order.
pub(super) const TYPES: [(&[ValType], &[ValType]); 8] = [
    (&[I32], &[I32]),   // __alloc_small(class) -> base
    (&[I32], &[I32]),   // __gc_take_pages(k) -> base | 0
    (&[I32, I32], &[]), // __gc_release_pages(base, k)
    (&[], &[]),         // __gc_sweep()
    (&[], &[]),         // __gc_collect()
    (&[I32], &[]),      // __gc_visit(ref)
    (&[I32], &[]),      // __gc_trace(block)
    (&[], &[]),         // __gc_drain()
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
    /// `__gc_collect()`: see [`emit_collect`].
    pub(super) fn gc_collect(self) -> u32 {
        self.gc_base() + 4
    }
    /// `__gc_visit(ref: i32)`: see [`emit_visit`].
    fn gc_visit(self) -> u32 {
        self.gc_base() + 5
    }
    /// `__gc_trace(block: i32)`: see [`emit_trace`].
    fn gc_trace(self) -> u32 {
        self.gc_base() + 6
    }
    /// `__gc_drain()`: see [`emit_drain`].
    fn gc_drain(self) -> u32 {
        self.gc_base() + 7
    }
}

/// The helper call graph of the allocator and collector, for the leaf
/// analysis: the page helpers and every collector helper never reach
/// `__alloc` (`__alloc_small` is an allocation and stays a safepoint).
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    vec![
        (helpers.gc_take_pages(), vec![]),
        (helpers.gc_release_pages(), vec![]),
        (helpers.gc_sweep(), vec![helpers.gc_release_pages()]),
        (
            helpers.gc_collect(),
            vec![
                helpers.gc_visit(),
                helpers.gc_trace(),
                helpers.gc_drain(),
                helpers.gc_release_pages(),
                helpers.gc_sweep(),
            ],
        ),
        (
            helpers.gc_visit(),
            vec![helpers.gc_take_pages(), helpers.gc_release_pages()],
        ),
        (helpers.gc_trace(), vec![helpers.gc_visit()]),
        (helpers.gc_drain(), vec![helpers.gc_trace()]),
    ]
}

/// Every allocator and collector helper body, in [`TYPES`] order.
pub(super) fn emit_helpers(helpers: Helpers<'_>, leaves: &LeafSet, gc: GcLayout) -> Vec<Function> {
    vec![
        emit_alloc_small(helpers, leaves),
        emit_take_pages(leaves),
        emit_release_pages(leaves),
        emit_sweep(helpers, leaves),
        emit_collect(helpers, leaves, gc),
        emit_visit(helpers, leaves, gc),
        emit_trace(helpers, leaves),
        emit_drain(helpers, leaves),
    ]
}

/// The first heap address: `static_end` rounded up to a page boundary.
pub(super) fn heap_start(static_end: i32) -> i32 {
    let page = abi::GC_PAGE_SIZE.cast_signed();
    (static_end + page - 1) & -page
}

/// The static root list's bytes (design/runtime-memory.md §5.4, "static
/// region"): `[count: u32][addr: u32 x count]` of every static object-family
/// block, in `addrs` order.
pub(super) fn root_list(addrs: &[u32]) -> Vec<u8> {
    let count = u32::try_from(addrs.len()).unwrap_or(u32::MAX);
    count
        .to_le_bytes()
        .into_iter()
        .chain(addrs.iter().flat_map(|addr| addr.to_le_bytes()))
        .collect()
}

/// The byte size of a [`root_list`] of `count` addresses.
pub(super) fn root_list_size(count: usize) -> i32 {
    i32::try_from(4 + 4 * count).unwrap_or(i32::MAX)
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

/// Emit the allocation budget step (§5.6 "When to collect", step 1):
/// `$__gc_debt -= bytes` (pushed by `bytes`); when the debt goes negative
/// outside a collection, `__gc_collect` runs and renews the budget.
pub(super) fn emit_gc_budget<S: Sink>(
    function: &mut S,
    helpers: Helpers<'_>,
    bytes: impl FnOnce(&mut S),
) {
    wasm!(function; GlobalGet(GC_DEBT_GLOBAL));
    bytes(function);
    wasm!(function;
        I32Sub, GlobalSet(GC_DEBT_GLOBAL),
        GlobalGet(GC_DEBT_GLOBAL), I32Const(0), I32LtS, GlobalGet(GC_BUSY_GLOBAL), I32Eqz, I32And,
        If(BlockType::Empty), Call(helpers.gc_collect()), End,
    );
}

/// Emit the collect-before-OOM step (§5.6 step 4) after a failed page take:
/// unless this request already collected (local `tried`) or a collection is
/// running, collect and branch to the retry loop `depth` labels out (counted
/// from the code that follows); else fall through to the out-of-memory exit.
pub(super) fn emit_collect_retry(
    function: &mut impl Sink,
    helpers: Helpers<'_>,
    tried: u32,
    depth: u32,
) {
    wasm!(function;
        LocalGet(tried), GlobalGet(GC_BUSY_GLOBAL), I32Or, I32Eqz,
        If(BlockType::Empty),
        I32Const(1), LocalSet(tried), Call(helpers.gc_collect()), Br(depth + 1),
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
/// slot `size`, the free-list `head`, the current `page`, its `bump`, the
/// collect-before-OOM flag `tried`). The order of §5.6: budget, class free
/// list (zeroing the slot), current page bump, a fresh page (zero) from
/// `__gc_take_pages`, else one collection and a retry, else the
/// out-of-memory exit.
fn emit_alloc_small(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32; 6]);
    let (class, ci, size, head, page, bump, tried) = (0, 1, 2, 3, 4, 5, 6);
    wasm!(f;
        LocalGet(class), I32Const(2), I32Shl, LocalTee(ci),
        I32Load(class_entry(abi::GC_CLASS_SIZES)), LocalSet(size),
    );
    emit_gc_budget(&mut f, helpers, |f| wasm!(f; LocalGet(size)));
    wasm!(f;
        Loop(BlockType::Empty),
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
    emit_collect_retry(&mut f, helpers, tried, 1);
    emit_oom_exit(&mut f);
    wasm!(f;
        End,
        LocalGet(page), I32Const(PAGE_SHIFT), I32ShrU, LocalGet(class),
        I32Store8(m8(abi::GC_PAGE_TABLE)),
        LocalGet(page), LocalGet(class), I32Store(m32(abi::GC_PAGE_CLASS_OFFSET)),
        LocalGet(page), LocalGet(page), I32Const(PAGE_HEADER), I32Add, LocalTee(bump),
        LocalGet(size), I32Add, I32Store(m32(abi::GC_PAGE_BUMP_OFFSET)),
        LocalGet(ci), LocalGet(page), I32Store(class_entry(abi::GC_CUR_PAGES)),
        LocalGet(bump), Return,
        End,
        Unreachable, End,
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

/// An `i64` [`MemArg`] at `offset` (`align` as given: `3` for an aligned
/// slot, `0` for the unaligned `TYPE_CELL` value).
fn m64(offset: u32, align: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align,
        memory_index: 0,
    }
}

/// The top 16 bits of a heap-reference jsval (`abi::HEAP >> 48`).
const HEAP_TAG: i64 = (abi::HEAP >> 48).cast_signed();

/// Emit: `__gc_visit` the jsval on the stack (through the `i64` scratch
/// `v`) when it is a heap reference (§5.4 jsval follow rule).
fn emit_visit_value(f: &mut impl Sink, helpers: Helpers<'_>, v: u32) {
    wasm!(f;
        LocalTee(v), I64Const(48), I64ShrU, I64Const(HEAP_TAG), I64Eq,
        If(BlockType::Empty), LocalGet(v), I32WrapI64, Call(helpers.gc_visit()), End,
    );
}

/// The static mark stack's end.
const MARK_STACK_END: i32 = (abi::GC_MARK_STACK + abi::GC_MARK_STACK_SIZE).cast_signed();

/// Emit `__gc_collect()` (§5.6 "Collection order", without the M3 weak
/// steps): set `$__gc_busy`, advance `$__gc_epoch` (`1..=255`), mark, then
/// release a paged mark stack, tombstone the intern table's unmarked heap
/// strings (slot value `1`), `__gc_sweep`, and renew the budget to
/// `max(GC_MIN_BUDGET, $__gc_live)` (`0` under `gc_stress`).
///
/// Marking is a sequence of passes. Each pass visits the roots (§5.4: the
/// shadow stack `[$__sp + 8, SS_SIZE)` as jsvals; `$exc_value`, `$cf_value`,
/// `$__this`, `$__new_target`, `$__spread_args` and the module cells as
/// jsvals; the promise queue and rejected list heads and the three render
/// buffers as raw refs; the grown intern table's own block, whose entries
/// stay weak) and traces every static root list block without marking it,
/// draining the mark stack after each group. A push that finds the stack
/// full and no page to grow into leaves its block unmarked and sets
/// `$__gc_overflow`; a pass that overflowed is followed by one that also
/// re-traces every marked heap block, until a pass completes without
/// overflow. The ephemeron fixpoint and the weak clearing of M3 (§6.2) slot
/// in between the last pass and the intern tombstones.
fn emit_collect(helpers: Helpers<'_>, leaves: &LeafSet, gc: GcLayout) -> Function {
    let mut f = helper_builder(leaves, &[], &[RAW_I32, RAW_I32, RAW_I32, RAW_I32, RAW_I64]);
    let (p, end, first, c, v) = (0, 1, 2, 3, 4);
    let hs = gc.heap_start;
    let min = abi::GC_MIN_BUDGET.cast_signed();
    let gc_byte = abi::HEADER_GC_OFFSET;
    let ss_end = abi::SHADOW_STACK_SIZE.cast_signed();
    wasm!(f;
        I32Const(1), GlobalSet(GC_BUSY_GLOBAL),
        GlobalGet(GC_EPOCH_GLOBAL), I32Const(255), I32RemU, I32Const(1), I32Add,
        GlobalSet(GC_EPOCH_GLOBAL),
        I32Const(abi::GC_MARK_STACK.cast_signed()), GlobalSet(GC_STACK_GLOBAL),
        I32Const(abi::GC_MARK_STACK.cast_signed()), GlobalSet(GC_STACK_TOP_GLOBAL),
        I32Const(MARK_STACK_END), GlobalSet(GC_STACK_END_GLOBAL),
        I32Const(1), LocalSet(first),
        Loop(BlockType::Empty),
        I32Const(0), GlobalSet(GC_OVERFLOW_GLOBAL),
        // The shadow stack.
        GlobalGet(SP_GLOBAL), I32Const(8), I32Add, LocalSet(p),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(p), I32Const(ss_end), I32GeU, BrIf(1),
        LocalGet(p), I64Load(m64(0, 3)),
    );
    emit_visit_value(&mut f, helpers, v);
    wasm!(f;
        LocalGet(p), I32Const(8), I32Add, LocalSet(p), Br(0),
        End, End,
    );
    let jsval_globals = [
        EXC_VALUE_GLOBAL,
        CF_VALUE_GLOBAL,
        THIS_GLOBAL,
        NEW_TARGET_GLOBAL,
        SPREAD_ARGS_GLOBAL,
    ];
    let cells = (0..gc.module_cells).map(|n| MODULE_CELL_GLOBAL_BASE + n);
    for global in jsval_globals.into_iter().chain(cells) {
        wasm!(f; GlobalGet(global));
        emit_visit_value(&mut f, helpers, v);
    }
    for global in [
        JOB_HEAD_GLOBAL,
        JOB_TAIL_GLOBAL,
        REJECTED_HEAD_GLOBAL,
        OUT_BUF_GLOBAL,
        PATH_BUF_GLOBAL,
        REFS_BUF_GLOBAL,
    ] {
        wasm!(f; GlobalGet(global), Call(helpers.gc_visit()));
    }
    wasm!(f;
        // The intern table's block once grown onto the heap (its data
        // follows an 8-byte `TYPE_RAW` header); the entries are weak.
        GlobalGet(INTERN_BASE_GLOBAL), I32Const(8), I32Sub, LocalTee(p), I32Const(hs), I32GeU,
        If(BlockType::Empty), LocalGet(p), Call(helpers.gc_visit()), End,
        Call(helpers.gc_drain()),
        // The static root list: traced, never marked.
        I32Const(gc.roots + 4), LocalSet(p),
        I32Const(gc.roots), I32Load(m32(0)), I32Const(2), I32Shl, LocalGet(p), I32Add,
        LocalSet(end),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(p), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(p), I32Load(m32(0)), Call(helpers.gc_trace()), Call(helpers.gc_drain()),
        LocalGet(p), I32Const(4), I32Add, LocalSet(p), Br(0),
        End, End,
        // After an overflow: re-trace every marked heap block.
        LocalGet(first), I32Eqz,
        If(BlockType::Empty),
    );
    emit_rescan(&mut f, helpers);
    wasm!(f;
        End,
        I32Const(0), LocalSet(first),
        GlobalGet(GC_OVERFLOW_GLOBAL), BrIf(0),
        End,
        // Return a paged mark stack to the pool.
        GlobalGet(GC_STACK_GLOBAL), I32Const(hs), I32GeU,
        If(BlockType::Empty),
        GlobalGet(GC_STACK_GLOBAL),
        GlobalGet(GC_STACK_END_GLOBAL), GlobalGet(GC_STACK_GLOBAL), I32Sub, I32Const(PAGE_SHIFT),
        I32ShrU,
        Call(helpers.gc_release_pages()),
        End,
        // Intern tombstones (§5.6): an unmarked heap string's slot becomes 1.
        GlobalGet(INTERN_BASE_GLOBAL), LocalTee(p),
        GlobalGet(INTERN_CAP_GLOBAL), I32Const(2), I32Shl, I32Add, LocalSet(end),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(p), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(p), I32Load(m32(0)), LocalTee(c), I32Const(hs), I32GeU,
        If(BlockType::Empty),
        LocalGet(c), I32Load8U(m8(gc_byte)), GlobalGet(GC_EPOCH_GLOBAL), I32Ne,
        If(BlockType::Empty), LocalGet(p), I32Const(1), I32Store(m32(0)), End,
        End,
        LocalGet(p), I32Const(4), I32Add, LocalSet(p), Br(0),
        End, End,
        Call(helpers.gc_sweep()),
    );
    if gc.stress {
        wasm!(f; I32Const(0), GlobalSet(GC_DEBT_GLOBAL));
    } else {
        wasm!(f;
            GlobalGet(GC_LIVE_GLOBAL), I32Const(min), GlobalGet(GC_LIVE_GLOBAL), I32Const(min),
            I32GtS, Select, GlobalSet(GC_DEBT_GLOBAL),
        );
    }
    wasm!(f; I32Const(0), GlobalSet(GC_BUSY_GLOBAL), End);
    finish_builder(f)
}

/// Emit the overflow rescan of [`emit_collect`]: `__gc_trace` (then
/// `__gc_drain`) every marked slot of every small page and every marked
/// large span. Uses `emit_collect`'s locals `0..=3` as scratch.
fn emit_rescan(f: &mut impl Sink, helpers: Helpers<'_>) {
    let (i, end, slot, limit) = (0, 1, 2, 3);
    let gc = abi::HEADER_GC_OFFSET;
    let classes = abi::GC_CLASS_COUNT.cast_signed();
    let large = abi::GC_LARGE_HEADER_SIZE.cast_signed();
    wasm!(f;
        I32Const(0), LocalSet(i),
        GlobalGet(HEAP_PTR_GLOBAL), I32Const(PAGE_SHIFT), I32ShrU, LocalSet(end),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(end), I32GeU, BrIf(1),
        LocalGet(i), I32Load8U(m8(abi::GC_PAGE_TABLE)), I32Const(1), I32Sub, I32Const(classes),
        I32LtU,
        If(BlockType::Empty),
        LocalGet(i), I32Const(PAGE_SHIFT), I32Shl, LocalTee(slot),
        I32Load(m32(abi::GC_PAGE_BUMP_OFFSET)), LocalSet(limit),
        LocalGet(slot), I32Const(PAGE_HEADER), I32Add, LocalSet(slot),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(slot), LocalGet(limit), I32GeU, BrIf(1),
        LocalGet(slot), I32Load8U(m8(0)), I32Const(TYPE_FREE), I32Ne,
        LocalGet(slot), I32Load8U(m8(gc)), GlobalGet(GC_EPOCH_GLOBAL), I32Eq, I32And,
        If(BlockType::Empty),
        LocalGet(slot), Call(helpers.gc_trace()), Call(helpers.gc_drain()),
        End,
        LocalGet(slot),
        LocalGet(i), I32Load8U(m8(abi::GC_PAGE_TABLE)), I32Const(2), I32Shl,
        I32Load(class_entry(abi::GC_CLASS_SIZES)),
        I32Add, LocalSet(slot), Br(0),
        End, End,
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i), Br(0),
        End, End,
        // The large spans.
        GlobalGet(GC_LARGE_GLOBAL), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), I32Eqz, BrIf(1),
        LocalGet(i), I32Load8U(m8(abi::GC_LARGE_HEADER_SIZE + gc)), GlobalGet(GC_EPOCH_GLOBAL),
        I32Eq,
        If(BlockType::Empty),
        LocalGet(i), I32Const(large), I32Add, Call(helpers.gc_trace()), Call(helpers.gc_drain()),
        End,
        LocalGet(i), I32Load(m32(abi::GC_LARGE_NEXT_OFFSET)), LocalSet(i), Br(0),
        End, End,
    );
}

/// Emit `__gc_visit(ref: i32)`: reach the block at raw ref `ref` (`0` = none).
///
/// - A heap block (a small slot start or the block of a large span head,
///   per the page table) is marked with the epoch and pushed, unless
///   already marked.
/// - An interior heap address holding a `TYPE_PROPS` header (the inline
///   table of an object literal or error) and a static `TYPE_PROPS` /
///   `TYPE_ELEMS` (the table of a static object-family block) are pushed
///   without marking: each is reached only through its owner, so it is
///   traced once per owner trace and no stale mark can hide it.
/// - Everything else (static strings, symbols, accessors, functions and
///   intrinsics, which the static root list covers; addresses outside the
///   heap) is ignored.
///
/// A full stack moves to `max(1, 2 * size >> 16)` fresh pages from
/// `__gc_take_pages` (copying, and releasing paged storage); when none are
/// left the block stays unmarked and `$__gc_overflow` is set (§5.4).
fn emit_visit(helpers: Helpers<'_>, leaves: &LeafSet, gc: GcLayout) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32; 5]);
    let (r, code, mark, used, cap, fresh) = (0, 1, 2, 3, 4, 5);
    let hs = gc.heap_start;
    let gc_byte = abi::HEADER_GC_OFFSET;
    let props = i32::from(abi::TYPE_PROPS);
    let classes = abi::GC_CLASS_COUNT.cast_signed();
    let memory_end = i32::try_from(abi::MEMORY_MAX_PAGES << PAGE_SHIFT).unwrap_or(i32::MAX);
    wasm!(f;
        Block(BlockType::Empty),
        Block(BlockType::Empty),
        // The static region: only a static PROPS / ELEMS table is pushed.
        LocalGet(r), I32Const(hs), I32LtU,
        If(BlockType::Empty),
        LocalGet(r), I32Const(HEAP_BASE), I32LtU, BrIf(2),
        LocalGet(r), I32Load8U(m8(0)), I32Const(props), I32Sub, I32Const(1), I32GtU, BrIf(2),
        Br(1),
        End,
        LocalGet(r), I32Const(memory_end), I32GeU, BrIf(1),
        LocalGet(r), I32Const(PAGE_SHIFT), I32ShrU, I32Load8U(m8(abi::GC_PAGE_TABLE)), LocalTee(code),
        I32Const(i32::from(abi::GC_PAGE_LARGE_HEAD)), I32Eq,
        If(BlockType::Empty),
        LocalGet(r), I32Const(PAGE_SIZE - 1), I32And, I32Const(PAGE_HEADER), I32Eq, LocalSet(mark),
        Else,
        LocalGet(code), I32Const(1), I32Sub, I32Const(classes), I32GeU, BrIf(2),
        LocalGet(r), I32Const(PAGE_SIZE - 1), I32And, I32Const(PAGE_HEADER), I32Sub, LocalTee(used),
        LocalGet(code), I32Const(2), I32Shl, I32Load(class_entry(abi::GC_CLASS_SIZES)), I32RemU,
        I32Eqz, LocalGet(used), I32Const(0), I32GeS, I32And, LocalSet(mark),
        End,
        LocalGet(mark),
        If(BlockType::Empty),
        LocalGet(r), I32Load8U(m8(gc_byte)), GlobalGet(GC_EPOCH_GLOBAL), I32Eq, BrIf(2),
        Else,
        LocalGet(r), I32Load8U(m8(0)), I32Const(props), I32Ne, BrIf(2),
        End,
        End,
        // Push `r`, growing the stack when full.
        GlobalGet(GC_STACK_TOP_GLOBAL), GlobalGet(GC_STACK_END_GLOBAL), I32Eq,
        If(BlockType::Empty),
        GlobalGet(GC_STACK_TOP_GLOBAL), GlobalGet(GC_STACK_GLOBAL), I32Sub, LocalSet(used),
        GlobalGet(GC_STACK_END_GLOBAL), GlobalGet(GC_STACK_GLOBAL), I32Sub, LocalTee(cap),
        I32Const(1), I32Shl, I32Const(PAGE_SHIFT), I32ShrU, LocalTee(code),
        I32Const(1), LocalGet(code), I32Const(1), I32GtU, Select, LocalTee(code),
        Call(helpers.gc_take_pages()), LocalTee(fresh), I32Eqz,
        If(BlockType::Empty),
        I32Const(1), GlobalSet(GC_OVERFLOW_GLOBAL), Br(2),
        End,
        LocalGet(fresh), GlobalGet(GC_STACK_GLOBAL), LocalGet(used), MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        },
        GlobalGet(GC_STACK_GLOBAL), I32Const(hs), I32GeU,
        If(BlockType::Empty),
        GlobalGet(GC_STACK_GLOBAL), LocalGet(cap), I32Const(PAGE_SHIFT), I32ShrU,
        Call(helpers.gc_release_pages()),
        End,
        LocalGet(fresh), GlobalSet(GC_STACK_GLOBAL),
        LocalGet(fresh), LocalGet(used), I32Add, GlobalSet(GC_STACK_TOP_GLOBAL),
        LocalGet(fresh), LocalGet(code), I32Const(PAGE_SHIFT), I32Shl, I32Add,
        GlobalSet(GC_STACK_END_GLOBAL),
        End,
        LocalGet(mark),
        If(BlockType::Empty), LocalGet(r), GlobalGet(GC_EPOCH_GLOBAL), I32Store8(m8(gc_byte)), End,
        GlobalGet(GC_STACK_TOP_GLOBAL), LocalGet(r), I32Store(m32(0)),
        GlobalGet(GC_STACK_TOP_GLOBAL), I32Const(4), I32Add, GlobalSet(GC_STACK_TOP_GLOBAL),
        End,
        End,
    );
    finish_builder(f)
}

/// Offset of a `CLASS_ITERATOR` object's iterated value / first payload
/// jsval (`emit/builtins/iter.rs`).
const ITER_ITERATED_OFFSET: u32 = abi::OBJ_PREFIX_SIZE;
/// Offset of a `CLASS_ITERATOR` object's `kind` byte.
const ITER_KIND_OFFSET: u32 = abi::OBJ_PREFIX_SIZE + 12;
/// The first `kind` with the wrapped record `[[Iterator]]` @32 /
/// `[[NextMethod]]` @40: the `Iterator.from` wrapper (`16`), the lazy
/// helpers (`17..=21`, `emit/builtins/iterator.rs`) and the async-from-sync
/// iterator (`32`, `emit/builtins/async_iter.rs`).
const ITER_RECORD_KIND: i32 = 16;
/// The first lazy-helper `kind`; helpers also hold the callback @56 and the
/// `flatMap` inner record @64 / @72 (`@48` is an `f64` counter).
const ITER_HELPER_KIND: i32 = 17;
/// The number of lazy-helper kinds.
const ITER_HELPER_KINDS: i32 = 5;

/// Emit `__gc_trace(block: i32)`: `__gc_visit` every reference of the block
/// per §5.4's per-TYPE table (jsval fields by the follow rule, raw `u32`
/// fields as refs).
#[allow(clippy::too_many_lines)] // one straight-line dispatch over the types
fn emit_trace(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[RAW_I32], &[RAW_I32, RAW_I32, RAW_I32, RAW_I64]);
    let (b, ty, at, end, v) = (0, 1, 2, 3, 4);
    let visit = helpers.gc_visit();
    let jsval = |f: &mut super::FnBuilder<'_>, offset: u32| {
        wasm!(f; LocalGet(b), I64Load(m64(offset, 3)));
        emit_visit_value(f, helpers, v);
    };
    let raw = |f: &mut super::FnBuilder<'_>, offset: u32| {
        wasm!(f; LocalGet(b), I32Load(m32(offset)), Call(visit));
    };
    // The `count`-word-sized run of entries of `stride` bytes from offset
    // `first`, each with a jsval at `+ value` and, for a table, a raw key at
    // `+ 0`.
    let run = |f: &mut super::FnBuilder<'_>, first: u32, shift: i32, keyed: bool| {
        wasm!(f;
            LocalGet(b), I32Const(first.cast_signed()), I32Add, LocalTee(at),
            LocalGet(b), I32Load(m32(4)), I32Const(shift), I32Shl, I32Add, LocalSet(end),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(at), LocalGet(end), I32GeU, BrIf(1),
        );
        if keyed {
            wasm!(f;
                LocalGet(at), I32Load(m32(abi::PROP_KEY_OFFSET)), Call(visit),
                LocalGet(at), I64Load(m64(abi::PROP_VALUE_OFFSET, 3)),
            );
        } else {
            wasm!(f; LocalGet(at), I64Load(m64(0, 3)));
        }
        emit_visit_value(f, helpers, v);
        wasm!(f;
            LocalGet(at), I32Const(1 << shift), I32Add, LocalSet(at), Br(0),
            End, End,
        );
    };
    let is = |f: &mut super::FnBuilder<'_>, kind: u8| {
        wasm!(f; LocalGet(ty), I32Const(i32::from(kind)), I32Eq, If(BlockType::Empty));
    };
    wasm!(f; LocalGet(b), I32Load8U(m8(0)), LocalSet(ty));
    // The non-object kinds.
    is(&mut f, abi::TYPE_ENV);
    run(&mut f, 8, 3, false);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_ELEMS);
    run(&mut f, abi::ELEMS_DATA_OFFSET, 3, false);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_PROPS);
    run(&mut f, abi::PROPS_HEADER_SIZE, 4, true);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_CELL);
    wasm!(f; LocalGet(b), I64Load(m64(4, 0)));
    emit_visit_value(&mut f, helpers, v);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_ACCESSOR);
    jsval(&mut f, abi::ACCESSOR_GET_OFFSET);
    jsval(&mut f, abi::ACCESSOR_SET_OFFSET);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_SYMBOL);
    jsval(&mut f, abi::SYMBOL_DESCRIPTION_OFFSET);
    wasm!(f; Return, End);
    // Strings, raw buffers and free slots are leaves; the object family
    // (`1..=3`, `6..=10`) starts with `props` and `proto`.
    wasm!(f;
        LocalGet(ty), I32Eqz, LocalGet(ty), I32Const(i32::from(abi::TYPE_DATE)), I32GtU, I32Or,
        If(BlockType::Empty), Return, End,
    );
    raw(&mut f, abi::OBJ_PROPS_OFFSET);
    jsval(&mut f, abi::OBJ_PROTO_OFFSET);
    is(&mut f, abi::TYPE_ARRAY);
    raw(&mut f, abi::ARRAY_ELEMS_OFFSET);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_FUNCTION);
    jsval(&mut f, abi::FUNC_ENV_OFFSET);
    wasm!(f; Return, End);
    wasm!(f;
        LocalGet(ty), I32Const(i32::from(abi::TYPE_MAP)), I32Sub, I32Const(1), I32LeU,
        If(BlockType::Empty),
    );
    jsval(&mut f, abi::MAP_ENTRIES_OFFSET);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_PROMISE);
    raw(&mut f, abi::PROMISE_REACTIONS_OFFSET);
    jsval(&mut f, abi::PROMISE_VALUE_OFFSET);
    raw(&mut f, abi::PROMISE_REJECTED_NEXT_OFFSET);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_GENERATOR);
    jsval(&mut f, abi::GEN_ENV_OFFSET);
    raw(&mut f, abi::GEN_QUEUE_OFFSET);
    raw(&mut f, abi::GEN_QUEUE_OFFSET + 4);
    wasm!(f; Return, End);
    is(&mut f, abi::TYPE_OBJECT);
    // `ty` now holds the class byte, `at` the iterator kind.
    wasm!(f;
        LocalGet(b), I32Load8U(m8(abi::HEADER_CLASS_OFFSET)), LocalTee(ty),
        I32Const(i32::from(abi::CLASS_BOOLEAN)), I32Sub, I32Const(2), I32LeU,
        LocalGet(ty), I32Const(i32::from(abi::CLASS_SYMBOL)), I32Eq, I32Or,
        If(BlockType::Empty),
    );
    jsval(&mut f, abi::WRAPPER_VALUE_OFFSET);
    wasm!(f;
        End,
        LocalGet(ty), I32Const(i32::from(abi::CLASS_ITERATOR)), I32Eq,
        If(BlockType::Empty),
    );
    jsval(&mut f, ITER_ITERATED_OFFSET);
    wasm!(f;
        LocalGet(b), I32Load8U(m8(ITER_KIND_OFFSET)), LocalTee(at), I32Const(ITER_RECORD_KIND),
        I32GeU,
        If(BlockType::Empty),
    );
    jsval(&mut f, 32);
    jsval(&mut f, 40);
    wasm!(f;
        End,
        LocalGet(at), I32Const(ITER_HELPER_KIND), I32Sub, I32Const(ITER_HELPER_KINDS), I32LtU,
        If(BlockType::Empty),
    );
    jsval(&mut f, 56);
    jsval(&mut f, 64);
    jsval(&mut f, 72);
    wasm!(f;
        End,
        End,
        End,
        End,
    );
    finish_builder(f)
}

/// Emit `__gc_drain()`: pop and `__gc_trace` mark-stack entries until the
/// stack is empty.
fn emit_drain(helpers: Helpers<'_>, leaves: &LeafSet) -> Function {
    let mut f = helper_builder(leaves, &[], &[]);
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        GlobalGet(GC_STACK_TOP_GLOBAL), GlobalGet(GC_STACK_GLOBAL), I32LeU, BrIf(1),
        GlobalGet(GC_STACK_TOP_GLOBAL), I32Const(4), I32Sub, GlobalSet(GC_STACK_TOP_GLOBAL),
        GlobalGet(GC_STACK_TOP_GLOBAL), I32Load(m32(0)), Call(helpers.gc_trace()),
        Br(0),
        End, End,
        End,
    );
    finish_builder(f)
}
