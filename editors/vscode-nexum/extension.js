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
}

function deactivate() {}

if (typeof module !== "undefined" && module.exports) {
    module.exports = { parseNxDiagnostics };
}

module.exports.activate = activate;
module.exports.deactivate = deactivate;
