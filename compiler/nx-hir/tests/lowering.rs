//! What lowering *decides*, asserted node by node.
//!
//! The corpus test proves everything lowers; these tests pin the
//! decisions themselves, so a change that re-derives a rule, renumbers a
//! slot, or moves a write-back fails here with a name instead of turning
//! into a subtle backend difference later.

use nx_ast::Span;
use nx_hir::*;

/// Lower a self-contained program (no imports) and hand back its entry
/// module's `<top>` body.
fn top(source: &str) -> HProgram {
    lower(source).unwrap_or_else(|e| panic!("{source}\n{e}"))
}

fn lower(source: &str) -> Result<HProgram, String> {
    nx_hir::lower::lower_source(source, std::path::Path::new("."))
        .map_err(|e| e.to_string())
}

/// The `<top>` body of a program that has no declared functions.
fn top_body(p: &HProgram) -> &Block {
    let entry = &p.modules[p.entry.0 as usize];
    &p.funcs[entry.top.0 as usize].body
}

fn stmts_of(source: &str) -> Vec<HStmt> {
    top_body(&top(source)).clone()
}

/// The single printed expression of a one-print program.
fn only_printed(source: &str) -> HExpr {
    let body = stmts_of(source);
    match body.as_slice() {
        [HStmt { kind: HStmtKind::Print { values }, .. }] => values[0].clone(),
        other => panic!("expected one print statement, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Rules: the headline first.
// ---------------------------------------------------------------------------

#[test]
fn int_arithmetic_records_trapping() {
    // The gate's headline case (hir.md §4): today the backend picks
    // between `add i64` and the overflow intrinsic by re-deriving what
    // the checker knows. Lowering records the answer instead.
    let body = stmts_of("print(1 + 2)");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Binary { rule, left, right, .. } => {
            assert_eq!(*rule, BinRule::Arith(ArithRule::Trap));
            assert_eq!(left.ty, HTy::Int);
            assert_eq!(right.ty, HTy::Int);
        }
        other => panic!("expected an addition, got {other:?}"),
    }
    assert_eq!(values[0].ty, HTy::Int);
}

#[test]
fn a_mixed_float_operand_records_promotion() {
    let body = stmts_of("print(1.0 + 2)");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Binary { rule, .. } => {
            assert_eq!(*rule, BinRule::Arith(ArithRule::PromoteFloat))
        }
        other => panic!("expected an addition, got {other:?}"),
    }
    assert_eq!(values[0].ty, HTy::Float);
}

#[test]
fn two_floats_record_plain_float_arithmetic() {
    let body = stmts_of("print(1.0 + 2.0)");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Binary { rule, .. } => assert_eq!(*rule, BinRule::Arith(ArithRule::Float)),
        other => panic!("expected an addition, got {other:?}"),
    }
}

#[test]
fn integer_power_records_saturation_not_trapping() {
    let body = stmts_of("print(2 ** 8)");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Binary { rule, .. } => assert_eq!(*rule, BinRule::Pow(PowRule::Saturate)),
        other => panic!("expected a power, got {other:?}"),
    }
}

#[test]
fn string_concatenation_is_its_own_rule() {
    let body = stmts_of("print(\"a\" + \"b\")");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Binary { rule, .. } => assert_eq!(*rule, BinRule::Concat),
        other => panic!("expected a concatenation, got {other:?}"),
    }
    assert_eq!(values[0].ty, HTy::Str);
}

#[test]
fn membership_is_decided_from_the_right_hand_side() {
    // `"" in s` is true by construction: the rule comes from the
    // haystack, and an empty needle is a runtime fact, not a spelling.
    for (src, want) in [
        ("print(1 in [1, 2])", MemberRule::ListEq),
        ("print(1 in \"ab\")", MemberRule::StrSub),
        ("print(1 in {\"a\": 1})", MemberRule::DictKey),
    ] {
        let body = stmts_of(src);
        let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print in {src}") };
        match &values[0].kind {
            HExprKind::Contains { rule, .. } => assert_eq!(*rule, want, "in {src}"),
            other => panic!("expected a membership test in {src}, got {other:?}"),
        }
    }
}

