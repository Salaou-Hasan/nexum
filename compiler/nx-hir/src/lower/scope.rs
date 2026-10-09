//! Function scope: slots, name resolution, and type queries.

use super::tables::{diag, no_diag, Ctx, NameRef};
use super::ty::{conv_ty, conv_ty_str, hty_compatible, hty_numeric};
use super::{lerr, LResult};
use crate::model::*;
use nx_ast::{BinOp, Expr, Span, UnaryOp};
use nx_types::{FnInfo, Ty};
use std::collections::HashMap;

pub(crate) struct FnLower<'a> {
    pub(crate) ctx: &'a Ctx<'a>,
    pub(crate) module: String,
    pub(crate) info: FnInfo,
    pub(crate) strings: &'a mut Vec<String>,
    pub(crate) string_ids: &'a mut HashMap<String, StrId>,
    pub(crate) slots: Vec<HTy>,
    pub(crate) env: HashMap<String, NameRef>,
    /// True for module tops: plain names bind globals, not slots.
    pub(crate) is_top: bool,
}

impl<'a> FnLower<'a> {
    pub(crate) fn alloc_slot(&mut self, ty: HTy) -> Slot {
        let s = Slot(self.slots.len() as u32);
        self.slots.push(ty);
        s
    }

    pub(crate) fn slot_ty(&self, s: Slot) -> HTy {
        self.slots[s.0 as usize].clone()
    }

    /// A bound name's static type from inference. Every bound name is in
    /// `locals` on checked programs, with one exception: `del` erases
    /// the name (as the checker does), so a name deleted and never
    /// rebound is genuinely unresolved. That case answers `Unknown`,
    /// which is what the checker knows; anything else is an internal
    /// error caught by the debug assertion, never a silent `Unknown`.
    pub(crate) fn local_ty(&self, name: &str, span: Span) -> LResult<HTy> {
        let ty = match self.info.locals.get(name) {
            Some(t) => t.clone(),
            None => {
                debug_assert!(
                    false,
                    "internal: '{name}' has no inferred type at {}:{}",
                    span.line, span.col
                );
                return Ok(HTy::Unknown);
            }
        };
        conv_ty(self.ctx.tables, &ty, &self.module, span)
    }

    pub(crate) fn global_ty(&self, g: GlobalId) -> HTy {
        self.ctx.global_tys.get(&g).cloned().unwrap_or(HTy::Unknown)
    }

    pub(crate) fn stmt(&self, span: Span, diag_name: Option<&str>, kind: HStmtKind) -> HStmt {
        HStmt {
            span,
            diag: diag_name.map(diag).unwrap_or_else(no_diag),
            kind,
        }
    }

    pub(crate) fn expr(
        &self,
        span: Span,
        diag_name: Option<&str>,
        ty: HTy,
        kind: HExprKind,
    ) -> HExpr {
        HExpr {
            span,
            diag: diag_name.map(diag).unwrap_or_else(no_diag),
            ty,
            kind,
        }
    }

    pub(crate) fn intern(&mut self, name: &str) -> StrId {
        if let Some(id) = self.string_ids.get(name) {
            return *id;
        }
        let id = StrId(self.strings.len() as u32);
        self.strings.push(name.to_string());
        self.string_ids.insert(name.to_string(), id);
        id
    }

    /// A field of a value: constant offset for known records, interned
    /// runtime name for dynamic bases. Anything else was rejected by
    /// the checker.
    pub(crate) fn field_ref(&mut self, base: &HTy, field: &str, span: Span) -> LResult<FieldRef> {
        match base {
            HTy::Record(tid) => {
                let names: Vec<String> = self
                    .ctx
                    .tables
                    .type_decls
                    .get(tid.0 as usize)
                    .map(|d| d.fields.iter().map(|(n, _)| n.clone()).collect())
                    .unwrap_or_default();
                match names.iter().position(|f| f == field) {
                    Some(i) => Ok(FieldRef::Static(FieldIdx(i))),
                    None => Err(lerr(span, format!("internal: no field '{field}'"))),
                }
            }
            HTy::Unknown => {
                let id = self.intern(field);
                Ok(FieldRef::Dynamic(id))
            }
            other => Err(lerr(span, format!("internal: no fields on {other:?}"))),
        }
    }

