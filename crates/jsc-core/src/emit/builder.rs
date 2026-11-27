//! The function builder: buffers one wasm function's instructions and places
//! its shadow-stack frame (DESIGN.md §5.7).
//!
//! # Public API (for the helper and builtin sweeps)
//!
//! 1. `FnBuilder::new(leaves)`; declare every parameter with
//!    [`FnBuilder::param`] and every local with [`FnBuilder::local`] /
//!    [`FnBuilder::locals`], each as [`LocalClass::Jsval`], [`LocalClass::Base`]
//!    (an `i32` block base rooted as `HEAP | base`) or [`LocalClass::Raw`]. There
//!    is no default: touching an undeclared local makes
//!    [`FnBuilder::finish`] fail.
//! 2. Write the body through [`Sink::instruction`] (the `wasm!` macro works
//!    unchanged). Every `Return` and the final `End` get the epilogue.
//! 3. Across code that may reach a safepoint, root operand-stack values with
//!    [`Sink::root_node`] (an IR value, elided when `NoRoot`) or
//!    [`Sink::root_temp`] (a jsval or base already pushed), and bracket them
//!    with [`Sink::temp_mark`] / [`Sink::release_temps`]; a temp whose range
//!    holds no safepoint costs nothing.
//! 4. Slow-path helpers shrink-wrap with [`FnBuilder::frame_begin`] /
//!    [`FnBuilder::frame_end`]: a `Return` inside the region restores SP, one
//!    outside does not; a safepoint outside the region, a branch out of it, a
//!    rooted local written before or after it and a kept temp rooted before
//!    it are errors. Without a region the frame covers the whole body.
//! 5. [`FnBuilder::finish`] solves slots and emits the frame; a function with
//!    no root live across a safepoint gets none.
//!
//! Plain [`Function`]s implement [`Sink`] with no-op rooting, so emitters
//! shared with builtins and helpers stay generic until those are converted.

use std::collections::{HashMap, HashSet};
use std::fmt;

use wasm_encoder::{Encode, Function, Instruction, MemArg, ValType};

use super::roots::{LeafSet, Solution, ValueClass, classify};
use super::{HEAP_BITS, IrNode, SP_GLOBAL};
use crate::abi;

/// How a local participates in rooting. Deliberately has no default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum LocalClass {
    /// An `i64` holding a jsval: written through to its slot when live.
    Jsval,
    /// An `i32` block base or key: written through as `HEAP | base`.
    Base,
    /// Anything else (integers, raw `f64`/`i64` bits): never rooted.
    Raw(ValType),
}

impl LocalClass {
    fn val_type(self) -> ValType {
        match self {
            Self::Jsval => ValType::I64,
            Self::Base => ValType::I32,
            Self::Raw(ty) => ty,
        }
    }
}

/// An operand-stack value being rooted by [`Sink::root_temp`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TempClass {
    /// An `i64` jsval on top of the stack.
    Jsval,
    /// An `i32` block base or key on top of the stack.
    Base,
}

