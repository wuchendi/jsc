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

use std::cell::{Cell, RefCell};

use std::collections::HashMap;

use anyhow::anyhow;

use swc_common::Spanned;

use crate::error::CompileError;

use swc_ecma_ast::{
    ArrayLit, ArrayPat, ArrowExpr, ArrowFunctionBody, AssignExpr, AssignOp, AssignTarget,
    AssignTargetPat, BinaryOp, Callee, Class, ClassDecl, ClassMember, ClassMethod, ClassProp, Decl,
    Expr, ExprOrSpread, ExprStmt, FnDecl, ForHead, Ident, Lit, MemberProp, MethodKind, ObjectLit,
    ObjectPat, ObjectPatProp, OptChainBase, OptChainExpr, ParamOrTsParamProp, Pat, Program, Prop,
    PropName, PropOrSpread, SimpleAssignTarget, Stmt, SuperProp, SuperPropExpr, SwitchStmt,
    UnaryOp, UpdateExpr, UpdateOp, VarDeclKind, VarDeclOrExpr,
};

use globals::GlobalFacts;

use crate::ir::{
    ArrayHofOp, BinOp, CalleeError, ConvertOp, DateOp, ErrorKind, FnInfo, FnKind, GlobalIntrinsic,
    GlobalRef, InstanceOfKind, IrCase, IrFunction, IrNode, IrProgram, IrStmt, LogicalOp, MapOp,
    MathOp, MethodOp, ObjectBuiltinOp, OpSetKind, PropKey, ProtoKind, SpreadCallee, SpreadElem,
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
    // which of `main`'s bindings the declarations reference.
    let mut all_fn_decls: Vec<&FnDecl> = Vec::new();
    let mut scan_top_stmts: Vec<&Stmt> = Vec::new();
    for stmt in &body {
        if let Stmt::Decl(Decl::Fn(fn_decl)) = stmt {
            all_fn_decls.push(fn_decl);
        } else {
            scan_top_stmts.push(stmt);
        }
    }

    // Capture analysis: which bindings are boxed into cells, and which
    // top-level bindings are referenced from top-level functions (module
    // bindings). Runs before any body is lowered so a captured binding's
    // declaration knows to allocate its cell up front.
    let analysis = analyze_captures(&scan_top_stmts, &all_fn_decls);

    // Pass 1b: register every hoisted top-level function declaration for the
    // direct-call fast path. A declaration referencing top-level bindings reaches
    // them through module cells, so it needs no closure environment.
    let mut funcs: HashMap<String, FnSig> = HashMap::new();
    let mut fn_decls: Vec<&FnDecl> = Vec::new();
    let mut top_stmts: Vec<&Stmt> = Vec::new();
    // Index 0 is `main`, so user functions are numbered from 1 upward.
    let mut next_index: u32 = 1;
    for stmt in &body {
        let Stmt::Decl(Decl::Fn(fn_decl)) = stmt else {
            top_stmts.push(stmt);
            continue;
        };
        let name = fn_decl.ident.sym.as_ref().to_string();
        if funcs
            .insert(name.clone(), FnSig { index: next_index })
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

    // The fixed positional-argument arity `K` of the uniform ABI: the
    // widest source arity across *every* function — top-level declarations AND
    // any nested arrow function, which lift to Table-registered functions
    // sharing the same signature. Scanning the whole program up front fixes `K`
    // before any body is lowered, so a single `call_indirect` type serves them
    // all and no arrow's params can overflow the fixed argument slots.
    // A generator's — and an `async function`'s — state machine is
    // driven with two positional arguments (`sent`, the handle), so `K` must be at
    // least 2 for the second slot to exist, regardless of the widest
    // source arity.
    let has_state_machine = fn_decls
        .iter()
        .any(|f| f.function.is_generator || f.function.is_async);
    // A builtin reached through its function value reads its arguments from
    // the same `K` slots, so `K` then also covers the widest call.
    let globals = GlobalFacts::scan(program);
    let max_arity = body
        .iter()
        .map(|s| scan_stmt_arity(s))
        .max()
        .unwrap_or(0)
        .max(if has_state_machine { 2 } else { 0 })
        .max(if globals.reaches_builtins() {
            globals.call_arity()
        } else {
            0
        });

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
        hidden_ids: Cell::new(0),
        module_slots: analysis
            .module_names
            .iter()
            .zip(0u32..)
            .map(|(name, slot)| (name.clone(), slot))
            .collect(),
        module_mutable: analysis.module_mutable,
        completion: Cell::new(None),
        globals,
    };

    // Pass 2: the top-level statements become `main`'s body, lowered in a single
    // function frame (`main` has no env parameter; captured top-level bindings
    // still become boxed cells sourced from its locals). `main` is lowered BEFORE
    // the declaration bodies so every class shape and module-binding mutability
    // they consult is already recorded.
    let mut ctx = LowerCtx::new(&shared);
    ctx.main_ctx = true;
    let mut stmts = Vec::new();
    // A `var` module binding is initialized to `undefined` (hoisted, no TDZ):
    // clear its module cell's TDZ hole before any user code runs.
    for (name, slot) in analysis.module_names.iter().zip(0u32..) {
        if analysis.module_vars.contains(name) {
            let local = ctx.fresh();
            stmts.push(IrStmt::Decl {
                local,
                init: IrNode::CellSet {
                    cell: Box::new(IrNode::ModuleCell(slot)),
                    value: Box::new(IrNode::Undefined),
                },
            });
        }
    }
    for stmt in top_stmts {
        ctx.lower_stmt(stmt, &mut stmts)?;
    }
    let locals = ctx.frame().next;
    // Release the shared borrow so the arrow accumulator can be moved out.
    drop(ctx);

    // Pass 3: lower each function body over its own locals, laid out for the
    // uniform ABI (`max_arity` argument slots, the env slot, then body locals).
    let mut functions = Vec::with_capacity(fn_decls.len());
    for fn_decl in &fn_decls {
        functions.push(lower_function(fn_decl, &shared)?);
    }

    // Lifted arrow functions follow the declarations in the functions vector, so
    // their positions match the funcref table indices handed out at their use
    // sites.
    functions.extend(shared.arrows.into_inner());

    Ok(IrProgram {
        locals,
        stmts,
        functions,
        module_cells: u32::try_from(analysis.module_names.len()).unwrap_or(u32::MAX),
        arity: u32::try_from(max_arity).unwrap_or(u32::MAX),
    })
}

