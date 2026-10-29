//! `JSON.stringify(value, replacer, space)` and `JSON.parse(text, reviver)`.
//! Without a replacer function or property list the serialization is exactly
//! the inline `JSON.stringify(v)`; both run `__json_rep`, which calls a
//! `toJSON` method and the replacer (with the holder as `this`), keeps only
//! the listed keys, and rejects a cyclic structure. A non-empty gap re-indents the compact text
//! (`__json_indent`). A callable reviver walks the parsed value through
//! `__json_revive` (`InternalizeJSONProperty`).

// The hand-written bodies name their wasm locals tersely, like `props.rs`.
#![allow(clippy::many_single_char_names)]

#[allow(clippy::wildcard_imports)]
use super::*;

/// The `TypeError` of serializing a cyclic structure.
const CYCLE: CalleeErrorText<'static> = [
    "Converting circular structure to JSON",
    "TypeError: Converting circular structure to JSON",
];

/// The static strings of this family: the serializer's tokens, the empty
/// root key, `toJSON`, the cycle error, and the parse syntax error.
pub(super) fn strings() -> impl Iterator<Item = &'static str> {
    JSON_TOKEN_STRS
        .into_iter()
        .chain(["", "toJSON"])
        .chain(CYCLE)
        .chain(JSON_PARSE_ERROR_STRS)
}

/// The IR fragments the bodies emit, for the static-string scan.
pub(super) fn fragments() -> Vec<IrNode> {
    vec![
        IrNode::JsonStringify {
            arg: Box::new(IrNode::Undefined),
        },
        IrNode::JsonParse {
            arg: Box::new(IrNode::Undefined),
        },
        root_holder(0),
    ]
}

/// `{ "": value }` over the i64 local `value`: the root holder of a replacer
/// or reviver walk.
fn root_holder(value: u32) -> IrNode {
    IrNode::Object {
        entries: vec![(PropKey::Static(String::new()), IrNode::Local(value))],
        proto: None,
        class: abi::CLASS_ORDINARY,
        reserve: 0,
    }
}

/// The payload offset of a string block (past the header and the length).
const PAYLOAD: i32 = HEADER_SIZE + 4;

/// A string block's u32 byte length.
const STR_LEN: MemArg = MemArg {
    offset: HEADER_SIZE as u64,
    align: 2,
    memory_index: 0,
};

/// One payload byte (the address on the stack is the block base plus index).
const BYTE: MemArg = MemArg {
    offset: PAYLOAD as u64,
    align: 0,
    memory_index: 0,
};

/// An array handle's u32 length.
const ARR_LEN: MemArg = MemArg {
    offset: ARRAY_LEN_OFFSET,
    align: 2,
    memory_index: 0,
};

/// Box the i32 heap base on the stack as a jsval.
fn box_heap(function: &mut Function) {
    wasm!(function; I64ExtendI32U, I64Const(HEAP_BITS), I64Or);
}

/// Push `1` when the jsval in `local` is a wrapper object (`TYPE_OBJECT`
/// class `BOOLEAN`, `NUMBER` or `STRING`).
fn is_any_wrapper(function: &mut Function, local: u32) {
    emit_is_pointer(function, local);
    wasm!(function;
        If(BlockType::Result(ValType::I32)),
        LocalGet(local), I32WrapI64, I32Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }),
        I32Const(TYPE_OBJECT), I32Eq,
        LocalGet(local), I32WrapI64,
        I32Load8U(MemArg { offset: u64::from(abi::HEADER_CLASS_OFFSET), align: 0, memory_index: 0 }),
        I32Const(i32::from(abi::CLASS_BOOLEAN)), I32Sub, I32Const(3), I32LtU,
        I32And,
        Else, I32Const(0), End,
    );
}

/// Replace a wrapper object in i64 `local` by its `[[PrimitiveValue]]`.
fn unwrap_primitive(function: &mut Function, local: u32) {
    is_any_wrapper(function, local);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(local), I32WrapI64,
        I64Load(MemArg { offset: u64::from(abi::WRAPPER_VALUE_OFFSET), align: 3, memory_index: 0 }),
        LocalSet(local),
        End,
    );
}

