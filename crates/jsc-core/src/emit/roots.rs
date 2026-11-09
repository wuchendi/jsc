//! The static facts the [`builder`](super::builder) needs to place roots:
//! which helpers cannot reach `__alloc` (the non-allocating leaf set) and which
//! jsval locals and operands can never hold a heap reference (`NoRoot`,
//! DESIGN.md §5.7).
//!
//! # Leaf set
//!
//! Every helper that might be a leaf declares its callees in
//! [`helper_graph`]. A helper is a leaf iff all its declared callees are, so
//! an undeclared helper (and `__alloc`, never declared) stays a safepoint.
//! Adding a `Call` to a declared helper requires adding the edge here, which
//! `leaf_declarations_match_bodies` enforces.
//!
//! # `NoRoot`
//!
//! A value is `NoRoot` when it can only be a number, boolean, `null`,
//! `undefined`, a hole or a static block. [`classify`] gives the shallow
//! class of one IR value and [`Solution::solve`] runs the fixpoint over a
//! function's local definitions. Unknown nodes are never `NoRoot`, so a new
//! IR variant is rooted until someone proves otherwise.

#[allow(clippy::wildcard_imports)]
use super::*;

use std::collections::{HashMap, HashSet};

/// The non-allocating leaf helpers, as wasm function indices. WASI imports
/// never call back; the remaining `env` imports do and are not leaves.
pub(super) struct LeafSet(HashSet<u32>);

impl LeafSet {
    /// Whether a `call` of function index `callee` can never allocate.
    pub(super) fn is_leaf(&self, callee: u32) -> bool {
        callee < WASI_IMPORT_COUNT || self.0.contains(&callee)
    }

    /// The leaf set of an empty call graph (builder unit tests).
    #[cfg(test)]
    pub(super) fn empty() -> Self {
        Self(HashSet::new())
    }
}

/// The count of WASI imports, occupying the lowest function indices.
const WASI_IMPORT_COUNT: u32 = 4;

/// Each candidate leaf helper and its declared callees.
pub(super) fn helper_graph(helpers: Helpers<'_>) -> Vec<(u32, Vec<u32>)> {
    vec![
        (helpers.str_eq, vec![]),
        (helpers.str_cmp, vec![]),
        (helpers.str_hash, vec![]),
        (helpers.intern_slot, vec![helpers.str_hash, helpers.str_eq]),
        (helpers.intern_find, vec![helpers.intern_slot]),
        (helpers.index_value, vec![]),
        (helpers.str_exotic_index(), vec![helpers.index_value]),
        (helpers.same_value_zero, vec![helpers.str_eq]),
        (helpers.map_find, vec![helpers.same_value_zero]),
        (helpers.map_get, vec![helpers.map_find]),
        (helpers.map_has, vec![helpers.map_find]),
        (helpers.map_delete, vec![helpers.map_find]),
        (helpers.box_f64, vec![]),
        (helpers.protos_indexless(), vec![]),
        // Not a leaf: reaches `__prop_find` (a lazy function holds no private
        // entry, so that call never materializes, but the graph is static).
        (helpers.private_find(), vec![helpers.prop_find]),
        (helpers.json_ws, vec![]),
        (helpers.test_integrity(), vec![]),
        (helpers.heap_object(), vec![]),
        (helpers.str_of(), vec![]),
        (helpers.fn_name(), vec![helpers.heap_object()]),
        // Not a leaf: a lazy function's own properties materialize on lookup.
        (helpers.prop_find, vec![helpers.define + 1]),
        // The number conversions: Ryu and the decimal parser only touch their
        // static scratch.
        (helpers.umul128(), vec![]),
        (helpers.ryu_pow5(), vec![helpers.umul128()]),
        (helpers.mul_shift(), vec![helpers.umul128()]),
        (helpers.multiple_of_pow5(), vec![]),
        (
            helpers.ryu_d2d(),
            vec![
                helpers.ryu_pow5(),
                helpers.mul_shift(),
                helpers.multiple_of_pow5(),
            ],
        ),
        (helpers.dec_expand(), vec![]),
        (helpers.num_unit(), vec![]),
        (helpers.str_ws(), vec![]),
        (helpers.dec_left(), vec![]),
        (helpers.dec_right(), vec![]),
        (helpers.dec_round(), vec![]),
        (
            helpers.dec_slow(),
            vec![helpers.dec_left(), helpers.dec_right(), helpers.dec_round()],
        ),
        (
            helpers.dec_parse(),
            vec![helpers.num_unit(), helpers.dec_slow()],
        ),
        (helpers.digit_span(), vec![helpers.num_unit()]),
        (helpers.radix_pow2(), vec![helpers.num_unit()]),
        (helpers.radix_fold(), vec![helpers.num_unit()]),
        (
            helpers.string_to_number(),
            vec![
                helpers.num_unit(),
                helpers.str_ws(),
                helpers.dec_parse(),
                helpers.digit_span(),
                helpers.radix_pow2(),
            ],
        ),
        (helpers.to_number_prim(), vec![helpers.string_to_number()]),
        (
            helpers.parse_float(),
            vec![helpers.num_unit(), helpers.str_ws(), helpers.dec_parse()],
        ),
        (
            helpers.parse_int(),
            vec![
                helpers.num_unit(),
                helpers.str_ws(),
                helpers.digit_span(),
                helpers.dec_parse(),
                helpers.radix_pow2(),
                helpers.radix_fold(),
            ],
        ),
        (helpers.now, vec![CLOCK_TIME_GET_IMPORT]),
        (
            helpers.random(),
            vec![CLOCK_TIME_GET_IMPORT, RANDOM_GET_IMPORT],
        ),
    ]
}

