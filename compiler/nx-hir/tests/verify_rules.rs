//! One negative test per verifier rule, plus the positive control.
//!
//! Each test builds well-formed HIR, breaks exactly one thing, and
//! asserts the rule that catches it. They are the reason HIR nodes are
//! plain data: if malformed HIR could not be built here, these tests
//! could not exist.

use nx_ast::{BinOp, Span, UnaryOp};
use nx_hir::*;

fn sp() -> Span {
    Span { line: 1, col: 1 }
}

fn int_e() -> HExpr {
    HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Int,
        kind: HExprKind::Int(1),
    }
}

fn bool_e() -> HExpr {
    HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Bool,
        kind: HExprKind::Bool(true),
    }
}

fn str_e() -> HExpr {
    HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Str,
        kind: HExprKind::Str("x".to_string()),
    }
}

fn float_e() -> HExpr {
    HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Float,
        kind: HExprKind::Float(1.0),
    }
}

fn slot_e(s: Slot, ty: HTy) -> HExpr {
    HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty,
        kind: HExprKind::Place(Place::Slot(s)),
    }
}

fn ret_stmt(v: HExpr) -> HStmt {
    HStmt {
        span: sp(),
        diag: DiagInfo::default(),
        kind: HStmtKind::Return { values: vec![v] },
    }
}

/// A minimal program that verifies:
///
/// ```text
/// type Point  { x: Int, y: Int }
/// type Bag    { n: Int }
/// impl Point { fn moved(self) -> Int { return self.x } }
/// <top>      {}
/// ```
fn base() -> HProgram {
    let field = |base: Box<HExpr>, idx: usize| HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Int,
        kind: HExprKind::Field {
            base,
            field: FieldRef::Static(FieldIdx(idx)),
        },
    };
    HProgram {
        modules: vec![HModule {
            diag: DiagInfo::named("m"),
            globals: Vec::new(),
            types: vec![TypeId(0), TypeId(1)],
            funcs: vec![FuncId(0), FuncId(1)],
            top: FuncId(1),
        }],
        types: vec![
            HType {
                module: ModuleId(0),
                fields: vec![HTy::Int, HTy::Int],
                diag: DiagInfo::named("Point"),
            },
            HType {
                module: ModuleId(0),
                fields: vec![HTy::Int],
                diag: DiagInfo::named("Bag"),
            },
        ],
        methods: vec![HMethod {
            type_id: TypeId(0),
            func: FuncId(0),
            receiver: Some(ReceiverKind::Read),
            diag: DiagInfo::named("moved"),
        }],
        funcs: vec![
            HFunc {
                params: vec![(Slot(0), HTy::Record(TypeId(0)))],
                ret: HTy::Int,
                body: vec![ret_stmt(field(
                    Box::new(slot_e(Slot(0), HTy::Record(TypeId(0)))),
                    0,
                ))],
                diag: DiagInfo::default(),
            },
            HFunc {
                params: Vec::new(),
                ret: HTy::None,
                body: Vec::new(),
                diag: DiagInfo::named("<top>"),
            },
        ],
        globals: Vec::new(),
        strings: Vec::new(),
        entry: ModuleId(0),
    }
}

fn rules_broken(p: &HProgram) -> Vec<String> {
    match verify::verify(p) {
        Ok(()) => Vec::new(),
        Err(v) => v.into_iter().map(|x| x.rule.to_string()).collect(),
    }
}

fn assert_rejects(p: &HProgram, rule: &str) {
    let broken = rules_broken(p);
    assert!(
        broken.iter().any(|r| r == rule),
        "expected {rule} to reject, got {broken:?}"
    );
}

fn assert_accepts(p: &HProgram) {
    let broken = rules_broken(p);
    assert!(broken.is_empty(), "expected no violations, got {broken:?}");
}

#[test]
fn the_base_program_verifies() {
    assert_accepts(&base());
}

#[test]
fn v1_rejects_a_read_of_a_never_bound_slot() {
    let mut p = base();
    p.funcs[1].body = vec![ret_stmt(slot_e(Slot(5), HTy::Int))];
    assert_rejects(&p, "V1");
}

