use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, EntityType, ExportKind, ExportSection, Function,
    FunctionSection, ImportSection, Instruction, MemArg, MemorySection, MemoryType, Module,
    TypeSection, ValType,
};

use super::run_inspect;
use super::value::{Value, decode, to_js_string};
use crate::abi::{TYPE_ARRAY, TYPE_FUNCTION, TYPE_OBJECT, TYPE_STRING};
use crate::emit::CompileOptions;

/// Lower and emit `src` with the completion value printed, run it, and return
/// its number result and final linear-memory size.
fn run_with_memory(src: &str) -> (f64, usize) {
    let program = crate::frontend::parse(src, crate::frontend::Dialect::Js).expect("parses");
    let ir = crate::codegen::lower(&program).expect("lowers");
    let wasm = crate::emit::emit(
        &ir,
        CompileOptions {
            print_completion: true,
        },
    );
    let done = run_inspect(&wasm).expect("runs");
    assert_eq!(done.code, 0, "{}", done.stderr);
    let number = done.stdout.trim_end().parse().expect("number completion");
    (number, done.memory.len())
}

#[test]
fn number_boxing_roundtrips() {
    // Boxing a number is `to_bits`; decode round-trips it back unchanged.
    for &x in &[0.0_f64, 42.0, -5.0, 0.5, 1e300, f64::INFINITY, f64::MIN] {
        assert!(
            matches!(decode(x.to_bits(), &[]), Value::Number(n) if n.to_bits() == x.to_bits()),
            "roundtrip {x}",
        );
    }
}

#[test]
fn decode_boxed_constants() {
    assert_eq!(decode(0x7FF9_0000_0000_0000, &[]), Value::Bool(false));
    assert_eq!(decode(0x7FF9_0000_0000_0001, &[]), Value::Bool(true));
    assert_eq!(decode(0x7FFA_0000_0000_0000, &[]), Value::Null);
    assert_eq!(decode(0x7FFB_0000_0000_0000, &[]), Value::Undefined);
}

#[test]
fn decode_canonical_nan_is_number() {
    assert!(
        matches!(decode(0x7FF8_0000_0000_0000, &[]), Value::Number(n) if n.is_nan()),
        "canonical NaN should decode as a NaN number",
    );
}

#[test]
fn decode_retired_typeof_tag_is_nan_number() {
    // Tag 0b100 is unused (`typeof` yields a real heap string), so it decodes as a
    // NaN number (decode stays total, like the other unused tags).
    assert!(matches!(decode(0x7FFC_0000_0000_0001, &[]), Value::Number(n) if n.is_nan()));
}

#[test]
fn decode_pointer_reads_string_from_memory() {
    // A hand-built heap string block: "hi" laid out as
    // [type=0][pad×3][len=2 LE][b'h', b'i'] at base offset 8.
    let mut memory = vec![0u8; 8];
    memory.extend_from_slice(&[TYPE_STRING, 0, 0, 0]); // header
    memory.extend_from_slice(&2u32.to_le_bytes());
    memory.extend_from_slice(b"hi");
    // Heap jsval: tag 0b101 header (HEAP_BITS) with block base offset 8.
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x8;
    assert_eq!(decode(bits, &memory), Value::Str("hi".to_owned()));
}

/// A hand-built heap string block at base offset 8: header with `flags`,
/// `len` code units, then `data`. Returns the memory and its heap jsval.
fn string_block(flags: u8, len: u32, data: &[u8]) -> (Vec<u8>, u64) {
    let mut memory = vec![0u8; 8];
    memory.extend_from_slice(&[TYPE_STRING, 0, flags, 0]);
    memory.extend_from_slice(&len.to_le_bytes());
    memory.extend_from_slice(data);
    (memory, 0x7FFD_0000_0000_0000_u64 | 0x8)
}

#[test]
fn decode_one_byte_string_is_latin1() {
    // One-byte blocks are Latin-1: 0x80..=0x9F are U+0080..U+009F (not the
    // windows-1252 punctuation) and 0xE9 is U+00E9.
    let (memory, bits) = string_block(0, 4, &[0x80, 0x99, 0xE9, 0xFF]);
    assert_eq!(
        decode(bits, &memory),
        Value::Str("\u{80}\u{99}\u{e9}\u{ff}".to_owned())
    );
}

