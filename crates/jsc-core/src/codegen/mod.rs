//! Codegen: lower a parsed swc [`Program`] into the crate's numeric [`IrProgram`].
//!
//! This stage walks the numeric-program subset of the swc AST and produces an
//! [`IrProgram`] (a sequence of [`IrStmt`] over `f64` locals). It relies
//! entirely on swc for operator precedence and associativity — no parsing
//! happens here. Supported constructs: number/boolean/`null` literals, the
//! `undefined` global, the four arithmetic
//! binary operators, parentheses, unary `+`/`-`, `let`/`const` declarations,
//! identifier references, simple assignments (`x = expr`), user-defined
//! function declarations, calls, and `return`. Anything else yields an error
//! rather than a panic.

use std::cell::RefCell;

use std::collections::HashMap;

use anyhow::anyhow;

use swc_common::Spanned;

use crate::error::CompileError;

use swc_ecma_ast::{
    ArrayLit, ArrayPat, ArrowExpr, AssignExpr, AssignOp, AssignTarget, AssignTargetPat, BinaryOp,
    BlockStmtOrExpr, Callee, ClassDecl, ClassMember, ClassMethod, ClassProp, Decl, Expr,
    ExprOrSpread, ExprStmt, FnDecl, ForHead, Ident, IdentName, Lit, MemberExpr, MemberProp,
    MethodKind, ObjectLit, ObjectPat, ObjectPatProp, OptChainBase, OptChainExpr,
    ParamOrTsParamProp, Pat, Program, Prop, PropName, PropOrSpread, SimpleAssignTarget, Stmt,
    SuperProp, SuperPropExpr, SwitchStmt, ThisExpr, UnaryOp, UpdateExpr, UpdateOp, VarDeclKind,
    VarDeclOrExpr,
};

use crate::ir::{
    ArrayHofOp, BinOp, ConvertOp, DateOp, InstanceOfKind, IrCase, IrFunction, IrNode, IrProgram,
    IrStmt, LogicalOp, MapOp, MathOp, MethodOp, ObjectBuiltinOp, SpreadCallee, SpreadElem,
};