#[test]
fn indexing_a_string_records_the_character_rule() {
    let body = stmts_of("print(\"abc\"[0])");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Index { rule, .. } => assert_eq!(*rule, IndexRule::StrChar),
        other => panic!("expected an index, got {other:?}"),
    }
    assert_eq!(values[0].ty, HTy::Str);
}

#[test]
fn equality_and_ordering_are_separate_relations() {
    let body = stmts_of("print(1 == 2)\nprint(\"a\" < \"b\")");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Equal { rule, .. } => assert_eq!(*rule, EqRule::Numeric),
        other => panic!("expected an equality, got {other:?}"),
    }
    let HStmtKind::Print { values } = &body[1].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Compare { rule, .. } => assert_eq!(*rule, CmpRule::StrOrder),
        other => panic!("expected a comparison, got {other:?}"),
    }
}

#[test]
fn and_or_are_short_circuit_nodes() {
    let body = stmts_of("print(true and false)");
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    assert!(matches!(values[0].kind, HExprKind::Logic { .. }));
}

// ---------------------------------------------------------------------------
// Slots.
// ---------------------------------------------------------------------------

#[test]
fn slots_are_parameters_first_then_first_bind_order() {
    let p = top("fn f(a, b):\n    c = a + b\n    d = c * 2\n    return d\nprint(f(1, 2))");
    let entry = &p.modules[p.entry.0 as usize];
    let fid = entry.funcs[0];
    let f = &p.funcs[fid.0 as usize];
    assert_eq!(
        f.params.iter().map(|(s, _)| s.0).collect::<Vec<_>>(),
        vec![0, 1],
        "parameters keep declaration order"
    );
    let mut seen: Vec<u32> = Vec::new();
    collect_slots(&f.body, &mut seen);
    // `c` binds before `d`, so the locals read as 2 then 3 -- after the
    // two parameters.
    assert_eq!(seen, vec![2, 3, 3], "locals follow source order of first binding");
}

fn collect_slots(b: &Block, out: &mut Vec<u32>) {
    for s in b {
        match &s.kind {
            HStmtKind::Assign { targets, .. } => {
                for t in targets {
                    if let HTarget::Slot(slot) = t {
                        out.push(slot.0);
                    }
                }
            }
            HStmtKind::AssignOp { target: HTarget::Slot(slot), .. } => out.push(slot.0),
            HStmtKind::Return { values } => {
                for v in values {
                    walk_slots_expr(v, out);
                }
            }
            HStmtKind::Expr(e) => walk_slots_expr(e, out),
            HStmtKind::ForEach { body, .. } | HStmtKind::ForRange { body, .. } => {
                collect_slots(body, out)
            }
            _ => {}
        }
    }
}

fn walk_slots_expr(e: &HExpr, out: &mut Vec<u32>) {
    match &e.kind {
        HExprKind::Place(Place::Slot(s)) => out.push(s.0),
        HExprKind::Binary { left, right, .. }
        | HExprKind::Equal { left, right, .. }
        | HExprKind::Compare { left, right, .. }
        | HExprKind::Logic { left, right, .. } => {
            walk_slots_expr(left, out);
            walk_slots_expr(right, out);
        }
        _ => {}
    }
}

#[test]
fn self_is_slot_zero_of_a_method() {
    let p = top("type P:\n    x: Int\n\nimpl P:\n    fn get(self):\n        return self.x\n\np = P(1)\nprint(p.get())");
    let method = &p.methods[0];
    assert_eq!(method.receiver, Some(ReceiverKind::Read));
    let body = &p.funcs[method.func.0 as usize];
    assert_eq!(body.params[0].0, Slot(0), "`self` is slot 0");
    assert_eq!(body.params[0].1, HTy::Record(TypeId(0)));
}