#[test]
fn decode_two_byte_string_reads_utf16le_units() {
    // "é😀" as UTF-16LE: U+00E9, then the pair D83D DE00; `len` counts units.
    let units: Vec<u8> = [0x00E9_u16, 0xD83D, 0xDE00]
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let (memory, bits) = string_block(crate::abi::STR_FLAG_TWO_BYTE, 3, &units);
    assert_eq!(decode(bits, &memory), Value::Str("é😀".to_owned()));
    assert_eq!(
        super::value::string_units(&memory, 8),
        Some(vec![0x00E9, 0xD83D, 0xDE00])
    );
}

#[test]
fn decode_lone_surrogate_renders_replacement_but_units_stay_lossless() {
    let (memory, bits) = string_block(crate::abi::STR_FLAG_TWO_BYTE, 2, &[0x00, 0xD8, 0x61, 0x00]);
    assert_eq!(decode(bits, &memory), Value::Str("\u{FFFD}a".to_owned()));
    assert_eq!(
        super::value::string_units(&memory, 8),
        Some(vec![0xD800, 0x0061])
    );
}

#[test]
fn decode_truncated_two_byte_string_is_empty() {
    // Two units need four payload bytes; a short block decodes as "".
    let (memory, bits) = string_block(crate::abi::STR_FLAG_TWO_BYTE, 2, &[0x61, 0x00]);
    assert_eq!(decode(bits, &memory), Value::Str(String::new()));
}

#[test]
fn decode_out_of_range_pointer_is_undefined() {
    // A heap pointer past the end of memory must not panic — its type byte
    // can't be read, so decode stays total by yielding `undefined`.
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x270F;
    assert_eq!(decode(bits, &[]), Value::Undefined);
}

#[test]
fn decode_reads_type_byte_to_pick_the_arm() {
    // The single heap tag dispatches on the block's TYPE BYTE. The same
    // base offset decodes as a string, array, or object purely by that byte.
    let base = 8usize;
    let bits = 0x7FFD_0000_0000_0000_u64 | base as u64;
    // type=array: a handle `[42.0]` with its element store right after it.
    let mut mem = vec![0u8; base];
    push_array(&mut mem, &[42.0_f64.to_bits()]);
    assert_eq!(
        decode(bits, &mem),
        Value::Array(vec![Value::Number(42.0)], Vec::new())
    );
    // Flip only the type byte to string: [type=0][pad×3][len=1]['x'] would need
    // a 1-byte payload, so rebuild the block accordingly.
    let mut mem = vec![0u8; base];
    mem.extend_from_slice(&[TYPE_STRING, 0, 0, 0]);
    mem.extend_from_slice(&1u32.to_le_bytes());
    mem.push(b'x');
    assert_eq!(decode(bits, &mem), Value::Str("x".to_owned()));
}

#[test]
fn decode_object_reads_entries_from_memory() {
    // A hand-built heap object `{a: 7}` (plus a non-enumerable `h` and a
    // tombstone): the key strings "a" at 8 and "h" at 24, the object at 40, its
    // property table at 56:
    //   object: [type=2][0×3][props=56][proto=null]
    //   table : [12][0][tflags=HAS_NON_ENUM][0][count=3][cap=3][live=2]
    //           ["a" | attrs 7 | 7.0] [tombstone] ["h" | attrs 5 | 1.0]
    let null = 0x7FFA_0000_0000_0000_u64;
    let mut memory = vec![0u8; 8];
    for key in *b"ah" {
        memory.extend_from_slice(&[TYPE_STRING, 0, 1, 0]);
        memory.extend_from_slice(&1u32.to_le_bytes());
        memory.push(key);
        memory.resize(memory.len() + 7, 0);
    }
    assert_eq!(memory.len(), 40);
    memory.extend_from_slice(&[TYPE_OBJECT, 0, 0, 0]);
    memory.extend_from_slice(&56u32.to_le_bytes());
    memory.extend_from_slice(&null.to_le_bytes());
    memory.extend_from_slice(&[12, 0, 2, 0]);
    for word in [3u32, 3, 2] {
        memory.extend_from_slice(&word.to_le_bytes());
    }
    for (key, attrs, value) in [(8u32, 7u32, 7.0_f64), (0, 0, 0.0), (24, 5, 1.0)] {
        memory.extend_from_slice(&key.to_le_bytes());
        memory.extend_from_slice(&attrs.to_le_bytes());
        memory.extend_from_slice(&value.to_bits().to_le_bytes());
    }
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x28;
    assert_eq!(
        decode(bits, &memory),
        Value::Object(vec![("a".to_owned(), Value::Number(7.0))])
    );
}