/// Solve [`helper_graph`]: a declared helper is a leaf iff every callee is.
pub(super) fn leaf_set(helpers: Helpers<'_>) -> LeafSet {
    let graph = helper_graph(helpers);
    let mut leaves: HashSet<u32> = graph.iter().map(|(f, _)| *f).collect();
    loop {
        let before = leaves.len();
        for (f, callees) in &graph {
            if !callees
                .iter()
                .all(|c| *c < WASI_IMPORT_COUNT || leaves.contains(c))
            {
                leaves.remove(f);
            }
        }
        if leaves.len() == before {
            return LeafSet(leaves);
        }
    }
}

/// The `NoRoot` class of one IR value, with the locals it depends on.
#[derive(Clone, Default)]
pub(super) struct ValueClass {
    /// Locals that must be numeric-like (never a string) for the value to be
    /// numeric-like; `None` when it may be a string or a heap block.
    numeric: Option<Vec<u32>>,
    /// `(numeric_deps, noroot_deps)` for the value to be `NoRoot`; `None` when
    /// it may be a heap block whatever the locals hold.
    noroot: Option<(Vec<u32>, Vec<u32>)>,
}

impl ValueClass {
    /// A class that needs nothing: a number, boolean, `null`, or `undefined`.
    fn numeric() -> Self {
        Self {
            numeric: Some(vec![]),
            noroot: Some((vec![], vec![])),
        }
    }

    /// A static block: `NoRoot`, but possibly a string.
    fn static_block() -> Self {
        Self {
            numeric: None,
            noroot: Some((vec![], vec![])),
        }
    }
}

/// Classify an IR value. Shallow by design: anything not listed may hold a
/// heap reference.
pub(super) fn classify(node: &IrNode) -> ValueClass {
    match node {
        IrNode::Num(_)
        | IrNode::Bool(_)
        | IrNode::Null
        | IrNode::Undefined
        | IrNode::ArrayHole
        | IrNode::Hole
        | IrNode::Not(_)
        | IrNode::InstanceOf { .. }
        | IrNode::ObjectHas { .. }
        | IrNode::ObjectDelete { .. }
        | IrNode::IsHole(_)
        | IrNode::IsObject(_)
        | IrNode::IsArray(_)
        | IrNode::IncDec { .. }
        | IrNode::MathCall { .. }
        | IrNode::MathSpread { .. }
        | IrNode::ConvertCall {
            op: ConvertOp::ToNumber,
            ..
        }
        | IrNode::Binary { .. }
            if !matches!(node, IrNode::Binary { op: BinOp::Add, .. }) =>
        {
            ValueClass::numeric()
        }
        IrNode::Str(_) | IrNode::TypeOf(_) | IrNode::DeclFunc(_) | IrNode::Intrinsic(_) => {
            ValueClass::static_block()
        }
        IrNode::Local(local) => ValueClass {
            numeric: Some(vec![*local]),
            noroot: Some((vec![], vec![*local])),
        },
        IrNode::Assign { value, .. } => classify(value),
        IrNode::Binary {
            op: BinOp::Add,
            lhs,
            rhs,
        } => match (classify(lhs).numeric, classify(rhs).numeric) {
            (Some(mut deps), Some(rhs)) => {
                deps.extend(rhs);
                ValueClass {
                    numeric: Some(deps.clone()),
                    noroot: Some((deps, vec![])),
                }
            }
            _ => ValueClass::default(),
        },
        _ => ValueClass::default(),
    }
}