#[test]
fn v2_rejects_an_id_outside_its_table() {
    let mut p = base();
    // A type id with two types in the table.
    p.funcs[1].body = vec![ret_stmt(HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Record(TypeId(99)),
        kind: HExprKind::Construct {
            type_id: TypeId(99),
            args: Vec::new(),
        },
    })];
    assert_rejects(&p, "V2");
}

#[test]
fn v3_rejects_trapping_arithmetic_on_a_float_operand() {
    let mut p = base();
    p.funcs[1].body = vec![ret_stmt(HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Float,
        kind: HExprKind::Binary {
            left: Box::new(int_e()),
            op: BinOp::Add,
            rule: BinRule::Arith(ArithRule::Trap),
            right: Box::new(float_e()),
        },
    })];
    assert_rejects(&p, "V3");
}

#[test]
fn v4_rejects_a_non_bool_condition() {
    let mut p = base();
    p.funcs[1].ret = HTy::None;
    p.funcs[1].body = vec![HStmt {
        span: sp(),
        diag: DiagInfo::default(),
        kind: HStmtKind::If {
            cond: int_e(),
            then_body: Vec::new(),
            elifs: Vec::new(),
            else_body: None,
        },
    }];
    assert_rejects(&p, "V4");
}

#[test]
fn v5_rejects_a_field_index_past_the_layout() {
    let mut p = base();
    p.funcs[1].ret = HTy::Int;
    p.funcs[1].body = vec![ret_stmt(HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Int,
        kind: HExprKind::Field {
            base: Box::new(HExpr {
                span: sp(),
                diag: DiagInfo::default(),
                ty: HTy::Record(TypeId(0)),
                kind: HExprKind::Construct {
                    type_id: TypeId(0),
                    args: Vec::new(),
                },
            }),
            field: FieldRef::Static(FieldIdx(7)),
        },
    })];
    assert_rejects(&p, "V5");
}

#[test]
fn v6_rejects_a_method_call_on_another_type() {
    let mut p = base();
    p.funcs[1].ret = HTy::Int;
    p.funcs[1].body = vec![ret_stmt(HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Int,
        kind: HExprKind::CallMethod {
            method: MethodId(0),
            receiver: Some(Box::new(HExpr {
                span: sp(),
                diag: DiagInfo::default(),
                ty: HTy::Record(TypeId(1)),
                kind: HExprKind::Construct {
                    type_id: TypeId(1),
                    args: vec![int_e()],
                },
            })),
            args: Vec::new(),
            writeback: None,
        },
    })];
    assert_rejects(&p, "V6");
}

#[test]
fn v7_rejects_a_partial_constructor() {
    let mut p = base();
    p.funcs[1].ret = HTy::None;
    p.funcs[1].body = vec![HStmt {
        span: sp(),
        diag: DiagInfo::default(),
        kind: HStmtKind::Expr(HExpr {
            span: sp(),
            diag: DiagInfo::default(),
            ty: HTy::Record(TypeId(0)),
            // Two fields, one argument.
            kind: HExprKind::Construct {
                type_id: TypeId(0),
                args: vec![int_e()],
            },
        }),
    }];
    assert_rejects(&p, "V7");
}

#[test]
fn v8_rejects_a_return_that_does_not_match() {
    let mut p = base();
    p.funcs[0].ret = HTy::Str;
    p.funcs[0].body = vec![ret_stmt(int_e())];
    assert_rejects(&p, "V8");
}

#[test]
fn v9_rejects_break_outside_a_loop() {
    let mut p = base();
    p.funcs[1].body = vec![HStmt {
        span: sp(),
        diag: DiagInfo::default(),
        kind: HStmtKind::Break,
    }];
    assert_rejects(&p, "V9");
}

#[test]
fn v10_rejects_a_string_index_into_a_list() {
    let mut p = base();
    p.funcs[1].ret = HTy::Int;
    p.funcs[1].body = vec![ret_stmt(HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Int,
        kind: HExprKind::Index {
            base: Box::new(HExpr {
                span: sp(),
                diag: DiagInfo::default(),
                ty: HTy::List(Box::new(HTy::Int)),
                kind: HExprKind::List(vec![int_e()]),
            }),
            index: Box::new(str_e()),
            rule: IndexRule::ListInt,
        },
    })];
    assert_rejects(&p, "V10");
}