/// Push array element `i` (i32 local) of the array handle in i32 local `arr`
/// as `Get` reads it: an element at or past the current length or a hole
/// reads through the prototype chain (`undefined` when absent; a throwing
/// getter returns).
fn elem_or_undefined(function: &mut Function, arr: u32, i: u32, out: u32, helpers: Helpers<'_>) {
    wasm!(function;
        LocalGet(i), LocalGet(arr), I32Load(ARR_LEN), I32LtU,
        If(BlockType::Result(ValType::I64)),
    );
    emit_elem_load(function, arr, i, helpers.sparse_get());
    wasm!(function;
        Else, I64Const(HOLE_BITS), End, LocalTee(out), I64Const(HOLE_BITS), I64Eq,
        If(BlockType::Empty),
        LocalGet(arr), LocalGet(i), Call(helpers.proto_elem()), LocalSet(out),
    );
    return_if_pending(function);
    wasm!(function; End);
    emit_elem_read(function, out);
}

/// Call the function in i64 local `target` with `this` = i64 local `this`
/// and the arguments `args` (i64 locals), leaving its result.
fn call_with(function: &mut Function, target: u32, this: u32, args: &[u32], helpers: Helpers<'_>) {
    for slot in 0..helpers.max_args as usize {
        if let Some(&arg) = args.get(slot) {
            wasm!(function; LocalGet(arg));
        } else {
            wasm!(function; I64Const(UNDEFINED_BITS));
        }
    }
    wasm!(function; LocalGet(this), GlobalSet(THIS_GLOBAL));
    let count = args.len().min(helpers.max_args as usize);
    invoke(
        function,
        target,
        |function| emit_argc(count, function),
        helpers,
    );
}

/// `JSON.stringify(value, replacer, space)`.
pub(super) fn emit_stringify(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (value, rep, list, s) = (frame.i64(0), frame.i64(1), frame.i64(2), frame.i64(3));
    let (space, elem) = (frame.x64(0), frame.x64(1));
    let (arr, n, i, gap) = (frame.i32(0), frame.i32(1), frame.i32(2), frame.i32(3));
    let (j, m) = (frame.x32(0), frame.x32(1));
    for (slot, local) in (0..).zip([value, rep, space]) {
        frame.arg(function, slot);
        wasm!(function; LocalSet(local));
    }
    // A replacer function, or else the property list of a replacer array:
    // its strings, numbers and String/Number wrappers as keys, in order,
    // without duplicates.
    wasm!(function; I64Const(UNDEFINED_BITS), LocalSet(list));
    emit_is_function(function, rep);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_pointer(function, rep);
    wasm!(function; If(BlockType::Result(ValType::I32)));
    emit_load_type_byte(function, rep);
    wasm!(function;
        I32Const(TYPE_ARRAY), I32Eq, Else, I32Const(0), End,
        If(BlockType::Empty),
        I32Const(0), Call(helpers.new_array()),
    );
    box_heap(function);
    wasm!(function;
        LocalSet(list),
        LocalGet(rep), I32WrapI64, LocalTee(arr), I32Load(ARR_LEN), LocalSet(n),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
    );
    elem_or_undefined(function, arr, i, elem, helpers);
    wasm!(function; LocalSet(elem));
    emit_is_string(function, elem);
    wasm!(function; I32Eqz, If(BlockType::Empty));
    emit_is_number(function, elem);
    for class in [abi::CLASS_NUMBER, abi::CLASS_STRING] {
        emit_is_wrapper(function, elem, class);
        wasm!(function; I32Or);
    }
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(elem), Call(helpers.to_str()),
    );
    box_heap(function);
    wasm!(function; LocalSet(elem));
    return_if_pending(function);
    wasm!(function;
        Else, I64Const(UNDEFINED_BITS), LocalSet(elem), End,
        End,
        LocalGet(elem), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Empty),
        LocalGet(elem), I32Const(1), Call(helpers.to_key),
    );
    box_heap(function);
    wasm!(function; LocalSet(elem), LocalGet(list), I32WrapI64, LocalSet(arr));
    emit_array_contains(function, arr, elem, j, m);
    wasm!(function;
        I32Eqz,
        If(BlockType::Empty), LocalGet(arr), LocalGet(elem), Call(helpers.array_push), Drop, End,
        LocalGet(rep), I32WrapI64, LocalSet(arr),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        End,
        I64Const(UNDEFINED_BITS), LocalSet(rep),
        End,
    );
    emit_gap(function, frame, space, gap, helpers);
    // Serialize: the inline serializer unless a replacer applies.
    wasm!(function;
        LocalGet(rep), I64Const(UNDEFINED_BITS), I64Eq,
        LocalGet(list), I64Const(UNDEFINED_BITS), I64Eq, I32And,
        If(BlockType::Empty),
    );
    let node = IrNode::JsonStringify {
        arg: Box::new(IrNode::Local(value)),
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(s), Else);
    emit_node(
        &root_holder(value),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    push_str(function, strings, "");
    wasm!(function;
        LocalGet(value), LocalGet(rep), LocalGet(list),
        I32Const(0), Call(helpers.new_array()), Call(helpers.json_rep()), LocalSet(s),
    );
    return_if_pending(function);
    wasm!(function;
        End,
        LocalGet(gap), I32Const(0), I32Ne, LocalGet(s), I64Const(UNDEFINED_BITS), I64Ne, I32And,
        If(BlockType::Empty),
        LocalGet(s), I32WrapI64, LocalGet(gap), Call(helpers.json_indent()),
    );
    box_heap(function);
    wasm!(function; LocalSet(s), End, LocalGet(s));
}