/// Lower a parsed [`Program`] into an [`IrProgram`].
///
/// The program is lowered as a sequence of statements: `let`/`const`
/// declarations and expression statements. Both [`Program::Script`] and
/// [`Program::Module`] bodies are supported (module-level import/export items
/// are rejected as unsupported).
///
/// # Errors
///
/// Returns an error if the program contains a statement outside the supported
/// subset, references an undeclared variable, or uses an expression construct
/// outside the supported numeric subset.
#[allow(clippy::too_many_lines)]
pub fn lower(program: &Program) -> anyhow::Result<IrProgram> {
    let body: Vec<&Stmt> = match program {
        Program::Script(script) => script.body.iter().collect(),
        Program::Module(module) => module
            .body
            .iter()
            .map(|item| {
                item.as_stmt().ok_or_else(|| {
                    anyhow::Error::from(CompileError::spanned(
                        item.span(),
                        "unsupported module item",
                    ))
                })
            })
            .collect::<anyhow::Result<_>>()?,
    };

    // Pass 1a: collect EVERY top-level function declaration and the non-function
    // statements, so capture analysis can see the whole program before deciding
    // which declarations close over `main`'s bindings.
    let mut all_fn_decls: Vec<&FnDecl> = Vec::new();
    let mut scan_top_stmts: Vec<&Stmt> = Vec::new();
    for stmt in &body {
        if let Stmt::Decl(Decl::Fn(fn_decl)) = stmt {
            all_fn_decls.push(fn_decl);
        } else {
            scan_top_stmts.push(stmt);
        }
    }

    // Capture analysis (S13.4 + G2): which bindings are boxed into cells, and
    // which top-level functions close over an enclosing `main` binding. Runs
    // before any body is lowered so a captured binding's declaration knows to
    // allocate its cell up front, and so Pass 1b can partition the declarations.
    let analysis = analyze_captures(&scan_top_stmts, &all_fn_decls);

    // Pass 1b: partition the hoisted function declarations. A NON-capturing
    // declaration keeps the direct-call fast path (registered in `funcs`, its
    // own function, hoisted). A CAPTURING declaration (G2) is DEMOTED: left in
    // `top_stmts` at its source position so `lower_stmt` routes it through
    // `lower_nested_fn_decl` (a boxed cell in `main`, indirect env-last call).
    // A demoted declaration loses hoisting (lowered at its source position);
    // this is the intended narrowing.
    let mut funcs: HashMap<String, FnSig> = HashMap::new();
    let mut fn_decls: Vec<&FnDecl> = Vec::new();
    let mut top_stmts: Vec<&Stmt> = Vec::new();
    let mut demoted_fns: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Index 0 is `main`, so user functions are numbered from 1 upward.
    let mut next_index: u32 = 1;
    for stmt in &body {
        let Stmt::Decl(Decl::Fn(fn_decl)) = stmt else {
            top_stmts.push(stmt);
            continue;
        };
        let name = fn_decl.ident.sym.as_ref().to_string();
        if analysis.capturing_fns.contains(&fn_decl.ident.span.lo.0) {
            demoted_fns.insert(name);
            top_stmts.push(stmt);
            continue;
        }
        let params = fn_decl.function.params.len();
        let required = fn_decl
            .function
            .params
            .iter()
            .take_while(|p| !matches!(p.pat, Pat::Assign(_) | Pat::Rest(_)))
            .count();
        let has_rest = fn_decl
            .function
            .params
            .last()
            .is_some_and(|p| matches!(p.pat, Pat::Rest(_)));
        if funcs
            .insert(
                name.clone(),
                FnSig {
                    index: next_index,
                    params,
                    required,
                    has_rest,
                },
            )
            .is_some()
        {
            return Err(CompileError::spanned(
                fn_decl.ident.span,
                format!("duplicate function declaration: {name}"),
            )
            .into());
        }
        next_index += 1;
        fn_decls.push(fn_decl);
    }

    // The fixed positional-argument arity `K` of the S13.2 uniform ABI: the
    // widest source arity across *every* function — top-level declarations AND
    // any nested arrow function (S13.4), which lift to Table-registered functions
    // sharing the same signature. Scanning the whole program up front fixes `K`
    // before any body is lowered, so a single `call_indirect` type serves them
    // all and no arrow's params can overflow the fixed argument slots.
    // A generator's — and an `async function`'s (P4.3 M4) — state machine is
    // driven with two positional arguments (`sent`, the handle), so `K` must be at
    // least 2 for the second slot to exist (P4.2 M2), regardless of the widest
    // source arity.
    let has_state_machine = fn_decls
        .iter()
        .any(|f| f.function.is_generator || f.function.is_async);
    let max_arity = body
        .iter()
        .map(|s| scan_stmt_arity(s))
        .max()
        .unwrap_or(0)
        .max(if has_state_machine { 2 } else { 0 });

    // Shared lowering state: the resolved declaration signatures, the uniform
    // arity, and the accumulator that lifted arrow functions append themselves
    // to. `arrow_base` is the wasm index of the first lifted arrow (right after
    // the declarations), so an arrow at accumulator position `p` gets funcref
    // table index `arrow_base - 1 + p`.
    let shared = Shared {
        funcs: &funcs,
        arrows: RefCell::new(Vec::new()),
        arrow_base: next_index,
        max_arity,
        captured: analysis.captured,
        classes: RefCell::new(HashMap::new()),
        demoted_fns,
        generic_method_sites: RefCell::new(Vec::new()),
        object_shapes: analysis.object_shapes,
    };

    // Pass 2: lower each function body over its own locals, laid out for the
    // uniform ABI (`max_arity` argument slots, the env slot, then body locals).
    let mut functions = Vec::with_capacity(fn_decls.len());
    for fn_decl in &fn_decls {
        functions.push(lower_function(fn_decl, &shared)?);
    }

    // The top-level statements become `main`'s body, lowered in a single
    // function frame (`main` has no env parameter; captured top-level bindings
    // still become boxed cells sourced from its locals).
    let mut ctx = LowerCtx::new(&shared);
    let mut stmts = Vec::new();
    for stmt in top_stmts {
        ctx.lower_stmt(stmt, &mut stmts)?;
    }
    let locals = ctx.frame().next;
    // Release the shared borrow so the arrow accumulator can be moved out.
    drop(ctx);

    // Lifted arrow functions follow the declarations in the functions vector, so
    // their positions match the funcref table indices handed out at their use
    // sites.
    functions.extend(shared.arrows.into_inner());

    // M1: a generic user-method call `obj.m(args)` lowers to a `call_indirect`
    // through the funcref table, which the backend only emits when the program
    // defines at least one function/method. When NONE exist, no object could
    // carry a method, so every recorded call site is provably an unknown method —
    // reject the first with a span diagnostic instead of emitting invalid wasm.
    if functions.is_empty()
        && let Some((span, name)) = shared.generic_method_sites.into_inner().into_iter().next()
    {
        return Err(unknown_method_error(span, name.as_deref()));
    }

    Ok(IrProgram {
        locals,
        stmts,
        functions,
    })
}