/// Shared lowering state threaded through every [`LowerCtx`]: the resolved
/// top-level function signatures, the uniform ABI arity, and the accumulator of
/// arrow functions lifted to top level.
struct Shared<'a> {
    /// Top-level function signatures, keyed by name.
    funcs: &'a HashMap<String, FnSig>,
    /// Arrow functions lifted to top level. Each use-site of an arrow
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
    /// whose id is in this set is boxed into a heap cell.
    captured: std::collections::HashSet<u32>,
    /// Classes lowered so far, keyed by name: a pure optimisation. A class
    /// records its instance shape here as it is lowered; a later `new A(args)`
    /// reads the shape back to pre-size the fresh instance (a rebound name only
    /// mis-sizes the table). Interior mutability lets the immutable-borrow
    /// lowering walk register new classes.
    classes: RefCell<HashMap<String, ClassInfo>>,
    /// The number of hidden by-value bindings allocated so far (see
    /// [`LowerCtx::hidden_local`]).
    hidden_ids: Cell<u32>,
    /// Module-cell slot of each module binding name (see
    /// [`CaptureAnalysis::module_names`]). A `main` declaration of one of these
    /// names in its outermost scope stores into the module cell; a top-level
    /// function body reaches it through [`IrNode::ModuleCell`].
    module_slots: HashMap<String, u32>,
    /// Mutability of each module binding (see [`CaptureAnalysis::module_mutable`]).
    module_mutable: HashMap<String, bool>,
    /// The `main` local tracking the completion value (for `-p`) of the
    /// top-level `try` statement being lowered, or `None` outside one. While set,
    /// every expression statement in `main`'s own frame stores its value here
    /// (see [`LowerCtx::completion_local`]), so a value produced inside a nested
    /// statement body reaches `main`'s result.
    completion: Cell<Option<u32>>,
    /// The program-level global facts (design 3.2).
    globals: GlobalFacts,
}

