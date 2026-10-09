//! Emitter core: generator state, slots, and value plumbing.

use crate::mangle::mangle_desc;
use crate::{err, CodegenError, PRELUDE};
use nx_ast::{Expr, Program, Span, Stmt};
use nx_types::Ty;
use std::collections::{HashMap, HashSet};

/// How a source name resolves in generated code. `Local` carries no slot:
/// the slot (and its representation) live in `locals`/`rep`, which the
/// unboxing pass rewrites as it goes.
#[derive(Debug, Clone)]
pub(crate) enum Binding {
    Local,
    Global(String),
    Module(String),
    ModuleFn(String, String),
}

/// Block termination state: which terminator the current block ends with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Term {
    Ret,
    Brk,
    Ctn,
}

pub(crate) struct Gen {
    pub(crate) pre: String,
    pub(crate) top: String,
    pub(crate) out: String,
    pub(crate) plan: nx_mem::Plan,
    pub(crate) programs: HashMap<String, Program>,
    pub(crate) memo: HashMap<(String, String), i64>,
    /// Inferred static types per (module, function); drives unboxing.
    pub(crate) types: HashMap<(String, String), nx_types::FnInfo>,
    /// Global switch: NX_NOUNBOX=1 keeps the old all-boxed behavior.
    pub(crate) unbox_on: bool,
    /// Representation of each *local* slot: present only when the name is
    /// a proven scalar, absent when the slot holds a boxed `%NxVal`. Kept
    /// in lockstep with `locals` so a slot's type always matches its alloca.
    pub(crate) rep: HashMap<String, Ty>,
    pub(crate) tmp: u64,
    pub(crate) label: u64,
    pub(crate) strc: u64,
    pub(crate) arity: HashMap<(String, String), usize>,
    pub(crate) modrefs: HashMap<String, String>,
    pub(crate) falias: HashMap<String, (String, String)>,
    pub(crate) locals: HashMap<String, String>,
    pub(crate) globals: HashSet<String>,
    pub(crate) cur_module: String,
    pub(crate) cur_fn: String,
    /// Name of the basic block the emitter is currently writing into: the
    /// last label passed to `block()`. A phi names the block its incoming
    /// value was computed in, so anything that assumes "I am still in the
    /// block I just opened" has to be able to check that. It is not: a
    /// subexpression can open blocks of its own -- every checked
    /// arithmetic op does, and so does a nested `and`/`or` -- and leave
    /// the emitter somewhere else entirely.
    pub(crate) cur_block: String,
    /// When true, `w()` writes to the top-level buffer (for outlining
    /// task functions in the middle of another function body).
    pub(crate) to_top: bool,
    pub(crate) in_init: bool,
    pub(crate) term: Option<Term>,
    pub(crate) loops: Vec<(String, String)>,
    /// Open entry-block frames, one per function currently being emitted.
    /// Allocas are collected here and spliced in at the end, because an
    /// alloca inside a loop body allocates fresh stack every iteration and
    /// is only released when the function returns.
    pub(crate) alloc_frames: Vec<AllocFrame>,
    /// Memo id for the function being emitted, decided once in the
    /// prologue so the epilogue cannot disagree with it.
    pub(crate) memo_id: Option<i64>,
    /// Declared `type` layouts: (module, type) to field names in
    /// declaration order. Harvested from every program before emission,
    /// so a constructor in one module can use a layout from another.
    pub(crate) layouts: HashMap<(String, String), Vec<String>>,
    /// Declared field types: (module, type, field) to the type name as
    /// written (`"Any"` when the field is unannotated). Harvested with the
    /// layouts. Lets method dispatch see through a field access (`q.p.get()`)
    /// without re-reading the AST: the field's declared record type is
    /// resolved exactly the way the checker resolves it.
    pub(crate) field_types: HashMap<(String, String, String), String>,
    /// `from m import T [as U]` in module `cur`: (cur, alias) to
    /// (declaring module, type). A bare `T(...)` in `cur` resolves
    /// through this exactly the way the checker does.
    pub(crate) type_alias: HashMap<(String, String), (String, String)>,
    /// Methods by (declaring module, canonical type, method). Harvested
    /// like layouts; `from m import T` brings T's methods along.
    pub(crate) methods: HashMap<(String, String, String), MethodSig>,
    /// Type-descriptor globals already emitted, so two records in one
    /// module share one descriptor.
    pub(crate) desc_emitted: HashSet<String>,
}

