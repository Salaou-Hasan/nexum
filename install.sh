# Nexum installer (Unix). Usage: curl -fsSL https://.../install.sh | sh
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
DEST="${HOME}/.local/bin"
mkdir -p "$DEST"
echo "Downloading $URL ..."
curl -fsSL "$URL" -o "$DEST/nx"
chmod +x "$DEST/nx"
echo "Installed nx to $DEST/nx"
echo "Ensure $DEST is on your PATH."