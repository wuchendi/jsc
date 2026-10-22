//! Opcount/fuel snapshot gate (`DESIGN.md` section 6.4).
//!
//! Every `tests/opcount/*.js` program is compiled in-process and measured:
//!
//! - static columns, walked with `wasmparser` over the code section of `main`
//!   and the user functions only (helpers are shared and excluded);
//! - `fuel`, the wasm instructions executed under wasmtime fuel metering.
//!
//! The numbers must equal `tests/opcount/snapshot.tsv` exactly: any increase or
//! decrease fails. `JSC_OPCOUNT_UPDATE=1` rewrites the snapshot instead,
//! keeping each row's `reason` cell (a human fills reasons for changed rows).
#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use wasmparser::{Operator, Parser, Payload};

const HEADER: &str = "program\tuser_ops\tcall_indirect\tprop_calls\thost_calls\tfuel\treason";

/// Offsets of the property-access helpers `__get`/`__set`/`__has`/`__delete`
/// past the first helper (`__alloc`) in the emitter's helper layout
/// (`emit/sections.rs`): `__alloc`, `__concat`, `__str_eq`, `__str_cmp`,
/// `__str_index`, the six interning helpers, `__to_key`, `__prop_find`,
/// `__define`, then `__get` (+14), `__set`, `__has`, `__delete` (+17).
const PROP_HELPERS_PAST_ALLOC: [u32; 4] = [14, 15, 16, 17];

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    user_ops: u64,
    call_indirect: u64,
    prop_calls: u64,
    host_calls: u64,
    fuel: u64,
    reason: String,
}

impl Row {
    fn numbers(&self) -> [(&'static str, u64); 5] {
        [
            ("user_ops", self.user_ops),
            ("call_indirect", self.call_indirect),
            ("prop_calls", self.prop_calls),
            ("host_calls", self.host_calls),
            ("fuel", self.fuel),
        ]
    }
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/opcount")
}

/// Measure one program. Function index layout (`emit/mod.rs`): the host
/// imports come first (`0..imports`), then `main`, then the `n` user functions
/// (`n` = `IrProgram::functions.len()`), then the runtime helpers. "User code"
/// is code-section bodies `0..=n` (`main` + user functions); a `call` target
/// below `imports` is a host call; `prop_calls` counts calls to the
/// property-access helpers `__get`/`__set`/`__has`/`__delete`.
fn measure(src: &str) -> Row {
    let program = jsc_core::frontend::parse(src, jsc_core::frontend::Dialect::Js).expect("parse");
    let ir = jsc_core::codegen::lower(&program).expect("lower");
    let user_bodies = ir.functions.len() + 1;
    let wasm = jsc_core::emit::emit(&ir);

    let mut imports = 0u32;
    let mut row = Row {
        user_ops: 0,
        call_indirect: 0,
        prop_calls: 0,
        host_calls: 0,
        fuel: 0,
        reason: String::new(),
    };
    let mut body = 0usize;
    for payload in Parser::new(0).parse_all(&wasm) {
        match payload.expect("valid payload") {
            Payload::ImportSection(reader) => {
                imports = reader.count();
            }
            Payload::CodeSectionEntry(func) => {
                if body < user_bodies {
                    let alloc = imports + u32::try_from(user_bodies).expect("index fits u32");
                    let mut ops = func.get_operators_reader().expect("operators");
                    while !ops.eof() {
                        let op = ops.read().expect("operator");
                        row.user_ops += 1;
                        match op {
                            Operator::CallIndirect { .. } => row.call_indirect += 1,
                            Operator::Call { function_index } if function_index < imports => {
                                row.host_calls += 1;
                            }
                            Operator::Call { function_index }
                                if PROP_HELPERS_PAST_ALLOC
                                    .iter()
                                    .any(|d| alloc + d == function_index) =>
                            {
                                row.prop_calls += 1;
                            }
                            _ => {}
                        }
                    }
                }
                body += 1;
            }
            _ => {}
        }
    }

    let (out, fuel) = jsc_core::run::run_metered(&wasm).expect("program runs cleanly");
    assert!(!out.is_empty(), "corpus program printed nothing");
    row.fuel = fuel;
    row
}

fn measure_corpus() -> BTreeMap<String, Row> {
    let mut rows = BTreeMap::new();
    for entry in std::fs::read_dir(corpus_dir()).expect("corpus dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|e| e == "js") {
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .expect("utf-8 name")
                .to_owned();
            let src = std::fs::read_to_string(&path).expect("read program");
            rows.insert(name, measure(&src));
        }
    }
    rows
}