/// The gap of `space` (i64 local) into i32 local `gap` (`0` for none): a
/// Number / String wrapper is converted first; a number is that many spaces
/// (at most 10), a string its first 10 characters.
fn emit_gap(function: &mut Function, frame: Frame, space: u32, gap: u32, helpers: Helpers<'_>) {
    let (len, i, count) = (frame.x32(0), frame.x32(1), frame.i32(0));
    let width = frame.f64(0);
    emit_is_wrapper(function, space, abi::CLASS_NUMBER);
    wasm!(function; If(BlockType::Empty), LocalGet(space), Call(helpers.to_number), LocalSet(space));
    return_if_pending(function);
    wasm!(function; End);
    emit_is_wrapper(function, space, abi::CLASS_STRING);
    wasm!(function; If(BlockType::Empty), LocalGet(space), Call(helpers.to_str()));
    box_heap(function);
    wasm!(function; LocalSet(space));
    return_if_pending(function);
    wasm!(function; End, I32Const(0), LocalSet(gap));
    emit_is_number(function, space);
    wasm!(function;
        If(BlockType::Empty),
        LocalGet(space), F64ReinterpretI64, F64Trunc, F64Const(Ieee64::from(10.0)), F64Min,
        LocalTee(width), F64Const(Ieee64::from(1.0)), F64Ge,
        If(BlockType::Empty),
        LocalGet(width), I32TruncF64U, LocalTee(count), Call(helpers.new_string), LocalTee(gap),
        I32Const(PAYLOAD), I32Add, I32Const(i32::from(b' ')), LocalGet(count),
        MemoryFill(0),
        End,
        Else,
    );
    emit_is_string(function, space);
    wasm!(function;
        If(BlockType::Empty),
        // `i` = the byte offset past the first 10 characters (UTF-8 lead
        // bytes), `count` the characters seen.
        LocalGet(space), I32WrapI64, LocalTee(gap), I32Load(STR_LEN), LocalSet(len),
        I32Const(0), LocalSet(i), I32Const(0), LocalSet(count),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
        LocalGet(gap), LocalGet(i), I32Add, I32Load8U(BYTE), I32Const(0xc0), I32And,
        I32Const(0x80), I32Ne,
        If(BlockType::Empty),
        LocalGet(count), I32Const(10), I32Eq, BrIf(2),
        LocalGet(count), I32Const(1), I32Add, LocalSet(count),
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(i), I32Eqz,
        If(BlockType::Empty),
        I32Const(0), LocalSet(gap),
        Else,
        LocalGet(i), LocalGet(len), I32LtU,
        If(BlockType::Empty),
        LocalGet(i), Call(helpers.new_string), LocalTee(len), I32Const(PAYLOAD), I32Add,
        LocalGet(gap), I32Const(PAYLOAD), I32Add, LocalGet(i),
        MemoryCopy { src_mem: 0, dst_mem: 0 },
        LocalGet(len), LocalSet(gap),
        End,
        End,
        End,
        End,
    );
}