/// Build the M1 unknown-method span diagnostic. Names the method when it was
/// statically known at the call site (`obj.m()` or `obj["m"]()`); falls back to a
/// generic message for a computed call whose key is only known at runtime.
pub(super) fn unknown_method_error(span: swc_common::Span, name: Option<&str>) -> anyhow::Error {
    let msg = match name {
        Some(name) => format!("method '{name}' is not supported"),
        None => "unsupported method call".to_owned(),
    };
    CompileError::spanned(span, msg).into()
}

/// Shared lowering state threaded through every [`LowerCtx`]: the resolved
/// top-level function signatures, the uniform ABI arity, and the accumulator of
/// arrow functions lifted to top level.
struct Shared<'a> {
    /// Top-level function signatures, keyed by name.
    funcs: &'a HashMap<String, FnSig>,
    /// Arrow functions lifted to top level (S13.4). Each use-site of an arrow
    /// appends its lowered [`IrFunction`] here and emits a
    /// [`IrNode::FuncValue`] referencing its funcref table slot. Interior
    /// mutability lets the immutable-borrow lowering walk register new functions.
    arrows: RefCell<Vec<IrFunction>>,
    /// The wasm function index of the first lifted arrow (one past the last
    /// declaration). An arrow at accumulator index `p` has funcref table index
    /// `arrow_base - 1 + p`.
    arrow_base: u32,
    /// The fixed positional arity `K` of the uniform ABI, covering every
    /// declaration and arrow.
    max_arity: usize,
    /// The binding ids (see [`binding_id`]) of every binding captured by a
    /// nested function, computed by [`analyze_captures`]. A declared binding
    /// whose id is in this set is boxed into a heap cell (S13.4).
    captured: std::collections::HashSet<u32>,
    /// Class declarations lowered so far, keyed by name (P2.2). A `class A`
    /// declaration records its instance shape here as it is lowered; a later
    /// `new A(args)` reads the shape back to pre-slot the fresh instance. Classes
    /// are not hoisted (TDZ), so a `new` always follows its declaration in source
    /// order, matching this build-then-read ordering. Interior mutability lets the
    /// immutable-borrow lowering walk register new classes.
    classes: RefCell<HashMap<String, ClassInfo>>,
    /// Names of the top-level function declarations demoted to closures (G2).
    /// They are bound (as boxed cells) only within `main`; a top-level function
    /// that references one is a SIBLING that cannot reach it, so a call/reference
    /// to a name in this set from a top-level function body is rejected with a
    /// span instead of the generic "undeclared" diagnostic.
    demoted_fns: std::collections::HashSet<String>,
    /// Every generic user-method call site `obj.m(args)` recorded during lowering
    /// (M1): its member span and the method name when statically known. A generic
    /// method call lowers to an [`IrNode::MethodCallUser`] whose `call_indirect`
    /// needs the funcref table — which only exists when the program defines at
    /// least one function/method. If NO functions are defined, every recorded site
    /// is provably an unknown method, so [`lower`] rejects the first one with a
    /// span diagnostic rather than emitting invalid wasm.
    generic_method_sites: RefCell<Vec<(swc_common::Span, Option<String>)>>,
    /// The provably-fixed object shape of each qualifying binding (see
    /// [`CaptureAnalysis::object_shapes`]): [`binding_id`] mapped to the own keys
    /// of the object literal it holds. A user method call whose receiver resolves
    /// to one of these bindings — or is itself a direct object literal — and whose
    /// statically-known name is absent from the shape is rejected as an unknown
    /// method, closing the wrong-value dispatch this static-shape guard targets.
    object_shapes: HashMap<u32, Vec<String>>,
}

