use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, EntityType, ExportKind, ExportSection, Function,
    FunctionSection, ImportSection, Instruction, MemArg, MemorySection, MemoryType, Module,
    TypeSection, ValType,
};

use super::run_inspect;
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