/// `JSON.parse(text, reviver)`.
pub(super) fn emit_parse(
    function: &mut Function,
    frame: Frame,
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) {
    let (text, reviver, value) = (frame.i64(0), frame.i64(1), frame.i64(2));
    frame.arg(function, 0);
    wasm!(function; LocalSet(text));
    frame.arg(function, 1);
    wasm!(function; LocalSet(reviver));
    let node = IrNode::JsonParse {
        arg: Box::new(IrNode::Local(text)),
    };
    emit_node(&node, function, frame.scratch(), strings, helpers);
    wasm!(function; LocalSet(value));
    return_if_pending(function);
    emit_is_function(function, reviver);
    wasm!(function; I32Eqz, If(BlockType::Empty), LocalGet(value), Return, End);
    emit_node(
        &root_holder(value),
        function,
        frame.scratch(),
        strings,
        helpers,
    );
    wasm!(function;
        I32Const(static_key(strings, "")), LocalGet(reviver), Call(helpers.json_revive()),
    );
}

/// `__json_rep(holder, key, value, replacer, list, stack) -> i64`:
/// `SerializeJSONProperty` — the JSON text of `value` (the property `key` of
/// `holder`) as a string jsval, or `undefined` when it is not serializable.
/// An object's callable `toJSON` maps `value` first, called with `this =
/// value` and the key; then the replacer function (when not `undefined`),
/// called with `this = holder`. `key` is a string jsval, or an array index
/// as a number jsval (converted only when a callee receives it). A wrapper
/// serializes as its primitive; an object serializes the keys of the
/// property-list array `list` (when not `undefined`) or its own enumerable
/// keys. Primitives are `__json_str`. `stack` (an array handle) holds the
/// arrays and objects being serialized: meeting one again throws a
/// `TypeError`. A throw returns with it pending.
#[allow(clippy::too_many_lines)]
pub(in crate::emit) fn emit_json_rep(
    strings: &HashMap<String, i32>,
    helpers: Helpers<'_>,
) -> Function {
    let (holder, key, value, rep, list, stack) = (0, 1, 2, 3, 4, 5);
    let (ptr, n, i, first, acc, keys, j, m) = (6, 7, 8, 9, 10, 11, 12, 13);
    let (elem, s, k, to_json) = (14, 15, 16, 17);
    let scratch = 18;
    let mut locals = vec![(8, ValType::I32), (4, ValType::I64)];
    locals.extend(type_error_helper_locals());
    let mut f = Function::new(locals);
    helpers.unwind.borrow_mut().reset();
    let tok = |text: &str| static_key(strings, text);
    let concat = |f: &mut Function| wasm!(f; Call(helpers.concat), LocalSet(acc));
    // The recursive serialization of `elem` (property `k` of `value`) into
    // `s`, returning when it throws.
    let recurse = |f: &mut Function| {
        wasm!(f;
            LocalGet(value), LocalGet(k), LocalGet(elem), LocalGet(rep), LocalGet(list),
            LocalGet(stack), Call(helpers.json_rep()), LocalSet(s),
        );
        return_if_pending(f);
    };
    // An array index key becomes its string.
    let key_string = |f: &mut Function| {
        emit_is_number(f, key);
        wasm!(f;
            If(BlockType::Empty),
            LocalGet(key), F64ReinterpretI64, I32TruncF64U, Call(helpers.index_string),
        );
        box_heap(f);
        wasm!(f; LocalSet(key), End);
    };
    emit_is_object(&mut f, value);
    wasm!(f;
        If(BlockType::Empty),
        LocalGet(value), I32Const(tok("toJSON")), Call(helpers.get), LocalSet(to_json),
    );
    return_if_pending(&mut f);
    emit_is_function(&mut f, to_json);
    wasm!(f; If(BlockType::Empty));
    key_string(&mut f);
    call_with(&mut f, to_json, value, &[key], helpers);
    wasm!(f; LocalSet(value));
    return_if_pending(&mut f);
    wasm!(f; End, End);
    wasm!(f; LocalGet(rep), I64Const(UNDEFINED_BITS), I64Ne, If(BlockType::Empty));
    key_string(&mut f);
    call_with(&mut f, rep, holder, &[key, value], helpers);
    wasm!(f; LocalSet(value));
    return_if_pending(&mut f);
    wasm!(f; End);
    unwrap_primitive(&mut f, value);
    wasm!(f; LocalGet(value), I64Const(UNDEFINED_BITS), I64Eq);
    emit_is_function(&mut f, value);
    wasm!(f; I32Or, If(BlockType::Empty), I64Const(UNDEFINED_BITS), Return, End);
    emit_is_object(&mut f, value);
    wasm!(f;
        I32Eqz,
        If(BlockType::Empty), LocalGet(value), Call(helpers.json_str),
    );
    box_heap(&mut f);
    wasm!(f; Return, End);
    // A value already being serialized is a cycle; otherwise it joins the
    // stack until its members are serialized.
    emit_array_contains(&mut f, stack, value, j, m);
    wasm!(f; If(BlockType::Empty));
    emit_throw_error(&callee_type_error(CYCLE), &mut f, scratch, strings, helpers);
    wasm!(f;
        End,
        LocalGet(stack), LocalGet(value), Call(helpers.array_push), Drop,
        LocalGet(value), I32WrapI64, LocalTee(ptr),
        I32Load8U(MemArg { offset: 0, align: 0, memory_index: 0 }), I32Const(TYPE_ARRAY), I32Eq,
        If(BlockType::Empty),
        // "[" + each element (`null` when not serializable) joined by "," + "]".
        I32Const(tok("[")), LocalSet(acc),
        LocalGet(ptr), I32Load(ARR_LEN), LocalSet(n),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(i), If(BlockType::Empty), LocalGet(acc), I32Const(tok(",")),
    );
    concat(&mut f);
    wasm!(f; End);
    elem_or_undefined(&mut f, ptr, i, elem, helpers);
    wasm!(f; LocalSet(elem), LocalGet(i), F64ConvertI32U, I64ReinterpretF64, LocalSet(k));
    recurse(&mut f);
    wasm!(f;
        LocalGet(acc),
        LocalGet(s), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Result(ValType::I32)), I32Const(tok("null")), Else, LocalGet(s), I32WrapI64, End,
    );
    concat(&mut f);
    wasm!(f;
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(acc), I32Const(tok("]")),
    );
    concat(&mut f);
    wasm!(f;
        Else,
        // "{" + each serializable `key:value` joined by "," + "}".
        LocalGet(list), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Result(ValType::I32)),
        LocalGet(list), I32WrapI64,
        Else,
        LocalGet(value), I32Const(OWN_KEYS), Call(helpers.own_keys), I32WrapI64,
        End,
        LocalTee(keys), I32Load(ARR_LEN), LocalSet(n),
        I32Const(tok("{")), LocalSet(acc),
        I32Const(1), LocalSet(first),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(keys), I32Load(MemArg { offset: ARRAY_ELEMS_OFFSET, align: 2, memory_index: 0 }),
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)), LocalSet(k),
        LocalGet(value), LocalGet(k), I32WrapI64, Call(helpers.get), LocalSet(elem),
    );
    return_if_pending(&mut f);
    recurse(&mut f);
    wasm!(f;
        LocalGet(s), I64Const(UNDEFINED_BITS), I64Ne,
        If(BlockType::Empty),
        LocalGet(first), I32Eqz, If(BlockType::Empty), LocalGet(acc), I32Const(tok(",")),
    );
    concat(&mut f);
    wasm!(f; End, I32Const(0), LocalSet(first), LocalGet(acc), LocalGet(k), I32WrapI64, Call(helpers.json_escape));
    concat(&mut f);
    wasm!(f; LocalGet(acc), I32Const(tok(":")));
    concat(&mut f);
    wasm!(f; LocalGet(acc), LocalGet(s), I32WrapI64);
    concat(&mut f);
    wasm!(f;
        End,
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        LocalGet(acc), I32Const(tok("}")),
    );
    concat(&mut f);
    // Pop `value` off the stack.
    wasm!(f;
        End,
        LocalGet(stack), LocalGet(stack), I32Load(ARR_LEN), I32Const(1), I32Sub, I32Store(ARR_LEN),
        LocalGet(acc),
    );
    box_heap(&mut f);
    wasm!(f; End);
    f
}

