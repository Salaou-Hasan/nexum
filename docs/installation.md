# Installing Nexum

Every release ships the compiler for all supported platforms, an installer
for Windows and Linux, and the Visual Studio Code extension.

## The setup program

`nx setup` is the one place that installs everything Nexum needs. It runs in
a console, asks about every step, and then does it:

```
$ nx setup
Nexum setup
===========

This installs everything Nexum needs:
  * the compiler, on your PATH
  * the file icon for .nx files, and the file association
  * the Visual Studio Code extension
  * the LLVM toolchain, when it is missing

[*] checking the LLVM toolchain
    the LLVM toolchain is installed

[*] checking the editor extension
    Visual Studio Code is installed

[*] installing Nexum
    the latest release is v0.3.5
  install it now? [Y/n]
    downloading https://github.com/Salaou-Hasan/nexum/releases/download/v0.3.5/Nexum-0.3.5-x64.msi
    running the installer ...
    Nexum is installed
    the extension is installed

[*] checking the installation
    the LLVM toolchain is installed
```

Every question can be answered with `n` to skip that step, and the setup can
be run again at any time.

## The quick way

```sh
# Windows (PowerShell)
irm https://github.com/Salaou-Hasan/nexum/releases/latest/download/install.ps1 | iex

# Linux and macOS
curl -fsSL https://github.com/Salaou-Hasan/nexum/releases/latest/download/install.sh | sh
```

Both download the compiler and hand over to `nx setup`, so there is one
implementation of the questions and one of the installation.

## What else is needed

Nexum hands the generated LLVM IR to **clang**, so the LLVM toolchain has to
be installed as well:

```sh
# Windows
winget install LLVM.LLVM

# macOS
brew install llvm

# Linux
sudo apt install clang
```

`nx setup` offers to install it when it is missing, and `nx doctor` reports
what is present and what is missing:

```sh
$ nx doctor
Nexum doctor
============
nx: /usr/local/bin/nx

needed to build and run Nexum programs:
  clang: clang version 18.1.8

only needed to build Nexum itself from source:
  rustc: rustc 1.83.0
  cargo: cargo 1.83.0

Everything Nexum needs is installed.
```

## The quick way

```sh
# Windows (PowerShell)
irm https://github.com/Salaou-Hasan/nexum/releases/latest/download/install.ps1 | iex

# Linux and macOS
curl -fsSL https://github.com/Salaou-Hasan/nexum/releases/latest/download/install.sh | sh
```

The scripts download the latest release and hand over to the installer.

## Windows: the MSI

The release contains `Nexum-<version>-x64.msi`. It installs, per user, so
it needs **no administrator rights**:

* the compiler, into `%LOCALAPPDATA%\Programs\Nexum`, and that directory is
  added to your `PATH`;
* the project icon, registered for `.nx` files, so a Nexum source file shows
  the Nexum icon in Explorer;
* the Visual Studio Code extension, which is installed for you if Visual
  Studio Code is on your `PATH`.

Everything happens by itself once the dialogs are done: there is nothing
left to run afterwards. The icon is registered with the shell, not with the
editor, so it is the same whatever icon theme you use in Visual Studio Code.
The extension also offers the icon as a file icon theme ("Nexum"), which you
can pick in the editor if you want it there too - picking it is your choice,
and installing Nexum never changes the icon theme you already have.

To install it by hand, double-click the MSI, or:

```powershell
msiexec /i Nexum-0.3.5-x64.msi
```

## Linux: the package

The release contains `nexum-<version>-amd64.deb`. It installs the compiler
into `/usr/bin`, the icon into the icon theme directory, registers the
`.nx` file type, and installs the Visual Studio Code extension if Visual
Studio Code is installed.

```sh
sudo dpkg -i nexum-0.3.5-amd64.deb
```

## The portable binaries

The release also contains a plain compiler binary for every platform, for
people who do not want an installer:

| Platform | Asset |
| -------- | ----- |
| Windows  | `nx-windows-x86_64.exe` |
| Linux (x86-64) | `nx-linux-x86_64` |
| Linux (aarch64) | `nx-linux-aarch64` |
| macOS (x86-64) | `nx-macos-x86_64` |
| macOS (arm64) | `nx-macos-aarch64` |

Put the binary somewhere on your `PATH` and it works on its own. The
portable binary does not register the file association or the icon; use the
installer for that.

## The editor extension

The release contains `nexum-language.vsix`. Install it with:

```sh
code --install-extension nexum-language.vsix
```

The extension provides the syntax highlighting, the formatter (it calls
`nx fmt`), and the Nexum file icon theme. The path to the compiler can be
set with the `nexum.nxPath` setting.

## Updating

```sh
nx update
```

`nx update` looks for the latest release and brings the language *and* the
editor extension back in step:

* on Windows it runs the installer, so the compiler, the icon, the file
  association and the extension are all updated together (a copy of the
  compiler that was installed outside the standard location is updated in
  place as well);
* on Linux and macOS it replaces the compiler binary and re-installs the
  extension;
* on every platform it installs the extension with
  `code --install-extension`, and says so if `code` was not found.

Restart the editor after an update so that it picks up the new extension.