/// The lowered shape of a `class` declaration (P2.2): the instance field names the
/// constructor assigns via `this.<field> = ..`, in first-seen order. `new A(args)`
/// pre-allocates the instance object with one slot per field (initialized to
/// `undefined`), because the object model has no dynamic key growth — a fresh
/// empty object could not gain a `this.v` property at runtime.
struct ClassInfo {
    /// Distinct instance field names, in first-assignment order. For a derived
    /// class (E-series `extends`) this already includes the inherited fields
    /// (the parent's flattened list, then this class's own), so `new C()`
    /// pre-slots every field a constructor up the chain may assign.
    fields: Vec<String>,
}

/// A lexical-scope resolver used only by [`analyze_captures`]. It mirrors the
/// lowering pass's scoping — a stack of function frames, each a stack of block
/// scopes — but records only which bindings are referenced across a function
/// boundary (and are therefore captured). It performs no codegen.
#[derive(Default)]
struct CaptureResolver {
    /// Function frames, outermost first; each is a stack of block scopes mapping
    /// a declared name to its [`binding_id`].
    frames: Vec<Vec<HashMap<String, u32>>>,
    /// Binding ids referenced across a function boundary.
    captured: std::collections::HashSet<u32>,
    /// The [`binding_id`] of the top-level function declaration currently being
    /// analyzed (G2), or `None` while analyzing `main`'s own statements. A
    /// reference reaching the `main` frame while this is set marks that
    /// declaration as capturing.
    current_top_fn: Option<u32>,
    /// Ids of the top-level function declarations whose bodies close over a
    /// `main` binding (G2), and must therefore be demoted to closures.
    capturing_fns: std::collections::HashSet<u32>,
    /// Names that are the target of an assignment (`x = ..`, `x op= ..`, `++x`,
    /// `x--`, or a destructuring assignment) ANYWHERE in the program. A
    /// name-based over-approximation used to disqualify object-shape candidates:
    /// a binding whose name is ever reassigned cannot be trusted to keep its
    /// declared literal shape, so it is excluded (see [`object_shapes`]).
    ///
    /// [`object_shapes`]: CaptureAnalysis::object_shapes
    assigned: std::collections::HashSet<String>,
    /// Provisional object-shape entries `(binding_id, name, own keys)`: every
    /// `let`/`const` binding initialized directly with an object literal whose
    /// keys are all statically known. Finalized in [`analyze_captures`] by
    /// dropping any whose name appears in [`assigned`], leaving the bindings
    /// whose literal shape is provably fixed.
    ///
    /// [`assigned`]: CaptureResolver::assigned
    shape_candidates: Vec<(u32, String, Vec<String>)>,
}

/// The result of [`analyze_captures`]: which bindings are boxed into cells, and
/// which top-level function declarations must be demoted to closures (G2).
struct CaptureAnalysis {
    /// Binding ids captured by a nested function (boxed into cells, S13.4).
    captured: std::collections::HashSet<u32>,
    /// [`binding_id`]s of the top-level function declarations that close over an
    /// enclosing `main` binding, and must be demoted from the direct-call fast
    /// path to a closure (G2).
    capturing_fns: std::collections::HashSet<u32>,
    /// The provably-fixed object shape of each qualifying binding: its
    /// [`binding_id`] mapped to the own keys of the object literal it is
    /// initialized with. A binding qualifies when it is a `let`/`const`
    /// initialized directly with an all-static-key object literal AND its name
    /// is never reassigned. A method call on such a receiver whose name is not
    /// among these keys is provably unknown (the object model has no dynamic key
    /// growth), so lowering rejects it with a span diagnostic instead of emitting
    /// a bogus runtime dispatch.
    object_shapes: HashMap<u32, Vec<String>>,
}