/// `__json_indent(s: i32, gap: i32) -> i32`: the compact JSON text `s`
/// re-indented with the gap string `gap` (`JSON.stringify`'s `space`): a
/// newline plus one gap per nesting level after each `[`/`{` of a non-empty
/// container, each `,` and before each closing bracket, and `": "` for `":"`;
/// string contents are copied unchanged. Two passes: the length, then the
/// bytes.
pub(in crate::emit) fn emit_json_indent(helpers: Helpers<'_>) -> Function {
    let (s, gap) = (0, 1);
    let (len, glen, i, o, depth, b, quoted, dst, g) = (2, 3, 4, 5, 6, 7, 8, 9, 10);
    let mut f = Function::new(vec![(9, ValType::I32)]);
    wasm!(f;
        LocalGet(s), I32Load(STR_LEN), LocalSet(len),
        LocalGet(gap), I32Load(STR_LEN), LocalSet(glen),
    );
    for write in [false, true] {
        let put = |f: &mut Function, byte: &dyn Fn(&mut Function)| {
            if write {
                wasm!(f; LocalGet(dst), LocalGet(o), I32Add);
                byte(f);
                wasm!(f; I32Store8(BYTE));
            }
            wasm!(f; LocalGet(o), I32Const(1), I32Add, LocalSet(o));
        };
        let put_const = |f: &mut Function, c: u8| put(f, &|f| wasm!(f; I32Const(i32::from(c))));
        let put_b = |f: &mut Function| put(f, &|f| wasm!(f; LocalGet(b)));
        // A newline, then `depth` gaps.
        let newline = |f: &mut Function| {
            put_const(f, b'\n');
            wasm!(f;
                LocalGet(depth), LocalSet(g),
                Block(BlockType::Empty), Loop(BlockType::Empty),
                LocalGet(g), I32Eqz, BrIf(1),
            );
            if write {
                wasm!(f;
                    LocalGet(dst), I32Const(PAYLOAD), I32Add, LocalGet(o), I32Add,
                    LocalGet(gap), I32Const(PAYLOAD), I32Add, LocalGet(glen),
                    MemoryCopy { src_mem: 0, dst_mem: 0 },
                );
            }
            wasm!(f;
                LocalGet(o), LocalGet(glen), I32Add, LocalSet(o),
                LocalGet(g), I32Const(1), I32Sub, LocalSet(g),
                Br(0),
                End, End,
            );
        };
        // b = s[i]; i += 1.
        let next = |f: &mut Function| {
            wasm!(f;
                LocalGet(s), LocalGet(i), I32Add, I32Load8U(BYTE), LocalSet(b),
                LocalGet(i), I32Const(1), I32Add, LocalSet(i),
            );
        };
        let is = |f: &mut Function, c: u8| wasm!(f; LocalGet(b), I32Const(i32::from(c)), I32Eq);
        wasm!(f;
            I32Const(0), LocalSet(i), I32Const(0), LocalSet(o),
            I32Const(0), LocalSet(depth), I32Const(0), LocalSet(quoted),
            Block(BlockType::Empty), Loop(BlockType::Empty),
            LocalGet(i), LocalGet(len), I32GeU, BrIf(1),
        );
        next(&mut f);
        wasm!(f; LocalGet(quoted), If(BlockType::Empty));
        put_b(&mut f);
        is(&mut f, b'\\');
        wasm!(f; If(BlockType::Empty));
        next(&mut f);
        put_b(&mut f);
        wasm!(f; Else);
        is(&mut f, b'"');
        wasm!(f; If(BlockType::Empty), I32Const(0), LocalSet(quoted), End, End, Else);
        is(&mut f, b'}');
        is(&mut f, b']');
        wasm!(f; I32Or, If(BlockType::Empty), LocalGet(depth), I32Const(1), I32Sub, LocalSet(depth));
        newline(&mut f);
        put_b(&mut f);
        wasm!(f; Else);
        put_b(&mut f);
        is(&mut f, b'"');
        wasm!(f; If(BlockType::Empty), I32Const(1), LocalSet(quoted), End);
        is(&mut f, b'{');
        is(&mut f, b'[');
        wasm!(f; I32Or, If(BlockType::Empty));
        // An empty container stays `{}` / `[]`.
        wasm!(f;
            LocalGet(s), LocalGet(i), I32Add, I32Load8U(BYTE), LocalTee(g),
            I32Const(i32::from(b'}')), I32Eq, LocalGet(g), I32Const(i32::from(b']')), I32Eq, I32Or,
            If(BlockType::Empty),
        );
        next(&mut f);
        put_b(&mut f);
        wasm!(f; Else, LocalGet(depth), I32Const(1), I32Add, LocalSet(depth));
        newline(&mut f);
        wasm!(f; End, End);
        is(&mut f, b',');
        wasm!(f; If(BlockType::Empty));
        newline(&mut f);
        wasm!(f; End);
        is(&mut f, b':');
        wasm!(f; If(BlockType::Empty));
        put_const(&mut f, b' ');
        wasm!(f; End, End, End, Br(0), End, End);
        if !write {
            wasm!(f; LocalGet(o), Call(helpers.new_string), LocalSet(dst));
        }
    }
    wasm!(f; LocalGet(dst), End);
    f
}