/// A method signature as the backend sees it: explicit parameter names
/// (excluding an implicit `self`), the receiver kind, and the body to
/// emit. Bodies are emitted as ordinary functions under a mangled name.
#[derive(Debug, Clone)]
pub(crate) struct MethodSig {
    pub(crate) params: Vec<String>,
    pub(crate) receiver: nx_ast::ReceiverKind,
    pub(crate) body: Vec<Stmt>,
    pub(crate) span: Span,
}

/// Where a function's allocas have to be spliced back in: the byte offset
/// Where a function's allocas have to be spliced back in: the byte offset
/// just past its `entry:` label, and which buffer it is being written to.
pub(crate) struct AllocFrame {
    pub(crate) to_top: bool,
    pub(crate) at: usize,
    pub(crate) items: Vec<(String, String)>,
}

/// A loop variable's binding, saved so it can be put back when the loop
/// ends.
///
/// Exists because a loop variable is scoped to its loop. An inner `for i`
/// inside an outer `for i` used to destroy the outer binding, which was
/// observable:
///
/// ```text
/// for i in 0..3:
///     for i in 0..2:
///         print("in", i)
///     print("out", i)     # printed the inner loop's last value, not its own
/// ```
pub(crate) struct LoopVarScope {
    pub(crate) name: String,
    pub(crate) saved_slot: Option<String>,
    pub(crate) saved_rep: Option<Ty>,
}

impl LoopVarScope {
    /// Put the loop's binding back, so the enclosing scope sees exactly
    /// what it saw before.
    pub(crate) fn restore(self, g: &mut Gen) {
        g.locals.remove(&self.name);
        g.rep.remove(&self.name);
        if let Some(s) = self.saved_slot {
            g.locals.insert(self.name.clone(), s);
        }
        if let Some(r) = self.saved_rep {
            g.rep.insert(self.name.clone(), r);
        }
    }
}

impl Gen {
    pub(crate) fn new(
        plan: nx_mem::Plan,
        programs: HashMap<String, Program>,
        memo: HashMap<(String, String), i64>,
        types: HashMap<(String, String), nx_types::FnInfo>,
    ) -> Self {
        Self {
            pre: String::new(),
            top: String::new(),
            out: String::new(),
            plan,
            programs,
            memo,
            types,
            unbox_on: std::env::var("NX_NOUNBOX").is_err(),
            rep: HashMap::new(),
            tmp: 0,
            label: 0,
            strc: 0,
            arity: HashMap::new(),
            modrefs: HashMap::new(),
            falias: HashMap::new(),
            locals: HashMap::new(),
            globals: HashSet::new(),
            cur_module: String::new(),
            cur_fn: String::new(),
            cur_block: String::new(),
            to_top: false,
            in_init: false,
            term: None,
            loops: Vec::new(),
            alloc_frames: Vec::new(),
            memo_id: None,
            layouts: HashMap::new(),
            field_types: HashMap::new(),
            type_alias: HashMap::new(),
            methods: HashMap::new(),
            desc_emitted: HashSet::new(),
        }
    }

    pub(crate) fn emit_prelude(&mut self) {
        self.pre.push_str(PRELUDE);
        self.pre.push('\n');
    }