/// Append a hand-built array handle holding `elems` at the end of `memory`
/// (`[type=1][…][props=0][proto=null][len][elems]`), followed by its
/// `TYPE_ELEMS` store (`[type=13][…][cap][elem × cap]`).
fn push_array(memory: &mut Vec<u8>, elems: &[u64]) {
    let base = memory.len();
    let len = u32::try_from(elems.len()).expect("len");
    let store = u32::try_from(base + 24).expect("store");
    memory.extend_from_slice(&[TYPE_ARRAY, 0, 0, 0]);
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0x7FFA_0000_0000_0000_u64.to_le_bytes());
    memory.extend_from_slice(&len.to_le_bytes());
    memory.extend_from_slice(&store.to_le_bytes());
    memory.extend_from_slice(&[crate::abi::TYPE_ELEMS, 0, 0, 0]);
    memory.extend_from_slice(&len.to_le_bytes());
    for bits in elems {
        memory.extend_from_slice(&bits.to_le_bytes());
    }
}

#[test]
fn decode_array_reads_elements_from_memory() {
    // A hand-built heap array `[1, 2]` at base offset 8: the handle, then its
    // element store.
    let mut memory = vec![0u8; 8];
    push_array(&mut memory, &[1.0_f64.to_bits(), 2.0_f64.to_bits()]);
    // Array jsval: single heap tag 0b101 (HEAP_BITS) with block base offset 8.
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x8;
    assert_eq!(
        decode(bits, &memory),
        Value::Array(vec![Value::Number(1.0), Value::Number(2.0)], Vec::new())
    );
}

#[test]
fn function_value_decode_reads_type_byte() {
    // A hand-built function block at base offset 8: [type=3][pad×3][props=0]
    // [proto=null][func_idx:u32=0][reserved:u32][env:i64=undefined], with no
    // `__fn_info` table (its address word is 0) and so no name.
    let mut memory = vec![0u8; 8];
    memory.extend_from_slice(&[TYPE_FUNCTION, 0, 0, 0]);
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0x7FFA_0000_0000_0000_u64.to_le_bytes());
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0u32.to_le_bytes());
    memory.extend_from_slice(&0x7FFB_0000_0000_0000_u64.to_le_bytes());
    let bits = 0x7FFD_0000_0000_0000_u64 | 0x8;
    assert_eq!(
        to_js_string(&decode(bits, &memory)),
        "[Function (anonymous)]"
    );
}

/// A store far past the end makes the array sparse: the gap is never
/// allocated, so a length of a million costs no ~8 MB element store.
#[test]
fn sparse_store_does_not_allocate_the_gap() {
    let (len, memory) = run_with_memory("const a = []; a[1e6] = 1; a.length");
    assert_eq!(len, 1_000_001.0);
    assert!(
        memory < 4 * 1024 * 1024,
        "linear memory grew to {memory} bytes"
    );
}

/// The dense bound is a gap of 1024 holes past the length, whatever the store
/// capacity: stores that each land 1024 past the end (or roughly double the
/// index, test262 `S15.4_A1.1_T10`) stay sparse and small, while a gap of
/// 1023 still grows the store densely.
#[test]
fn dense_growth_is_bounded_by_the_gap() {
    let (len, sparse) = run_with_memory(
        "const a = []; for (let k = 0; k < 1000; k++) a[a.length + 1024] = k; a.length",
    );
    assert_eq!(len, 1_025_000.0);
    assert!(
        sparse < 4 * 1024 * 1024,
        "sparse stores grew memory to {sparse} bytes"
    );
    let (len, dense) = run_with_memory(
        "const a = []; for (let k = 0; k < 1000; k++) a[a.length + 1023] = k; a.length",
    );
    assert_eq!(len, 1_024_000.0);
    assert!(
        dense > 8 * 1024 * 1024,
        "dense stores only grew memory to {dense} bytes"
    );
    let (len, doubling) = run_with_memory(
        "const x = []; let k = 1; for (let i = 0; i < 32; i++) { k = k * 2; x[k - 2] = k } x.length",
    );
    assert_eq!(len, 4_294_967_295.0);
    assert!(
        doubling < 4 * 1024 * 1024,
        "doubling stores grew memory to {doubling} bytes"
    );
}

