#!/bin/sh
# Downloads the latest monitorzinho release binary and installs it to
# ~/.local/bin. Usage:
#   curl -fsSL https://raw.githubusercontent.com/willguitaradmfar/monitorzinho/main/install.sh | sh
set -eu

REPO="willguitaradmfar/monitorzinho"
ASSET="monitorzinho-linux-x86_64"
DEST_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
DEST="$DEST_DIR/monitorzinho"

os="$(uname -s)"
arch="$(uname -m)"
if [ "$os" != "Linux" ] || { [ "$arch" != "x86_64" ] && [ "$arch" != "amd64" ]; }; then
    echo "Error: pre-built binaries are only available for Linux x86_64 right now (got $os/$arch)." >&2
    echo "Build from source instead: https://github.com/$REPO#build-from-source" >&2
    exit 1
fi

mkdir -p "$DEST_DIR"

url="https://github.com/$REPO/releases/latest/download/$ASSET"
echo "Downloading monitorzinho from $url ..."
curl -fsSL "$url" -o "$DEST"
chmod +x "$DEST"

echo "Installed to $DEST"

case ":$PATH:" in
    *":$DEST_DIR:"*) ;;
    *)
        echo
        echo "$DEST_DIR is not in your PATH. Add this to your shell profile (~/.bashrc, ~/.zshrc, ...):"
        echo "  export PATH=\"$DEST_DIR:\$PATH\""
        ;;
esac

echo "Run 'monitorzinho' to start."
