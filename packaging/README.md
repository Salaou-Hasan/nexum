# Packaging

The installers for Nexum.

## Windows (`packaging/windows`)

`Package.wxs` is the WiX source of the MSI. The build needs nothing but the
WiX tool - the interface and the `PATH` handling are done with the core
elements and two small scripts, so no WiX extension is required.

```sh
wix build Package.wxs -d NexumVersion=0.3.2 -o Nexum-0.3.2-x64.msi
```

The sources it expects next to it: `nx.exe`, `nexum.ico`,
`nexum-language.vsix`, `add-to-path.cmd` and `install-vscode-extension.cmd`.

### The icon

`nexum.ico` is built from the project icon. The icon is a PNG, and Windows
accepts PNG-compressed entries in an `.ico`, so the image is embedded as it
is instead of being re-encoded:

```sh
python packaging/windows/make_ico.py icon/icon-256.png packaging/windows/nexum.ico
```

`icon/icon-256.png` is the project icon at 256 pixels, and
`icon/icon.png` is the full size original. The marketplace icon of the
editor extension is the same image at 128 pixels
(`editors/vscode/icon.png`).

## Linux (`packaging/linux`)

The `.deb` is assembled by hand and built with `dpkg-deb`:

```
nexum-<version>-amd64.deb
  DEBIAN/control          from control.in
  DEBIAN/postinst         from postinst.in   (installs the extension)
  usr/bin/nx              the compiler
  usr/share/nexum/        the extension
  usr/share/icons/...     the icon
  usr/share/applications/ the desktop entry
  usr/share/mime/packages the file type
```

The `.in` files carry an `@VERSION@` placeholder that the release workflow
replaces.