/// Where the WASI test guests store the errno of their call.
const GUEST_ERRNO: u64 = 32;
/// Where the WASI test guests have `nwritten` stored.
const GUEST_NWRITTEN: i32 = 16;

/// A command module importing the four WASI functions (indices `0..=3`), with
/// `data` in memory and `body` as `_start`.
fn wasi_guest(data: &[(i32, &[u8])], body: &[Instruction<'_>]) -> Vec<u8> {
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32; 4], [ValType::I32]);
    types
        .ty()
        .function([ValType::I32, ValType::I64, ValType::I32], [ValType::I32]);
    types.ty().function([ValType::I32; 2], [ValType::I32]);
    types.ty().function([ValType::I32], []);
    types.ty().function([], []);

    let mut imports = ImportSection::new();
    for (ty, name) in ["fd_write", "clock_time_get", "random_get", "proc_exit"]
        .into_iter()
        .enumerate()
    {
        let ty = u32::try_from(ty).expect("four imports");
        imports.import("wasi_snapshot_preview1", name, EntityType::Function(ty));
    }

    let mut functions = FunctionSection::new();
    functions.function(4);

    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 1,
        maximum: Some(1),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });

    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("_start", ExportKind::Func, 4);

    let mut code = CodeSection::new();
    let mut start = Function::new([]);
    for instruction in body {
        start.instruction(instruction);
    }
    start.instruction(&Instruction::End);
    code.function(&start);

    let mut segments = DataSection::new();
    for &(offset, bytes) in data {
        segments.active(0, &ConstExpr::i32_const(offset), bytes.iter().copied());
    }

    let mut module = Module::new();
    module
        .section(&types)
        .section(&imports)
        .section(&functions)
        .section(&memories)
        .section(&exports)
        .section(&code)
        .section(&segments);
    module.finish()
}

/// `_start` calling `fd_write(fd, 0, iovs_len, GUEST_NWRITTEN)` and storing
/// the errno at [`GUEST_ERRNO`].
fn fd_write_body(fd: i32, iovs_len: i32) -> [Instruction<'static>; 7] {
    [
        Instruction::I32Const(0),
        Instruction::I32Const(fd),
        Instruction::I32Const(0),
        Instruction::I32Const(iovs_len),
        Instruction::I32Const(GUEST_NWRITTEN),
        Instruction::Call(0),
        Instruction::I32Store(MemArg {
            offset: GUEST_ERRNO,
            align: 2,
            memory_index: 0,
        }),
    ]
}

/// The little-endian `u32` at `offset` of a finished guest's memory.
fn guest_u32(memory: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        memory[offset..offset + 4]
            .try_into()
            .expect("four-byte slice"),
    )
}

/// Two iovecs at address `0`: `"he"` at 100 and `"llo"` at 200.
const TWO_IOVECS: [(i32, &[u8]); 3] = [
    (0, &[100, 0, 0, 0, 2, 0, 0, 0, 200, 0, 0, 0, 3, 0, 0, 0]),
    (100, b"he"),
    (200, b"llo"),
];

#[test]
fn wasi_fd_write_multi_iovec() {
    let done = run_inspect(&wasi_guest(&TWO_IOVECS, &fd_write_body(1, 2))).expect("runs");
    assert_eq!(done.code, 0);
    assert_eq!(done.stdout, "hello");
    assert_eq!(done.stderr, "");
    assert_eq!(guest_u32(&done.memory, 32), 0);
    assert_eq!(guest_u32(&done.memory, 16), 5);
}

#[test]
fn wasi_unknown_fd_returns_badf() {
    let done = run_inspect(&wasi_guest(&TWO_IOVECS, &fd_write_body(3, 2))).expect("runs");
    assert_eq!(done.code, 0);
    assert_eq!(done.stdout, "");
    assert_eq!(done.stderr, "");
    assert_eq!(guest_u32(&done.memory, 32), 8);
    assert_eq!(guest_u32(&done.memory, 16), 0);
}

#[test]
fn wasi_proc_exit_code_passthrough() {
    let body = [Instruction::I32Const(42), Instruction::Call(3)];
    let done = run_inspect(&wasi_guest(&[], &body)).expect("runs");
    assert_eq!(done.code, 42);
}
