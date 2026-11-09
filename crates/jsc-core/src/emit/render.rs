//! The guest console renderer and output writer (design/runtime-memory.md
//! §2.3, §3.1): a port of the host formatter that appends the console form of
//! a value to `$__out_buf` as UTF-8 and issues one `fd_write` per line.
//!
//! Rendering runs no user code and never throws. The output buffer, the
//! recursion path and the ref table are headerless `__alloc` blocks, the raw
//! form `__intern` growth uses until `TYPE_RAW` arrives.

use wasm_encoder::{BlockType, Function, Instruction, MemArg, ValType};

use super::{
    CONVERT_OP_TO_STRING, FD_WRITE_IMPORT, HEAP_BITS, Helpers, INVALID_DATE, OUT_BUF_GLOBAL,
    OUT_CAP_GLOBAL, OUT_LEN_GLOBAL, OWN_KEYS, PATH_BUF_GLOBAL, PATH_CAP_GLOBAL, PROC_EXIT_IMPORT,
    QNAN_BITS, REFS_BUF_GLOBAL, REFS_CAP_GLOBAL, REFS_LEN_GLOBAL, StrOffsets, abi, emit_date_field,
    emit_str_two_byte, emit_str_unit, emit_surrogate_pair_at, static_key, unicode,
};
use crate::ir::DateOp;

/// Emit each listed instruction in order.
macro_rules! wasm {
    ($function:expr; $($op:expr),* $(,)?) => {{
        #[allow(unused_imports)]
        use Instruction::*;
        $( $function.instruction(&$op); )*
    }};
}

/// A byte [`MemArg`] at `offset`.
fn m8(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 0,
        memory_index: 0,
    }
}

/// A `u32` [`MemArg`] at `offset`.
fn m32(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 2,
        memory_index: 0,
    }
}

/// An `i64` [`MemArg`] at `offset`.
fn m64(offset: u32) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: 3,
        memory_index: 0,
    }
}

const COPY: Instruction<'static> = Instruction::MemoryCopy {
    src_mem: 0,
    dst_mem: 0,
};

const HEAP_TAG: i32 = 0b101;
const TAG_SHIFT: i64 = 48;
const NEGATIVE_ZERO: i64 = i64::MIN;
const F64_EXP_MASK: i64 = 0x7FF0_0000_0000_0000;
const HOLE_BITS: i64 = abi::HOLE.cast_signed();
const UNDEFINED_BITS: i64 = abi::UNDEFINED.cast_signed();
const ACCESSOR: i32 = abi::ATTR_ACCESSOR.cast_signed();
const ENUMERABLE: i32 = abi::ATTR_ENUMERABLE.cast_signed();
const ENTRY: i32 = abi::PROP_ENTRY_SIZE.cast_signed();
const HEADER: i32 = abi::PROPS_HEADER_SIZE.cast_signed();
const INDEX_FLAG: i32 = abi::STR_FLAG_ARRAY_INDEX as i32;
const HAS_INDEX: i32 = abi::PROPS_HAS_INDEX_KEYS as i32;
const HAS_SYMBOL: i32 = abi::PROPS_HAS_SYMBOL_KEYS as i32;
const TYPE_STRING: i32 = abi::TYPE_STRING as i32;
const TYPE_ARRAY: i32 = abi::TYPE_ARRAY as i32;
const TYPE_OBJECT: i32 = abi::TYPE_OBJECT as i32;
const TYPE_FUNCTION: i32 = abi::TYPE_FUNCTION as i32;
const TYPE_PROMISE: i32 = abi::TYPE_PROMISE as i32;
const TYPE_MAP: i32 = abi::TYPE_MAP as i32;
const TYPE_SET: i32 = abi::TYPE_SET as i32;
const TYPE_GENERATOR: i32 = abi::TYPE_GENERATOR as i32;
const TYPE_DATE: i32 = abi::TYPE_DATE as i32;
const TYPE_SYMBOL: i32 = abi::TYPE_SYMBOL as i32;

const OPEN_ARRAY: &str = "[ ";
const OPEN_OBJECT: &str = "{ ";
const OPEN_TRAILING: &str = " { ";
const CLOSE_OBJECT: &str = " }";

/// The static strings the renderer writes or looks properties up by.
pub(super) const STRINGS: [&str; 56] = [
    "",
    "true",
    "false",
    "null",
    "undefined",
    "[]",
    "{}",
    CLOSE_OBJECT,
    " ]",
    ", ",
    ": ",
    OPEN_ARRAY,
    OPEN_OBJECT,
    OPEN_TRAILING,
    "[",
    "]",
    "'",
    "[class ",
    "(anonymous)",
    " extends ",
    "[Function (anonymous)]",
    "[Function: ",
    "Symbol(",
    ")",
    "<ref *",
    "> ",
    "[Circular *",
    "<",
    " empty item",
    "s",
    ">",
    "[Arguments] ",
    "[String: '",
    "']",
    "[Number: -0]",
    "[Number: ",
    "[Boolean: ",
    "[Symbol: ",
    "Promise { <pending> }",
    "Promise { ",
    "Promise { <rejected> ",
    "Map(",
    "Set(",
    "Object [Generator]",
    "Uncaught ",
    "Uncaught (in promise) ",
    INVALID_DATE,
    "name",
    "message",
    "constructor",
    "Object",
    "Error",
    " ",
    "[Getter/Setter]",
    "[Setter]",
    "[Getter]",
];

use ValType::{I32, I64};

/// The `(params, results)` of the render helpers, in function-index order,
/// then `_start`.
pub(super) const TYPES: [(&[ValType], &[ValType]); 24] = [
    (&[I32], &[]),                             // __out_reserve(need)
    (&[I32], &[]),                             // __path_reserve(need)
    (&[I32], &[]),                             // __refs_reserve(need)
    (&[I32], &[]),                             // __out_byte(b)
    (&[I32], &[]),                             // __out_cp(cp)
    (&[I32], &[]),                             // __out_str(s)
    (&[I32], &[]),                             // __out_uint(n)
    (&[I32], &[]),                             // __out_flush(fd)
    (&[I32, I32, I64], &[]),                   // __report(fd, prefix, v)
    (&[I64], &[]),                             // __render_top(v)
    (&[I64, I32], &[]),                        // __render(v, depth_path)
    (&[I32, I32], &[]),                        // __render_heap(p, depth)
    (&[I32, I32], &[]),                        // __render_array(p, depth)
    (&[I32, I32], &[]),                        // __render_object(p, depth)
    (&[I32, I32], &[]),                        // __render_function(p, depth)
    (&[I32, I32, I32, I32, I32, I32], &[I32]), // __render_props(..) -> first
    (&[I32, I32, I32, I32, I32], &[]),         // __render_entry(e, quote, first, open, depth)
    (&[I32, I32], &[]),                        // __render_key(key, quote)
    (&[I32, I32], &[I32]),                     // __render_lookup(o, key) -> entry
    (&[I64], &[I32]),                          // __heap_object(v) -> p
    (&[I64], &[I32]),                          // __str_of(v) -> s
    (&[I64], &[I32]),                          // __fn_name(v) -> s
    (&[I64], &[I64]),                          // __date_text(v) -> s
    (&[], &[]),                                // _start
];