    /// A field value's type: layout lookup for static offsets, dynamic
    /// for runtime-resolved names.
    pub(crate) fn field_ty_of(&self, base: &HTy, field: FieldRef, span: Span) -> LResult<HTy> {
        match (base, field) {
            (HTy::Record(tid), FieldRef::Static(idx)) => self.lower_field_ty(*tid, idx, span),
            (_, FieldRef::Dynamic(_)) => Ok(HTy::Unknown),
            (other, _) => Err(lerr(span, format!("internal: no fields on {other:?}"))),
        }
    }

    pub(crate) fn lower_field_ty(&self, tid: TypeId, idx: FieldIdx, span: Span) -> LResult<HTy> {
        let decl = self
            .ctx
            .tables
            .type_decls
            .get(tid.0 as usize)
            .ok_or_else(|| lerr(span, format!("internal: no such type id {}", tid.0)))?;
        let (_, spelling) = decl.fields.get(idx.0).ok_or_else(|| {
            lerr(
                span,
                format!("internal: field index {} out of range", idx.0),
            )
        })?;
        conv_ty_str(self.ctx.tables, spelling, &decl.module.clone())
    }

    /// Element type of an index read, mirroring the checker.
    pub(crate) fn index_elem_ty(&self, base: &HTy, span: Span) -> LResult<HTy> {
        match base {
            HTy::List(t) => Ok((**t).clone()),
            HTy::Str => Ok(HTy::Str),
            HTy::Dict(_) | HTy::Unknown => Ok(HTy::Unknown),
            other => Err(lerr(span, format!("internal: cannot index into {other:?}"))),
        }
    }

    pub(crate) fn index_rule(&self, base: &HTy, span: Span) -> LResult<IndexRule> {
        match base {
            HTy::List(_) => Ok(IndexRule::ListInt),
            HTy::Str => Ok(IndexRule::StrChar),
            HTy::Dict(_) => Ok(IndexRule::DictKey),
            HTy::Unknown => Ok(IndexRule::Dynamic),
            other => Err(lerr(span, format!("internal: cannot index into {other:?}"))),
        }
    }

    pub(crate) fn slice_rule(&self, base: &HTy, span: Span) -> LResult<SliceRule> {
        match base {
            HTy::List(_) => Ok(SliceRule::ListCopy),
            HTy::Str => Ok(SliceRule::StrChars),
            HTy::Unknown => Ok(SliceRule::Dynamic),
            other => Err(lerr(span, format!("internal: cannot slice {other:?}"))),
        }
    }

    pub(crate) fn iter_rule(&self, it: &HTy, span: Span) -> LResult<IterRule> {
        match it {
            HTy::List(_) => Ok(IterRule::List),
            HTy::Str => Ok(IterRule::StrChars),
            HTy::Dict(_) => Ok(IterRule::DictKeys),
            HTy::Unknown => Ok(IterRule::Dynamic),
            other => Err(lerr(
                span,
                format!("internal: cannot iterate over {other:?}"),
            )),
        }
    }

    pub(crate) fn iter_elem_ty(&self, it: &HTy, span: Span) -> LResult<HTy> {
        match it {
            HTy::List(t) => Ok((**t).clone()),
            HTy::Str => Ok(HTy::Str),
            HTy::Dict(_) | HTy::Unknown => Ok(HTy::Unknown),
            other => Err(lerr(
                span,
                format!("internal: cannot iterate over {other:?}"),
            )),
        }
    }

    pub(crate) fn member_rule(&self, hay: &HTy, span: Span) -> LResult<MemberRule> {
        match hay {
            HTy::List(_) => Ok(MemberRule::ListEq),
            HTy::Str => Ok(MemberRule::StrSub),
            HTy::Dict(_) => Ok(MemberRule::DictKey),
            HTy::Unknown => Ok(MemberRule::Dynamic),
            other => Err(lerr(
                span,
                format!("internal: cannot test membership in {other:?}"),
            )),
        }
    }

