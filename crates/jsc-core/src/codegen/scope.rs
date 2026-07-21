#[allow(clippy::wildcard_imports)]
use super::*;

impl Frame {
    fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            next: 0,
            breakable: 0,
            loopable: 0,
            by_id: HashMap::new(),
            env_backed: false,
            cells: std::collections::HashSet::new(),
            env: Vec::new(),
            env_index: HashMap::new(),
            labels: Vec::new(),
            next_label: 0,
            allows_this: false,
            this_local: None,
        }
    }
}

impl<'a> LowerCtx<'a> {
    /// Create a fresh lowering context (one initial function frame) sharing the
    /// given lowering state.
    pub(super) fn new(shared: &'a Shared<'a>) -> Self {
        Self {
            frames: vec![Frame::new()],
            shared,
            top_level_fn: false,
            async_body: false,
            current_super: None,
            static_init_class: None,
        }
    }

    /// The index of the innermost (current) function frame.
    fn top(&self) -> usize {
        self.frames.len() - 1
    }

    /// A mutable handle to the innermost function frame.
    pub(super) fn frame(&mut self) -> &mut Frame {
        let i = self.top();
        &mut self.frames[i]
    }

    /// Enter a nested function frame (an arrow or function literal).
    pub(super) fn push_frame(&mut self) {
        self.frames.push(Frame::new());
    }

    /// Leave the innermost function frame.
    pub(super) fn pop_frame(&mut self) {
        self.frames.pop();
    }

    /// Enter a new lexical scope (a `{ .. }` block or a `for` head).
    pub(super) fn push_scope(&mut self) {
        self.frame().scopes.push(HashMap::new());
    }

    /// Leave the innermost lexical scope, discarding its name bindings.
    pub(super) fn pop_scope(&mut self) {
        self.frame().scopes.pop();
    }

    /// `break` nesting of the current frame.
    pub(super) fn breakable(&self) -> u32 {
        self.frames.last().map_or(0, |f| f.breakable)
    }

    pub(super) fn breakable_inc(&mut self) {
        self.frame().breakable += 1;
    }

    pub(super) fn breakable_dec(&mut self) {
        let frame = self.frame();
        frame.breakable = frame.breakable.saturating_sub(1);
    }

    /// `continue` nesting of the current frame (loop bodies only).
    pub(super) fn loopable(&self) -> u32 {
        self.frames.last().map_or(0, |f| f.loopable)
    }

    pub(super) fn loopable_inc(&mut self) {
        self.frame().loopable += 1;
    }

    pub(super) fn loopable_dec(&mut self) {
        let frame = self.frame();
        frame.loopable = frame.loopable.saturating_sub(1);
    }

    /// Allocate a fresh loop id for a labeled loop in the current frame.
    pub(super) fn fresh_label(&mut self) -> u32 {
        let frame = self.frame();
        let id = frame.next_label;
        frame.next_label += 1;
        id
    }

    /// Register `name` as labeling the loop with `id` for the duration of that
    /// loop's body, then run `f`, then remove the label. A labeled `break`/
    /// `continue` lowered inside `f` resolves `name` to `id` via [`Self::label_id`].
    pub(super) fn with_label<T>(
        &mut self,
        name: &str,
        id: u32,
        f: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.frame().labels.push((name.to_owned(), id));
        let result = f(self);
        self.frame().labels.pop();
        result
    }

