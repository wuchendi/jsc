//! Runtime shaking: the body of every runtime function a program cannot
//! reach is replaced by an `unreachable` stub (DESIGN.md §4.2). Function
//! indices, types and the funcref table are unchanged.
//!
//! While a body is emitted the emitters record what it uses ([`Uses`]): the
//! static strings it looks up, the builtins and intrinsics it names by
//! address or table slot, and whether it converts a run-time value to a
//! property key. [`shake`] then keeps, to a fixpoint:
//!
//! - `main`, the user functions, the settlers and `_start`;
//! - every function a kept body calls directly;
//! - every builtin and callable intrinsic a kept body names;
//! - every builtin installed in the realm under a string key some kept body
//!   looks up, under a symbol key, as an accessor half, or nowhere (the
//!   internal ones), and every callable intrinsic so installed or on the
//!   prototype chain of a kept one.
//!
//! In a program that cannot reach binary data (`Helpers::has_binary` false)
//! none of these realm rules applies to a binary-data builtin or to an entry
//! of, or naming, a binary-data constructor or prototype ([`Entry::is_binary`]):
//! nothing else can hand out such a value, so their bodies are stubbed unless a
//! kept body names them.
//!
//! A program is left whole when its own code converts a run-time value that
//! may be a string to a property key (a computed member access), or when it
//! keeps a body that reads or redefines a property chosen by a run-time key
//! ([`note_runtime_key`]), or that reads the values under the own-key list of
//! an object that may be a proxy ([`note_listed_values`]): either can reach a
//! builtin by a name no body mentions. The registry match above is by key
//! text alone, so a builtin with such a use that shares a common name (`get`,
//! `set`, `defineProperty`, ...) is kept whenever any kept body looks that
//! name up; its use counts only when the program can call it with a key of
//! its choosing ([`unnamed`]), and without a way to reach `%Reflect%` a
//! `Reflect` body is a stub that records no use at all (DESIGN.md §6.11).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use wasm_encoder::{Encode, Function, Instruction};
use wasmparser::{BinaryReader, FunctionBody, Operator};

use super::realm::{self, Builtin, Entry, Intrinsic, Key, Value};
use super::roots::{classify_is_numeric, never_string};
use super::{Helpers, IMPORT_FUNC_COUNT, IrNode, StrOffsets};
use crate::ir::KEY_SECOND;

/// What the emission of one body (or one family of bodies) used.
#[derive(Debug, Default, Clone)]
pub(super) struct Uses {
    /// Offsets of the static strings looked up.
    strings: HashSet<i32>,
    /// The subset of `strings` not looked up as the callee of a call whose
    /// key argument is a constant ([`constant_key_callee`]).
    names: HashSet<i32>,
    /// Builtins named by method block, table slot or function index.
    builtins: Vec<Builtin>,
    /// Intrinsics named by address.
    intrinsics: Vec<Intrinsic>,
    /// A run-time value that may be a string was converted to a property key.
    dynamic_key: bool,
    /// A runtime body reads or redefines a property chosen by a run-time key.
    runtime_key: bool,
}

thread_local! {
    /// The uses recorded since the last [`take`].
    static USES: RefCell<Uses> = RefCell::default();
    /// The nesting depth of [`quiet`].
    static QUIET: Cell<u32> = const { Cell::new(0) };
    /// The nesting depth of [`constant_key_callee`].
    static CALLEE: Cell<u32> = const { Cell::new(0) };
}

/// Record a lookup of the static string at `offset`.
pub(super) fn note_string(offset: i32) {
    USES.with_borrow_mut(|uses| {
        uses.strings.insert(offset);
        if CALLEE.get() == 0 {
            uses.names.insert(offset);
        }
    });
}

/// Record a reference to `builtin`.
pub(super) fn note_builtin(builtin: Builtin) {
    USES.with_borrow_mut(|uses| uses.builtins.push(builtin));
}