impl Helpers<'_> {
    fn render_base(self) -> u32 {
        self.to_primitive() + 26
    }
    fn out_reserve(self) -> u32 {
        self.render_base()
    }
    fn path_reserve(self) -> u32 {
        self.render_base() + 1
    }
    fn refs_reserve(self) -> u32 {
        self.render_base() + 2
    }
    /// `__out_byte(b)`.
    pub(super) fn out_byte(self) -> u32 {
        self.render_base() + 3
    }
    fn out_cp(self) -> u32 {
        self.render_base() + 4
    }
    fn out_str(self) -> u32 {
        self.render_base() + 5
    }
    fn out_uint(self) -> u32 {
        self.render_base() + 6
    }
    /// `__out_flush(fd)`: write the whole buffer to `fd` and reset it.
    pub(super) fn out_flush(self) -> u32 {
        self.render_base() + 7
    }
    /// `__report(fd, prefix, v)`: one `prefix + render(v) + "\n"` line on `fd`.
    pub(super) fn report(self) -> u32 {
        self.render_base() + 8
    }
    /// `__render_top(v)`: append the console form of `v` to the buffer.
    pub(super) fn render_top(self) -> u32 {
        self.render_base() + 9
    }
    fn render(self) -> u32 {
        self.render_base() + 10
    }
    fn render_heap(self) -> u32 {
        self.render_base() + 11
    }
    fn render_array(self) -> u32 {
        self.render_base() + 12
    }
    fn render_object(self) -> u32 {
        self.render_base() + 13
    }
    fn render_function(self) -> u32 {
        self.render_base() + 14
    }
    fn render_props(self) -> u32 {
        self.render_base() + 15
    }
    fn render_entry(self) -> u32 {
        self.render_base() + 16
    }
    fn render_key(self) -> u32 {
        self.render_base() + 17
    }
    fn render_lookup(self) -> u32 {
        self.render_base() + 18
    }
    fn heap_object(self) -> u32 {
        self.render_base() + 19
    }
    fn str_of(self) -> u32 {
        self.render_base() + 20
    }
    fn fn_name(self) -> u32 {
        self.render_base() + 21
    }
    fn date_text(self) -> u32 {
        self.render_base() + 22
    }
    /// The exported `_start`; the Unicode helpers follow it.
    pub(super) fn start(self) -> u32 {
        self.render_base() + 23
    }
}

/// Every render helper body, in [`TYPES`] order (without `_start`).
pub(super) fn emit_helpers(strings: &StrOffsets, helpers: Helpers<'_>) -> Vec<Function> {
    let alloc = helpers.alloc;
    vec![
        emit_reserve(OUT_BUF_GLOBAL, OUT_CAP_GLOBAL, alloc),
        emit_reserve(PATH_BUF_GLOBAL, PATH_CAP_GLOBAL, alloc),
        emit_reserve(REFS_BUF_GLOBAL, REFS_CAP_GLOBAL, alloc),
        emit_out_byte(helpers),
        emit_out_cp(helpers),
        emit_out_str(helpers),
        emit_out_uint(helpers),
        emit_out_flush(),
        emit_report(helpers),
        emit_render_top(helpers),
        emit_render(strings, helpers),
        emit_render_heap(strings, helpers),
        emit_render_array(strings, helpers),
        emit_render_object(strings, helpers),
        emit_render_function(strings, helpers),
        emit_render_props(helpers),
        emit_render_entry(strings, helpers),
        emit_render_key(strings, helpers),
        emit_render_lookup(helpers),
        emit_heap_object(),
        emit_str_of(),
        emit_fn_name(helpers),
        emit_date_text(strings, helpers),
    ]
}

/// Append the static string `text`.
fn lit(f: &mut Function, strings: &StrOffsets, helpers: Helpers<'_>, text: &str) {
    wasm!(f; I32Const(static_key(strings, text)), Call(helpers.out_str()));
}

/// `reserve(need: i32)`: grow the raw block in `base`/`cap` to at least `need`
/// bytes, keeping its contents. `max(need, 2 * cap, 64)` bytes when it grows.
fn emit_reserve(base: u32, cap: u32, alloc: u32) -> Function {
    let mut f = Function::new(vec![(2, I32)]);
    let (need, block, size) = (0, 1, 2);
    wasm!(f;
        LocalGet(need), GlobalGet(cap), I32LeU, If(BlockType::Empty), Return, End,
        GlobalGet(cap), I32Const(1), I32Shl, LocalTee(size),
        I32Const(64), LocalGet(size), I32Const(64), I32GtU, Select, LocalTee(size),
        LocalGet(need), LocalGet(size), LocalGet(need), I32GtU, Select, LocalTee(size),
        Call(alloc), LocalSet(block),
        GlobalGet(cap), If(BlockType::Empty),
        LocalGet(block), GlobalGet(base), GlobalGet(cap), COPY,
        End,
        LocalGet(block), GlobalSet(base),
        LocalGet(size), GlobalSet(cap),
        End,
    );
    f
}

/// `__out_byte(b)`.
fn emit_out_byte(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![]);
    wasm!(f;
        GlobalGet(OUT_LEN_GLOBAL), I32Const(1), I32Add, Call(helpers.out_reserve()),
        GlobalGet(OUT_BUF_GLOBAL), GlobalGet(OUT_LEN_GLOBAL), I32Add, LocalGet(0), I32Store8(m8(0)),
        GlobalGet(OUT_LEN_GLOBAL), I32Const(1), I32Add, GlobalSet(OUT_LEN_GLOBAL),
        End,
    );
    f
}

/// `__out_cp(cp)`: append the UTF-8 encoding of the scalar value `cp`.
fn emit_out_cp(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, I32)]);
    let (cp, p, n) = (0, 1, 2);
    // Store `lead | (cp >> shift & mask)` at byte `at` of the sequence.
    let unit = |f: &mut Function, at: u32, lead: i32, shift: i32, mask: i32| {
        wasm!(f;
            LocalGet(p), LocalGet(cp), I32Const(shift), I32ShrU, I32Const(mask), I32And,
            I32Const(lead), I32Or, I32Store8(m8(at)),
        );
    };
    wasm!(f;
        GlobalGet(OUT_LEN_GLOBAL), I32Const(4), I32Add, Call(helpers.out_reserve()),
        GlobalGet(OUT_BUF_GLOBAL), GlobalGet(OUT_LEN_GLOBAL), I32Add, LocalSet(p),
        LocalGet(cp), I32Const(0x80), I32LtU, If(BlockType::Empty),
    );
    unit(&mut f, 0, 0, 0, 0x7F);
    wasm!(f; I32Const(1), LocalSet(n), Else, LocalGet(cp), I32Const(0x800), I32LtU, If(BlockType::Empty));
    unit(&mut f, 0, 0xC0, 6, 0x1F);
    unit(&mut f, 1, 0x80, 0, 0x3F);
    wasm!(f; I32Const(2), LocalSet(n), Else, LocalGet(cp), I32Const(0x1_0000), I32LtU, If(BlockType::Empty));
    unit(&mut f, 0, 0xE0, 12, 0x0F);
    unit(&mut f, 1, 0x80, 6, 0x3F);
    unit(&mut f, 2, 0x80, 0, 0x3F);
    wasm!(f; I32Const(3), LocalSet(n), Else);
    unit(&mut f, 0, 0xF0, 18, 0x07);
    unit(&mut f, 1, 0x80, 12, 0x3F);
    unit(&mut f, 2, 0x80, 6, 0x3F);
    unit(&mut f, 3, 0x80, 0, 0x3F);
    wasm!(f;
        I32Const(4), LocalSet(n), End, End, End,
        GlobalGet(OUT_LEN_GLOBAL), LocalGet(n), I32Add, GlobalSet(OUT_LEN_GLOBAL),
        End,
    );
    f
}

/// `__out_str(s)`: append the string block `s` as UTF-8, a lone surrogate as
/// U+FFFD.
fn emit_out_str(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, I32)]);
    let (s, count, at, wide, unit, next) = (0, 1, 2, 3, 4, 5);
    wasm!(f; LocalGet(s), I32Load(m32(abi::STR_LEN_OFFSET)), LocalSet(count));
    emit_str_two_byte(&mut f, s);
    wasm!(f; LocalSet(wide), Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(count), I32GeU, BrIf(1),
    );
    emit_str_unit(&mut f, s, at, wide);
    wasm!(f;
        LocalTee(unit), I32Const(0xF800), I32And, I32Const(0xD800), I32Eq,
        If(BlockType::Empty),
    );
    emit_surrogate_pair_at(&mut f, s, at, count);
    wasm!(f; If(BlockType::Empty), LocalGet(at), I32Const(1), I32Add, LocalSet(next));
    wasm!(f;
        LocalGet(unit), I32Const(0xD800), I32Sub, I32Const(10), I32Shl,
    );
    emit_str_unit(&mut f, s, next, wide);
    wasm!(f;
        I32Const(0xDC00), I32Sub, I32Add, I32Const(0x1_0000), I32Add, LocalSet(unit),
        LocalGet(next), LocalSet(at),
        Else, I32Const(0xFFFD), LocalSet(unit), End,
        End,
        LocalGet(unit), Call(helpers.out_cp()),
        LocalGet(at), I32Const(1), I32Add, LocalSet(at),
        Br(0), End, End,
        End,
    );
    f
}

