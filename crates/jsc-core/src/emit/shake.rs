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
//! A program is left whole when its own code converts a run-time value that
//! may be a string to a property key (a computed member access), or when it
//! keeps a runtime body that reads or redefines a property chosen by a
//! run-time key ([`note_runtime_key`]): either can reach a builtin by a name
//! no body mentions.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;

use wasm_encoder::{Encode, Function, Instruction};
use wasmparser::{BinaryReader, FunctionBody, Operator};

use super::realm::{self, Builtin, Intrinsic, Key, Value};
use super::roots::{classify_is_numeric, never_string};
use super::{Helpers, IMPORT_FUNC_COUNT, IrNode, StrOffsets};

/// What the emission of one body (or one family of bodies) used.
#[derive(Debug, Default, Clone)]
pub(super) struct Uses {
    /// Offsets of the static strings looked up.
    strings: HashSet<i32>,
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
}

/// Record a lookup of the static string at `offset`.
pub(super) fn note_string(offset: i32) {
    USES.with_borrow_mut(|uses| {
        uses.strings.insert(offset);
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
    if QUIET.get() == 0
        && !matches!(key, IrNode::Str(_))
        && !classify_is_numeric(key)
        && !never_string(key)
    {
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
        if !named {
            shaker.keep_builtin(builtin);
        }
    }
    for entry in &registry {
        if matches!(entry.key, Key::Symbol(_)) || matches!(entry.value, Value::Accessor { .. }) {
            shaker.obtain_value(entry.value);
        }
    }
    shaker.obtain(Intrinsic::FunctionPrototype);
    loop {
        shaker.drain();
        for entry in &registry {
            let Key::Str(key) = entry.key else { continue };
            if strings
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
    if kept
        .iter()
        .zip(uses)
        .any(|(kept, uses)| *kept && uses.runtime_key)
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