/// Record a reference to `intrinsic`.
pub(super) fn note_intrinsic(intrinsic: Intrinsic) {
    USES.with_borrow_mut(|uses| uses.intrinsics.push(intrinsic));
}

/// Record the conversion of the run-time value `key` to a property key,
/// unless inside [`quiet`].
pub(super) fn note_key(key: &IrNode) {
    if QUIET.get() == 0 && !constant_key(key) {
        USES.with_borrow_mut(|uses| uses.dynamic_key = true);
    }
}

/// Record that the runtime body being emitted returns, or changes the
/// attributes of, a property its caller picks by a run-time key: a
/// descriptor read, a `defineProperty`, the property list of
/// `JSON.stringify`.
pub(super) fn note_runtime_key() {
    USES.with_borrow_mut(|uses| uses.runtime_key = true);
}

/// Record that the body being emitted reads, with `[[Get]]`, the value under
/// each key of the own-key list of an object, and hands it to user code
/// (`Object.values` / `entries`, `Object.assign`, `CopyDataProperties`, a
/// `JSON.parse` reviver walk). An ordinary object lists only its enumerable
/// keys and no builtin is enumerable, but a proxy's `ownKeys` and
/// `getOwnPropertyDescriptor` traps can list any key of its target as
/// enumerable, whose value the `[[Get]]` then forwards: a use only while the
/// program can hold a proxy (DESIGN.md §6.11).
pub(super) fn note_listed_values(helpers: Helpers<'_>) {
    if helpers.proxies() {
        note_runtime_key();
    }
}

/// Whether `node`, as a property key, is a constant [`note_key`] does not
/// record.
pub(super) fn constant_key(node: &IrNode) -> bool {
    matches!(node, IrNode::Str(_)) || classify_is_numeric(node) || never_string(node)
}

/// Whether `o.method(args)` is a constant-key call: `method` is one of
/// [`KEY_SECOND`] and the key argument `args[1]` is a [`constant_key`].
pub(super) fn constant_key_call(method: &(impl PartialEq<str> + ?Sized), args: &[IrNode]) -> bool {
    KEY_SECOND.iter().any(|m| method.eq(*m)) && args.get(1).is_some_and(constant_key)
}

/// Run `emit`, the lookup of the callee `o.m` of a call `o.m(a, key, ..)`
/// that is a [`constant_key_call`] (an [`IrNode::MethodCallUser`], or the
/// [`IrNode::ProtoGuard`] / [`IrNode::ProtoMethod`] of a `get` call, which
/// lowers as a `Map.prototype.get` fast path): the strings it looks up keep
/// their realm entries but do not name them for [`unnamed`]
/// (`Reflect.get(o, 'x')` shakes like `o.x`).
pub(super) fn constant_key_callee<T>(emit: impl FnOnce() -> T) -> T {
    CALLEE.set(CALLEE.get() + 1);
    let value = emit();
    CALLEE.set(CALLEE.get() - 1);
    value
}

/// Run `emit`, whose keys are locals holding a key already recorded.
pub(super) fn quiet(emit: impl FnOnce()) {
    QUIET.set(QUIET.get() + 1);
    emit();
    QUIET.set(QUIET.get() - 1);
}

/// The uses recorded since the last call.
pub(super) fn take() -> Uses {
    USES.take()
}

/// The function indices a body calls directly.
fn callees(body: &Function) -> Vec<u32> {
    let mut bytes = Vec::new();
    body.encode(&mut bytes);
    // Skip the LEB128 size prefix.
    let prefix = bytes.iter().take_while(|byte| **byte & 0x80 != 0).count() + 1;
    let Some(code) = bytes.get(prefix..) else {
        return Vec::new();
    };
    let Ok(reader) = FunctionBody::new(BinaryReader::new(code, 0)).get_operators_reader() else {
        return Vec::new();
    };
    reader
        .into_iter()
        .filter_map(|op| match op {
            Ok(Operator::Call { function_index } | Operator::ReturnCall { function_index }) => {
                Some(function_index)
            }
            _ => None,
        })
        .collect()
}

