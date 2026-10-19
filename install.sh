#!/bin/sh
# Nexum installer (Unix). Usage: curl -fsSL https://.../install.sh | sh
#
# It downloads the compiler and hands over to `nx setup`, which asks about
# everything that is needed and then does it: the LLVM toolchain, the
# compiler with the file icon and the file association, and the Visual
# Studio Code extension.
set -e
REPO="Salaou-Hasan/nexum"
OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Linux)  PLATFORM="linux" ;;
  Darwin) PLATFORM="macos" ;;
  *) echo "unsupported OS: $OS"; exit 1 ;;
esac
case "$ARCH" in
  x86_64) ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) echo "unsupported arch: $ARCH"; exit 1 ;;
esac

ASSET="nx-${PLATFORM}-${ARCH}"
URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
SETUP="$(mktemp -t nexum-setup)"
echo "Downloading the Nexum setup program ..."
curl -fsSL "$URL" -o "$SETUP"
chmod +x "$SETUP"
"$SETUP" setup
STATUS=$?
rm -f "$SETUP"
exit $STATUS