/// `__out_uint(n)`: append the decimal digits of the unsigned `n`.
fn emit_out_uint(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![]);
    wasm!(f;
        LocalGet(0), I32Const(10), I32GeU, If(BlockType::Empty),
        LocalGet(0), I32Const(10), I32DivU, Call(helpers.out_uint()),
        End,
        LocalGet(0), I32Const(10), I32RemU, I32Const(0x30), I32Add, Call(helpers.out_byte()),
        End,
    );
    f
}

/// `__out_flush(fd)`: write the whole buffer, retrying a partial write, and
/// reset it. An errno, or no progress, ends the program with `proc_exit(1)`.
fn emit_out_flush() -> Function {
    let mut f = Function::new(vec![(2, I32)]);
    let (fd, p, left) = (0, 1, 2);
    let iov = abi::WASI_SCRATCH.cast_signed();
    let nwritten = abi::WASI_NWRITTEN.cast_signed();
    wasm!(f;
        GlobalGet(OUT_BUF_GLOBAL), LocalSet(p),
        GlobalGet(OUT_LEN_GLOBAL), LocalSet(left),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(left), I32Eqz, BrIf(1),
        I32Const(iov), LocalGet(p), I32Store(m32(0)),
        I32Const(iov), LocalGet(left), I32Store(m32(4)),
        LocalGet(fd), I32Const(iov), I32Const(1), I32Const(nwritten), Call(FD_WRITE_IMPORT),
        I32Const(nwritten), I32Load(m32(0)), I32Eqz, I32Or,
        If(BlockType::Empty), I32Const(1), Call(PROC_EXIT_IMPORT), Unreachable, End,
        I32Const(nwritten), I32Load(m32(0)), LocalGet(p), I32Add, LocalSet(p),
        LocalGet(left), I32Const(nwritten), I32Load(m32(0)), I32Sub, LocalSet(left),
        Br(0), End, End,
        I32Const(0), GlobalSet(OUT_LEN_GLOBAL),
        End,
    );
    f
}

/// `__report(fd, prefix, v)`: a line left unfinished on stdout by a throwing
/// `console.log` argument goes out first, then `prefix + render(v) + "\n"`.
fn emit_report(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![]);
    let (fd, prefix, v) = (0, 1, 2);
    wasm!(f;
        I32Const(1), Call(helpers.out_flush()),
        LocalGet(prefix), Call(helpers.out_str()),
        LocalGet(v), Call(helpers.render_top()),
        I32Const(0x0A), Call(helpers.out_byte()),
        LocalGet(fd), Call(helpers.out_flush()),
        End,
    );
    f
}

/// `__render_top(v)`: render one value with a fresh ref numbering.
fn emit_render_top(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![]);
    wasm!(f;
        I32Const(0), GlobalSet(REFS_LEN_GLOBAL),
        LocalGet(0), I32Const(0), Call(helpers.render()),
        End,
    );
    f
}

/// Push the 3-bit NaN-box tag of the boxed jsval in i64 local `v`, as i32.
fn tag_of(f: &mut Function, v: u32) {
    wasm!(f; LocalGet(v), I64Const(TAG_SHIFT), I64ShrU, I64Const(7), I64And, I32WrapI64);
}

/// `__render(v, depth_path)`: `depth_path` is the number of containers on the
/// recursion path. A number, and a boxed value of an unused tag, renders
/// through `__convert` (decode reads those as numbers).
fn emit_render(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(1, I32)]);
    let (v, depth, tag) = (0, 1, 2);
    wasm!(f; LocalGet(v), I64Const(QNAN_BITS), I64And, I64Const(QNAN_BITS), I64Eq, If(BlockType::Empty));
    tag_of(&mut f, v);
    wasm!(f; LocalSet(tag), LocalGet(tag), I32Const(1), I32Eq, If(BlockType::Empty));
    wasm!(f; LocalGet(v), I64Const(1), I64And, I64Eqz, If(BlockType::Empty));
    lit(&mut f, strings, helpers, "false");
    wasm!(f; Else);
    lit(&mut f, strings, helpers, "true");
    wasm!(f; End, Return, End, LocalGet(tag), I32Const(2), I32Eq, If(BlockType::Empty));
    lit(&mut f, strings, helpers, "null");
    wasm!(f; Return, End, LocalGet(tag), I32Const(3), I32Eq, If(BlockType::Empty));
    lit(&mut f, strings, helpers, "undefined");
    wasm!(f;
        Return, End,
        LocalGet(tag), I32Const(HEAP_TAG), I32Eq, If(BlockType::Empty),
        LocalGet(v), I32WrapI64, LocalGet(depth), Call(helpers.render_heap()), Return,
        End,
        End,
        LocalGet(v), I32Const(CONVERT_OP_TO_STRING), Call(helpers.convert), I32WrapI64,
        Call(helpers.out_str()),
        End,
    );
    f
}

/// Render the container `p` at `depth` by its type byte in `ty`.
fn render_container(f: &mut Function, helpers: Helpers<'_>, p: u32, ty: u32, depth: u32) {
    let dispatch = |f: &mut Function, kind: i32, helper: u32| {
        wasm!(f;
            LocalGet(ty), I32Const(kind), I32Eq, If(BlockType::Empty),
            LocalGet(p), LocalGet(depth), I32Const(1), I32Add, Call(helper),
            End,
        );
    };
    dispatch(f, TYPE_ARRAY, helpers.render_array());
    dispatch(f, TYPE_OBJECT, helpers.render_object());
    dispatch(f, TYPE_FUNCTION, helpers.render_function());
}

/// The promise arm of `__render_heap`: `Promise { .. }` by settlement.
fn render_promise(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    p: u32,
    ty: u32,
    depth: u32,
) {
    wasm!(f; LocalGet(ty), I32Const(TYPE_PROMISE), I32Eq, If(BlockType::Empty));
    let state = |f: &mut Function, value: i32| {
        wasm!(f;
            LocalGet(p), I32Load(m32(abi::PROMISE_STATE_OFFSET)), I32Const(value), I32Eq,
            If(BlockType::Empty),
        );
    };
    let body = |f: &mut Function, open: &str| {
        lit(f, strings, helpers, open);
        wasm!(f;
            LocalGet(p), I64Load(m64(abi::PROMISE_VALUE_OFFSET)),
            LocalGet(depth), I32Const(1), I32Add, Call(helpers.render()),
        );
        lit(f, strings, helpers, CLOSE_OBJECT);
    };
    state(f, abi::PROMISE_FULFILLED);
    body(f, "Promise { ");
    wasm!(f; Else);
    state(f, abi::PROMISE_REJECTED);
    body(f, "Promise { <rejected> ");
    wasm!(f; Else);
    lit(f, strings, helpers, "Promise { <pending> }");
    wasm!(f; End, End, End);
}

