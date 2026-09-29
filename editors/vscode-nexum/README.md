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
* **Material Icon Theme**: maps `*.nx` to its Python icon.
* **Anything else**: select the bundled **Nexum Icons** file icon theme.

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
