# Nexum VS Code extension (local, unpackaged)

## File icons: coexisting with your theme

VS Code allows exactly **one** active file icon theme, so no extension can
inject just its own icon into another theme. Run this once (also runs
automatically at the end of the Windows MSI install):

```
nx setup          # shows what it would add for your current theme
nx setup --apply  # writes it (keeps a .bak backup)
```

* **vscode-icons**: writes our blue N into `%APPDATA%/Nexum/icons` and adds
  the `.nx` mapping — your other icons untouched.
* **Material Icon Theme**: writes our N into `.vscode/extensions/icons/`
  and maps `*.nx` to it (their documented custom-SVG mechanism).
* **Anything else** (Catppuccin explicitly refuses custom SVGs; the rest
  have no API): setup leaves your theme **untouched** — no borrowed icons.
  Select the bundled **Nexum Icons** file icon theme if you want the N.

## Install (pick one)

### A. Install from the marketplace (recommended)
Extensions → search **Nexum** → Install. The marketplace keeps one copy, so
there is nothing to clean up when you upgrade.

### B. Copy into extensions
1. Close VS Code.
2. **Delete any Nexum extension folders you already have.** This is the
   step people skip, and it matters: VS Code loads *every* folder that
   contributes the `nexum` language, so a leftover copy silently competes
   with the new one and you get the old behaviour with no error anywhere.
   Check `%USERPROFILE%\.vscode\extensions` for `nexum-*` and
   `*.vscode-nexum-*` and remove them.
3. Copy `editors/vscode-nexum` to `%USERPROFILE%\.vscode\extensions\nexum-0.4.5`
   (result: `%USERPROFILE%\.vscode\extensions\nexum-0.4.5\package.json`).
4. Reopen VS Code on `C:\nexum`. Open any `.nx` file — bottom-right should say **Nexum**.

### C. Debug once (no copy)
1. VS Code → File → Open Folder → `C:\nexum\editors\vscode-nexum`.
2. Press `F5`. A new window opens with Nexum loaded.
3. Open a `.nx` file there to test.

## What you get
- `.nx` recognized as Nexum (no Python extension needed)
- `#` comments, `"strings"`, `123` / `10.8`, keywords, `fn()` calls
- **Python's indentation system.** `language-configuration.json` has the
  same shape as the one VS Code ships for Python: `brackets`,
  `autoClosingPairs`, `surroundingPairs`, off-side folding with `#region`
  markers, one `onEnter` rule for block openers, and **no
  `indentationRules`** — which is the part that matters. With
  `indentationRules` present, VS Code stops keeping each line's own indent
  and instead recomputes it from the nearest preceding line that matches a
  rule; where none matches it gives up and returns line 1's indent. That is
  what put the cursor back inside the block you had just left, and what
  made a list literal continue at column 0. NX's keywords and its
  comment-tolerant trailing colon are its own; everything else tracks
  Python. Concretely: `:` + Enter indents on block openers only; Enter
  anywhere else keeps that line's own indent, so pressing Enter on a blank
  line at column 0 stays at column 0 and pressing Enter inside a block
  stays in it; blank lines fold with the block above, so guides stop at
  scope boundaries
- `elif:`/`else:` dedent themselves to their `if`'s column as you type.
  NX requires that column — `    else:` under a column-0 `if` is a parse
  error, not a style choice — and this used to come from
  `indentationRules`, which had to go. It is now in `extension.js`, where
  it is unit-tested
- `()` `[]` `""` auto-close
- Spaces, tabSize 4 (tabs still rejected by the `nx` lexer)
- Error squiggles from `nx check`: on save, on tab switch, and on
  demand via **Nexum: Check current file** (needs `nx` on `PATH`;
  `nexum.executablePath` overrides it, `nexum.checkOnSave` turns the
  save hook off)

## Uninstall / update
Delete **every** Nexum extension folder — `%USERPROFILE%\.vscode\extensions\nexum-*`
and `*.vscode-nexum-*` — then repeat the install step. Leaving one behind is
how a stale copy keeps winning.

If the indentation behaves as if nothing changed, run **Developer: Reload
Window** first: VS Code reads `language-configuration.json` once, when the
extension activates, so an edit to it needs a reload to take effect.