#[test]
fn an_associated_function_takes_no_receiver_slot() {
    let p = top("type P:\n    x: Int\n\nimpl P:\n    fn make(v):\n        return P(v)\n\nprint(P.make(3))");
    let method = &p.methods[0];
    assert_eq!(method.receiver, None, "an associated function has no receiver");
    let body = &p.funcs[method.func.0 as usize];
    assert_eq!(body.params[0].0, Slot(0), "`v` is slot 0 -- there is no `self` slot");
}

// ---------------------------------------------------------------------------
// Storage: globals, copy discipline, `del`, write-back.
// ---------------------------------------------------------------------------

#[test]
fn module_level_names_are_globals_and_function_locals_are_slots() {
    let p = top("g = 7\nfn f():\n    l = 1\n    return l\nprint(f())");
    let entry = &p.modules[p.entry.0 as usize];
    assert_eq!(entry.globals.len(), 1, "only the top-level name is a global");
    assert_eq!(
        entry.globals[0].0 as usize,
        p.globals.iter().position(|g| g.diag.name.as_deref() == Some("g")).expect("g"),
    );
    let f = &p.funcs[entry.funcs[0].0 as usize];
    assert_eq!(f.params.len(), 0);
}

#[test]
fn copy_rules_follow_value_semantics() {
    let p = top("a = 1\nb = \"s\"\nc = [1]\nd = {\"k\": 1}\ntype P:\n    x: Int\ne = P(1)\n");
    let rules: Vec<CopyRule> = top_body(&p)
        .iter()
        .filter_map(|s| match &s.kind {
            HStmtKind::Assign { rules, .. } => rules.first().copied(),
            _ => None,
        })
        .collect();
    assert_eq!(
        rules,
        vec![
            CopyRule::CopyScalar,
            CopyRule::ShareStr,
            CopyRule::DeepClone,
            CopyRule::DeepClone,
            CopyRule::DeepClone,
        ]
    );
}

#[test]
fn del_records_the_rule_for_each_target_shape() {
    let p = top("xs = [1, 2]\nds = {\"a\": 1}\ntype P:\n    x: Int\nq = P(1)\ndel xs[0]\ndel ds[\"a\"]\ndel q.x\nn = 1\ndel n\n");
    let rules: Vec<DelRule> = top_body(&p)
        .iter()
        .filter_map(|s| match &s.kind {
            HStmtKind::Del { targets } => Some(targets[0].rule),
            _ => None,
        })
        .collect();
    assert_eq!(
        rules,
        vec![DelRule::ListRemove, DelRule::DictRemove, DelRule::RecordBlank, DelRule::Unbind]
    );
}