/// `__json_revive(holder: i64, key: i32, reviver: i64) -> i64`:
/// `InternalizeJSONProperty` — the value of property `key` of `holder` with
/// each array element / own enumerable property first replaced by its own
/// revived value (`CreateDataProperty`, deleted when that is `undefined`; a
/// non-configurable property rejects both silently), then passed to the
/// reviver as `(key, value)` with `this = holder`. A throw returns with it
/// pending.
pub(in crate::emit) fn emit_json_revive(helpers: Helpers<'_>) -> Function {
    let (holder, key, reviver) = (0, 1, 2);
    let (val, result, name) = (3, 4, 5);
    let (ptr, n, i, k, array, e) = (6, 7, 8, 9, 10, 11);
    let mut f = Function::new(vec![(3, ValType::I64), (6, ValType::I32)]);
    wasm!(f; LocalGet(holder), LocalGet(key), Call(helpers.get), LocalSet(val));
    return_if_pending(&mut f);
    emit_is_pointer(&mut f, val);
    wasm!(f; If(BlockType::Empty));
    emit_load_type_byte(&mut f, val);
    wasm!(f;
        LocalTee(n), I32Const(TYPE_ARRAY), I32Eq, LocalTee(array),
        LocalGet(n), I32Const(TYPE_OBJECT), I32Eq, I32Or,
        If(BlockType::Empty),
        // The keys: the indices below the length of an array, else the own
        // enumerable keys (`ptr` the key array).
        LocalGet(array),
        If(BlockType::Result(ValType::I32)),
        LocalGet(val), I32WrapI64, I32Load(ARR_LEN),
        Else,
        LocalGet(val), I32Const(OWN_KEYS), Call(helpers.own_keys), I32WrapI64, LocalTee(ptr),
        I32Load(ARR_LEN),
        End,
        LocalSet(n),
        I32Const(0), LocalSet(i),
        Block(BlockType::Empty), Loop(BlockType::Empty),
        LocalGet(i), LocalGet(n), I32GeU, BrIf(1),
        LocalGet(array),
        If(BlockType::Result(ValType::I32)),
        LocalGet(i), F64ConvertI32U, I64ReinterpretF64, I32Const(1), Call(helpers.to_key),
        Else,
        LocalGet(ptr), I32Load(MemArg { offset: ARRAY_ELEMS_OFFSET, align: 2, memory_index: 0 }),
        LocalGet(i), I32Const(3), I32Shl, I32Add, I64Load(elem_slot(0)), I32WrapI64,
        End,
        LocalSet(k),
        LocalGet(val), LocalGet(k), LocalGet(reviver), Call(helpers.json_revive()), LocalSet(result),
    );
    return_if_pending(&mut f);
    wasm!(f;
        LocalGet(result), I64Const(UNDEFINED_BITS), I64Eq,
        If(BlockType::Empty),
        // `[[Delete]]`: a non-configurable property stays (no throw).
        LocalGet(val), I32WrapI64, LocalGet(k), Call(helpers.prop_find), LocalTee(e),
        If(BlockType::Result(ValType::I32)),
        LocalGet(e), I32Load(MemArg { offset: u64::from(abi::PROP_ATTRS_OFFSET), align: 2, memory_index: 0 }),
        I32Const(abi::ATTR_CONFIGURABLE.cast_signed()), I32And,
        Else, I32Const(1),
        End,
        If(BlockType::Empty), LocalGet(val), LocalGet(k), Call(helpers.delete), End,
        Else,
        // `CreateDataProperty` (an array index included): a rejection is
        // ignored.
        LocalGet(val), I32WrapI64, LocalGet(k), I32Const(DESC_DATA_DEFAULT), LocalGet(result),
        I64Const(UNDEFINED_BITS), I64Const(UNDEFINED_BITS), Call(helpers.define_own()), Drop,
        End,
    );
    return_if_pending(&mut f);
    wasm!(f;
        LocalGet(i), I32Const(1), I32Add, LocalSet(i),
        Br(0),
        End, End,
        End,
        End,
        LocalGet(key),
    );
    box_heap(&mut f);
    wasm!(f; LocalSet(name));
    call_with(&mut f, reviver, holder, &[name, val], helpers);
    wasm!(f; End);
    f
}
