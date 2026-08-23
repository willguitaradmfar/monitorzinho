#!/bin/sh
# Downloads the latest monitorzinho release and installs it. Usage:
#   curl -fsSL https://raw.githubusercontent.com/willguitaradmfar/monitorzinho/main/install.sh | sh
#
# As root it installs system-wide to /usr/local/bin, which is on everyone's PATH.
# As a normal user it installs to ~/.local/bin. Set DEST_DIR to override either.
set -eu

REPO="willguitaradmfar/monitorzinho"
ASSET="monitorzinho-linux-x86_64"

if [ -n "${DEST_DIR:-}" ]; then
    dest_dir="$DEST_DIR"
elif [ "$(id -u)" = "0" ]; then
    # Root's PATH does not include ~/.local/bin on most distributions, so installing
    # there as root produces a binary that cannot be run by name — which is exactly
    # what happened on a server this was installed on.
    dest_dir="/usr/local/bin"
else
    dest_dir="${XDG_BIN_HOME:-$HOME/.local/bin}"
fi
dest="$dest_dir/monitorzinho"

os="$(uname -s)"
arch="$(uname -m)"
if [ "$os" != "Linux" ] || { [ "$arch" != "x86_64" ] && [ "$arch" != "amd64" ]; }; then
    echo "Error: pre-built binaries are only available for Linux x86_64 right now (got $os/$arch)." >&2
    echo "Build from source instead: https://github.com/$REPO#build-from-source" >&2
    exit 1
fi

mkdir -p "$dest_dir"

url="https://github.com/$REPO/releases/latest/download/$ASSET"
# Downloaded beside the destination rather than onto it: curl truncates its output file
# before it knows whether the download will succeed, so writing straight to $dest turns
# a network failure into a broken install of a program that was working a second ago.
tmp="$(mktemp "$dest_dir/.monitorzinho.XXXXXX")"
trap 'rm -f "$tmp"' EXIT INT TERM

echo "Downloading monitorzinho from $url ..."
curl -fsSL "$url" -o "$tmp"

# The release publishes a SHA-256 next to the binary. Checking it costs one more small
# request and is the difference between "it downloaded" and "it downloaded intact".
if command -v sha256sum >/dev/null 2>&1; then
    expected="$(curl -fsSL "$url.sha256" | awk '{print $1}')" || expected=""
    if [ -n "$expected" ]; then
        actual="$(sha256sum "$tmp" | awk '{print $1}')"
        if [ "$expected" != "$actual" ]; then
            echo "Error: checksum mismatch — refusing to install." >&2
            echo "  expected $expected" >&2
            echo "  got      $actual" >&2
            exit 1
        fi
        echo "Checksum verified."
    else
        echo "Warning: could not fetch the published checksum; installing unverified." >&2
    fi
fi

chmod 755 "$tmp"
# One rename, so the binary is either the old one or the new one and never half of
# either — including for a shell that is running it at this moment.
mv -f "$tmp" "$dest"
trap - EXIT INT TERM

echo "Installed to $dest"
"$dest" --version || true

case ":$PATH:" in
    *":$dest_dir:"*) ;;
    *)
        echo
        echo "$dest_dir is not in your PATH. Add this to your shell profile (~/.bashrc, ~/.zshrc, ...):"
        echo "  export PATH=\"$dest_dir:\$PATH\""
        ;;
esac

echo "Run 'monitorzinho' to start."