/// `__render_heap(p, depth)`: the cycle guard over the four container kinds,
/// and the leaf kinds. A cycle target's `<ref *N> ` prefix is inserted at the
/// container's start offset once its children have been rendered.
fn emit_render_heap(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(7, I32)]);
    let (p, depth, ty, i, id, mark, end, size, cond) = (0, 1, 2, 3, 4, 5, 6, 7, 8);
    // id = the 1-based ref number of `p`, or 0.
    let ref_of = |f: &mut Function| {
        wasm!(f;
            I32Const(0), LocalSet(id), I32Const(0), LocalSet(i),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(i), GlobalGet(REFS_LEN_GLOBAL), I32GeU, BrIf(1),
            GlobalGet(REFS_BUF_GLOBAL), LocalGet(i), I32Const(2), I32Shl, I32Add, I32Load(m32(0)),
            LocalGet(p), I32Eq, If(BlockType::Empty),
            LocalGet(i), I32Const(1), I32Add, LocalSet(id), Br(2),
            End,
            LocalGet(i), I32Const(1), I32Add, LocalSet(i),
            Br(0), End, End,
        );
    };
    wasm!(f;
        LocalGet(p), I32Load8U(m8(0)), LocalTee(ty), I32Const(TYPE_ARRAY), I32Eq,
        LocalGet(ty), I32Const(TYPE_OBJECT), I32Eq, I32Or,
        LocalGet(ty), I32Const(TYPE_PROMISE), I32Eq, I32Or,
        LocalGet(ty), I32Const(TYPE_FUNCTION), I32Eq, I32Or,
        LocalSet(cond),
        LocalGet(cond), If(BlockType::Empty),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(depth), I32GeU, BrIf(1),
        GlobalGet(PATH_BUF_GLOBAL), LocalGet(i), I32Const(2), I32Shl, I32Add, I32Load(m32(0)),
        LocalGet(p), I32Eq, If(BlockType::Empty),
    );
    ref_of(&mut f);
    wasm!(f;
        LocalGet(id), I32Eqz, If(BlockType::Empty),
        GlobalGet(REFS_LEN_GLOBAL), I32Const(1), I32Add, I32Const(2), I32Shl, Call(helpers.refs_reserve()),
        GlobalGet(REFS_BUF_GLOBAL), GlobalGet(REFS_LEN_GLOBAL), I32Const(2), I32Shl, I32Add,
        LocalGet(p), I32Store(m32(0)),
        GlobalGet(REFS_LEN_GLOBAL), I32Const(1), I32Add, GlobalSet(REFS_LEN_GLOBAL),
        GlobalGet(REFS_LEN_GLOBAL), LocalSet(id),
        End,
    );
    lit(&mut f, strings, helpers, "[Circular *");
    wasm!(f; LocalGet(id), Call(helpers.out_uint()));
    lit(&mut f, strings, helpers, "]");
    wasm!(f;
        Return,
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0), End, End,
        LocalGet(depth), I32Const(1), I32Add, I32Const(2), I32Shl, Call(helpers.path_reserve()),
        GlobalGet(PATH_BUF_GLOBAL), LocalGet(depth), I32Const(2), I32Shl, I32Add,
        LocalGet(p), I32Store(m32(0)),
        GlobalGet(OUT_LEN_GLOBAL), LocalSet(mark),
    );
    render_container(&mut f, helpers, p, ty, depth);
    render_promise(&mut f, strings, helpers, p, ty, depth);
    ref_of(&mut f);
    wasm!(f; LocalGet(id), If(BlockType::Empty), GlobalGet(OUT_LEN_GLOBAL), LocalSet(end));
    lit(&mut f, strings, helpers, "<ref *");
    wasm!(f; LocalGet(id), Call(helpers.out_uint()));
    lit(&mut f, strings, helpers, "> ");
    // Rotate the prefix to `mark`: stash it past the end, shift the body up,
    // copy it back.
    wasm!(f;
        GlobalGet(OUT_LEN_GLOBAL), LocalGet(end), I32Sub, LocalSet(size),
        GlobalGet(OUT_LEN_GLOBAL), LocalGet(size), I32Add, Call(helpers.out_reserve()),
        GlobalGet(OUT_BUF_GLOBAL), GlobalGet(OUT_LEN_GLOBAL), I32Add,
        GlobalGet(OUT_BUF_GLOBAL), LocalGet(end), I32Add, LocalGet(size), COPY,
        GlobalGet(OUT_BUF_GLOBAL), LocalGet(mark), I32Add, LocalGet(size), I32Add,
        GlobalGet(OUT_BUF_GLOBAL), LocalGet(mark), I32Add,
        LocalGet(end), LocalGet(mark), I32Sub, COPY,
        GlobalGet(OUT_BUF_GLOBAL), LocalGet(mark), I32Add,
        GlobalGet(OUT_BUF_GLOBAL), GlobalGet(OUT_LEN_GLOBAL), I32Add, LocalGet(size), COPY,
        End,
        Return,
        End,
    );
    render_leaf(&mut f, strings, helpers, p, ty, i);
    wasm!(f; End);
    f
}

/// The non-container kinds of `__render_heap`; any other type byte is
/// `undefined`.
fn render_leaf(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    p: u32,
    ty: u32,
    tmp: u32,
) {
    let is = |f: &mut Function, kind: i32| {
        wasm!(f; LocalGet(ty), I32Const(kind), I32Eq, If(BlockType::Empty));
    };
    is(f, TYPE_STRING);
    wasm!(f; LocalGet(p), Call(helpers.out_str()), Return, End);
    is(f, TYPE_SYMBOL);
    render_symbol(f, strings, helpers, p, tmp);
    wasm!(f; Return, End);
    for (kind, open) in [(TYPE_MAP, "Map("), (TYPE_SET, "Set(")] {
        is(f, kind);
        lit(f, strings, helpers, open);
        wasm!(f; LocalGet(p), I32Load(m32(abi::MAP_SIZE_OFFSET)), Call(helpers.out_uint()));
        lit(f, strings, helpers, ")");
        wasm!(f; Return, End);
    }
    is(f, TYPE_GENERATOR);
    lit(f, strings, helpers, "Object [Generator]");
    wasm!(f; Return, End);
    is(f, TYPE_DATE);
    wasm!(f;
        LocalGet(p), I64ExtendI32U, I64Const(HEAP_BITS), I64Or, Call(helpers.date_text()),
        I32WrapI64, Call(helpers.out_str()), Return, End,
    );
    lit(f, strings, helpers, "undefined");
}

/// Append `Symbol(desc)` for the symbol block in i32 local `p`; a description
/// that is not a string is empty.
fn render_symbol(f: &mut Function, strings: &StrOffsets, helpers: Helpers<'_>, p: u32, tmp: u32) {
    lit(f, strings, helpers, "Symbol(");
    wasm!(f;
        LocalGet(p), I64Load(m64(abi::SYMBOL_DESCRIPTION_OFFSET)), Call(helpers.str_of()),
        LocalTee(tmp), If(BlockType::Empty), LocalGet(tmp), Call(helpers.out_str()), End,
    );
    lit(f, strings, helpers, ")");
}

/// Push `1` when the array in `p` is `SPARSE`, else `0`.
fn is_sparse(f: &mut Function, p: u32) {
    wasm!(f;
        LocalGet(p), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::ARRAY_FLAG_SPARSE)), I32And, I32Const(0), I32Ne,
    );
}

/// Push `1` when the jsval in i64 local `v` is a heap pointer, else `0`.
fn is_heap(f: &mut Function, v: u32) {
    wasm!(f; LocalGet(v), I64Const(QNAN_BITS), I64And, I64Const(QNAN_BITS), I64Eq);
    tag_of(f, v);
    wasm!(f; I32Const(HEAP_TAG), I32Eq, I32And);
}

