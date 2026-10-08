// Nexum VS Code extension host: `nx check` diagnostics in the editor.
//
// No dependencies: plain Node plus the `vscode` API the host injects.
// Keep it that way -- `vsce package` must succeed with nothing
// installed, and this file is syntax-checked in CI with `node --check`.
//
// The parser is pure (no `vscode` import at load time) so it can be
// exercised outside the host: `require("./extension.js")` in plain
// node and feed it real `nx check` output. `activate` is the only
// entry point VS Code calls.

"use strict";

const { execFile } = require("child_process");

const OUTPUT_LINE =
    /^(?:nx:\s+)?(lex|parse|type) error at (\d+):(\d+): (.*)$/;

/**
 * Parse `nx check` output into zero-based { line, col, message } hits.
 * Lines that do not match are ignored: the compiler also prints status
 * lines (`nx: no type errors`, build chatter) that carry no position.
 * Identical hits collapse to one: the checker reports root causes, but
 * nothing here should ever double-underline the same span for the same
 * reason even if a future checker change reintroduces a duplicate.
 */
function parseNxDiagnostics(output) {
    const hits = [];
    const seen = new Set();
    for (const raw of String(output).split(/\r?\n/)) {
        const m = OUTPUT_LINE.exec(raw.trim());
        if (!m) {
            continue;
        }
        const line = Number(m[2]);
        const col = Number(m[3]);
        if (!Number.isFinite(line) || !Number.isFinite(col) || line < 1 || col < 1) {
            continue;
        }
        const key = `${m[1]}@${line}:${col}:${m[4].trim()}`;
        if (seen.has(key)) {
            continue;
        }
        seen.add(key);
        hits.push({ kind: m[1], line: line - 1, col: col - 1, message: m[4].trim() });
    }
    return hits;
}

/**
 * Clamp a hit to a document: a diagnostic past the end of the file (a
 * stale position from a mid-save check) is dropped, and a column past
 * the line end is pulled back to it. VS Code throws on out-of-range
 * ranges, so this is load-bearing, not cosmetic.
 */
function toRange(vscode, doc, hit) {
    if (hit.line >= doc.lineCount) {
        return null;
    }
    const text = doc.lineAt(hit.line).text;
    // Columns in `nx` output are 1-based character positions; the end
    // of the range extends one character so the squiggle is visible
    // even on an empty line.
    const start = Math.min(hit.col, text.length);
    const end = Math.min(start + 1, Math.max(text.length, 1));
    return new vscode.Range(hit.line, start, hit.line, Math.max(end, start));
}

