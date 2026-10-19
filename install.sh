# Nexum installer (Unix). Usage: curl -fsSL https://.../install.sh | sh
#
# On Linux it installs the .deb package, which brings the compiler, the file
# icon and the editor extension. On macOS it installs the compiler into
# ~/.local/bin and the editor extension into Visual Studio Code.
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

VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)"
if [ -z "$VERSION" ]; then
  echo "could not determine the latest release"
  exit 1
fi

# The editor extension travels with the release.
VSIX_URL="https://github.com/$REPO/releases/download/v$VERSION/nexum-language.vsix"

install_extension() {
  if command -v code >/dev/null 2>&1; then
    VSIX="$(mktemp -t nexum-extension).vsix"
    if curl -fsSL "$VSIX_URL" -o "$VSIX"; then
      code --install-extension "$VSIX" --force >/dev/null 2>&1 \
        && echo "Installed the editor extension." \
        || echo "Could not install the editor extension."
    fi
    rm -f "$VSIX"
  else
    echo "Visual Studio Code was not found; the editor extension was not installed."
  fi
}

if [ "$PLATFORM" = "linux" ]; then
  DEB="nexum-${VERSION}-amd64.deb"
  URL="https://github.com/$REPO/releases/download/v$VERSION/$DEB"
  DEB_PATH="$(mktemp -t nexum).deb"
  echo "Downloading $URL ..."
  curl -fsSL "$URL" -o "$DEB_PATH"
  if command -v sudo >/dev/null 2>&1; then
    echo "Installing the Nexum package (it needs administrator rights) ..."
    sudo dpkg -i "$DEB_PATH"
  else
    echo "Installing the Nexum package ..."
    dpkg -i "$DEB_PATH"
  fi
  rm -f "$DEB_PATH"
  echo "Installed Nexum $VERSION."
  install_extension
  exit 0
fi

ASSET="nx-${PLATFORM}-${ARCH}"
URL="https://github.com/$REPO/releases/latest/download/${ASSET}"
DEST="${HOME}/.local/bin"
mkdir -p "$DEST"
echo "Downloading $URL ..."
curl -fsSL "$URL" -o "$DEST/nx"
chmod +x "$DEST/nx"
echo "Installed nx to $DEST/nx"
echo "Ensure $DEST is on your PATH."
install_extension