/// The reachability state of [`shake`].
struct Shaker<'a> {
    bodies: &'a [Function],
    /// The [`Uses`] of each body.
    uses: &'a [Uses],
    helpers: Helpers<'a>,
    kept: Vec<bool>,
    /// Kept bodies whose calls and uses are still to be followed.
    pending: Vec<usize>,
    strings: HashSet<i32>,
    intrinsics: Vec<Intrinsic>,
}

impl Shaker<'_> {
    /// Keep the function of index `function`.
    fn keep(&mut self, function: u32) {
        let Some(slot) = function.checked_sub(IMPORT_FUNC_COUNT) else {
            return;
        };
        let slot = slot as usize;
        if self.kept.get(slot) == Some(&false) {
            self.kept[slot] = true;
            self.pending.push(slot);
        }
    }

    fn keep_builtin(&mut self, builtin: Builtin) {
        self.keep(self.helpers.builtin_index(builtin));
    }

    /// The program can hold `intrinsic`: keep its body and its prototype
    /// chain.
    fn obtain(&mut self, intrinsic: Intrinsic) {
        let mut next = Some(intrinsic);
        while let Some(intrinsic) = next {
            if self.intrinsics.contains(&intrinsic) {
                return;
            }
            self.intrinsics.push(intrinsic);
            if let Some(builtin) = intrinsic.call() {
                self.keep_builtin(builtin);
            }
            next = intrinsic.proto();
        }
    }

    fn obtain_value(&mut self, value: Value) {
        match value {
            Value::Method(builtin) => self.keep_builtin(builtin),
            Value::Intrinsic(intrinsic) => self.obtain(intrinsic),
            Value::Accessor { get, set } => {
                self.keep_builtin(get);
                if let Some(set) = set {
                    self.keep_builtin(set);
                }
            }
            Value::Str(_)
            | Value::Num(_)
            | Value::Undefined
            | Value::Bool(_)
            | Value::Symbol(_) => {}
        }
    }

    /// Follow every pending body.
    fn drain(&mut self) {
        while let Some(slot) = self.pending.pop() {
            for callee in callees(&self.bodies[slot]) {
                self.keep(callee);
            }
            let uses = &self.uses[slot];
            self.strings.extend(&uses.strings);
            for builtin in uses.builtins.clone() {
                self.keep_builtin(builtin);
            }
            for intrinsic in uses.intrinsics.clone() {
                self.obtain(intrinsic);
            }
        }
    }
}

/// The body slots of the builtins whose run-time-key uses do not count: a
/// builtin installed in the realm under a string key (not a callable
/// intrinsic) that no kept body references (by method block, table slot or
/// call) and whose keys the program's own code (`main` / user bodies) never
/// looks up, except as the callee of a constant-key call
/// ([`constant_key_callee`]). Such a builtin is kept only because a kept
/// runtime body looks up a property of that common name (a proxy trap name,
/// the `Map` constructor's `"set"`, a descriptor field) on an object of its
/// own; it runs only if that object is its home object handed over by the
/// program (`new Proxy(t, Reflect)`), and then with keys the runtime body
/// chose: the program's own (already recorded) keys, or the keys of an
/// own-key list, whose values reach user code only through a body that
/// records [`note_listed_values`] (DESIGN.md §4.2, §6.11).
fn unnamed(
    uses: &[Uses],
    kept: &[bool],
    user_count: usize,
    helpers: Helpers<'_>,
    strings: &StrOffsets,
) -> Vec<usize> {
    let registry = realm::registry();
    let own = &uses[..uses.len().min(user_count + 1)];
    let referenced: Vec<Builtin> = uses
        .iter()
        .zip(kept)
        .filter(|(_, kept)| **kept)
        .flat_map(|(uses, _)| uses.builtins.iter().copied())
        .collect();
    Builtin::ALL
        .into_iter()
        .filter(|builtin| {
            !referenced.contains(builtin)
                && !realm::intrinsics().any(|intrinsic| intrinsic.call() == Some(*builtin))
        })
        .filter(|builtin| {
            let keys: Vec<&str> = registry
                .iter()
                .filter_map(|entry| match entry.key {
                    Key::Str(key) if entry.value == Value::Method(*builtin) => Some(key),
                    _ => None,
                })
                .collect();
            !keys.is_empty()
                && !keys.iter().any(|key| {
                    strings
                        .peek(key)
                        .is_some_and(|offset| own.iter().any(|uses| uses.names.contains(&offset)))
                })
        })
        .filter_map(|builtin| {
            helpers
                .builtin_index(builtin)
                .checked_sub(IMPORT_FUNC_COUNT)
                .map(|slot| slot as usize)
        })
        .collect()
}