// `elif:`/`else:` must sit at the same column as the `if` they belong to --
// `    else:` under a column-0 `if` is a parse error, not a style question.
// VS Code used to do that dedent for us through the language configuration's
// `indentationRules`, but those rules are what made Enter on a blank line
// recompute indentation from the nearest preceding line and drop the cursor
// back inside the block it had just left, and they also broke continuation
// inside an open bracket. Removing them fixed both, and moved this one
// behaviour here, where it can be tested.
const ELIF_RE = /^\s*elif\b[^:]*:\s*(?:#.*)?$/;
const ELSE_RE = /^\s*else\b[^:]*:\s*(?:#.*)?$/;
const CLAUSE_RE = /^\s*(?:elif|else)\b[^:]*:\s*(?:#.*)?$/;
const OPENER_RE = /^\s*(?:fn|type|impl|if|for|while)\b[^:]*:\s*(?:#.*)?$/;

/**
 * The indent an `elif:`/`else:` line should have, or null when the line is
 * not a clause head or is already correct.
 *
 * `lines` is the document's lines and `index` the zero-based line to inspect.
 * The target is the column of the opener this clause belongs to.
 *
 * Finding that opener is the whole problem, because an `else:` above the
 * cursor is ambiguous: it is either a block whose body the cursor is inside,
 * or the branch that already closed the `if` the cursor's own clause belongs
 * to. Indentation alone cannot tell them apart -- both sit at the same
 * column -- so they are counted. Walking up, an `else:` passed is a branch
 * already closed, so the next opener found belongs to an enclosing block; an
 * `elif:` passed is not, because it continues the same chain rather than
 * closing one, and so the chain's `if` is still the answer.
 *
 * No indent size is needed: both columns are read off existing lines.
 *
 * Returns { from, to } as indent widths, or null when there is nothing to do.
 */
function clauseDedent(lines, index) {
    if (!Array.isArray(lines) || index < 0 || index >= lines.length) {
        return null;
    }
    const text = lines[index];
    if (!CLAUSE_RE.test(text)) {
        return null;
    }
    const from = text.length - text.replace(/^[ \t]+/, "").length;
    if (from === 0) {
        return null;
    }
    let closed = 0;
    for (let i = index - 1; i >= 0; i--) {
        const above = lines[i];
        if (above.trim() === "") {
            continue;
        }
        const width = above.length - above.replace(/^[ \t]+/, "").length;
        // Deeper than the clause: inside the body this clause heads, not
        // around it.
        if (width > from) {
            continue;
        }
        if (ELSE_RE.test(above)) {
            closed++;
            continue;
        }
        if (ELIF_RE.test(above)) {
            continue;
        }
        if (!OPENER_RE.test(above)) {
            continue;
        }
        if (closed > 0) {
            closed--;
            continue;
        }
        return width === from ? null : { from, to: width };
    }
    return null;
}

function checkDocument(vscode, collection, config, doc, opts) {
    const showErrors = !opts || opts.showErrors !== false;
    if (!doc || doc.languageId !== "nexum" || doc.isUntitled) {
        return;
    }
    const exe = config.get("executablePath") || "nx";
    execFile(exe, ["check", doc.fileName], { timeout: 60000 }, (err, stdout, stderr) => {
        if (err && err.code === "ENOENT") {
            collection.delete(doc.uri);
            if (showErrors) {
                vscode.window.showWarningMessage(
                    `Nexum: could not run "${exe}" -- is nx on PATH? (setting: nexum.executablePath)`
                );
            }
            return;
        }
        const diags = [];
        for (const hit of parseNxDiagnostics(`${stdout || ""}\n${stderr || ""}`)) {
            const range = toRange(vscode, doc, hit);
            if (!range) {
                continue;
            }
            diags.push(
                new vscode.Diagnostic(
                    range,
                    `nx (${hit.kind}): ${hit.message}`,
                    vscode.DiagnosticSeverity.Error
                )
            );
        }
        if (diags.length > 0 || !err) {
            collection.set(doc.uri, diags);
        }
        // A successful check clears stale squiggles; any other failure
        // (timeout, crash) leaves the previous diagnostics in place
        // rather than flashing the file clean on an inconclusive run.
    });
}

function activate(context) {
    const vscode = require("vscode");
    const collection = vscode.languages.createDiagnosticCollection("nexum");
    context.subscriptions.push(collection);

    const checkActive = (opts) => {
        const config = vscode.workspace.getConfiguration("nexum");
        const editor = vscode.window.activeTextEditor;
        if (editor) {
            checkDocument(vscode, collection, config, editor.document, opts);
        }
    };

    context.subscriptions.push(
        vscode.commands.registerCommand("nexum.check", () => checkActive({ showErrors: true })),
        vscode.workspace.onDidSaveTextDocument((doc) => {
            const config = vscode.workspace.getConfiguration("nexum");
            if (config.get("checkOnSave") !== false) {
                checkDocument(vscode, collection, config, doc, { showErrors: false });
            }
        }),
        vscode.window.onDidChangeActiveTextEditor((editor) => {
            if (editor && editor.document.languageId === "nexum") {
                const config = vscode.workspace.getConfiguration("nexum");
                checkDocument(vscode, collection, config, editor.document, { showErrors: false });
            }
        })
    );

    // `elif:`/`else:` dedent, moved out of the language configuration (see
    // clauseDedent). It runs on the line the caret is on, and it is
    // self-limiting: once the column matches the target, every further
    // keystroke on that line is a no-op, so there is nothing to suppress.
    let applying = false;
    context.subscriptions.push(
        vscode.workspace.onDidChangeTextDocument(async (event) => {
            if (applying) {
                return;
            }
            const doc = event.document;
            const editor = vscode.window.activeTextEditor;
            if (doc.languageId !== "nexum" || !editor || editor.document !== doc) {
                return;
            }
            const options = editor.options;
            if (options.insertSpaces === false) {
                return;
            }
            const indentSize = typeof options.tabSize === "number" ? options.tabSize : 4;
            const line = editor.selection.active.line;
            const lines = [];
            for (let i = 0; i < doc.lineCount; i++) {
                lines.push(doc.lineAt(i).text);
            }
            const fix = clauseDedent(lines, line, indentSize);
            if (!fix) {
                return;
            }
            applying = true;
            try {
                await editor.edit((builder) => {
                    builder.replace(
                        new vscode.Range(line, 0, line, fix.from),
                        " ".repeat(fix.to)
                    );
                });
            } finally {
                applying = false;
            }
        })
    );
}

function deactivate() {}

if (typeof module !== "undefined" && module.exports) {
    module.exports = { parseNxDiagnostics, clauseDedent };
}

module.exports.activate = activate;
module.exports.deactivate = deactivate;