/// The instruction sink shared by the builder and plain [`Function`]s.
pub(super) trait Sink {
    /// Append one instruction.
    fn instruction(&mut self, instruction: &Instruction<'_>) -> &mut Self;
    /// Root the jsval of IR value `value` just pushed, unless it is `NoRoot`.
    fn root_node(&mut self, value: &IrNode);
    /// Root the value just pushed.
    fn root_temp(&mut self, class: TempClass);
    /// The current temp depth, for [`Sink::release_temps`].
    fn temp_mark(&self) -> u32;
    /// Stop rooting every temp opened since `mark`.
    fn release_temps(&mut self, mark: u32);
    /// Announce the next write to `local` stores IR value `value`.
    fn note_def(&mut self, local: u32, value: &IrNode);
    /// Announce the next write to `local` stores a number.
    fn note_numeric_def(&mut self, local: u32);
}

impl Sink for Function {
    fn instruction(&mut self, instruction: &Instruction<'_>) -> &mut Self {
        Function::instruction(self, instruction)
    }
    fn root_node(&mut self, _: &IrNode) {}
    fn root_temp(&mut self, _: TempClass) {}
    fn temp_mark(&self) -> u32 {
        0
    }
    fn release_temps(&mut self, _: u32) {}
    fn note_def(&mut self, _: u32, _: &IrNode) {}
    fn note_numeric_def(&mut self, _: u32) {}
}

/// A builder invariant the emitter violated: a compiler bug.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BuilderError {
    /// Local `0` was used without a declaration.
    UndeclaredLocal(u32),
    /// A safepoint outside the [`FnBuilder::frame_begin`] region.
    SafepointOutsideFrame,
    /// A branch leaves the frame region without restoring SP.
    BranchOutOfFrame,
    /// A `frame_begin` / `frame_end` pair is unmatched or repeated.
    BadFrameRegion,
    /// Rooted local `0` (a parameter too) is written before the frame exists,
    /// so its write-through would store through `$fp = 0`.
    WriteBeforeFrame(u32),
    /// A kept temp is rooted before the frame exists, so its slot store would
    /// go through `$fp = 0` and the slot would then be cleared by the prologue.
    TempBeforeFrame,
    /// Rooted local `0` is written after [`FnBuilder::frame_end`], so its
    /// write-through would store through the stale `$fp` of a popped frame.
    WriteAfterFrame(u32),
}

impl fmt::Display for BuilderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UndeclaredLocal(local) => {
                write!(f, "local {local} has no root declaration")
            }
            Self::SafepointOutsideFrame => write!(f, "safepoint outside the frame region"),
            Self::BranchOutOfFrame => write!(f, "branch out of the frame region"),
            Self::BadFrameRegion => write!(f, "unbalanced frame region"),
            Self::WriteBeforeFrame(local) => {
                write!(f, "rooted local {local} is written before the frame region")
            }
            Self::TempBeforeFrame => write!(f, "kept temp rooted before the frame region"),
            Self::WriteAfterFrame(local) => {
                write!(f, "rooted local {local} is written after the frame region")
            }
        }
    }
}

/// What the finish pass needs to know about a buffered instruction.
#[derive(Clone, Copy)]
enum Kind {
    Get(u32),
    Set(u32),
    Tee(u32),
    Call(u32),
    CallIndirect,
    Return,
    Open { is_loop: bool },
    End,
    Br(u32),
    Other,
}

/// One buffered item; instruction bytes live in [`FnBuilder::bytes`].
enum Op {
    Instr {
        kind: Kind,
        end: usize,
    },
    Root {
        class: TempClass,
        value: Option<ValueClass>,
    },
    Release(u32),
    FrameBegin,
    FrameEnd,
}

/// See the module documentation.
pub(super) struct FnBuilder<'a> {
    leaves: &'a LeafSet,
    classes: Vec<LocalClass>,
    params: u32,
    bytes: Vec<u8>,
    ops: Vec<Op>,
    defs: Vec<(u32, ValueClass)>,
    open_temps: u32,
    error: Option<BuilderError>,
}

impl<'a> FnBuilder<'a> {
    /// A builder with no locals; declare them before writing the body.
    pub(super) fn new(leaves: &'a LeafSet) -> Self {
        Self {
            leaves,
            classes: Vec::new(),
            params: 0,
            bytes: Vec::new(),
            ops: Vec::new(),
            defs: Vec::new(),
            open_temps: 0,
            error: None,
        }
    }

    /// Declare the next parameter; all must precede the locals.
    pub(super) fn param(&mut self, class: LocalClass) -> u32 {
        self.params += 1;
        self.local(class)
    }

    /// Declare the next local, returning its index.
    pub(super) fn local(&mut self, class: LocalClass) -> u32 {
        self.classes.push(class);
        u32::try_from(self.classes.len() - 1).unwrap_or(u32::MAX)
    }

    /// Declare `count` consecutive locals of one class.
    pub(super) fn locals(&mut self, count: u32, class: LocalClass) {
        for _ in 0..count {
            self.local(class);
        }
    }