#[test]
fn v11_rejects_a_float_range_bound() {
    let mut p = base();
    p.funcs[1].ret = HTy::List(Box::new(HTy::Int));
    p.funcs[1].body = vec![HStmt {
        span: sp(),
        diag: DiagInfo::default(),
        kind: HStmtKind::Expr(HExpr {
            span: sp(),
            diag: DiagInfo::default(),
            ty: HTy::List(Box::new(HTy::Int)),
            kind: HExprKind::Range {
                start: Box::new(float_e()),
                end: Box::new(int_e()),
                rule: RangeRule::AscendingOrEmpty,
            },
        }),
    }];
    assert_rejects(&p, "V11");
}

#[test]
fn v12_names_live_in_the_intern_table_and_nowhere_else() {
    // Half of V12 is a property of the model's type definitions, not a
    // runtime check: `DiagInfo` is the only field that holds a source
    // identifier, and string literals plus `HProgram::strings` hold
    // data. The other half is checkable, and is checked here: every
    // runtime field name is interned.
    let mut p = base();
    p.strings = vec!["n".to_string()];
    p.funcs[1].body = vec![HStmt {
        span: sp(),
        diag: DiagInfo::default(),
        kind: HStmtKind::Expr(HExpr {
            span: sp(),
            diag: DiagInfo::default(),
            ty: HTy::Unknown,
            kind: HExprKind::Field {
                base: Box::new(HExpr {
                    span: sp(),
                    diag: DiagInfo::default(),
                    ty: HTy::Unknown,
                    kind: HExprKind::None,
                }),
                field: FieldRef::Dynamic(StrId(4)),
            },
        }),
    }];
    assert_rejects(&p, "V2");

    // And the diagnostic sidecar is inert: erasing every name changes
    // no verdict, so no rule consulted a spelling.
    let good = base();
    assert_accepts(&good);
    assert_accepts(&verify::strip_diag(&good));
}

#[test]
fn stripping_diag_does_not_change_a_verdict() {
    let mut broken = base();
    broken.funcs[1].body = vec![ret_stmt(slot_e(Slot(5), HTy::Int))];
    assert_rejects(&broken, "V1");
    assert_rejects(&verify::strip_diag(&broken), "V1");
}

#[test]
fn a_dynamic_base_is_the_only_place_a_runtime_name_may_appear() {
    // The same interned name on a *known* record is a lowering bug: the
    // checker resolved every field of a known record to an offset.
    let mut p = base();
    p.strings = vec!["n".to_string()];
    p.funcs[1].ret = HTy::Unknown;
    p.funcs[1].body = vec![HStmt {
        span: sp(),
        diag: DiagInfo::default(),
        kind: HStmtKind::Expr(HExpr {
            span: sp(),
            diag: DiagInfo::default(),
            ty: HTy::Unknown,
            kind: HExprKind::Field {
                base: Box::new(HExpr {
                    span: sp(),
                    diag: DiagInfo::default(),
                    ty: HTy::Record(TypeId(0)),
                    kind: HExprKind::Construct {
                        type_id: TypeId(0),
                        args: vec![int_e(), int_e()],
                    },
                }),
                field: FieldRef::Dynamic(StrId(0)),
            },
        }),
    }];
    assert_rejects(&p, "V5");
}

#[test]
fn unary_and_dynamic_rules_are_checked_too() {
    // `not` on an Int is not a rule the checker could have decided, so
    // hand-built HIR claiming it is rejected.
    let mut p = base();
    p.funcs[1].ret = HTy::Bool;
    p.funcs[1].body = vec![ret_stmt(HExpr {
        span: sp(),
        diag: DiagInfo::default(),
        ty: HTy::Bool,
        kind: HExprKind::Unary {
            op: UnaryOp::Not,
            rule: UnaryRule::Not,
            operand: Box::new(int_e()),
        },
    })];
    assert_rejects(&p, "V3");
    let _ = bool_e();
}
