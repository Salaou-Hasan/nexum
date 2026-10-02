//! PROBE FILE - temporary, will be replaced.
use nx_e2e::*;

fn p(label: &str, src: &str) {
    let o = run(src);
    println!("[{}] code={} out={:?}", label, o.code, o.out);
}

fn c(label: &str, src: &str) {
    match check(src) {
        Ok(()) => println!("[{}] ACCEPTED", label),
        Err(e) => println!("[{}] REJECTED: {}", label, e),
    }
}

#[test]
fn probe() {
    p("int-ops", "print(1+2, 5-3, 3*4, 7/2, 7//2, 7%2)");
    p("mod-signs", "print(7//3, -7//3, 7//-3, -7//-3)\nprint(7%3, -7%3, 7%-3, -7%-3)\nprint(-1%5, 5%5)");
    p("floats", "print(1.0/3.0)\nprint(10.8+20.1)\nx = 10.8 + 20\nprint(x * 2)\nprint(2.5e-3, 1e3)");
    p("int-float", "print(1+2.0)\nprint(3/2.0)\nprint(1.0+2)");
    p("bitwise", "print(6&3, 6|3, 6^3, ~6, 1<<10, 1024>>3)\nprint(-8>>1, -1>>63, 1<<63)");
    c("shift-big", "x = 1 << 64\nprint(x)");
    c("shift-neg", "x = 1 << -1\nprint(x)");
    p("shift-neg-rt", "x = 1 << -1\nprint(x)");
    c("shift-var-big", "n = 3\nx = 1 << n\nprint(x)");
    p("shift-var-big-rt", "n = 3\nx = 1 << n\nprint(x)");
    p("prec", "print(1+2*3)\nprint(-2**2)\nprint(2**-1)\nprint(2.0**-1)");
    p("not-prec", "a = 1\nb = [1,2]\nprint(not a in b)\nprint(not 1 == 2)");
    c("chain-cmp", "print(1 < 2 < 3)");
    p("chain-cmp", "print(1 < 2 < 3)");
    p("lits", "print(1_000_000, 0xFF, 0o17, 0b1011, 1e3, 2.5e-3)");
    p("range", "print(0..5)\nprint(3..3)\nprint(5..1)");
    p("range-len", "print(len(0..5))");
    p("if-else", "x = 5\nif x > 3:\n    print(\"big\")\nelif x > 1:\n    print(\"mid\")\nelse:\n    print(\"small\")");
    p("while", "i = 0\nwhile i < 3:\n    print(i)\n    i += 1");
    p("break-cont", "for i in 0..5:\n    if i == 1:\n        continue\n    if i == 3:\n        break\n    print(i)");
    p("ternary-nodiv", "print(1 if true else 1/0)");
    c("ternary-nodiv-type", "print(1 if true else 1/0)");
    p("comp-leak", "xs = [i * i for i in 0..5]\nprint(xs)\nprint(i)");
    c("comp-leak-type", "xs = [i * i for i in 0..5]\nprint(xs)\nprint(i)");
    p("for-leak", "for i in 0..3:\n    print(i)\nprint(i)");
    c("for-leak-type", "for i in 0..3:\n    print(i)\nprint(i)");
    p("for-leak-fn", "fn f():\n    for i in 0..3:\n        print(i)\n    print(i)\nf()");
    c("for-leak-fn-type", "fn f():\n    for i in 0..3:\n        print(i)\n    print(i)\nf()");
    p("nested-loop", "for i in 0..2:\n    for j in 0..2:\n        print(i, j)\nprint(i)");
    c("nested-loop-type", "for i in 0..2:\n    for j in 0..2:\n        print(i, j)\nprint(i)");
    p("nested-loop-fn", "fn f():\n    for i in 0..2:\n        for j in 0..2:\n            print(i, j)\n    print(i)\nf()");
    c("nested-loop-fn-type", "fn f():\n    for i in 0..2:\n        for j in 0..2:\n            print(i, j)\n    print(i)\nf()");
    p("bools", "print(true and false, true or false, not true)\nprint(1 and 2)");
    c("bools-type", "print(1 and 2)");
    p("none", "x = None\nprint(x)\nprint(None == None)\nprint(None == 1)");
    c("none-cmp", "print(None == 1)");
    c("nonbool-cond", "if 1:\n    print(1)");
    c("while-nonbool", "while 1:\n    print(1)");
    c("assert-int", "assert 1");
    p("assert-int-rt", "assert 1");
    p("assert-ok", "assert 1 == 1\nprint(\"ok\")");
    p("assert-fail", "assert 1 == 2, \"boom\"");
    p("fn-none", "fn f():\n    return\nprint(f())");
    p("recursion", "fn fact(n):\n    if n <= 1:\n        return 1\n    return n * fact(n-1)\nprint(fact(5))");
    p("neg-div-zero", "print(1/0)");
    p("mod-div-zero", "print(1%0)");
    p("int-overflow", "print(9223372036854775807 + 1)");
    p("float-fmt", "print(1.0/3.0, 0.1+0.2, 2.0, 100.0, 1e100)");
    p("div-zero-type", "x = 0\nprint(1/x)");
    p("pow-sat", "print(2**63, (-2)**63, 0**0, 0**3, (-1)**3)");
    p("float-div", "print(7.0/2.0, 7//2.0, 7.5%2.0)");
    c("string-arith", "print(\"a\" + \"b\")");
    p("str-plus", "print(\"a\" + \"b\")");
    c("rebind", "x = \"a\"\nx = 1");
    p("while-leak", "while false:\n    print(1)\nprint(i)");
}