/// Box the block base in i32 local `p` as a heap jsval.
fn box_heap(f: &mut Function, p: u32) {
    wasm!(f; LocalGet(p), I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// Emit an append of `, `, or of the static string in i32 local `open` before
/// the first part, reading and clearing the i32 local `first`.
fn separator(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    first: u32,
    open: &Open,
) {
    wasm!(f; LocalGet(first), If(BlockType::Empty));
    match open {
        Open::Text(text) => lit(f, strings, helpers, text),
        Open::Local(local) => wasm!(f; LocalGet(*local), Call(helpers.out_str())),
    }
    wasm!(f; Else);
    lit(f, strings, helpers, ", ");
    wasm!(f; End, I32Const(0), LocalSet(first));
}

/// Where a list's opening text comes from.
enum Open {
    Text(&'static str),
    Local(u32),
}

/// Append `close` when the i32 local `first` is clear, else `empty`.
fn close_list(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    first: u32,
    empty: &str,
    close: &str,
) {
    wasm!(f; LocalGet(first), I32Eqz, If(BlockType::Empty));
    lit(f, strings, helpers, close);
    wasm!(f; Else);
    lit(f, strings, helpers, empty);
    wasm!(f; End);
}

/// Call `__render_props(p, array, quote, 1, open, depth)` and store the
/// resulting `first` flag.
fn props_call(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    (p, depth, first): (u32, u32, u32),
    quote: i32,
    open: &str,
) {
    wasm!(f;
        LocalGet(p), I32Const(0), I32Const(quote), I32Const(1),
        I32Const(static_key(strings, open)), LocalGet(depth), Call(helpers.render_props()),
        LocalSet(first),
    );
}

/// Push element `k` (i32 local) of the array handle in i32 local `list`, as
/// the i32 base of the heap jsval it holds.
fn list_key(f: &mut Function, list: u32, k: u32) {
    wasm!(f;
        LocalGet(list), I32Load(m32(abi::ARRAY_ELEMS_OFFSET)),
        LocalGet(k), I32Const(3), I32Shl, I32Add, I64Load(m64(abi::ELEMS_DATA_OFFSET)), I32WrapI64,
    );
}

/// `list = __own_keys(p, enumerable strings)`, `n = list.length`, `k = 0`.
fn own_keys_list(f: &mut Function, helpers: Helpers<'_>, p: u32, list: u32, n: u32, k: u32) {
    box_heap(f, p);
    wasm!(f;
        I32Const(OWN_KEYS), Call(helpers.own_keys), I32WrapI64, LocalTee(list),
        I32Load(m32(abi::ARRAY_LEN_OFFSET)), LocalSet(n), I32Const(0), LocalSet(k),
    );
}

/// Append the hole run of `count` slots pushed by `count` as one list part.
fn hole_run(f: &mut Function, strings: &StrOffsets, helpers: Helpers<'_>, first: u32, run: u32) {
    separator(f, strings, helpers, first, &Open::Text(OPEN_ARRAY));
    lit(f, strings, helpers, "<");
    wasm!(f; LocalGet(run), Call(helpers.out_uint()));
    lit(f, strings, helpers, " empty item");
    wasm!(f; LocalGet(run), I32Const(1), I32Ne, If(BlockType::Empty));
    lit(f, strings, helpers, "s");
    wasm!(f; End);
    lit(f, strings, helpers, ">");
}

/// The locals of `__render_array`.
#[derive(Clone, Copy)]
struct ArrayLocals {
    depth: u32,
    next: u32,
    first: u32,
    run: u32,
    index: u32,
    bits: u32,
}

/// Append the present element `index` with value `bits`, after the hole run
/// that precedes it.
fn array_element(f: &mut Function, strings: &StrOffsets, helpers: Helpers<'_>, l: ArrayLocals) {
    wasm!(f; LocalGet(l.index), LocalGet(l.next), I32GtU, If(BlockType::Empty),
        LocalGet(l.index), LocalGet(l.next), I32Sub, LocalSet(l.run),
    );
    hole_run(f, strings, helpers, l.first, l.run);
    wasm!(f; End);
    separator(f, strings, helpers, l.first, &Open::Text(OPEN_ARRAY));
    wasm!(f;
        LocalGet(l.bits), LocalGet(l.depth), Call(helpers.render()),
        LocalGet(l.index), I32Const(1), I32Add, LocalSet(l.next),
    );
}

/// `__render_array(p, depth)`: store elements, then a `SPARSE` array's
/// integer-keyed elements ascending, hole runs between them, then named
/// properties.
fn emit_render_array(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(12, I32), (1, I64)]);
    let (p, depth) = (0, 1);
    let (len, store, stored, slot, next, first, list, count, key, entry_at, index, run) =
        (2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13);
    let bits = 14;
    let l = ArrayLocals {
        depth,
        next,
        first,
        run,
        index,
        bits,
    };
    wasm!(f;
        LocalGet(p), I32Load(m32(abi::ARRAY_LEN_OFFSET)), LocalTee(len), LocalSet(stored),
        LocalGet(p), I32Load(m32(abi::ARRAY_ELEMS_OFFSET)), LocalSet(store),
        I32Const(1), LocalSet(first),
    );
    is_sparse(&mut f, p);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(store), If(BlockType::Result(I32)),
        LocalGet(store), I32Load(m32(abi::ELEMS_CAP_OFFSET)),
        Else, I32Const(0), End,
        LocalTee(slot), LocalGet(stored), LocalGet(slot), LocalGet(stored), I32LtU, Select, LocalSet(stored),
        End,
        I32Const(0), LocalSet(slot),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(slot), LocalGet(stored), I32GeU, BrIf(1),
        LocalGet(store), LocalGet(slot), I32Const(3), I32Shl, I32Add,
        I64Load(m64(abi::ELEMS_DATA_OFFSET)), LocalTee(bits), I64Const(HOLE_BITS), I64Ne,
        If(BlockType::Empty), LocalGet(slot), LocalSet(index),
    );
    array_element(&mut f, strings, helpers, l);
    wasm!(f; End, LocalGet(slot), I32Const(1), I32Add, LocalSet(slot), Br(0), End, End);
    is_sparse(&mut f, p);
    wasm!(f;
        LocalGet(p), I32Load(m32(abi::OBJ_PROPS_OFFSET)), I32Const(0), I32Ne, I32And,
        If(BlockType::Empty),
        LocalGet(p), I32Load(m32(abi::OBJ_PROPS_OFFSET)),
        I32Load8U(m8(abi::PROPS_TFLAGS_OFFSET)), I32Const(HAS_INDEX), I32And,
        If(BlockType::Empty),
    );
    own_keys_list(&mut f, helpers, p, list, count, slot);
    wasm!(f; Block(BlockType::Empty), Loop(BlockType::Empty), LocalGet(slot), LocalGet(count), I32GeU, BrIf(1));
    list_key(&mut f, list, slot);
    wasm!(f;
        LocalTee(key), Call(helpers.index_value), LocalTee(index), I32Const(-1), I32Eq, BrIf(1),
        LocalGet(p), LocalGet(key), Call(helpers.prop_find), LocalTee(entry_at),
        If(BlockType::Empty), LocalGet(entry_at), I64Load(m64(abi::PROP_VALUE_OFFSET)), LocalSet(bits),
    );
    array_element(&mut f, strings, helpers, l);
    wasm!(f; End, LocalGet(slot), I32Const(1), I32Add, LocalSet(slot), Br(0), End, End, End, End);
    wasm!(f; LocalGet(len), LocalGet(next), I32GtU, If(BlockType::Empty),
        LocalGet(len), LocalGet(next), I32Sub, LocalSet(run),
    );
    hole_run(&mut f, strings, helpers, first, run);
    wasm!(f;
        End,
        LocalGet(p), I32Const(1), I32Const(0), LocalGet(first),
        I32Const(static_key(strings, OPEN_ARRAY)), LocalGet(depth), Call(helpers.render_props()),
        LocalSet(first),
    );
    close_list(&mut f, strings, helpers, first, "[]", " ]");
    wasm!(f; End);
    f
}

/// `dst = name` when `o` holds (along its chain) a data property `key` whose
/// value is a string, else `0`.
fn string_field(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    (p, e, dst): (u32, u32, u32),
    key: &str,
) {
    wasm!(f;
        LocalGet(p), I32Const(static_key(strings, key)), Call(helpers.render_lookup()),
        LocalTee(e), If(BlockType::Result(I32)),
        LocalGet(e), I64Load(m64(abi::PROP_VALUE_OFFSET)), Call(helpers.str_of()),
        Else, I32Const(0), End,
        LocalSet(dst),
    );
}