    /// Open the shrink-wrapped frame region at the current position.
    pub(super) fn frame_begin(&mut self) {
        self.ops.push(Op::FrameBegin);
    }

    /// Close the region opened by [`Self::frame_begin`], restoring SP.
    pub(super) fn frame_end(&mut self) {
        self.ops.push(Op::FrameEnd);
    }

    fn check_local(&mut self, local: u32) {
        if local as usize >= self.classes.len() && self.error.is_none() {
            self.error = Some(BuilderError::UndeclaredLocal(local));
        }
    }

    /// Assign slots and produce the finished [`Function`].
    pub(super) fn finish(self) -> Result<Function, BuilderError> {
        if let Some(error) = self.error {
            return Err(error);
        }
        Ok(Layout::new(&self)?.emit(&self))
    }
}

impl Sink for FnBuilder<'_> {
    fn instruction(&mut self, instruction: &Instruction<'_>) -> &mut Self {
        let kind = match instruction {
            Instruction::LocalGet(l) => {
                self.check_local(*l);
                Kind::Get(*l)
            }
            Instruction::LocalSet(l) => {
                self.check_local(*l);
                Kind::Set(*l)
            }
            Instruction::LocalTee(l) => {
                self.check_local(*l);
                Kind::Tee(*l)
            }
            Instruction::Call(f) => Kind::Call(*f),
            Instruction::CallIndirect { .. } => Kind::CallIndirect,
            Instruction::Return => Kind::Return,
            Instruction::Block(_) | Instruction::If(_) => Kind::Open { is_loop: false },
            Instruction::Loop(_) => Kind::Open { is_loop: true },
            Instruction::End => Kind::End,
            Instruction::Br(l) | Instruction::BrIf(l) => Kind::Br(*l),
            Instruction::BrTable(targets, default) => {
                Kind::Br(targets.iter().copied().fold(*default, u32::max))
            }
            _ => Kind::Other,
        };
        instruction.encode(&mut self.bytes);
        self.ops.push(Op::Instr {
            kind,
            end: self.bytes.len(),
        });
        self
    }

    fn root_node(&mut self, value: &IrNode) {
        self.ops.push(Op::Root {
            class: TempClass::Jsval,
            value: Some(classify(value)),
        });
        self.open_temps += 1;
    }

    fn root_temp(&mut self, class: TempClass) {
        self.ops.push(Op::Root { class, value: None });
        self.open_temps += 1;
    }

    fn temp_mark(&self) -> u32 {
        self.open_temps
    }

    fn release_temps(&mut self, mark: u32) {
        if mark < self.open_temps {
            self.ops.push(Op::Release(mark));
            self.open_temps = mark;
        }
    }

    fn note_def(&mut self, local: u32, value: &IrNode) {
        self.defs.push((local, classify(value)));
    }

    fn note_numeric_def(&mut self, local: u32) {
        self.defs.push((local, classify(&IrNode::Num(0.0))));
    }
}

/// The frame placement computed from a finished buffer.
struct Layout {
    /// The slot of each rooted local (by local index).
    local_slots: HashMap<u32, u32>,
    /// The slot of each kept temp (by `Root` op position).
    temp_slots: HashMap<usize, u32>,
    /// Total slots; `0` means no frame at all.
    slots: u32,
    /// Op position where the frame is pushed.
    begin: usize,
    /// Op position after which SP is restored, when shrink-wrapped.
    end: Option<usize>,
}

/// The extra locals of a framed function, after every declared one.
struct FrameLocals {
    old: u32,
    fp: u32,
    tmp64: u32,
    tmp32: u32,
}