/// The body shape passed to [`CaptureResolver::arrow_or_fn`].
#[derive(Clone, Copy)]
enum AnalyzeBody<'a> {
    Block(&'a [Stmt]),
    Expr(&'a Expr),
}

/// The resolved signature of a top-level function declaration.
struct FnSig {
    /// The callee's wasm function index (`>= 1`; `0` is `main`).
    index: u32,
    /// The declared parameter count (the maximum accepted call arity).
    params: usize,
    /// The number of required arguments: leading parameters before the first one
    /// with a default (`a = expr`) or the trailing rest parameter (`...rest`). A
    /// call may supply `required..=params` args (or more when [`has_rest`]); the
    /// surplus slots are padded with `undefined` by the uniform ABI.
    ///
    /// [`has_rest`]: FnSig::has_rest
    required: usize,
    /// Whether the final parameter is a rest parameter (`...rest`, P1.4), which
    /// lifts the upper bound on call arity — any surplus arguments are gathered
    /// into the rest array rather than rejected.
    has_rest: bool,
}

/// The body shape of a function/arrow being lowered: an ordinary statement
/// block, or a single expression (an expression-bodied arrow, desugared to
/// `return <expr>`).
#[derive(Clone, Copy)]
enum BodyKind<'a> {
    Block(&'a [Stmt]),
    Expr(&'a Expr),
}

/// A resolved name binding within a function frame: its wasm local, whether it
/// may be reassigned, whether it is a **boxed cell** (S13.4, captured by a
/// nested function), and its [`binding_id`].
#[derive(Clone, Copy)]
struct Binding {
    local: u32,
    mutable: bool,
    cell: bool,
    id: u32,
    /// For a binding in an `env_backed` frame (a generator local, P4.2 M2): the
    /// environment slot holding its boxed cell, reached via `EnvLoad`. `None` for
    /// every ordinary binding (whose cell, if any, lives in a wasm local).
    env_slot: Option<u32>,
}

/// How a resolved name is accessed from the current function frame.
#[derive(Clone)]
enum AccessKind {
    /// A plain wasm local, read/written directly.
    Plain(u32),
    /// A boxed cell reached through the given pointer expression (a `Local` for
    /// a cell declared in this frame, or an `EnvLoad` for a captured cell).
    Cell(IrNode),
}

/// A resolved name reference: how to reach it plus its mutability.
struct Resolved {
    kind: AccessKind,
    mutable: bool,
}

/// One function frame: its lexical scopes, local counter, `break` nesting, and
/// closure bookkeeping (the boxed cells it declares and the environment slots it
/// captures from enclosing frames).
struct Frame {
    /// Lexical scopes (block scopes) within this function, outermost first.
    scopes: Vec<HashMap<String, Binding>>,
    /// The next local index to hand out in this frame.
    next: u32,
    /// How many `switch`/loop bodies enclose the statement being lowered.
    breakable: u32,
    /// How many LOOP bodies enclose the statement being lowered (a subset of
    /// `breakable` that excludes `switch`). A `continue` is legal only when this
    /// is nonzero — it targets a loop, never a bare `switch`.
    loopable: u32,
    /// Binding id → local for the boxed cells declared in this frame; used to
    /// source a captured cell when building an inner function's environment.
    by_id: HashMap<u32, u32>,
    /// When set, EVERY binding declared in this frame is a boxed cell reached
    /// through the frame's own environment (an `EnvLoad`), not a wasm local — the
    /// generator state-machine model (P4.2 M2). The persistent frame of cells is
    /// built once by the generator constructor and threaded in as the env
    /// parameter, so a local survives across a `yield` (a `call_indirect` return).
    env_backed: bool,
    /// Local indices that hold a boxed cell pointer rather than a plain value.
    cells: std::collections::HashSet<u32>,
    /// This function's environment layout: env slot → captured binding id.
    env: Vec<u32>,
    /// Captured binding id → env slot (the inverse of `env`).
    env_index: HashMap<u32, u32>,
    /// Active statement labels enclosing the point being lowered, innermost last:
    /// each is a `(label name, loop id)` pair, where the loop id is the
    /// [`IrStmt::While::label`] of the labeled loop. A labeled `break`/`continue`
    /// resolves its target name to a loop id here. Labels do not cross a function
    /// boundary, so this is per-frame.
    labels: Vec<(String, u32)>,
    /// The next loop-id to hand out for a labeled loop in this frame.
    next_label: u32,
    /// Whether this frame is a function scope that binds `this` (P2.1). `true` for
    /// every lowered function/arrow/method body, `false` for the top-level `main`
    /// frame — so a top-level `this` is rejected.
    allows_this: bool,
    /// The wasm local this frame's function snapshots the `__this` global into at
    /// entry (P2.1), allocated lazily on the first `this` read (`None` until then,
    /// and for a function that never reads `this`). An `Expr::This` in this frame
    /// reads this local.
    this_local: Option<u32>,
}

/// Lowering context: a stack of function frames (S13.4). Each frame maps
/// declared names to their [`Binding`] over its own wasm locals; name
/// resolution walks the innermost frame's scopes outward, then continues into
/// enclosing frames. A name resolving in an enclosing frame is a **captured**
/// binding, reached through the closure environment.
///
/// Within a frame, each `{ .. }` block (and the head of a `for` loop) pushes a
/// scope, so an inner `let`/`const` shadows an outer binding of the same name
/// and does not leak out of its block. Wasm locals are never reused when a
/// scope is popped, so a shadowed binding always maps to a distinct local.
struct LowerCtx<'a> {
    /// Function frames, outermost first; the last is the one being lowered.
    frames: Vec<Frame>,
    /// Shared lowering state (function signatures, uniform arity, the
    /// lifted-function accumulator, and the captured-binding set).
    shared: &'a Shared<'a>,
    /// Whether this context lowers a NON-capturing top-level function body (its
    /// own independent frame, with no `main` beneath it). Set only by
    /// [`lower_function`]. Used to report a call to a demoted sibling function
    /// (G2) with a precise span diagnostic rather than "undeclared".
    top_level_fn: bool,
    /// Whether the body currently being lowered is an `async function` body
    /// (P4.3 M4). When set, an `await e` lowers to a generator `yield e` (the
    /// async body is compiled as a state machine, driven by the host async
    /// engine) rather than a top-level `Await`. Set/cleared by
    /// [`LowerCtx::lower_generator_body`] around an async body.
    async_body: bool,
    /// The name of the superclass whose constructor/methods `super` resolves to
    /// while lowering a derived class's constructor and method bodies (E-series
    /// `extends`), or `None` outside any `extends` body. Saved and restored by
    /// [`LowerCtx::lower_class_decl`] so nested class declarations do not leak
    /// their superclass to an enclosing one.
    current_super: Option<String>,
    /// The class name that `this` resolves to while lowering a deferred `static`
    /// field initializer (P2b): a static initializer runs with `this` bound to the
    /// class, so `static f = this.g` reads the class's own static `g`. Set/cleared
    /// by [`LowerCtx::lower_class_decl`] around the post-binding static assignments;
    /// `None` everywhere else, where `this` keeps its ordinary function-frame slot.
    static_init_class: Option<String>,
}

mod arity;
mod capture;
mod expr;
mod function;
mod generator;
mod op;
mod scope;
mod stmt;

#[allow(clippy::wildcard_imports)]
use arity::*;
#[allow(clippy::wildcard_imports)]
use capture::*;
#[allow(clippy::wildcard_imports)]
use function::*;
#[allow(clippy::wildcard_imports)]
use op::*;

#[cfg(test)]
mod tests;