#[test]
fn mut_self_writes_back_only_where_the_receiver_has_storage() {
    let src = "type P:\n    x: Int\n\nimpl P:\n    fn bump(mut self):\n        self.x = self.x + 1\n        return self\n\np = P(1)\np.bump()\nprint(P(9).bump())\n";
    let p = top(src);
    let writebacks: Vec<bool> = top_body(&p)
        .iter()
        .filter_map(|s| match &s.kind {
            HStmtKind::Expr(e) => match &e.kind {
                HExprKind::CallMethod { writeback, .. } => Some(writeback.is_some()),
                _ => None,
            },
            HStmtKind::Print { values } => match &values[0].kind {
                HExprKind::CallMethod { writeback, .. } => Some(writeback.is_some()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        writebacks,
        vec![true, false],
        "a name receiver writes back; a temporary has nowhere to write"
    );
}

#[test]
fn an_associated_call_has_no_receiver_to_evaluate() {
    let p = top("type P:\n    x: Int\n\nimpl P:\n    fn make(v):\n        return P(v)\n\nprint(P.make(3))");
    let body = top_body(&p);
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::CallMethod { receiver, .. } => {
            assert!(receiver.is_none(), "a type name is not a value to evaluate")
        }
        other => panic!("expected a method call, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Structure that lowering must preserve.
// ---------------------------------------------------------------------------

#[test]
fn every_right_hand_side_evaluates_before_any_store() {
    // `a, b = b, a` swaps, so the swap is structural: both values come
    // before both stores.
    let p = top("a = 1\nb = 2\na, b = b, a");
    let body = top_body(&p);
    let last = body.last().expect("a statement");
    let HStmtKind::Assign { targets, values, .. } = &last.kind else { panic!("expected an assign") };
    assert_eq!(targets.len(), 2);
    assert_eq!(values.len(), 2);
    assert!(matches!(values[0].kind, HExprKind::Place(_)));
}

#[test]
fn a_from_imported_value_becomes_an_eager_assignment() {
    // A lazy read would see later mutations; the backend snapshots into
    // a fresh local, so HIR says so structurally. `utils.VERSION` is a
    // module-level value, so importing it must materialize storage.
    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("examples")
        .join("modules");
    let p = nx_hir::lower::lower_source("from utils import VERSION as v\nprint(v)", &base)
        .expect("the imported value lowers");
    let body = top_body(&p);
    let HStmtKind::Assign { targets, values, .. } = &body[1].kind else {
        panic!("expected the import to materialize an assign, got {:?}", body[1])
    };
    assert!(matches!(values[0].kind, HExprKind::Place(Place::Global(_))));
    assert!(matches!(targets[0], HTarget::Slot(_)));
    assert!(
        matches!(body[0].kind, HStmtKind::EnsureInit { .. }),
        "the import is marked first"
    );
}

#[test]
fn a_comprehension_over_a_string_is_a_string() {
    let e = only_printed("print([c for c in \"abc\"])");
    assert_eq!(e.ty, HTy::Str);
    match &e.kind {
        HExprKind::Compr { rule, .. } => assert_eq!(*rule, IterRule::StrChars),
        other => panic!("expected a comprehension, got {other:?}"),
    }
}

#[test]
fn a_comprehension_over_a_list_is_a_list() {
    let e = only_printed("print([i * 2 for i in [1, 2, 3]])");
    assert_eq!(e.ty, HTy::List(Box::new(HTy::Int)));
}

#[test]
fn a_range_in_value_position_is_an_ascending_list() {
    let e = only_printed("print(0..3)");
    assert_eq!(e.ty, HTy::List(Box::new(HTy::Int)));
    match &e.kind {
        HExprKind::Range { rule, .. } => assert_eq!(*rule, RangeRule::AscendingOrEmpty),
        other => panic!("expected a range, got {other:?}"),
    }
}

#[test]
fn builtin_sugar_becomes_the_direct_call() {
    let p = top("xs = [1]\nprint(len(xs))");
    let body = top_body(&p);
    let HStmtKind::Print { values } = &body[1].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Builtin { op, .. } => assert_eq!(*op, BuiltinOp::Len),
        other => panic!("expected a builtin, got {other:?}"),
    }

    // Sugar form: `xs.push(1)` is `push(xs, 1)`, so the base becomes the
    // first argument of the same node.
    let p = top("xs = [1]\nxs.push(2)");
    let body = top_body(&p);
    let HStmtKind::Expr(e) = &body[1].kind else { panic!("expected an expression") };
    match &e.kind {
        HExprKind::Builtin { op, args } => {
            assert_eq!(*op, BuiltinOp::Push);
            assert_eq!(args.len(), 2);
            assert!(matches!(args[0].kind, HExprKind::Place(_)), "push targets the variable");
        }
        other => panic!("expected a builtin, got {other:?}"),
    }
}

#[test]
fn int_and_float_lower_to_conversion_builtins() {
    // The checker owns which types arrive; lowering only records the
    // operation and its answer type.
    let p = top("print(int(\"42\"))\nprint(float(2))\n");
    let body = top_body(&p);
    let HStmtKind::Print { values } = &body[0].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Builtin { op, args } => {
            assert_eq!(*op, BuiltinOp::ToInt);
            assert_eq!(args.len(), 1);
        }
        other => panic!("expected a builtin, got {other:?}"),
    }
    assert_eq!(values[0].ty, HTy::Int);
    let HStmtKind::Print { values } = &body[1].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Builtin { op, args } => {
            assert_eq!(*op, BuiltinOp::ToFloat);
            assert_eq!(args.len(), 1);
        }
        other => panic!("expected a builtin, got {other:?}"),
    }
    assert_eq!(values[0].ty, HTy::Float);
}

#[test]
fn a_field_of_a_known_record_is_a_constant_offset() {
    let p = top("type P:\n    x: Int\n    y: Int\nq = P(1, 2)\nprint(q.y)");
    let body = top_body(&p);
    let HStmtKind::Print { values } = &body[1].kind else { panic!("expected a print") };
    match &values[0].kind {
        HExprKind::Field { field, .. } => assert_eq!(*field, FieldRef::Static(FieldIdx(1))),
        other => panic!("expected a field read, got {other:?}"),
    }
}

#[test]
fn an_unknown_base_keeps_an_interned_runtime_name() {
    let p = top("fn f(q):\n    return q.x\n");
    let e = {
        let entry = &p.modules[p.entry.0 as usize];
        let f = &p.funcs[entry.funcs[0].0 as usize];
        let HStmtKind::Return { values } = &f.body[0].kind else { panic!("expected a return") };
        values[0].clone()
    };
    match &e.kind {
        HExprKind::Field { field, base, .. } => {
            assert_eq!(base.ty, HTy::Unknown, "an unannotated parameter is unknown");
            let FieldRef::Dynamic(id) = field else { panic!("expected a runtime name") };
            assert_eq!(p.strings[id.0 as usize], "x");
        }
        other => panic!("expected a field read, got {other:?}"),
    }
}

#[test]
fn dynamic_field_names_are_interned_once_for_the_program() {
    let p = top("fn f(q):\n    return q.x + q.x\n");
    let entry = &p.modules[p.entry.0 as usize];
    let f = &p.funcs[entry.funcs[0].0 as usize];
    let HStmtKind::Return { values } = &f.body[0].kind else { panic!("expected a return") };
    let mut names = Vec::new();
    collect_dynamic_names(&values[0], &mut names);
    assert_eq!(names.len(), 2, "both reads are interned");
    assert_eq!(p.strings.len(), 1, "the same name is interned once");
}

fn collect_dynamic_names(e: &HExpr, out: &mut Vec<u32>) {
    match &e.kind {
        HExprKind::Field { base, field } => {
            if let FieldRef::Dynamic(id) = field {
                out.push(id.0);
            }
            collect_dynamic_names(base, out);
        }
        HExprKind::Binary { left, right, .. } => {
            collect_dynamic_names(left, out);
            collect_dynamic_names(right, out);
        }
        _ => {}
    }
}

#[test]
fn lowering_is_deterministic_across_runs() {
    let src = "type P:\n    x: Int\n\nimpl P:\n    fn moved(mut self, d):\n        self.x = self.x + d\n        return self\n\np = P(1)\np.moved(2)\nprint(p.x)\n";
    let a = top(src);
    let b = top(src);
    assert_eq!(a, b, "two lowerings of one program are identical");
    assert_eq!(dump::dump(&a), dump::dump(&b));
}

#[test]
fn spans_survive_so_diagnostics_can_point_at_the_source() {
    let p = top("x = 1\ny = 2\n");
    let body = top_body(&p);
    assert_eq!(body[0].span, Span { line: 1, col: 1 });
    assert_eq!(body[1].span, Span { line: 2, col: 1 });
}