/// The lowered shape of a `class` declaration: the instance field names the
/// constructor assigns via `this.<field> = ..`, in first-seen order. `new A(args)`
/// sizes the instance's property table for them up front (design 3.2), so the
/// constructor's adds do not regrow it.
struct ClassInfo {
    /// Distinct instance field names, in first-assignment order. For a class
    /// extending a recorded class name this already includes the inherited
    /// fields (the parent's flattened list, then this class's own).
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
    /// Whether a top-level function declaration's body is being analyzed (rather
    /// than `main`'s own statements). A reference reaching the `main` frame while
    /// this is set records a module binding.
    in_top_fn: bool,
    /// Names of `main`'s outermost-scope bindings referenced from a top-level
    /// function declaration, or from a closure in `main` ahead of their
    /// declaration, in first-reference order (see
    /// [`CaptureAnalysis::module_names`]).
    module_names: Vec<String>,
    /// Every binding `main`'s outermost scope declares, mapped to its mutability
    /// (`false` for `const`). Collected before the walk so a forward reference
    /// from a closure resolves to a module binding.
    top_bindings: HashMap<String, bool>,
}

/// The result of [`analyze_captures`]: which bindings are boxed into cells, and
/// which top-level bindings live in module cells.
struct CaptureAnalysis {
    /// Binding ids captured by a nested function (boxed into cells).
    captured: std::collections::HashSet<u32>,
    /// Names of `main`'s outermost-scope bindings referenced from a top-level
    /// function declaration, in first-reference order. Each is stored in a
    /// module cell (slot = position here) so the hoisted declaration — and
    /// anything it is called from — reaches it without a closure environment.
    module_names: Vec<String>,
    /// The [`module_names`] declared by a top-level `var` (hoisted and
    /// initialized to `undefined`, so they carry no TDZ).
    ///
    /// [`module_names`]: CaptureAnalysis::module_names
    module_vars: std::collections::HashSet<String>,
    /// Mutability of each [`module_names`] binding, known from its top-level
    /// declaration kind (`const` is immutable), so a closure lowered before the
    /// declaration (a forward reference) still resolves it.
    ///
    /// [`module_names`]: CaptureAnalysis::module_names
    module_mutable: HashMap<String, bool>,
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
/// may be reassigned, whether it is a **boxed cell** (captured by a
/// nested function), and its [`binding_id`].
#[derive(Clone, Copy)]
struct Binding {
    local: u32,
    mutable: bool,
    cell: bool,
    id: u32,
    /// The module-cell slot of a module binding (see [`Shared::module_slots`]):
    /// its value lives in that cell rather than a wasm local. `None` otherwise.
    module: Option<u32>,
    /// For a binding in an `env_backed` frame (a generator local): the
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
    /// An unresolved name: a property of the global object (design 4.3).
    Global(GlobalRef),
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
    /// Binding id → local for the boxed cells declared in this frame (and its
    /// by-value `new.target` snapshot); used to source a captured binding when
    /// building an inner function's environment.
    by_id: HashMap<u32, u32>,
    /// When set, EVERY binding declared in this frame is a boxed cell reached
    /// through the frame's own environment (an `EnvLoad`), not a wasm local — the
    /// generator state-machine model. The persistent frame of cells is
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
    /// Whether this frame is a function scope that binds `this`. `true` for
    /// every lowered function/arrow/method body, `false` for the top-level `main`
    /// frame — so a top-level `this` is rejected.
    allows_this: bool,
    /// The wasm local this frame's function snapshots the `__this` global into at
    /// entry, allocated lazily on the first `this` read (`None` until then,
    /// and for a function that never reads `this`). An `Expr::This` in this frame
    /// reads this local.
    this_local: Option<u32>,
    /// Whether this frame is an arrow body, which has no `new.target` of its
    /// own: it reads the enclosing function's.
    arrow: bool,
    /// The wasm local this (non-arrow) function snapshots `new.target` into at
    /// entry, allocated on the first `new.target` reference in it or in a nested
    /// arrow (`None` until then).
    new_target_local: Option<u32>,
    /// The wasm local this (non-arrow) function builds its `arguments` object
    /// into at entry, allocated on the first `arguments` reference in it or in
    /// a nested arrow (`None` until then).
    arguments_local: Option<u32>,
    /// The wasm local this function snapshots the `__spread_args` global into
    /// at entry, allocated when it gathers its arguments (a rest parameter or
    /// `arguments`) and so may read the ones beyond `K`.
    spread_args_local: Option<u32>,
    /// [`FnKind::Class`] for a base class constructor, which initializes the
    /// class's instance fields ([`LowerCtx::class_fields`]) on entry;
    /// [`FnKind::DerivedClass`] for a derived one, whose `this` starts in the
    /// TDZ and is bound by `super(..)` (which also runs the instance fields).
    /// [`FnKind::Plain`] otherwise.
    ctor: FnKind,
    /// A derived constructor's `super(..)` bookkeeping.
    supers: SuperCalls,
}

/// A derived constructor's `super(..)` bookkeeping (see [`Frame::ctor`]).
#[derive(Default)]
struct SuperCalls {
    /// The span of the first `super(..)` call that is a top-level statement of
    /// the body. Once it is lowered, `this` is definitely bound for everything
    /// lowered after it in this frame.
    top_level: Option<swc_common::Span>,
    /// Whether `this` is definitely bound (see `top_level`): reads skip the
    /// TDZ check.
    this_bound: bool,
    /// How many `super(..)` calls this frame has lowered.
    lowered: u32,
    /// The hidden id of a cell holding `this`, when an arrow in the body
    /// calls `super(..)` or reads `this` and so shares the constructor's
    /// `this` (every derived `this` read and write then goes through the cell).
    this_cell: Option<u32>,
}

impl Frame {
    /// Whether this frame is a derived class constructor.
    fn derived(&self) -> bool {
        self.ctor == FnKind::DerivedClass
    }
}

/// What `super` resolves to inside a class body (design 4.5): the class
/// frame's hidden by-value bindings of the parent constructor and of its
/// `prototype` (the home object's prototype for instance members).
#[derive(Clone, Copy)]
struct SuperScope {
    /// The frame depth holding the hidden bindings.
    frame: usize,
    /// Hidden id of the parent constructor (`None`: a base class).
    parent: Option<u32>,
    /// Hidden id of `parent.prototype`.
    proto_parent: Option<u32>,
    /// Whether the member being lowered is static (its home object is the
    /// class, so `super.x` reads the parent constructor).
    is_static: bool,
    /// Property-table capacity `super(..)` pre-reserves on `this`.
    reserve: u32,
}

/// Lowering context: a stack of function frames. Each frame maps
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
    /// Whether this context lowers `main` (frame 0 is `main`'s frame) rather than
    /// a top-level function declaration body. Only `main` declares module
    /// bindings; a function body resolves them through module cells.
    main_ctx: bool,
    /// Whether the body currently being lowered is an `async function` body.
    /// When set, an `await e` lowers to a generator `yield e` (the
    /// async body is compiled as a state machine, driven by the host async
    /// engine) rather than a top-level `Await`. Set/cleared by
    /// [`LowerCtx::lower_generator_body`] around an async body.
    async_body: bool,
    /// What `super` resolves to while lowering a class body, or `None` outside
    /// any class. Saved and restored by [`LowerCtx::lower_class`] so a nested
    /// class does not leak its scope to an enclosing one.
    current_super: Option<SuperScope>,
    /// The instance field declarations of the class whose constructor is being
    /// lowered: a base constructor defines them on entry, a derived one after
    /// each `super(..)`.
    class_fields: Vec<ClassProp>,
    /// While lowering `static` field initializers: the class frame and the
    /// hidden id of the class value, which `this` resolves to there.
    static_init: Option<(usize, u32)>,
}

mod arity;
mod capture;
mod expr;
mod function;
mod generator;
mod globals;
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