/// Replace the body of every unreachable runtime function in `bodies` by a
/// stub. `bodies[0]` is `main`, followed by the `user_count` user functions and
/// the helpers in function-index order; `uses[i]` is what `bodies[i]` used.
pub(super) fn shake(
    bodies: &mut [Function],
    uses: &[Uses],
    user_count: usize,
    helpers: Helpers<'_>,
    strings: &StrOffsets,
) {
    if uses
        .iter()
        .take(user_count + 1)
        .any(|uses| uses.dynamic_key)
    {
        return;
    }
    let registry = realm::registry();
    // Without binary data no binary realm entry or builtin is a root.
    let binary = helpers.has_binary();
    let live = |entry: &Entry| binary || !entry.is_binary();
    let mut shaker = Shaker {
        bodies,
        uses,
        helpers,
        kept: vec![false; bodies.len()],
        pending: Vec::new(),
        strings: HashSet::new(),
        intrinsics: Vec::new(),
    };
    for function in 0..=u32::try_from(user_count).unwrap_or(u32::MAX) {
        shaker.keep(IMPORT_FUNC_COUNT + function);
    }
    let settlers = helpers.group(super::registry::Group::Settlers);
    for function in [settlers, settlers + 1, helpers.start()] {
        shaker.keep(function);
    }
    // The builtins no string key installs.
    for builtin in Builtin::ALL {
        let named = registry
            .iter()
            .any(|entry| matches!(entry.key, Key::Str(_)) && entry.value == Value::Method(builtin))
            || realm::intrinsics().any(|intrinsic| intrinsic.call() == Some(builtin));
        if !named && (binary || !builtin.is_binary()) {
            shaker.keep_builtin(builtin);
        }
    }
    for entry in &registry {
        if live(entry)
            && (matches!(entry.key, Key::Symbol(_))
                || matches!(entry.value, Value::Accessor { .. }))
        {
            shaker.obtain_value(entry.value);
        }
    }
    shaker.obtain(Intrinsic::FunctionPrototype);
    loop {
        shaker.drain();
        for entry in &registry {
            let Key::Str(key) = entry.key else { continue };
            if live(entry)
                && strings
                    .peek(key)
                    .is_some_and(|offset| shaker.strings.contains(&offset))
            {
                shaker.obtain_value(entry.value);
            }
        }
        if shaker.pending.is_empty() {
            break;
        }
    }
    let kept = shaker.kept;
    let unnamed = unnamed(uses, &kept, user_count, helpers, strings);
    if kept
        .iter()
        .zip(uses)
        .enumerate()
        .any(|(slot, (kept, uses))| *kept && uses.runtime_key && !unnamed.contains(&slot))
    {
        return;
    }
    for (body, kept) in bodies.iter_mut().zip(kept) {
        if !kept {
            let mut stub = Function::new([]);
            stub.instruction(&Instruction::Unreachable);
            stub.instruction(&Instruction::End);
            *body = stub;
        }
    }
}