/// The wrapper arm of `__render_object`: `[String: 'ab']`, `[Number: 1]`,
/// `[Symbol: Symbol(a)]`, `[Boolean: true]`, then own properties.
fn render_wrapper(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    (p, depth, class, first, prim): (u32, u32, u32, u32, u32),
) {
    let class_is = |f: &mut Function, byte: u8| {
        wasm!(f; LocalGet(class), I32Const(i32::from(byte)), I32Eq);
    };
    let bracketed = |f: &mut Function, open: &str| {
        lit(f, strings, helpers, open);
        wasm!(f; LocalGet(prim), LocalGet(depth), Call(helpers.render()));
        lit(f, strings, helpers, "]");
    };
    wasm!(f; LocalGet(p), I64Load(m64(abi::WRAPPER_VALUE_OFFSET)), LocalSet(prim));
    class_is(f, abi::CLASS_STRING);
    wasm!(f; If(BlockType::Empty));
    lit(f, strings, helpers, "[String: '");
    wasm!(f; LocalGet(prim), I32WrapI64, Call(helpers.out_str()));
    lit(f, strings, helpers, "']");
    wasm!(f; Else);
    class_is(f, abi::CLASS_NUMBER);
    wasm!(f; If(BlockType::Empty), LocalGet(prim), I64Const(NEGATIVE_ZERO), I64Eq, If(BlockType::Empty));
    lit(f, strings, helpers, "[Number: -0]");
    wasm!(f; Else);
    bracketed(f, "[Number: ");
    wasm!(f; End, Else);
    class_is(f, abi::CLASS_SYMBOL);
    wasm!(f; If(BlockType::Empty));
    bracketed(f, "[Symbol: ");
    wasm!(f; Else);
    bracketed(f, "[Boolean: ");
    wasm!(f; End, End, End);
    props_call(f, strings, helpers, (p, depth, first), 0, OPEN_TRAILING);
    wasm!(f; LocalGet(first), I32Eqz, If(BlockType::Empty));
    lit(f, strings, helpers, CLOSE_OBJECT);
    wasm!(f; End);
}

/// `__render_object(p, depth)`: an error, `arguments`, a wrapper, or a plain
/// object with its constructor's name when that is not `Object`.
fn emit_render_object(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, I32), (1, I64)]);
    let (p, depth, class, e, name, msg, first, prim) = (0, 1, 2, 3, 4, 5, 6, 7);
    wasm!(f;
        LocalGet(p), I32Load8U(m8(abi::HEADER_CLASS_OFFSET)), LocalTee(class),
        I32Const(i32::from(abi::CLASS_ERROR)), I32Eq, If(BlockType::Empty),
    );
    string_field(&mut f, strings, helpers, (p, e, name), "name");
    string_field(&mut f, strings, helpers, (p, e, msg), "message");
    wasm!(f; LocalGet(name), If(BlockType::Empty), LocalGet(name), Call(helpers.out_str()), Else);
    lit(&mut f, strings, helpers, "Error");
    wasm!(f; End, LocalGet(msg), If(BlockType::Result(I32)),
        LocalGet(msg), I32Load(m32(abi::STR_LEN_OFFSET)), Else, I32Const(0), End,
        If(BlockType::Empty),
    );
    lit(&mut f, strings, helpers, ": ");
    wasm!(f; LocalGet(msg), Call(helpers.out_str()), End, Return, End);
    wasm!(f; LocalGet(class), I32Const(i32::from(abi::CLASS_ARGUMENTS)), I32Eq, If(BlockType::Empty));
    lit(&mut f, strings, helpers, "[Arguments] ");
    props_call(&mut f, strings, helpers, (p, depth, first), 1, OPEN_OBJECT);
    close_list(&mut f, strings, helpers, first, "{}", CLOSE_OBJECT);
    wasm!(f; Return, End);
    // The four wrapper classes: `BOOLEAN`/`NUMBER`/`STRING` are consecutive.
    wasm!(f;
        LocalGet(class), I32Const(i32::from(abi::CLASS_BOOLEAN)), I32Sub, I32Const(3), I32LtU,
        LocalGet(class), I32Const(i32::from(abi::CLASS_SYMBOL)), I32Eq, I32Or,
        If(BlockType::Empty),
    );
    render_wrapper(&mut f, strings, helpers, (p, depth, class, first, prim));
    wasm!(f; Return, End);
    // The prototype's `constructor`, named other than `Object`.
    wasm!(f;
        LocalGet(p), I64Load(m64(abi::OBJ_PROTO_OFFSET)), Call(helpers.heap_object()), LocalTee(e),
        If(BlockType::Empty),
        LocalGet(e), I32Const(static_key(strings, "constructor")), Call(helpers.render_lookup()),
        LocalTee(e), If(BlockType::Empty),
        LocalGet(e), I64Load(m64(abi::PROP_VALUE_OFFSET)), Call(helpers.fn_name()), LocalSet(name),
        End, End,
        LocalGet(name), If(BlockType::Result(I32)),
        LocalGet(name), I32Load(m32(abi::STR_LEN_OFFSET)), I32Const(0), I32Ne,
        LocalGet(name), I32Const(static_key(strings, "Object")), Call(helpers.str_eq), I32Eqz, I32And,
        Else, I32Const(0), End,
        If(BlockType::Empty), LocalGet(name), Call(helpers.out_str()),
    );
    lit(&mut f, strings, helpers, " ");
    wasm!(f; End);
    props_call(&mut f, strings, helpers, (p, depth, first), 0, OPEN_OBJECT);
    close_list(&mut f, strings, helpers, first, "{}", CLOSE_OBJECT);
    wasm!(f; End);
    f
}

/// `__render_function(p, depth)`: the `[Function: f]` / `[class A extends B]`
/// head, then own enumerable properties.
fn emit_render_function(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(5, I32)]);
    let (p, depth, flags, name, parent, first, e) = (0, 1, 2, 3, 4, 5, 6);
    let flag = |f: &mut Function, bit: u8| {
        wasm!(f; LocalGet(flags), I32Const(i32::from(bit)), I32And);
    };
    wasm!(f; LocalGet(p), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)), LocalSet(flags));
    flag(&mut f, abi::FUNC_FLAG_BOUND);
    wasm!(f; If(BlockType::Empty),
        LocalGet(p), I32Const(static_key(strings, "name")), Call(helpers.prop_find), LocalTee(e),
        If(BlockType::Empty),
        LocalGet(e), I32Load(m32(abi::PROP_ATTRS_OFFSET)), I32Const(ACCESSOR), I32And, I32Eqz,
        If(BlockType::Empty),
        LocalGet(e), I64Load(m64(abi::PROP_VALUE_OFFSET)), Call(helpers.str_of()), LocalSet(name),
        End, End,
        Else,
    );
    box_heap(&mut f, p);
    wasm!(f; Call(helpers.fn_name()), LocalSet(name), End);
    flag(&mut f, abi::FUNC_FLAG_CLASS_CTOR);
    wasm!(f; If(BlockType::Empty),
        LocalGet(p), I64Load(m64(abi::OBJ_PROTO_OFFSET)), Call(helpers.heap_object()), LocalTee(e),
        If(BlockType::Empty),
        LocalGet(e), I32Load8U(m8(0)), I32Const(TYPE_FUNCTION), I32Eq,
        LocalGet(e), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)),
        I32Const(i32::from(abi::FUNC_FLAG_CONSTRUCTOR)), I32And, I32Const(0), I32Ne, I32And,
        If(BlockType::Empty),
    );
    box_heap(&mut f, e);
    wasm!(f; Call(helpers.fn_name()), LocalSet(parent), End, End);
    lit(&mut f, strings, helpers, "[class ");
    named_or(&mut f, strings, helpers, name, "(anonymous)");
    wasm!(f; LocalGet(parent), If(BlockType::Empty));
    lit(&mut f, strings, helpers, " extends ");
    wasm!(f; LocalGet(parent), Call(helpers.out_str()), End);
    lit(&mut f, strings, helpers, "]");
    wasm!(f; Else);
    name_nonempty(&mut f, name);
    wasm!(f; If(BlockType::Empty));
    lit(&mut f, strings, helpers, "[Function: ");
    wasm!(f; LocalGet(name), Call(helpers.out_str()));
    lit(&mut f, strings, helpers, "]");
    wasm!(f; Else);
    lit(&mut f, strings, helpers, "[Function (anonymous)]");
    wasm!(f; End, End);
    props_call(
        &mut f,
        strings,
        helpers,
        (p, depth, first),
        0,
        OPEN_TRAILING,
    );
    wasm!(f; LocalGet(first), I32Eqz, If(BlockType::Empty));
    lit(&mut f, strings, helpers, CLOSE_OBJECT);
    wasm!(f; End, End);
    f
}