    /// The loop id a statement label resolves to in the current frame, or `None`
    /// when no active label has that name.
    pub(super) fn label_id(&self, name: &str) -> Option<u32> {
        self.frames
            .last()?
            .labels
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    /// Allocate a fresh anonymous local in the current frame.
    pub(super) fn fresh(&mut self) -> u32 {
        let frame = self.frame();
        let idx = frame.next;
        frame.next += 1;
        idx
    }

    /// Allocate a fresh local for `name`, binding it in the innermost scope with
    /// the given mutability and [`binding_id`]. If capture analysis marked the
    /// id captured, the binding is a boxed cell (its local holds a cell pointer).
    pub(super) fn declare(&mut self, name: &str, mutable: bool, id: u32) -> u32 {
        let idx = self.fresh();
        let env_backed = self.frame().env_backed;
        // An `env_backed` (generator) frame boxes EVERY binding into an env cell so
        // it survives across a `yield`; an ordinary frame boxes only the bindings
        // capture analysis flagged (S13.4).
        let cell = env_backed || self.shared.captured.contains(&id);
        // In an `env_backed` frame the cell lives at a fresh env slot (reached via
        // `EnvLoad`), not in a wasm local; the constructor pre-creates it.
        let env_slot = if env_backed {
            let frame = self.frame();
            let slot = u32::try_from(frame.env.len()).unwrap_or(u32::MAX);
            frame.env.push(id);
            frame.env_index.insert(id, slot);
            Some(slot)
        } else {
            None
        };
        let frame = self.frame();
        let innermost = frame.scopes.len() - 1;
        frame.scopes[innermost].insert(
            name.to_owned(),
            Binding {
                local: idx,
                mutable,
                cell,
                id,
                env_slot,
            },
        );
        if cell {
            frame.cells.insert(idx);
            frame.by_id.insert(id, idx);
        }
        idx
    }

    /// Resolve `this` for the current frame (P2.1): allocate the frame's `this`
    /// slot on first use (so a `this`-free function keeps its original local
    /// layout) and return its local index, or `None` in a non-function scope (the
    /// top-level `main` frame), where `this` has no binding. A prologue snapshot of
    /// the `__this` global into this slot is added by `build_function_body` when
    /// the slot was allocated.
    pub(super) fn ensure_this_local(&mut self) -> Option<u32> {
        if !self.frame().allows_this {
            return None;
        }
        if let Some(slot) = self.frame().this_local {
            return Some(slot);
        }
        let slot = self.fresh();
        self.frame().this_local = Some(slot);
        Some(slot)
    }

    /// Whether the local `idx` in the current frame is a boxed cell.
    pub(super) fn is_cell(&self, idx: u32) -> bool {
        self.frames.last().is_some_and(|f| f.cells.contains(&idx))
    }

    /// The environment slot of the binding holding `local` in the current frame,
    /// when that frame is `env_backed` (a generator, P4.2 M2). `None` for an
    /// ordinary frame or a non-binding local.
    pub(super) fn env_slot_for_local(&self, local: u32) -> Option<u32> {
        let frame = self.frames.last()?;
        if !frame.env_backed {
            return None;
        }
        frame
            .scopes
            .iter()
            .rev()
            .find_map(|scope| scope.values().find(|b| b.local == local))
            .and_then(|b| b.env_slot)
    }

    /// Mark the current frame `env_backed`: every binding it declares becomes an
    /// env cell reached via `EnvLoad`, and [`Self::env_layout`] returns the frame
    /// slots. Used to lower a generator body (P4.2 M2).
    pub(super) fn set_env_backed(&mut self) {
        self.frame().env_backed = true;
    }

    /// The number of environment slots the current frame has allocated (its
    /// generator frame size, P4.2 M2).
    pub(super) fn env_len(&self) -> usize {
        self.frames.last().map_or(0, |f| f.env.len())
    }

    /// Find the binding a name resolves to: the frame index it lives in and its
    /// [`Binding`] (walking innermost scope/frame outward), or `None`.
    fn find_binding(&self, name: &str) -> Option<(usize, Binding)> {
        for (fi, frame) in self.frames.iter().enumerate().rev() {
            for scope in frame.scopes.iter().rev() {
                if let Some(binding) = scope.get(name) {
                    return Some((fi, *binding));
                }
            }
        }
        None
    }

    /// The [`binding_id`] the name resolves to in the current scope chain, or
    /// `None` when it is not a local/captured binding (a top-level function name
    /// or an undeclared reference). Used by the static-shape unknown-method guard
    /// to look a receiver's tracked object shape up by its declaring id.
    pub(super) fn binding_id_of(&self, name: &str) -> Option<u32> {
        self.find_binding(name).map(|(_, binding)| binding.id)
    }

    /// Ensure every frame between the declaring frame and the current frame
    /// carries an environment slot for `id` (transitive capture), returning the
    /// current frame's env slot for it.
    fn route_env(&mut self, binding_frame: usize, id: u32) -> u32 {
        let top = self.top();
        let mut slot = 0;
        for j in (binding_frame + 1)..=top {
            slot = if let Some(&existing) = self.frames[j].env_index.get(&id) {
                existing
            } else {
                let new_slot = u32::try_from(self.frames[j].env.len()).unwrap_or(u32::MAX);
                self.frames[j].env.push(id);
                self.frames[j].env_index.insert(id, new_slot);
                new_slot
            };
        }
        slot
    }

    /// Resolve a name to a [`Resolved`] access, or `None` if it is not bound in
    /// any frame (the caller then tries top-level functions / `undefined`).
    pub(super) fn resolve(&mut self, name: &str) -> Option<Resolved> {
        let (fi, binding) = self.find_binding(name)?;
        let kind = if fi == self.top() {
            if let Some(slot) = binding.env_slot {
                // A generator local (P4.2 M2): its cell lives in this frame's own
                // environment, reached via `EnvLoad`, so it survives a `yield`.
                AccessKind::Cell(IrNode::EnvLoad(slot))
            } else if binding.cell {
                AccessKind::Cell(IrNode::Local(binding.local))
            } else {
                AccessKind::Plain(binding.local)
            }
        } else {
            // Captured from an enclosing frame: reached through this frame's env.
            let slot = self.route_env(fi, binding.id);
            AccessKind::Cell(IrNode::EnvLoad(slot))
        };
        Some(Resolved {
            kind,
            mutable: binding.mutable,
        })
    }

    /// The read expression for a resolved access.
    pub(super) fn read_of(kind: AccessKind) -> IrNode {
        match kind {
            AccessKind::Plain(local) => IrNode::Local(local),
            AccessKind::Cell(ptr) => IrNode::CellGet(Box::new(ptr)),
        }
    }

    /// The write expression for a resolved access, yielding the stored value.
    pub(super) fn write_of(kind: AccessKind, value: IrNode) -> IrNode {
        match kind {
            AccessKind::Plain(local) => IrNode::Assign {
                local,
                value: Box::new(value),
            },
            AccessKind::Cell(ptr) => IrNode::CellSet {
                cell: Box::new(ptr),
                value: Box::new(value),
            },
        }
    }

    /// The source expression yielding the boxed-cell pointer for captured
    /// binding `id`, evaluated in the current (enclosing) frame while building an
    /// inner function's environment: a `Local` when this frame declares the
    /// cell, else an `EnvLoad` from this frame's own environment (routing the
    /// capture transitively out to the declaring frame).
    pub(super) fn capture_source(&mut self, id: u32) -> IrNode {
        let top = self.top();
        if let Some(&local) = self.frames[top].by_id.get(&id) {
            return IrNode::Local(local);
        }
        let declaring = (0..top).find(|&fi| self.frames[fi].by_id.contains_key(&id));
        let slot = match declaring {
            Some(df) => self.route_env(df, id),
            None => self.frames[top].env_index.get(&id).copied().unwrap_or(0),
        };
        IrNode::EnvLoad(slot)
    }
}