    /// Walk every program's top-level statements for `type` declarations
    /// and `from ... import` type aliases, before any module is emitted.
    /// A constructor in one module can use a layout declared in another,
    /// so this cannot be done lazily during emission.
    pub(crate) fn harvest_layouts(&mut self) {
        let modules: Vec<String> = self.programs.keys().cloned().collect();
        for module in &modules {
            let prog = match self.programs.get(module) {
                Some(p) => p.clone(),
                None => continue,
            };
            for s in &prog.stmts {
                match s {
                    Stmt::TypeDecl { name, fields, .. } => {
                        self.layouts.insert(
                            (module.clone(), name.clone()),
                            fields.iter().map(|f| f.name.clone()).collect(),
                        );
                        for f in fields {
                            self.field_types.insert(
                                (module.clone(), name.clone(), f.name.clone()),
                                f.ty.clone(),
                            );
                        }
                    }
                    // Methods harvest like layouts: the checker has already
                    // enforced the orphan rule, so the named type is declared
                    // in this same module and the name is canonical.
                    Stmt::Impl {
                        type_name, methods, ..
                    } => {
                        for m in methods {
                            self.methods.insert(
                                (module.clone(), type_name.clone(), m.name.clone()),
                                MethodSig {
                                    params: m.params.clone(),
                                    receiver: m.receiver,
                                    body: m.body.clone(),
                                    span: m.span,
                                },
                            );
                        }
                    }
                    Stmt::FromImport {
                        module: m, names, ..
                    } => {
                        for (name, alias) in names {
                            let bind = alias.clone().unwrap_or_else(|| name.clone());
                            self.type_alias
                                .insert((module.clone(), bind), (m.clone(), name.clone()));
                        }
                    }
                    _ => {}
                }
            }
        }
        // Descriptor globals, one per declared type, in a stable order so
        // the emitted IR is reproducible run to run.
        let mut keys: Vec<(String, String)> = self.layouts.keys().cloned().collect();
        keys.sort();
        for (module, name) in keys {
            self.emit_descriptor(&module, &name);
        }
    }