/// Push `1` when the string in i32 local `name` exists and is not empty.
fn name_nonempty(f: &mut Function, name: u32) {
    wasm!(f;
        LocalGet(name), If(BlockType::Result(I32)),
        LocalGet(name), I32Load(m32(abi::STR_LEN_OFFSET)), I32Const(0), I32Ne,
        Else, I32Const(0), End,
    );
}

/// Append the string in i32 local `name`, or `fallback` when it is empty.
fn named_or(
    f: &mut Function,
    strings: &StrOffsets,
    helpers: Helpers<'_>,
    name: u32,
    fallback: &str,
) {
    name_nonempty(f, name);
    wasm!(f; If(BlockType::Empty), LocalGet(name), Call(helpers.out_str()), Else);
    lit(f, strings, helpers, fallback);
    wasm!(f; End);
}

/// `__render_props(o, array, quote, first, open, depth) -> first`: the own
/// enumerable properties of `o` as list parts: integer keys ascending (an
/// array's are its elements, so `array` skips them), string keys in slot
/// order, then symbol keys in slot order.
fn emit_render_props(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(8, I32)]);
    let (o, array, quote, first, open, depth) = (0, 1, 2, 3, 4, 5);
    let (table, tflags, list, slot, count, key, entry_at, end) = (6, 7, 8, 9, 10, 11, 12, 13);
    let entry = |f: &mut Function| {
        wasm!(f;
            LocalGet(entry_at), LocalGet(quote), LocalGet(first), LocalGet(open), LocalGet(depth),
            Call(helpers.render_entry()),
            I32Const(0), LocalSet(first),
        );
    };
    // One slot-order walk over the entries that pass `keep`.
    let walk = |f: &mut Function, symbols: bool| {
        wasm!(f;
            LocalGet(table), I32Const(HEADER), I32Add, LocalSet(entry_at),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(entry_at), LocalGet(end), I32GeU, BrIf(1),
            LocalGet(entry_at), I32Load(m32(abi::PROP_KEY_OFFSET)), LocalTee(key),
            If(BlockType::Empty),
            LocalGet(entry_at), I32Load(m32(abi::PROP_ATTRS_OFFSET)), I32Const(ENUMERABLE), I32And,
            If(BlockType::Empty),
            LocalGet(key), I32Load8U(m8(0)), I32Const(TYPE_SYMBOL), I32Eq,
        );
        if symbols {
            wasm!(f; If(BlockType::Empty));
        } else {
            wasm!(f;
                I32Eqz,
                LocalGet(key), I32Load8U(m8(abi::HEADER_FLAGS_OFFSET)), I32Const(INDEX_FLAG), I32And,
                I32Const(0), I32Ne, LocalGet(tflags), I32Const(HAS_INDEX), I32And, I32And, I32Eqz, I32And,
                If(BlockType::Empty),
            );
        }
        entry(f);
        wasm!(f;
            End, End, End,
            LocalGet(entry_at), I32Const(ENTRY), I32Add, LocalSet(entry_at),
            Br(0), End, End,
        );
    };
    wasm!(f;
        LocalGet(o), I32Load(m32(abi::OBJ_PROPS_OFFSET)), LocalTee(table), I32Eqz,
        If(BlockType::Empty), LocalGet(first), Return, End,
        LocalGet(table), I32Load8U(m8(abi::PROPS_TFLAGS_OFFSET)), LocalSet(tflags),
        LocalGet(table), I32Load(m32(abi::PROPS_COUNT_OFFSET)), I32Const(ENTRY), I32Mul,
        LocalGet(table), I32Add, I32Const(HEADER), I32Add, LocalSet(end),
        LocalGet(tflags), I32Const(HAS_INDEX), I32And, LocalGet(array), I32Eqz, I32And,
        If(BlockType::Empty),
    );
    own_keys_list(&mut f, helpers, o, list, count, slot);
    wasm!(f; Block(BlockType::Empty), Loop(BlockType::Empty), LocalGet(slot), LocalGet(count), I32GeU, BrIf(1));
    list_key(&mut f, list, slot);
    wasm!(f;
        LocalTee(key), Call(helpers.index_value), I32Const(-1), I32Eq, BrIf(1),
        LocalGet(o), LocalGet(key), Call(helpers.prop_find), LocalTee(entry_at), If(BlockType::Empty),
    );
    entry(&mut f);
    wasm!(f; End, LocalGet(slot), I32Const(1), I32Add, LocalSet(slot), Br(0), End, End, End);
    walk(&mut f, false);
    wasm!(f; LocalGet(tflags), I32Const(HAS_SYMBOL), I32And, If(BlockType::Empty));
    walk(&mut f, true);
    wasm!(f; End, LocalGet(first), End);
    f
}

/// `__render_entry(e, quote, first, open, depth)`: one `key: value` list part
/// of the table entry `e`; an accessor shows which halves it has.
fn emit_render_entry(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(3, I32)]);
    let (e, quote, first, open, depth) = (0, 1, 2, 3, 4);
    let (pair, getter, setter) = (5, 6, 7);
    separator(&mut f, strings, helpers, first, &Open::Local(open));
    wasm!(f; LocalGet(e), I32Load(m32(abi::PROP_KEY_OFFSET)), LocalGet(quote), Call(helpers.render_key()));
    lit(&mut f, strings, helpers, ": ");
    wasm!(f;
        LocalGet(e), I32Load(m32(abi::PROP_ATTRS_OFFSET)), I32Const(ACCESSOR), I32And,
        If(BlockType::Empty),
        LocalGet(e), I64Load(m64(abi::PROP_VALUE_OFFSET)), I32WrapI64, LocalTee(pair),
        I64Load(m64(abi::ACCESSOR_GET_OFFSET)), I64Const(UNDEFINED_BITS), I64Ne, LocalSet(getter),
        LocalGet(pair), I64Load(m64(abi::ACCESSOR_SET_OFFSET)), I64Const(UNDEFINED_BITS), I64Ne,
        LocalSet(setter),
        LocalGet(setter), If(BlockType::Empty), LocalGet(getter), If(BlockType::Empty),
    );
    lit(&mut f, strings, helpers, "[Getter/Setter]");
    wasm!(f; Else);
    lit(&mut f, strings, helpers, "[Setter]");
    wasm!(f; End, Else);
    lit(&mut f, strings, helpers, "[Getter]");
    wasm!(f;
        End,
        Else,
        LocalGet(e), I64Load(m64(abi::PROP_VALUE_OFFSET)), LocalGet(depth), Call(helpers.render()),
        End,
        End,
    );
    f
}