impl Layout {
    fn new(builder: &FnBuilder<'_>) -> Result<Self, BuilderError> {
        let ops = &builder.ops;
        let safepoints: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter_map(|(i, op)| match op {
                Op::Instr {
                    kind: Kind::CallIndirect,
                    ..
                } => Some(i),
                Op::Instr {
                    kind: Kind::Call(f),
                    ..
                } if !builder.leaves.is_leaf(*f) => Some(i),
                _ => None,
            })
            .collect();
        let (begin, end) = Self::frame_region(ops)?;

        let loops = outer_loops(ops);
        let extend = |pos: usize, to_end: bool| {
            loops
                .iter()
                .find(|(start, stop)| *start <= pos && pos <= *stop)
                .map_or(pos, |(start, stop)| if to_end { *stop } else { *start })
        };
        let has_safepoint =
            |low: usize, high: usize| safepoints.iter().any(|s| low <= *s && *s <= high);

        let solution = solve_noroot(builder);
        let mut local_slots = HashMap::new();
        let mut next_slot = 0;
        if !safepoints.is_empty() {
            for (local, class, range) in def_use_ranges(builder) {
                if class == LocalClassKind::Rooted
                    && !solution.local_is_noroot(local)
                    && let Some((first_def, last_use)) = range
                    && has_safepoint(extend(first_def, false), extend(last_use, true))
                {
                    local_slots.insert(local, next_slot);
                    next_slot += 1;
                }
            }
        }

        let (temp_slots, max_depth) = assign_temp_slots(ops, next_slot, |root, release| {
            let Op::Root { value, .. } = &ops[root] else {
                return false;
            };
            value
                .as_ref()
                .is_none_or(|class| !solution.value_is_noroot(class))
                && has_safepoint(root + 1, release)
        });

        let slots = next_slot + max_depth;
        if slots > 0 {
            let last = end.unwrap_or(ops.len());
            if safepoints.iter().any(|s| *s < begin || *s > last) {
                return Err(BuilderError::SafepointOutsideFrame);
            }
            check_branches(ops, begin, end)?;
            let written_early = ops[..begin].iter().find_map(|op| match op {
                Op::Instr {
                    kind: Kind::Set(l) | Kind::Tee(l),
                    ..
                } if local_slots.contains_key(l) => Some(*l),
                _ => None,
            });
            if let Some(local) = written_early {
                return Err(BuilderError::WriteBeforeFrame(local));
            }
            let written_late = end.and_then(|end| {
                ops[end..].iter().find_map(|op| match op {
                    Op::Instr {
                        kind: Kind::Set(l) | Kind::Tee(l),
                        ..
                    } if local_slots.contains_key(l) => Some(*l),
                    _ => None,
                })
            });
            if let Some(local) = written_late {
                return Err(BuilderError::WriteAfterFrame(local));
            }
            if temp_slots.keys().any(|root| *root < begin) {
                return Err(BuilderError::TempBeforeFrame);
            }
        }
        Ok(Self {
            local_slots,
            temp_slots,
            slots,
            begin,
            end,
        })
    }

    /// The explicit frame region, or the whole body.
    fn frame_region(ops: &[Op]) -> Result<(usize, Option<usize>), BuilderError> {
        let begins: Vec<usize> = positions(ops, |op| matches!(op, Op::FrameBegin));
        let ends: Vec<usize> = positions(ops, |op| matches!(op, Op::FrameEnd));
        match (begins.as_slice(), ends.as_slice()) {
            ([], []) => Ok((0, None)),
            ([begin], []) => Ok((*begin, None)),
            ([begin], [end]) if begin < end => Ok((*begin, Some(*end))),
            _ => Err(BuilderError::BadFrameRegion),
        }
    }

    fn frame_locals(builder: &FnBuilder<'_>) -> FrameLocals {
        let first = u32::try_from(builder.classes.len()).unwrap_or(u32::MAX);
        FrameLocals {
            old: first,
            fp: first + 1,
            tmp64: first + 2,
            tmp32: first + 3,
        }
    }

