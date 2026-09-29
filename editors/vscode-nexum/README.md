# Nexum VS Code extension (local, unpackaged)

## File icons: coexisting with your theme

VS Code allows exactly **one** active file icon theme, so no extension can
inject just its own icon into another theme. Pick one:

### A. Nexum Icons theme (blue N on `.nx`)
Preferences → File Icon Theme → **Nexum Icons**. Other files fall back to
VS Code defaults while this theme is active.

### B. Keep your current theme (recommended)
Map `.nx` to an icon your theme already has, in VS Code settings:

- **vscode-icons**: `"vsicons.associations.files": [{ "icon": "py", "extensions": ["nx"] }]`
- **Material Icon Theme**: `"material-icon-theme.files.associations": { "*.nx": "python" }`
- **Minimal/Seti (built-in)**: no custom mappings supported — use option A.

## Install (pick one)

### A. Copy into extensions (simplest)
1. Close VS Code.
2. Copy `editors/vscode-nexum` to `%USERPROFILE%\.vscode\extensions\nexum-0.0.1`
   (result: `%USERPROFILE%\.vscode\extensions\nexum-0.0.1\package.json`).
3. Reopen VS Code on `C:\nexum`. Open any `.nx` file — bottom-right should say **Nexum**.

### B. Debug once (no copy)
1. VS Code → File → Open Folder → `C:\nexum\editors\vscode-nexum`.
2. Press `F5`. A new window opens with Nexum loaded.
3. Open a `.nx` file there to test.

## What you get
- `.nx` recognized as Nexum (no Python extension needed)
- `#` comments, `"strings"`, `123` / `10.8`, keywords, `fn()` calls
- `:` + Enter auto-indents; `elif:` / `else:` dedent-then-indent
- `()` `[]` `""` auto-close
- Spaces, tabSize 4 (tabs still rejected by the `nx` lexer)

## Uninstall / update
Delete `%USERPROFILE%\.vscode\extensions\nexum-0.0.1` and repeat A.
