// Tests for the pure helpers in editors/vscode-nexum/extension.js.
//
// The extension host itself cannot be exercised outside VS Code, so every
// piece of logic that can be pure is pure and exported, and tested here.
// Run with:  node editors/vscode-nexum/test/extension.test.js
//
// ASCII only, and no dependencies: this has to run on whatever Node the
// runner has, with nothing installed.

"use strict";

const assert = require("assert");
const path = require("path");

const { parseNxDiagnostics, clauseDedent } = require(path.join(__dirname, "..", "extension.js"));

let passed = 0;
const failures = [];

function test(name, fn) {
    try {
        fn();
        passed++;
    } catch (err) {
        failures.push({ name, message: err && err.message ? err.message : String(err) });
    }
}

// --- parseNxDiagnostics ---------------------------------------------------

test("parses the three diagnostic kinds and converts to zero-based", () => {
    const out = [
        "nx: lex error at 3:1: unexpected byte",
        "nx: parse error at 12:5: expected ':'",
        " nx: type error at 1:1: undefined variable 'do' "
    ].join("\n");
    const hits = parseNxDiagnostics(out);
    assert.deepStrictEqual(
        hits.map((h) => [h.kind, h.line, h.col, h.message]),
        [
            ["lex", 2, 0, "unexpected byte"],
            ["parse", 11, 4, "expected ':'"],
            ["type", 0, 0, "undefined variable 'do'"]
        ]
    );
});

test("ignores status lines and chatter", () => {
    const out = ["nx: no type errors", "   Compiling nx-driver v0.4.5", ""].join("\n");
    assert.deepStrictEqual(parseNxDiagnostics(out), []);
});

test("collapses identical hits", () => {
    const out = ["nx: type error at 2:3: same", "nx: type error at 2:3: same"].join("\n");
    assert.strictEqual(parseNxDiagnostics(out).length, 1);
});

test("tolerates CRLF", () => {
    const hits = parseNxDiagnostics("nx: parse error at 5:2: boom\r\nnx: parse error at 6:2: bang\r\n");
    assert.strictEqual(hits.length, 2);
    assert.strictEqual(hits[1].line, 5);
});

// --- clauseDedent --------------------------------------------------------

test("dedents else: to the column of its if", () => {
    const lines = ["if a:", "    b = 1", "    else:"];
    assert.deepStrictEqual(clauseDedent(lines, 2), { from: 4, to: 0 });
});

test("dedents elif: the same way", () => {
    const lines = ["if a:", "    b = 1", "    elif c:"];
    assert.deepStrictEqual(clauseDedent(lines, 2), { from: 4, to: 0 });
});

test("steps over the body lines between the if and the clause", () => {
    // The clause is typed a level deeper than its `if`; the line directly
    // above it is a statement, not an opener, and must not stop the search.
    const lines = ["if a:", "    b = 1", "        else:"];
    assert.deepStrictEqual(clauseDedent(lines, 2), { from: 8, to: 0 });
});

test("leaves a clause already at its if's column alone", () => {
    // The nested shape: `else:` at column 4 belongs to `if a:` at column 4,
    // and must not be dragged out to the enclosing `fn`.
    const lines = ["fn f():", "    if a:", "        b = 1", "    else:", "        c = 2"];
    assert.strictEqual(clauseDedent(lines, 3), null);
});

test("finds the right if when branches are nested and repeated", () => {
    // Line 3's `else:` closed `if b:`, so line 5's `else:` belongs to `if a:`
    // at column 0 -- not to the `else:` directly above it, which sits at the
    // same column and would be the obvious answer.
    const lines = [
        "if a:",
        "    if b:",
        "        c = 1",
        "    else:",
        "        d = 2",
        "    else:"
    ];
    assert.strictEqual(clauseDedent(lines, 3), null, "line 3 is already correct");
    assert.deepStrictEqual(clauseDedent(lines, 5), { from: 4, to: 0 });
});

test("counts closed branches through a chain of elif", () => {
    const lines = [
        "if a:",
        "    b = 1",
        "elif c:",
        "    d = 2",
        "elif e:",
        "    f = 3",
        "elif g:"
    ];
    assert.strictEqual(clauseDedent(lines, 6), null, "already at column 0");
});

test("an elif one level too deep returns to its if", () => {
    const lines = ["if a:", "    b = 1", "elif c:", "    d = 2", "    elif e:"];
    // Walk up: `    d = 2` is deeper, `elif c:` is a closed branch, `    b = 1`
    // is not an opener, `if a:` is the partner at column 0.
    assert.deepStrictEqual(clauseDedent(lines, 4), { from: 4, to: 0 });
});

test("skips blank lines", () => {
    const lines = ["if a:", "    b = 1", "", "    else:"];
    assert.deepStrictEqual(clauseDedent(lines, 3), { from: 4, to: 0 });
});

test("leaves an already correct clause alone", () => {
    assert.strictEqual(clauseDedent(["if a:", "    b = 1", "else:"], 2), null);
});

test("leaves a clause at column 0 alone", () => {
    assert.strictEqual(clauseDedent(["if a:", "else:"], 1), null);
});

test("does not fire on lines that merely contain else:", () => {
    const cases = [
        ["x = 1", "    elsewhere = 2"],
        ["    x = 'else:'"],
        ["    y = f(else)"],
        ["    # else:"],
        ["    return"],
        ["    if a:"]
    ];
    for (const lines of cases) {
        assert.strictEqual(
            clauseDedent(lines, lines.length - 1),
            null,
            `should not fire on ${JSON.stringify(lines[lines.length - 1])}`
        );
    }
});

test("does not fire on a call whose argument is a dict", () => {
    assert.strictEqual(clauseDedent(["d = {", "    else: 1", "}"], 1), null);
});

test("does not fire when there is no opener above", () => {
    assert.strictEqual(clauseDedent(["    b = 1", "    else:"], 1), null);
});

test("rejects out-of-range input instead of throwing", () => {
    assert.strictEqual(clauseDedent(["if a:"], -1), null);
    assert.strictEqual(clauseDedent(["if a:"], 1), null);
    assert.strictEqual(clauseDedent(null, 0), null);
});

test("tolerates a trailing comment on the clause", () => {
    const lines = ["if a:", "    b = 1", "    else:  # fallback"];
    assert.deepStrictEqual(clauseDedent(lines, 2), { from: 4, to: 0 });
});

test("tolerates a trailing comment on the opener", () => {
    const lines = ["if a:  # yes", "    b = 1", "    else:"];
    assert.deepStrictEqual(clauseDedent(lines, 2), { from: 4, to: 0 });
});

// --- report ---------------------------------------------------------------

for (const f of failures) {
    console.log(`FAIL ${f.name}\n       ${f.message}`);
}
console.log(`extension tests: ${passed} passed, ${failures.length} failed`);
process.exit(failures.length === 0 ? 0 : 1);