fn parse_snapshot(text: &str) -> BTreeMap<String, Row> {
    let mut rows = BTreeMap::new();
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some(HEADER), "snapshot header");
    for line in lines.filter(|l| !l.is_empty()) {
        let cells: Vec<&str> = line.split('\t').collect();
        assert!(cells.len() >= 6, "malformed snapshot row: {line:?}");
        let num = |i: usize| -> u64 {
            cells[i]
                .parse()
                .unwrap_or_else(|_| panic!("bad number in snapshot row: {line:?}"))
        };
        rows.insert(
            cells[0].to_owned(),
            Row {
                user_ops: num(1),
                call_indirect: num(2),
                prop_calls: num(3),
                host_calls: num(4),
                fuel: num(5),
                reason: cells.get(6).copied().unwrap_or_default().to_owned(),
            },
        );
    }
    rows
}

fn render_snapshot(rows: &BTreeMap<String, Row>) -> String {
    let mut out = format!("{HEADER}\n");
    for (name, r) in rows {
        let _ = writeln!(
            out,
            "{name}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.user_ops, r.call_indirect, r.prop_calls, r.host_calls, r.fuel, r.reason
        );
    }
    out
}

/// A readable per-row diff of `measured` against `snapshot`; empty when every
/// numeric column and the program set match.
fn diff(snapshot: &BTreeMap<String, Row>, measured: &BTreeMap<String, Row>) -> String {
    let mut out = String::new();
    for (name, m) in measured {
        match snapshot.get(name) {
            None => {
                let _ = writeln!(out, "{name}: missing from snapshot");
            }
            Some(s) => {
                for ((col, old), (_, new)) in s.numbers().into_iter().zip(m.numbers()) {
                    if old != new {
                        let delta = i128::from(new) - i128::from(old);
                        let _ = writeln!(out, "{name}: {col} {old} -> {new} ({delta:+})");
                    }
                }
            }
        }
    }
    for name in snapshot.keys().filter(|n| !measured.contains_key(*n)) {
        let _ = writeln!(out, "{name}: in snapshot but no corpus program");
    }
    out
}

#[test]
fn opcount_snapshot() {
    let path = corpus_dir().join("snapshot.tsv");
    let mut measured = measure_corpus();
    assert!(!measured.is_empty(), "empty opcount corpus");
    let snapshot = std::fs::read_to_string(&path)
        .map(|t| parse_snapshot(&t))
        .unwrap_or_default();

    if std::env::var_os("JSC_OPCOUNT_UPDATE").is_some_and(|v| v == "1") {
        for (name, row) in &mut measured {
            if let Some(old) = snapshot.get(name) {
                row.reason.clone_from(&old.reason);
            }
        }
        std::fs::write(&path, render_snapshot(&measured)).expect("write snapshot");
        return;
    }

    let report = diff(&snapshot, &measured);
    assert!(
        report.is_empty(),
        "opcount snapshot mismatch (rerun with JSC_OPCOUNT_UPDATE=1 and fill `reason` for \
         changed rows if intended):\n{report}"
    );
}

/// `n` pushes onto one array cost amortized O(1) each: the fuel of 1e5 pushes
/// stays well under 2.5x the fuel of 5e4 (a per-push copy would be ~4x).
#[test]
fn push_fuel_is_linear() {
    let pushes = |n: u32| {
        format!("const a = []; for (let i = 0; i < {n}; i++) a.push(i); console.log(a.length)")
    };
    let half = measure(&pushes(50_000)).fuel;
    let full = measure(&pushes(100_000)).fuel;
    assert!(
        full * 2 < half * 5,
        "push fuel is not linear: fuel(5e4) = {half}, fuel(1e5) = {full}"
    );
}
