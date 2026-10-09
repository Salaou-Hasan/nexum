use crate::parse_source;
use nx_ast::{BinOp, Expr, Program, Stmt, Target, UnaryOp};

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(src: &str) -> Program {
        parse_source(src).unwrap_or_else(|e| panic!("{src:?}: {e}"))
    }

    #[test]
    fn print_string() {
        let p = prog("print(\"hi\")");
        assert_eq!(p.stmts.len(), 1);
        assert!(matches!(p.stmts[0], Stmt::Print { .. }));
    }

    #[test]
    fn assign_add() {
        let p = prog("x = 10 + 20");
        match &p.stmts[0] {
            Stmt::Assign {
                targets, values, ..
            } => {
                assert_eq!(targets.len(), 1);
                assert_eq!(targets[0], Target::Name("x".to_string()));
                assert!(matches!(values[0], Expr::Binary { op: BinOp::Add, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn precedence_mul_binds_tighter() {
        let p = prog("x = 1 + 2 * 3");
        match &p.stmts[0] {
            Stmt::Assign { values, .. } => {
                let right = match &values[0] {
                    Expr::Binary {
                        op: BinOp::Add,
                        right,
                        ..
                    } => right,
                    other => panic!("expected +, got {other:?}"),
                };
                assert!(matches!(&**right, Expr::Binary { op: BinOp::Mul, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn generic_call() {
        let p = prog("foo(1, 2)");
        assert!(matches!(
            &p.stmts[0],
            Stmt::Expr(Expr::Call { callee, .. })
            if matches!(callee.as_ref(), Expr::Var(f, _) if f == "foo")
        ));
    }

    #[test]
    fn print_multi_arg() {
        let p = prog("print(y, x)");
        match &p.stmts[0] {
            Stmt::Print { values, .. } => assert_eq!(values.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn float_expr() {
        let p = prog("x = 10.8 + 20");
        assert_eq!(p.stmts.len(), 1);
    }

    #[test]
    fn if_else_block() {
        let p = prog("if x > 1:\n    print(x)\nelse:\n    print(0)");
        assert!(matches!(p.stmts[0], Stmt::If { .. }));
    }

    #[test]
    fn while_block() {
        let p = prog("while x < 5:\n    x = x + 1");
        assert!(matches!(p.stmts[0], Stmt::While { .. }));
    }

    #[test]
    fn bool_logic_precedence() {
        let p = prog("x = true and false or true");
        assert_eq!(p.stmts.len(), 1);
    }

    #[test]
    fn for_range() {
        let p = prog("for i in 0..5:\n    print(i)");
        assert!(matches!(p.stmts[0], Stmt::For { .. }));
    }

    #[test]
    fn for_each() {
        let p = prog("for i in nums:\n    print(i)");
        assert!(matches!(p.stmts[0], Stmt::For { .. }));
    }

    #[test]
    fn import_forms() {
        let p = prog("import utils\nimport utils as u\nfrom utils import foo, bar as b");
        assert_eq!(p.stmts.len(), 3);
        assert!(matches!(p.stmts[0], Stmt::Import { .. }));
        assert!(matches!(p.stmts[2], Stmt::FromImport { .. }));
    }

    #[test]
    fn attr_call() {
        let p = prog("print(utils.foo(1))");
        assert_eq!(p.stmts.len(), 1);
    }

    #[test]
    fn fn_def() {
        let p = prog("fn add(a, b):\n    return a + b");
        assert!(matches!(p.stmts[0], Stmt::Fn { .. }));
    }

    #[test]
    fn parallel_is_not_a_keyword() {
        // `parallel:` was removed rather than deprecated. It never had a
        // sound implementation: the scheduler proved race-freedom by static
        // approximation and the approximation had holes, so a program could
        // print the wrong answer whenever it lost a race. `parallel` is now
        // an ordinary identifier, and the old spelling is a syntax error
        // rather than a silent no-op.
        assert!(parse_source("parallel:\n    a()\n    b()\n").is_err());
        // ...and the name itself is free to use.
        assert!(parse_source("parallel = 1\nprint(parallel)\n").is_ok());
    }

    #[test]
    fn elif_chain() {
        let p = prog("if a:\n    print(1)\nelif b:\n    print(2)\nelse:\n    print(3)");
        match &p.stmts[0] {
            Stmt::If {
                elifs, else_body, ..
            } => {
                assert_eq!(elifs.len(), 1);
                assert!(else_body.is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn list_and_index() {
        let p = prog("x = [1, 2]\nprint(x[0])");
        assert_eq!(p.stmts.len(), 2);
    }

    #[test]
    fn aug_assign() {
        let p = prog("x += 1");
        assert!(matches!(p.stmts[0], Stmt::AssignOp { .. }));
    }

    #[test]
    fn missing_paren_errors() {
        assert!(parse_source("print(\"hi\"").is_err());
    }

    // ---- Stage 1 syntax ----

    /// Every new operator has to reach the AST as itself, not as a
    /// mistokenized pair. `%` in particular was missing long enough that
    /// the benchmark suite had to work around it with `x - (x / m) * m`.
    #[test]
    fn new_operators_parse() {
        for (src, op) in [
            ("x = a % b", BinOp::Mod),
            ("x = a // b", BinOp::FloorDiv),
            ("x = a ** b", BinOp::Pow),
            ("x = a & b", BinOp::BitAnd),
            ("x = a | b", BinOp::BitOr),
            ("x = a ^ b", BinOp::BitXor),
            ("x = a << b", BinOp::Shl),
            ("x = a >> b", BinOp::Shr),
        ] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            match &p.stmts[0] {
                Stmt::Assign { values, .. } => match &values[0] {
                    Expr::Binary { op: got, .. } => assert_eq!(*got, op, "{src}"),
                    other => panic!("{src} gave {other:?}"),
                },
                other => panic!("{src} gave {other:?}"),
            }
        }
    }

    /// `**` is right associative, so `2 ** 3 ** 2` is 2 ** 9.
    #[test]
    fn pow_is_right_associative() {
        let p = prog("x = 2 ** 3 ** 2");
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Binary {
                op: BinOp::Pow,
                right,
                ..
            } => {
                assert!(matches!(&**right, Expr::Binary { op: BinOp::Pow, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A prefix operator is looser than `**` on its left, so `-2 ** 2` is
    /// `-(2 ** 2)` and evaluates to -4. Binding it the other way would give
    /// 4 and silently change results.
    #[test]
    fn unary_is_looser_than_pow_on_its_left() {
        let p = prog("x = -2 ** 2");
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            &values[0],
            Expr::Unary {
                op: UnaryOp::Neg,
                ..
            }
        ));
    }

    /// The exponent may be signed, which is the other half of why power
    /// and unary sit on opposite sides of each other.
    #[test]
    fn pow_exponent_may_be_signed() {
        let p = parse_source("x = 2 ** -1").unwrap();
        assert_eq!(p.stmts.len(), 1);
    }

    /// i64::MIN has no literal spelling -- the digits overflow i64 -- so a
    /// unary minus in front of exactly 2^63 folds to MIN. Every radix
    /// spelling works; anything else overflowing stays a range error, and
    /// a unary *plus* never folds (it would be +2^63, still out of range).
    #[test]
    fn unary_minus_folds_two_to_the_63_to_min() {
        for src in [
            "x = -9223372036854775808",
            "x = -9_223_372_036_854_775_808",
            "x = -0x8000000000000000",
            "x = -0o1000000000000000000000",
            "x = -0b1000000000000000000000000000000000000000000000000000000000000000",
        ] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            match &p.stmts[0] {
                Stmt::Assign { values, .. } => {
                    assert!(
                        matches!(&values[0], Expr::Int(v, _) if *v == i64::MIN),
                        "{src} did not fold to MIN"
                    );
                }
                other => panic!("{src}: {other:?}"),
            }
        }
        // The bare literal is still out of range, with or without a plus,
        // and so is every other overflow -- including hex whose digits
        // happen to parse as decimal (which once silently became 8e15).
        for src in [
            "x = 9223372036854775808",
            "x = +9223372036854775808",
            "x = 0x8000000000000000",
            "x = 0xFFFFFFFFFFFFFFFF",
            "x = -9223372036854775809",
        ] {
            assert!(parse_source(src).is_err(), "{src} should not parse");
        }
    }

    /// The bitwise ladder sits below comparison and above arithmetic, so
    /// `a < b & c` is `a < (b & c)`.
    #[test]
    fn bitwise_binds_looser_than_arithmetic() {
        let p = prog("x = a & b + c");
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Binary {
                op: BinOp::BitAnd,
                right,
                ..
            } => {
                assert!(matches!(&**right, Expr::Binary { op: BinOp::Add, .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn membership_operators() {
        for (src, op) in [("x = a in b", BinOp::In), ("x = a not in b", BinOp::NotIn)] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let values = match &p.stmts[0] {
                Stmt::Assign { values, .. } => values,
                other => panic!("{src} gave {other:?}"),
            };
            match &values[0] {
                Expr::Binary { op: got, .. } => assert_eq!(*got, op, "{src}"),
                other => panic!("{src} gave {other:?}"),
            }
        }
    }

    /// `not in` has to beat the prefix `not`, or `x not in y` reads as
    /// `x` followed by a dangling negation.
    #[test]
    fn membership_beats_prefix_not() {
        let p = parse_source("x = not a in b").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(
            &values[0],
            Expr::Unary {
                op: UnaryOp::Not,
                ..
            }
        ));
    }

    #[test]
    fn ternary_expression() {
        let p = parse_source("x = 1 if c else 2").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::IfExpr { .. }));
    }

    #[test]
    fn indexed_assignment() {
        let p = parse_source("a[0] = 5").unwrap();
        match &p.stmts[0] {
            Stmt::Assign {
                targets, values, ..
            } => {
                assert!(matches!(&targets[0], Target::Index { .. }));
                assert_eq!(values.len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    /// `a[i][j] = v` needs the postfix chain to be kept whole rather than
    /// stopping at the first bracket.
    #[test]
    fn nested_index_assignment() {
        let p = parse_source("a[i][j] = v").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { targets, .. } => match &targets[0] {
                Target::Index { base, .. } => assert!(matches!(&**base, Expr::Index { .. })),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn indexed_augmented_assignment() {
        let p = parse_source("a[i] += 2").unwrap();
        match &p.stmts[0] {
            Stmt::AssignOp { target, op, .. } => {
                assert!(matches!(target, Target::Index { .. }));
                assert_eq!(*op, BinOp::Add);
            }
            other => panic!("{other:?}"),
        }
    }

    /// A plain call must not be mistaken for an assignment just because it
    /// starts with an identifier.
    #[test]
    fn call_is_not_an_assignment() {
        let p = parse_source("f(1)").unwrap();
        assert!(matches!(&p.stmts[0], Stmt::Expr(Expr::Call { .. })));
    }

    #[test]
    fn multiple_assignment_pairs() {
        let p = parse_source("a, b = 1, 2").unwrap();
        match &p.stmts[0] {
            Stmt::Assign {
                targets, values, ..
            } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 2);
            }
            other => panic!("{other:?}"),
        }
    }

    /// `a, b = f()` destructures a single tuple, so the right side stays a
    /// one-element list at parse time and the checker resolves it.
    #[test]
    fn multiple_assignment_from_call() {
        let p = parse_source("a, b = f()").unwrap();
        match &p.stmts[0] {
            Stmt::Assign {
                targets, values, ..
            } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 1);
                assert!(matches!(&values[0], Expr::Call { .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// Element targets pair up positionally too: `xs[0], xs[1] = 7, 8`.
    /// Only bare-name lists are decided by lookahead; anything with a
    /// bracket or dot parses here, one postfix target at a time.
    #[test]
    fn multiple_element_targets_assign() {
        let p = parse_source("xs[0], xs[1] = 7, 8").unwrap();
        match &p.stmts[0] {
            Stmt::Assign {
                targets, values, ..
            } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 2);
                assert!(matches!(&targets[0], Target::Index { .. }));
                assert!(matches!(&targets[1], Target::Index { .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A mixed list takes the same path: the bare-name lookahead only
    /// fires when every target is a name.
    #[test]
    fn mixed_name_and_element_targets_assign() {
        let p = parse_source("a, xs[0] = 1, 2").unwrap();
        match &p.stmts[0] {
            Stmt::Assign {
                targets, values, ..
            } => {
                assert_eq!(targets.len(), 2);
                assert_eq!(values.len(), 2);
                assert!(matches!(&targets[0], Target::Name(_)));
                assert!(matches!(&targets[1], Target::Index { .. }));
            }
            other => panic!("{other:?}"),
        }
    }

    /// A trailing comma with no `=` is not an assignment: the parse
    /// rewinds and the statement fails as an expression instead.
    #[test]
    fn comma_without_equals_is_not_an_assignment() {
        assert!(parse_source("xs[0], xs[1]").is_err());
    }

    #[test]
    fn return_tuple() {
        let p = parse_source("fn f():\n    return 1, 2").unwrap();
        let stmts = match &p.stmts[0] {
            Stmt::Fn { body, .. } => body,
            other => panic!("{other:?}"),
        };
        match &stmts[0] {
            Stmt::Return { values, .. } => assert_eq!(values.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    /// Bare `return` stays a no-value return, distinct from `return None`.
    #[test]
    fn bare_return_has_no_values() {
        let p = parse_source("fn f():\n    return").unwrap();
        let stmts = match &p.stmts[0] {
            Stmt::Fn { body, .. } => body,
            other => panic!("{other:?}"),
        };
        match &stmts[0] {
            Stmt::Return { values, .. } => assert!(values.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn none_literal() {
        let p = parse_source("x = None").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::NoneLit(_)));
    }

    #[test]
    fn slices_in_every_form() {
        for src in [
            "x = a[1:3]",
            "x = a[:3]",
            "x = a[1:]",
            "x = a[:]",
            "x = a[::2]",
        ] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let values = match &p.stmts[0] {
                Stmt::Assign { values, .. } => values,
                other => panic!("{src} gave {other:?}"),
            };
            match &values[0] {
                Expr::Slice { .. } => {}
                other => panic!("{src} gave {other:?}"),
            }
        }
        // A step must not swallow the index.
        let p = parse_source("x = a[1:3:2]").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Slice { from, to, step, .. } => {
                assert!(from.is_some() && to.is_some() && step.is_some());
            }
            other => panic!("{other:?}"),
        }
    }

    /// `a[i]` and `a[i:j]` must not be confused: an index is not a slice
    /// with an empty range.
    #[test]
    fn index_is_not_a_slice() {
        let p = parse_source("x = a[i]").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        assert!(matches!(&values[0], Expr::Index { .. }));
    }

    #[test]
    fn del_forms() {
        for src in ["del a", "del a[i]", "del p.x", "del a, b"] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            match &p.stmts[0] {
                Stmt::Del { targets, .. } => assert!(!targets.is_empty(), "{src}"),
                other => panic!("{src} gave {other:?}"),
            }
        }
    }

    #[test]
    fn assert_forms() {
        let p = parse_source("assert x > 1").unwrap();
        assert!(matches!(&p.stmts[0], Stmt::Assert { message: None, .. }));
        let p = parse_source("assert x > 1, \"too small\"").unwrap();
        match &p.stmts[0] {
            Stmt::Assert { message, .. } => assert!(message.is_some()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn dict_literal() {
        let p = parse_source("d = {\"a\": 1, \"b\": 2}").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Dict(pairs, _) => assert_eq!(pairs.len(), 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn empty_dict_literal() {
        let p = parse_source("d = {}").unwrap();
        let values = match &p.stmts[0] {
            Stmt::Assign { values, .. } => values,
            other => panic!("{other:?}"),
        };
        match &values[0] {
            Expr::Dict(pairs, _) => assert!(pairs.is_empty()),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn comprehensions() {
        for src in [
            "x = [i * 2 for i in ys]",
            "x = [i * 2 for i in ys if i > 1]",
        ] {
            let p = parse_source(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            let values = match &p.stmts[0] {
                Stmt::Assign { values, .. } => values,
                other => panic!("{src} gave {other:?}"),
            };
            assert!(matches!(&values[0], Expr::Comprehension { .. }), "{src}");
        }
    }

    #[test]
    fn extended_number_literals() {
        let p = parse_source("a = 1_000\nb = 0xff\nc = 1e3\nd = 2.5e-3").unwrap();
        assert_eq!(p.stmts.len(), 4);
    }

    // ---- impl blocks ----

    #[test]
    fn impl_block_parses() {
        let p = parse_source(
            "impl Point:\n    fn area(self):\n        return 1\n    fn origin():\n        return 2",
        )
        .unwrap();
        match &p.stmts[0] {
            Stmt::Impl {
                type_name, methods, ..
            } => {
                assert_eq!(type_name, "Point");
                assert_eq!(methods.len(), 2);
                assert_eq!(methods[0].name, "area");
                assert_eq!(methods[0].receiver, nx_ast::ReceiverKind::Read);
                assert!(methods[0].params.is_empty());
                assert_eq!(methods[1].receiver, nx_ast::ReceiverKind::None);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn impl_receivers() {
        let p = parse_source(
            "impl P:\n    fn a(self):\n        return 1\n    fn b(mut self, x):\n        return 2\n    fn c(own self):\n        return 3",
        )
        .unwrap();
        let methods = match &p.stmts[0] {
            Stmt::Impl { methods, .. } => methods,
            other => panic!("{other:?}"),
        };
        assert_eq!(methods[0].receiver, nx_ast::ReceiverKind::Read);
        assert_eq!(methods[1].receiver, nx_ast::ReceiverKind::Mut);
        assert_eq!(methods[1].params, vec!["x".to_string()]);
        assert_eq!(methods[2].receiver, nx_ast::ReceiverKind::Own);
    }

    #[test]
    fn impl_rejects_non_fn_bodies() {
        assert!(parse_source("impl P:\n    x = 1").is_err());
        assert!(parse_source("impl P:\n    print(1)").is_err());
    }

    #[test]
    fn impl_rejects_empty_and_duplicate_methods() {
        assert!(parse_source("impl P:\n").is_err());
        assert!(parse_source(
            "impl P:\n    fn a(self):\n        return 1\n    fn a(self):\n        return 2"
        )
        .is_err());
    }

    #[test]
    fn self_only_first() {
        // `self` anywhere but first is refused, even though the name
        // would otherwise parse as a parameter.
        assert!(parse_source("impl P:\n    fn a(x, self):\n        return 1").is_err());
        assert!(parse_source("impl P:\n    fn a(mut self, mut self):\n        return 1").is_err());
    }

    /// `self` is a keyword, but inside a body it is just the receiver
    /// binding. Reading it, writing through it, and indexing it must all
    /// parse as ordinary variable targets, because those are exactly the
    /// operations a `mut self` method exists to perform.
    #[test]
    fn self_is_a_variable_in_a_body() {
        let p = parse_source(
            "impl P:\n    fn m(mut self):\n        self.x = 1\n        self.y = self.x + 2\n        self.items[0] = self.y\n        return self",
        )
        .unwrap();
        let body = match &p.stmts[0] {
            Stmt::Impl { methods, .. } => &methods[0].body,
            other => panic!("{other:?}"),
        };
        // First statement must be an assignment, not a bare expression:
        // that is the whole point of the promotion.
        assert!(
            matches!(body[0], Stmt::Assign { .. }),
            "self.x = 1 must parse as an assignment: {:?}",
            body[0]
        );
        // Reading `self` yields a plain variable reference.
        let ret = match &body[3] {
            Stmt::Return { values, .. } => values[0].clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(ret.span().line, 6);
        assert!(
            matches!(&ret, Expr::Var(n, _) if n == "self"),
            "return self must be a variable read: {ret:?}"
        );
    }

    /// `q.moved(1, 1).moved(2, 2)` parses as a call on a call, which is
    /// what makes a `mut self` chain one expression.
    #[test]
    fn method_calls_chain_left() {
        let p = parse_source("q.moved(1, 1).moved(2, 2).area()").unwrap();
        match &p.stmts[0] {
            Stmt::Expr(e) => {
                let inner = match e {
                    Expr::Call { callee, .. } => callee.as_ref(),
                    other => panic!("{other:?}"),
                };
                // The receiver of the outer call is itself a call.
                assert!(
                    matches!(inner, Expr::Attr { base, .. } if matches!(base.as_ref(), Expr::Call { .. })),
                    "chained method must nest: {inner:?}"
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// `Point.origin()` is `Point` followed by a call on an attribute --
    /// the associated-function spelling, distinct from constructing
    /// `Point(...)`.
    #[test]
    fn associated_function_call_parses_as_attribute() {
        let p = parse_source("p = Point.origin()").unwrap();
        match &p.stmts[0] {
            Stmt::Assign { values, .. } => match &values[0] {
                Expr::Call { callee, .. } => assert!(
                    matches!(callee.as_ref(), Expr::Attr { attr, .. } if attr == "origin"),
                    "callee must be an attribute: {callee:?}"
                ),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    /// An `impl` block for a name that is not a type is a checker error, not
    /// a parse error, so the parser accepts it and lets the checker report
    /// the precise reason.
    #[test]
    fn impl_of_unknown_type_parses() {
        assert!(parse_source("impl Missing:\n    fn a(self):\n        return 1").is_ok());
    }

    #[test]
    fn impl_rejects_missing_type_name() {
        assert!(parse_source("impl:\n    fn a(self):\n        return 1").is_err());
        assert!(parse_source("impl P:\n    fn (self):\n        return 1").is_err());
    }
}