/// The solved `NoRoot` facts of one function.
#[derive(Default)]
pub(super) struct Solution {
    numeric: HashSet<u32>,
    noroot: HashSet<u32>,
}

impl Solution {
    /// Greatest fixpoint over `defs`, one entry per announced definition.
    /// A candidate survives only if every write to it was announced
    /// (`writes[local] == its def count`), so a write the analysis did not
    /// see (an emitter stashing a heap value there) disqualifies it.
    pub(super) fn solve(
        candidates: impl IntoIterator<Item = u32>,
        defs: &[(u32, ValueClass)],
        writes: &HashMap<u32, usize>,
    ) -> Self {
        let mut counts: HashMap<u32, usize> = HashMap::new();
        for (local, _) in defs {
            *counts.entry(*local).or_default() += 1;
        }
        let announced: HashSet<u32> = candidates
            .into_iter()
            .filter(|l| writes.get(l).copied().unwrap_or(0) == counts.get(l).copied().unwrap_or(0))
            .collect();
        let mut numeric = announced.clone();
        retain_fixpoint(&mut numeric, defs, |set, class| {
            class
                .numeric
                .as_ref()
                .is_some_and(|deps| deps.iter().all(|d| set.contains(d)))
        });
        let mut noroot = announced;
        retain_fixpoint(&mut noroot, defs, |set, class| {
            class.noroot.as_ref().is_some_and(|(num, nr)| {
                num.iter().all(|d| numeric.contains(d)) && nr.iter().all(|d| set.contains(d))
            })
        });
        Self { numeric, noroot }
    }

    /// Whether local `local` can never hold a heap reference.
    pub(super) fn local_is_noroot(&self, local: u32) -> bool {
        self.noroot.contains(&local)
    }

    /// Whether a value of class `class` can never be a heap reference.
    pub(super) fn value_is_noroot(&self, class: &ValueClass) -> bool {
        class.noroot.as_ref().is_some_and(|(num, nr)| {
            num.iter().all(|d| self.numeric.contains(d))
                && nr.iter().all(|d| self.noroot.contains(d))
        })
    }
}

/// Remove locals with a definition failing `holds` until nothing changes.
fn retain_fixpoint(
    set: &mut HashSet<u32>,
    defs: &[(u32, ValueClass)],
    holds: impl Fn(&HashSet<u32>, &ValueClass) -> bool,
) {
    loop {
        let before = set.len();
        for (local, class) in defs {
            if set.contains(local) && !holds(set, class) {
                set.remove(local);
            }
        }
        if set.len() == before {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(op: ConvertOp) -> IrNode {
        IrNode::ConvertCall {
            op,
            arg: Box::new(IrNode::Local(0)),
            radix: None,
        }
    }

    fn is_numeric(node: &IrNode) -> bool {
        classify(node).numeric.is_some_and(|deps| deps.is_empty())
    }

    #[test]
    fn number_producing_calls_are_numeric() {
        assert!(is_numeric(&IrNode::MathCall {
            op: MathOp::Floor,
            args: vec![IrNode::Local(0)],
        }));
        assert!(is_numeric(&IrNode::MathSpread {
            op: MathOp::Max,
            args: Box::new(IrNode::ArraySpread(vec![])),
        }));
        assert!(is_numeric(&convert(ConvertOp::ToNumber)));
    }

    #[test]
    fn other_conversions_may_hold_heap_values() {
        for op in [
            ConvertOp::ToString,
            ConvertOp::String,
            ConvertOp::ParseFloat,
        ] {
            assert!(classify(&convert(op)).noroot.is_none());
        }
    }
}