    fn emit(self, builder: &FnBuilder<'_>) -> Function {
        let mut locals = run_lengths(&builder.classes[builder.params as usize..]);
        let frame = (self.slots > 0).then(|| Self::frame_locals(builder));
        if frame.is_some() {
            locals.extend([(2, ValType::I32), (1, ValType::I64), (1, ValType::I32)]);
        }
        let mut function = Function::new(locals);
        let mut out = Writer::default();
        let mut start = 0;
        let mut depth = 0u32;
        for (i, op) in builder.ops.iter().enumerate() {
            if let Some(frame) = &frame
                && i == self.begin
                && !matches!(op, Op::FrameBegin)
            {
                self.prologue(&mut out, frame, builder);
            }
            match op {
                Op::FrameBegin => {
                    if let Some(frame) = &frame {
                        self.prologue(&mut out, frame, builder);
                    }
                }
                Op::FrameEnd => {
                    if let Some(frame) = &frame {
                        out.epilogue(frame);
                    }
                }
                Op::Release(_) => {}
                Op::Root { class, .. } => {
                    if let (Some(frame), Some(&slot)) = (&frame, self.temp_slots.get(&i)) {
                        out.root_temp(frame, *class, slot);
                    }
                }
                Op::Instr { kind, end } => {
                    let in_frame = i >= self.begin && self.end.is_none_or(|e| i < e);
                    let restores = frame.is_some()
                        && in_frame
                        && match kind {
                            Kind::Return => true,
                            Kind::End => depth == 0,
                            _ => false,
                        };
                    if restores && let Some(frame) = &frame {
                        out.epilogue(frame);
                    }
                    out.raw(&builder.bytes[start..*end]);
                    start = *end;
                    match kind {
                        Kind::Open { .. } => depth += 1,
                        Kind::End => depth = depth.saturating_sub(1),
                        Kind::Set(l) | Kind::Tee(l) => {
                            if let (Some(frame), Some(&slot)) = (&frame, self.local_slots.get(l)) {
                                out.write_through(frame, *l, builder.classes[*l as usize], slot);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        function.raw(out.0);
        function
    }

    /// `$old = $__sp; $fp = $__sp - 8N; $__sp = $fp`, clear the slots, then
    /// store the parameters that need a slot. Slot `k` is at `$fp + 8 + 8k`.
    /// With `N <= 4` a slot a parameter store fills is not cleared first: no
    /// safepoint lies between the two.
    ///
    /// The fill starts at `$fp` and covers the 8 free bytes below slot 0: the
    /// destination `$fp + 8` would wrap to `0` at `$fp = -8` and write the
    /// protected bytes instead of trapping.
    fn prologue(&self, out: &mut Writer, frame: &FrameLocals, builder: &FnBuilder<'_>) {
        let n = self.slots;
        out.instr(&Instruction::GlobalGet(SP_GLOBAL));
        out.instr(&Instruction::LocalTee(frame.old));
        out.instr(&Instruction::I32Const((8 * n).cast_signed()));
        out.instr(&Instruction::I32Sub);
        out.instr(&Instruction::LocalTee(frame.fp));
        out.instr(&Instruction::GlobalSet(SP_GLOBAL));
        if n <= 4 {
            let filled: HashSet<u32> = (0..builder.params)
                .filter_map(|param| self.local_slots.get(&param).copied())
                .collect();
            for k in (0..n).filter(|k| !filled.contains(k)) {
                out.instr(&Instruction::LocalGet(frame.fp));
                out.instr(&Instruction::I64Const(0));
                out.instr(&Instruction::I64Store(slot_arg(k)));
            }
        } else {
            out.instr(&Instruction::LocalGet(frame.fp));
            out.instr(&Instruction::I32Const(0));
            out.instr(&Instruction::I32Const(
                (8 * n + abi::SHADOW_FP_BIAS).cast_signed(),
            ));
            out.instr(&Instruction::MemoryFill(0));
        }
        for param in 0..builder.params {
            if let Some(&slot) = self.local_slots.get(&param) {
                out.write_through(frame, param, builder.classes[param as usize], slot);
            }
        }
    }
}

/// Pair every root with its release, keep those `keep` accepts, and number the
/// kept ones by nesting depth from `first_slot`. Returns their slots and depth.
fn assign_temp_slots(
    ops: &[Op],
    first_slot: u32,
    keep: impl Fn(usize, usize) -> bool,
) -> (HashMap<usize, u32>, u32) {
    let mut kept: HashSet<usize> = HashSet::new();
    let mut open: Vec<usize> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::Root { .. } => open.push(i),
            Op::Release(mark) => {
                while open.len() > *mark as usize {
                    if let Some(root) = open.pop()
                        && keep(root, i)
                    {
                        kept.insert(root);
                    }
                }
            }
            _ => {}
        }
    }
    kept.extend(open.into_iter().filter(|root| keep(*root, ops.len())));
    let mut slots = HashMap::new();
    let mut open = Vec::new();
    let (mut depth, mut max_depth) = (0, 0);
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::Root { .. } => {
                let is_kept = kept.contains(&i);
                if is_kept {
                    slots.insert(i, first_slot + depth);
                    depth += 1;
                    max_depth = max_depth.max(depth);
                }
                open.push(is_kept);
            }
            Op::Release(mark) => {
                while open.len() > *mark as usize {
                    if open.pop() == Some(true) {
                        depth -= 1;
                    }
                }
            }
            _ => {}
        }
    }
    (slots, max_depth)
}

/// `Rooted` iff the class holds references.
#[derive(PartialEq, Eq)]
enum LocalClassKind {
    Rooted,
    NotRooted,
}

/// One local's class and `(first_def, last_use)` positions.
type LocalRange = (u32, LocalClassKind, Option<(usize, usize)>);

/// Each local with its class and definition/use range.
fn def_use_ranges(builder: &FnBuilder<'_>) -> Vec<LocalRange> {
    let mut first_def: HashMap<u32, usize> = HashMap::new();
    let mut last_use: HashMap<u32, usize> = HashMap::new();
    for param in 0..builder.params {
        first_def.insert(param, 0);
    }
    for (i, op) in builder.ops.iter().enumerate() {
        if let Op::Instr { kind, .. } = op {
            match kind {
                Kind::Set(l) | Kind::Tee(l) => {
                    first_def.entry(*l).or_insert(i);
                }
                Kind::Get(l) => {
                    last_use.insert(*l, i);
                }
                _ => {}
            }
        }
    }
    builder
        .classes
        .iter()
        .enumerate()
        .map(|(local, class)| {
            let local = u32::try_from(local).unwrap_or(u32::MAX);
            let rooted = match class {
                LocalClass::Raw(_) => LocalClassKind::NotRooted,
                LocalClass::Jsval | LocalClass::Base => LocalClassKind::Rooted,
            };
            let range = first_def
                .get(&local)
                .zip(last_use.get(&local))
                .map(|(d, u)| (*d, *u));
            (local, rooted, range)
        })
        .collect()
}

fn solve_noroot(builder: &FnBuilder<'_>) -> Solution {
    let mut writes: HashMap<u32, usize> = HashMap::new();
    for op in &builder.ops {
        if let Op::Instr {
            kind: Kind::Set(l) | Kind::Tee(l),
            ..
        } = op
        {
            *writes.entry(*l).or_default() += 1;
        }
    }
    let candidates = builder
        .classes
        .iter()
        .enumerate()
        .skip(builder.params as usize)
        .filter(|(_, class)| **class == LocalClass::Jsval)
        .map(|(l, _)| u32::try_from(l).unwrap_or(u32::MAX));
    Solution::solve(candidates, &builder.defs, &writes)
}

fn positions(ops: &[Op], matches: impl Fn(&Op) -> bool) -> Vec<usize> {
    ops.iter()
        .enumerate()
        .filter_map(|(i, op)| matches(op).then_some(i))
        .collect()
}

/// The outermost loops as op-position ranges.
fn outer_loops(ops: &[Op]) -> Vec<(usize, usize)> {
    let mut loops = Vec::new();
    let mut stack: Vec<Option<usize>> = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if let Op::Instr { kind, .. } = op {
            match kind {
                Kind::Open { is_loop } => {
                    let outermost = *is_loop && !stack.iter().any(Option::is_some);
                    stack.push(outermost.then_some(i));
                }
                Kind::End => {
                    if let Some(Some(start)) = stack.pop() {
                        loops.push((start, i));
                    }
                }
                _ => {}
            }
        }
    }
    loops
}

/// Reject a branch that exits the frame region (or the function body).
fn check_branches(ops: &[Op], begin: usize, end: Option<usize>) -> Result<(), BuilderError> {
    let mut depth = 0u32;
    let mut region_depth = 0u32;
    for (i, op) in ops.iter().enumerate() {
        if i == begin {
            region_depth = depth;
        }
        if let Op::Instr { kind, .. } = op {
            match kind {
                Kind::Open { .. } => depth += 1,
                Kind::End => depth = depth.saturating_sub(1),
                Kind::Br(label)
                    if i >= begin
                        && end.is_none_or(|e| i < e)
                        && *label >= depth - region_depth =>
                {
                    return Err(BuilderError::BranchOutOfFrame);
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn run_lengths(classes: &[LocalClass]) -> Vec<(u32, ValType)> {
    let mut runs: Vec<(u32, ValType)> = Vec::new();
    for class in classes {
        let ty = class.val_type();
        match runs.last_mut() {
            Some((count, last)) if *last == ty => *count += 1,
            _ => runs.push((1, ty)),
        }
    }
    runs
}

/// The biased address of `slot`: a static offset, so `$fp = -8` traps instead
/// of wrapping to address `0`.
fn slot_arg(slot: u32) -> MemArg {
    MemArg {
        offset: u64::from(abi::SHADOW_FP_BIAS) + 8 * u64::from(slot),
        align: 3,
        memory_index: 0,
    }
}

/// Encoded output of the finish pass.
#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn instr(&mut self, instruction: &Instruction<'_>) {
        instruction.encode(&mut self.0);
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    fn epilogue(&mut self, frame: &FrameLocals) {
        self.instr(&Instruction::LocalGet(frame.old));
        self.instr(&Instruction::GlobalSet(SP_GLOBAL));
    }

    /// Store local `local` to `slot`, leaving the stack unchanged.
    fn write_through(&mut self, frame: &FrameLocals, local: u32, class: LocalClass, slot: u32) {
        self.instr(&Instruction::LocalGet(frame.fp));
        self.instr(&Instruction::LocalGet(local));
        if class == LocalClass::Base {
            self.box_base();
        }
        self.instr(&Instruction::I64Store(slot_arg(slot)));
    }

    /// Tee the value on top of the stack to a temp slot, leaving it there.
    fn root_temp(&mut self, frame: &FrameLocals, class: TempClass, slot: u32) {
        let tmp = match class {
            TempClass::Jsval => frame.tmp64,
            TempClass::Base => frame.tmp32,
        };
        self.instr(&Instruction::LocalTee(tmp));
        self.instr(&Instruction::LocalGet(frame.fp));
        self.instr(&Instruction::LocalGet(tmp));
        if class == TempClass::Base {
            self.box_base();
        }
        self.instr(&Instruction::I64Store(slot_arg(slot)));
    }

    fn box_base(&mut self) {
        self.instr(&Instruction::I64ExtendI32U);
        self.instr(&Instruction::I64Const(HEAP_BITS));
        self.instr(&Instruction::I64Or);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_encoder::BlockType;

    fn builder(leaves: &LeafSet) -> FnBuilder<'_> {
        let mut b = FnBuilder::new(leaves);
        b.param(LocalClass::Jsval);
        b
    }

    #[test]
    fn builder_rejects_undeclared_local() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        b.instruction(&Instruction::LocalGet(9));
        b.instruction(&Instruction::End);
        assert_eq!(b.finish().err(), Some(BuilderError::UndeclaredLocal(9)));
    }

    #[test]
    fn return_before_frame_begin_is_not_restored() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::If(BlockType::Empty));
        b.instruction(&Instruction::I64Const(1));
        b.instruction(&Instruction::Return);
        b.instruction(&Instruction::End);
        b.frame_begin();
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::Return);
        b.frame_end();
        b.instruction(&Instruction::I64Const(2));
        b.instruction(&Instruction::End);
        let function = b.finish();
        assert!(function.is_ok());
    }

    #[test]
    fn safepoint_outside_region_is_rejected() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        b.instruction(&Instruction::Call(99));
        b.frame_begin();
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::End);
        assert_eq!(b.finish().err(), Some(BuilderError::SafepointOutsideFrame));
    }

    #[test]
    fn branch_out_of_region_is_rejected() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        b.instruction(&Instruction::Block(BlockType::Empty));
        b.frame_begin();
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::Drop);
        b.instruction(&Instruction::Br(0));
        b.frame_end();
        b.instruction(&Instruction::End);
        b.instruction(&Instruction::End);
        assert_eq!(b.finish().err(), Some(BuilderError::BranchOutOfFrame));
    }

    #[test]
    fn rooted_local_written_before_region_is_rejected() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        let local = b.local(LocalClass::Jsval);
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::LocalSet(local));
        b.frame_begin();
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(local));
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::End);
        assert_eq!(
            b.finish().err(),
            Some(BuilderError::WriteBeforeFrame(local))
        );
    }

    #[test]
    fn rooted_local_written_inside_region_is_accepted() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        let local = b.local(LocalClass::Jsval);
        b.frame_begin();
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::LocalSet(local));
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(local));
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::End);
        assert!(b.finish().is_ok());
    }

    #[test]
    fn rooted_param_reassigned_before_region_is_rejected() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::LocalSet(0));
        b.frame_begin();
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::End);
        assert_eq!(b.finish().err(), Some(BuilderError::WriteBeforeFrame(0)));
    }

    #[test]
    fn rooted_param_reassigned_inside_region_is_accepted() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        b.frame_begin();
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::LocalSet(0));
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::End);
        assert!(b.finish().is_ok());
    }

    #[test]
    fn rooted_local_written_after_region_is_rejected() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        let local = b.local(LocalClass::Jsval);
        b.frame_begin();
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::LocalSet(local));
        b.instruction(&Instruction::Call(99));
        b.instruction(&Instruction::LocalGet(local));
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::LocalGet(0));
        b.instruction(&Instruction::LocalSet(local));
        b.instruction(&Instruction::LocalGet(local));
        b.instruction(&Instruction::Drop);
        b.instruction(&Instruction::End);
        assert_eq!(b.finish().err(), Some(BuilderError::WriteAfterFrame(local)));
    }

    #[test]
    fn kept_temp_rooted_before_region_is_rejected() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        let mark = b.temp_mark();
        b.instruction(&Instruction::LocalGet(0));
        b.root_temp(TempClass::Jsval);
        b.frame_begin();
        b.instruction(&Instruction::Call(99));
        b.release_temps(mark);
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::End);
        assert_eq!(b.finish().err(), Some(BuilderError::TempBeforeFrame));
    }

    #[test]
    fn kept_temp_rooted_inside_region_is_accepted() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        let mark = b.temp_mark();
        b.frame_begin();
        b.instruction(&Instruction::LocalGet(0));
        b.root_temp(TempClass::Jsval);
        b.instruction(&Instruction::Call(99));
        b.release_temps(mark);
        b.instruction(&Instruction::Drop);
        b.frame_end();
        b.instruction(&Instruction::End);
        assert!(b.finish().is_ok());
    }

    #[test]
    fn temp_without_safepoint_is_elided() {
        let leaves = LeafSet::empty();
        let mut b = builder(&leaves);
        let mark = b.temp_mark();
        b.instruction(&Instruction::LocalGet(0));
        b.root_temp(TempClass::Jsval);
        b.release_temps(mark);
        b.instruction(&Instruction::End);
        assert!(b.finish().is_ok());
    }
}