    pub(crate) fn eq_rule(&self, l: &HTy, r: &HTy, span: Span) -> LResult<EqRule> {
        if matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown) {
            return Ok(EqRule::Dynamic);
        }
        if hty_numeric(l) && hty_numeric(r) {
            return Ok(EqRule::Numeric);
        }
        match (l, r) {
            (HTy::Str, HTy::Str) => Ok(EqRule::StrEq),
            (HTy::List(_), HTy::List(_))
            | (HTy::Dict(_), HTy::Dict(_))
            | (HTy::Record(_), HTy::Record(_)) => Ok(EqRule::Structural),
            (HTy::None, HTy::None) => Ok(EqRule::IdentityNone),
            _ => Err(lerr(
                span,
                format!("internal: cannot compare {l:?} and {r:?}"),
            )),
        }
    }

    pub(crate) fn cmp_rule(&self, l: &HTy, r: &HTy, span: Span) -> LResult<CmpRule> {
        if matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown) {
            return Ok(CmpRule::Dynamic);
        }
        if hty_numeric(l) && hty_numeric(r) {
            return Ok(CmpRule::Numeric);
        }
        match (l, r) {
            (HTy::Str, HTy::Str) => Ok(CmpRule::StrOrder),
            _ => Err(lerr(
                span,
                format!("internal: cannot order {l:?} and {r:?}"),
            )),
        }
    }

    pub(crate) fn unary_rule(&self, op: UnaryOp, operand: &HTy, span: Span) -> LResult<UnaryRule> {
        match (op, operand) {
            (UnaryOp::Neg, HTy::Int) => Ok(UnaryRule::Neg(ArithRule::Trap)),
            (UnaryOp::Neg, HTy::Float) => Ok(UnaryRule::Neg(ArithRule::Float)),
            (UnaryOp::Not, HTy::Bool) => Ok(UnaryRule::Not),
            (UnaryOp::BitNot, HTy::Int) => Ok(UnaryRule::BitNot),
            (UnaryOp::Pos, HTy::Int) | (UnaryOp::Pos, HTy::Float) => Ok(UnaryRule::Pos),
            (_, HTy::Unknown) => Ok(UnaryRule::Dynamic),
            _ => Err(lerr(
                span,
                format!("internal: '{op:?}' not supported for {operand:?}"),
            )),
        }
    }

    /// If-expression join, mirroring the checker exactly: compatible
    /// sides keep the known one, `None` on either side is the optional
    /// idiom, anything else was already rejected.
    pub(crate) fn join_ty(&self, t: &HTy, e: &HTy, span: Span) -> LResult<HTy> {
        if hty_compatible(t, e) {
            if matches!(t, HTy::Unknown) {
                return Ok(e.clone());
            }
            return Ok(t.clone());
        }
        if matches!(t, HTy::None) {
            return Ok(e.clone());
        }
        if matches!(e, HTy::None) {
            return Ok(t.clone());
        }
        Err(lerr(
            span,
            format!("internal: branches disagree: {t:?} vs {e:?}"),
        ))
    }

    /// A bare variable read. Resolution mirrors the checker's
    /// `check_expr` order for values: locals, then globals, then import
    /// aliases. Function, type and module aliases are callable,
    /// constructible or callable-through -- never values -- so reaching
    /// one here means the checker already rejected the program.
    pub(crate) fn lower_var(&mut self, name: &str, span: Span) -> LResult<HExpr> {
        if let Some(r) = self.env.get(name).cloned() {
            match r {
                NameRef::Slot(s) => {
                    let ty = self.slot_ty(s);
                    return Ok(self.expr(span, Some(name), ty, HExprKind::Place(Place::Slot(s))));
                }
                NameRef::Global(g) => {
                    let ty = self.global_ty(g);
                    return Ok(self.expr(span, Some(name), ty, HExprKind::Place(Place::Global(g))));
                }
                _ => {
                    return Err(lerr(span, format!("internal: '{name}' is not a value")));
                }
            }
        }
        // Globals bound before any use in this body are pre-declared;
        // anything else never reaches lowering through checked code.
        if let Some(g) = self
            .ctx
            .global_ids
            .get(&(self.module.clone(), name.to_string()))
        {
            let ty = self.global_ty(*g);
            self.env.insert(name.to_string(), NameRef::Global(*g));
            return Ok(self.expr(span, Some(name), ty, HExprKind::Place(Place::Global(*g))));
        }
        Err(lerr(span, format!("internal: undefined variable '{name}'")))
    }

    /// A method on a record type visible here: the type's own impls
    /// (the orphan rule puts every impl with its type), which covers
    /// from-imported types too -- their declarations traveled with them.
    /// Anything missing was rejected by the checker.
    pub(crate) fn resolve_method(
        &self,
        tid: TypeId,
        method: &str,
        span: Span,
    ) -> LResult<MethodId> {
        self.ctx
            .tables
            .method_id
            .get(&(tid, method.to_string()))
            .copied()
            .ok_or_else(|| lerr(span, format!("internal: no such method '{method}'")))
    }

    /// An [`HTy`] back to the checker's spelling, so operator result
    /// types come from `nx_types::arith_result` -- the single owner of
    /// Int/Float promotion -- instead of a second matrix here. Records
    /// become their canonical declared name, which is what the checker's
    /// `Ty::Record` holds.
    pub(crate) fn to_ty(&self, h: &HTy) -> Ty {
        match h {
            HTy::Int => Ty::Int,
            HTy::Float => Ty::Float,
            HTy::Bool => Ty::Bool,
            HTy::Str => Ty::Str,
            HTy::None => Ty::None,
            HTy::Unknown => Ty::Unknown,
            HTy::List(t) => Ty::List(Box::new(self.to_ty(t))),
            HTy::Dict(t) => Ty::Dict(Box::new(self.to_ty(t))),
            HTy::Record(tid) => Ty::Record(self.type_name(*tid)),
        }
    }

    /// The declared (canonical) name of a record type.
    pub(crate) fn type_name(&self, tid: TypeId) -> String {
        self.ctx
            .tables
            .type_decls
            .get(tid.0 as usize)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("type#{}", tid.0))
    }

    /// Result type of an arithmetic, power or bitwise operator. The
    /// matrix is the checker's; only the bitwise arm is spelled out,
    /// because the checker answers `Int` for it regardless of unknown
    /// operands (shifts and bit operators never widen).
    pub(crate) fn bin_result_ty(&self, op: BinOp, l: &HTy, r: &HTy, span: Span) -> LResult<HTy> {
        match op {
            BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => Ok(HTy::Int),
            _ => {
                let lt = self.to_ty(l);
                let rt = self.to_ty(r);
                match nx_types::arith_result(&lt, op, &rt) {
                    Some(t) => conv_ty(self.ctx.tables, &t, &self.module, span),
                    None => Err(lerr(
                        span,
                        format!("internal: no result type for '{op:?}' on {l:?} and {r:?}"),
                    )),
                }
            }
        }
    }

    /// Element type of a slice: a fresh list for a list base,
    /// characters for a string base, dynamic otherwise.
    pub(crate) fn slice_ty(&self, base: &HTy, span: Span) -> LResult<HTy> {
        match base {
            HTy::List(t) => Ok(HTy::List(t.clone())),
            HTy::Str => Ok(HTy::Str),
            HTy::Unknown => Ok(HTy::Unknown),
            other => Err(lerr(span, format!("internal: cannot slice {other:?}"))),
        }
    }

    pub(crate) fn opt_expr(&mut self, e: &Option<Box<Expr>>) -> LResult<Option<Box<HExpr>>> {
        match e {
            Some(x) => Ok(Some(Box::new(self.lower_expr(x)?))),
            None => Ok(None),
        }
    }
}