    /// The static type descriptor for one declared type: the type name and
    /// the field names, which is what the dynamic field-by-name path and
    /// `print` read at runtime.
    pub(crate) fn emit_descriptor(&mut self, module: &str, name: &str) {
        let key = format!("{module}::{name}");
        if !self.desc_emitted.insert(key.clone()) {
            return;
        }
        let fields = match self.layouts.get(&(module.to_string(), name.to_string())) {
            Some(f) => f.clone(),
            None => return,
        };
        let d = mangle_desc(module, name);
        let tname = format!("@.rec.tname.{d}");
        let tn = name.len();
        // The type name as a byte string. Not NUL-terminated: the length
        // travels alongside it, the same convention strings use.
        self.pre
            .push_str(&format!("{tname} = private constant [{tn} x i8] c\""));
        for b in name.bytes() {
            if (32..=126).contains(&b) && b != b'"' && b != b'\\' {
                self.pre.push(b as char);
            } else {
                self.pre.push_str(&format!("\\{b:02X}"));
            }
        }
        self.pre.push_str("\"\n");
        // One { len, bytes } entry per field name.
        let mut entries = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            let g = format!("@.rec.fname.{d}.{i}");
            self.pre
                .push_str(&format!("{g} = private constant [{} x i8] c\"", f.len()));
            for b in f.bytes() {
                if (32..=126).contains(&b) && b != b'"' && b != b'\\' {
                    self.pre.push(b as char);
                } else {
                    self.pre.push_str(&format!("\\{b:02X}"));
                }
            }
            self.pre.push_str("\"\n");
            entries.push(format!("%NxRecName {{ i64 {}, ptr {g} }}", f.len()));
        }
        let n = fields.len();
        let names = format!("@.rec.names.{d}");
        self.pre
            .push_str(&format!("{names} = private constant [{n} x %NxRecName] ["));
        self.pre.push_str(&entries.join(", "));
        self.pre.push_str("]\n");
        // The array's own address is the name table: it points at element
        // zero, which is exactly what the runtime indexes. An extra global
        // holding the pointer would add an indirection the reader would
        // have to load through.
        self.pre.push_str(&format!(
            "@{d} = private constant %NxDesc {{ ptr {tname}, i64 {tn}, i64 {n}, ptr {names} }}\n"
        ));
    }

    /// Resolve a constructor or field-access type name in the current
    /// module to its declaring module, canonical name and layout. Follows
    /// `from ... import` aliases exactly the way the checker does; a local
    /// declaration wins over an alias, matching source-order semantics.
    /// The global fallback covers a canonical name that is reachable but
    /// neither declared nor aliased locally -- the checker guarantees such
    /// a program only when the declaration exists somewhere, and the
    /// sorted search keeps the choice deterministic.
    pub(crate) fn resolve_type(&self, name: &str) -> Option<(String, String, Vec<String>)> {
        self.resolve_type_in(&self.cur_module.clone(), name)
    }

    /// `resolve_type` against an explicit module, so a field's declared
    /// type resolves in its own type's context rather than the caller's.
    pub(crate) fn resolve_type_in(
        &self,
        module: &str,
        name: &str,
    ) -> Option<(String, String, Vec<String>)> {
        if let Some(fields) = self.layouts.get(&(module.to_string(), name.to_string())) {
            return Some((module.to_string(), name.to_string(), fields.clone()));
        }
        if let Some((m, t)) = self.type_alias.get(&(module.to_string(), name.to_string())) {
            if let Some(fields) = self.layouts.get(&(m.clone(), t.clone())) {
                return Some((m.clone(), t.clone(), fields.clone()));
            }
        }
        let mut mods: Vec<String> = self.layouts.keys().map(|(m, _)| m.clone()).collect();
        mods.sort();
        mods.dedup();
        for m in mods {
            if let Some(fields) = self.layouts.get(&(m.clone(), name.to_string())) {
                return Some((m, name.to_string(), fields.clone()));
            }
        }
        None
    }

    /// The static type of a field read from a value of a known record
    /// type, from the declaration alone. Mirrors the checker's `field_ty`:
    /// scalars map directly, a name that resolves to a declared type is
    /// that record, and anything else (`Any`, containers, unknown names)
    /// is not statically known here.
    pub(crate) fn declared_field_ty(&self, type_name: &str, field: &str) -> Option<Ty> {
        let (decl_module, canon, _) = self.resolve_type(type_name)?;
        let written = self
            .field_types
            .get(&(decl_module.clone(), canon, field.to_string()))?;
        match written.as_str() {
            "Int" => Some(Ty::Int),
            "Float" => Some(Ty::Float),
            "Bool" => Some(Ty::Bool),
            "Str" => Some(Ty::Str),
            "None" => Some(Ty::None),
            other => {
                let (_, other_canon, _) = self.resolve_type_in(&decl_module, other)?;
                Some(Ty::Record(other_canon))
            }
        }
    }

    /// The static record type behind an arbitrary receiver expression.
    /// A bare variable asks the checker's inference; a field read recurses
    /// through the holder into the field's declared type, so `q.p` is
    /// known when `q` is a record with a record-typed field. Anything
    /// else is dynamic, exactly as before.
    pub(crate) fn static_ty_of(&self, e: &Expr) -> Ty {
        match e {
            Expr::Var(n, _) => self.ty_dispatch(n),
            Expr::Attr { base, attr, .. } => match self.static_ty_of(base) {
                Ty::Record(t) => self.declared_field_ty(&t, attr).unwrap_or(Ty::Unknown),
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    /// Descriptor global for a resolved type.
    pub(crate) fn desc_of(&self, module: &str, name: &str) -> String {
        format!("@{}", mangle_desc(module, name))
    }

    /// Field offset of `field` in a resolved layout, or an error naming
    /// the type rather than the index.
    pub(crate) fn field_index(
        fields: &[String],
        type_name: &str,
        field: &str,
        span: Span,
    ) -> Result<usize, CodegenError> {
        fields.iter().position(|f| f == field).ok_or(err(
            span,
            format!("type '{type_name}' has no field '{field}'"),
        ))
    }

    pub(crate) fn finish(self) -> String {
        format!("{}{}{}", self.pre, self.top, self.out)
    }

    pub(crate) fn reg(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    pub(crate) fn lab(&mut self, hint: &str) -> String {
        self.label += 1;
        format!("{hint}{}", self.label)
    }

    pub(crate) fn w(&mut self, s: &str) {
        if self.to_top {
            self.top.push_str(s);
            self.top.push('\n');
        } else {
            self.out.push_str(s);
            self.out.push('\n');
        }
    }

    /// Open a basic block. Every label in the generated IR goes through
    /// here, so `cur_block` is always the truth about where the next
    /// instruction lands.
    pub(crate) fn block(&mut self, name: &str) {
        self.w(&format!("{name}:"));
        self.cur_block = name.to_string();
    }

    /// Close the current block and continue in a fresh one, unless the
    /// emitter is still in `opened`. Returns the label that now holds
    /// whatever the just-emitted expression produced.
    ///
    /// This exists because a phi has to name the block its incoming value
    /// was computed in, and "the block I opened before emitting that
    /// expression" is only true when the expression stayed straight-line.
    /// `xs[i - 1] > xs[i]` does not: the two checked subtractions each open
    /// an `ovf_ok`/`ovf_done` diamond, so the comparison lands in the last
    /// of those, not in the block the caller opened. Naming the caller's
    /// block there produced IR clang rejects --
    /// `PHI node entries do not match predecessors!` -- because the named
    /// block was never a predecessor of the merge at all.
    ///
    /// The new block has exactly one predecessor, so the value dominates it
    /// and a phi naming it is well formed.
    pub(crate) fn funnel(&mut self, opened: &str) -> String {
        if self.cur_block == opened {
            return opened.to_string();
        }
        let j = self.lab("funnel");
        self.w(&format!("  br label %{j}"));
        self.block(&j);
        j
    }

    // --- entry-block allocation -------------------------------------
    //
    // Every temporary needs stack space, and the obvious place to put an
    // `alloca` is wherever the value is first needed. That is wrong inside
    // a loop: LLVM gives the allocation a fresh address per iteration and
    // only releases it when the enclosing function returns, so a call
    // inside a long loop walks off the end of the stack. A program with a
    // function call in a 35,000-iteration loop died with a stack overflow.
    //
    // Hoisting all allocas to the entry block fixes that and is also what
    // mem2reg needs to promote them to registers at all.

    /// Open an allocas frame at the current position, which must be just
    /// after the function's `entry:` label.
    pub(crate) fn begin_allocs(&mut self) {
        let at = if self.to_top {
            self.top.len()
        } else {
            self.out.len()
        };
        self.alloc_frames.push(AllocFrame {
            to_top: self.to_top,
            at,
            items: Vec::new(),
        });
    }

    /// Splice the collected allocas in and close the frame.
    pub(crate) fn end_allocs(&mut self) {
        let f = match self.alloc_frames.pop() {
            Some(f) => f,
            None => return,
        };
        if f.items.is_empty() {
            return;
        }
        let mut text = String::new();
        for (reg, ty) in &f.items {
            text.push_str(&format!("  {reg} = alloca {ty}\n"));
        }
        if f.to_top {
            self.top.insert_str(f.at, &text);
        } else {
            self.out.insert_str(f.at, &text);
        }
    }

    /// Reserve stack space of `ty`, hoisted to the current entry block.
    pub(crate) fn alloca(&mut self, ty: &str) -> String {
        let r = self.reg();
        match self.alloc_frames.last_mut() {
            Some(f) => f.items.push((r.clone(), ty.to_string())),
            // No open frame means top-level emission, which cannot happen
            // for a value; fall back to writing it in place.
            None => self.w(&format!("  {r} = alloca {ty}")),
        }
        r
    }
}