/// Push `1` when the character at unit `at` of the string `key`, whose first
/// unit is in i32 local `u`, may appear in a bare key: an ASCII identifier
/// unit, or a non-ASCII `Alphanumeric` code point (a lone surrogate is not).
/// A surrogate pair advances `at` to its trail unit.
fn key_unit_ok(
    f: &mut Function,
    helpers: Helpers<'_>,
    key: u32,
    count: u32,
    wide: u32,
    at: u32,
    u: u32,
) {
    wasm!(f; LocalGet(u), I32Const(0x80), I32GeU, If(BlockType::Result(I32)), LocalGet(wide), If(BlockType::Result(I32)));
    emit_surrogate_pair_at(f, key, at, count);
    wasm!(f; Else, I32Const(0), End, If(BlockType::Empty));
    unicode::push_pair_cp(f, key, at);
    wasm!(f; LocalSet(u), LocalGet(at), I32Const(1), I32Add, LocalSet(at), End);
    unicode::push_is_alphanumeric(f, helpers, u);
    wasm!(f;
        Else,
        LocalGet(u), I32Const(0x20), I32Or, I32Const(0x61), I32Sub, I32Const(26), I32LtU,
        LocalGet(u), I32Const(0x30), I32Sub, I32Const(10), I32LtU, I32Or,
        LocalGet(u), I32Const(0x5F), I32Eq, I32Or,
        LocalGet(u), I32Const(0x24), I32Eq, I32Or,
        End,
    );
}

/// `__render_key(key, quote)`: a symbol key as `[Symbol(desc)]`, a string key
/// raw; with `quote`, anything but a bare identifier is single-quoted.
fn emit_render_key(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(6, I32)]);
    let (key, quote, count, at, wide, code, bare, tmp) = (0, 1, 2, 3, 4, 5, 6, 7);
    let quote_mark = |f: &mut Function, on: u32| {
        wasm!(f; LocalGet(on), If(BlockType::Empty));
        lit(f, strings, helpers, "'");
        wasm!(f; End);
    };
    wasm!(f; LocalGet(key), I32Load8U(m8(0)), I32Const(TYPE_SYMBOL), I32Eq, If(BlockType::Empty));
    quote_mark(&mut f, quote);
    lit(&mut f, strings, helpers, "[");
    render_symbol(&mut f, strings, helpers, key, tmp);
    lit(&mut f, strings, helpers, "]");
    quote_mark(&mut f, quote);
    wasm!(f; Return, End);
    wasm!(f; LocalGet(key), I32Load(m32(abi::STR_LEN_OFFSET)), LocalTee(count), I32Const(0), I32Ne, LocalSet(bare));
    emit_str_two_byte(&mut f, key);
    wasm!(f; LocalSet(wide), LocalGet(bare), If(BlockType::Empty));
    emit_str_unit(&mut f, key, at, wide);
    wasm!(f; I32Const(0x30), I32Sub, I32Const(10), I32GeU, LocalSet(bare), End,
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(at), LocalGet(count), I32GeU, BrIf(1),
        LocalGet(bare), I32Eqz, BrIf(1),
    );
    emit_str_unit(&mut f, key, at, wide);
    wasm!(f; LocalSet(code));
    key_unit_ok(&mut f, helpers, key, count, wide, at, code);
    wasm!(f; LocalSet(bare), LocalGet(at), I32Const(1), I32Add, LocalSet(at), Br(0), End, End);
    wasm!(f; LocalGet(bare), I32Eqz, LocalGet(quote), I32And, LocalSet(bare));
    quote_mark(&mut f, bare);
    wasm!(f; LocalGet(key), Call(helpers.out_str()));
    quote_mark(&mut f, bare);
    wasm!(f; End);
    f
}

/// `__render_lookup(o, key) -> entry`: the entry of the data property `key`
/// on `o` or the nearest object of its `proto` chain, or `0`. Accessors are
/// skipped and no getter runs; the walk is bounded.
fn emit_render_lookup(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(2, I32)]);
    let (cur, key, e, steps) = (0, 1, 2, 3);
    wasm!(f;
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(steps), I32Const(1024), I32GeU, BrIf(1),
        LocalGet(cur), LocalGet(key), Call(helpers.prop_find), LocalTee(e),
        If(BlockType::Empty),
        LocalGet(e), I32Load(m32(abi::PROP_ATTRS_OFFSET)), I32Const(ACCESSOR), I32And, I32Eqz,
        If(BlockType::Empty), LocalGet(e), Return, End,
        End,
        LocalGet(cur), I64Load(m64(abi::OBJ_PROTO_OFFSET)), Call(helpers.heap_object()),
        LocalTee(cur), I32Eqz, BrIf(1),
        LocalGet(steps), I32Const(1), I32Add, LocalSet(steps),
        Br(0), End, End,
        I32Const(0),
        End,
    );
    f
}

/// `__heap_object(v) -> p`: the base of an object-family heap value that
/// carries a `props` slot (not a string, array or symbol), or `0`.
fn emit_heap_object() -> Function {
    let mut f = Function::new(vec![(1, I32)]);
    let (v, ty) = (0, 1);
    is_heap(&mut f, v);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(v), I32WrapI64, I32Load8U(m8(0)), LocalTee(ty), I32Const(TYPE_STRING), I32Ne,
        LocalGet(ty), I32Const(TYPE_ARRAY), I32Ne, I32And,
        LocalGet(ty), I32Const(TYPE_SYMBOL), I32Ne, I32And,
        If(BlockType::Empty), LocalGet(v), I32WrapI64, Return, End,
        End,
        I32Const(0),
        End,
    );
    f
}

/// `__str_of(v) -> s`: the string block of a heap string, or `0`.
fn emit_str_of() -> Function {
    let mut f = Function::new(vec![]);
    is_heap(&mut f, 0);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(0), I32WrapI64, I32Load8U(m8(0)), I32Const(TYPE_STRING), I32Eq,
        If(BlockType::Empty), LocalGet(0), I32WrapI64, Return, End,
        End,
        I32Const(0),
        End,
    );
    f
}

/// `__fn_name(v) -> s`: the `__fn_info` name of the function `v`, or `0`.
fn emit_fn_name(helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![(1, I32)]);
    let (v, p) = (0, 1);
    wasm!(f;
        LocalGet(v), Call(helpers.heap_object()), LocalTee(p),
        If(BlockType::Result(I32)),
        LocalGet(p), I32Load8U(m8(0)), I32Const(TYPE_FUNCTION), I32Eq,
        Else, I32Const(0), End,
        I32Eqz, If(BlockType::Empty), I32Const(0), Return, End,
        I32Const(abi::FN_INFO_ADDR.cast_signed()), I32Load(m32(0)),
        LocalGet(p), I32Load(m32(abi::FUNC_INDEX_OFFSET)),
        I32Const(abi::FN_INFO_ENTRY_SIZE.cast_signed()), I32Mul, I32Add,
        I32Load(m32(abi::FN_INFO_NAME_OFFSET)),
        End,
    );
    f
}

/// `__date_text(v) -> s`: the ISO text of the Date jsval `v`, or
/// `Invalid Date`. The local layout from local 1 is the function scratch
/// layout [`emit_date_field`] expects; its throwing arm is unreachable behind
/// the finiteness gate.
fn emit_date_text(strings: &StrOffsets, helpers: Helpers<'_>) -> Function {
    let mut f = Function::new(vec![
        (3, I64),
        (1, I32),
        (3, I64),
        (1, I32),
        (5, I32),
        (1, I64),
        (7, I64),
        (1, I32),
        (2, I64),
        (6, I64),
    ]);
    let (v, scratch) = (0, 1);
    helpers.unwind.borrow_mut().reset();
    wasm!(f;
        LocalGet(v), I32WrapI64, I64Load(m64(abi::DATE_MS_OFFSET)), I64Const(F64_EXP_MASK), I64And,
        I64Const(F64_EXP_MASK), I64Eq,
        If(BlockType::Result(I64)),
        I64Const(HEAP_BITS | i64::from(static_key(strings, INVALID_DATE))),
        Else,
    );
    emit_date_field(DateOp::ToIsoString, v, &mut f, scratch, strings, helpers);
    wasm!(f; End, End);
    f
}
