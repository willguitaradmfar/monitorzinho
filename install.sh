#!/bin/sh
# Downloads the latest monitorzinho release and installs it. Usage:
#   curl -fsSL https://raw.githubusercontent.com/willguitaradmfar/monitorzinho/main/install.sh | sh
#
# As root it installs system-wide to /usr/local/bin, which is on everyone's PATH.
# As a normal user it installs to ~/.local/bin. Set DEST_DIR to override either.
set -eu

REPO="willguitaradmfar/monitorzinho"

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

# Which C library this machine has decides which build can run at all. A glibc binary
# on a musl-only system (Alpine) installs perfectly and then fails to execute, because
# the loader it names — /lib64/ld-linux-x86-64.so.2 — isn't there; the shell reports
# that as "not found", pointing at the program rather than at what's missing. So the
# choice is made here, before downloading, instead of discovered afterwards.
if [ -e /lib64/ld-linux-x86-64.so.2 ] || [ -e /lib/x86_64-linux-gnu/libc.so.6 ]; then
    libc="gnu"
elif [ -e /lib/ld-musl-x86_64.so.1 ] || [ -x /sbin/apk ]; then
    libc="musl"
else
    # Neither loader is where it usually lives. musl is the safer guess: that build is
    # statically linked and needs no loader at all, so it runs either way.
    echo "Note: could not tell glibc from musl here; using the static (musl) build, which runs on both."
    libc="musl"
fi

if [ "$libc" = "musl" ]; then
    ASSET="monitorzinho-linux-x86_64-musl"
else
    ASSET="monitorzinho-linux-x86_64"
fi

mkdir -p "$dest_dir"

url="https://github.com/$REPO/releases/latest/download/$ASSET"
# Downloaded beside the destination rather than onto it: curl truncates its output file
# before it knows whether the download will succeed, so writing straight to $dest turns
# a network failure into a broken install of a program that was working a second ago.
tmp="$(mktemp "$dest_dir/.monitorzinho.XXXXXX")"
trap 'rm -f "$tmp"' EXIT INT TERM

echo "Downloading monitorzinho ($libc) from $url ..."
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

# Run it *before* it becomes the installed binary. A download that can't execute here
# can't execute after the rename either, and replacing a working install with one that
# only prints "not found" is worse than not installing at all.
if ! version="$("$tmp" --version 2>&1)"; then
    echo "Error: the downloaded binary does not run on this machine." >&2
    echo "  $version" >&2
    echo >&2
    echo "  That message usually means the C library doesn't match: this script picked" >&2
    echo "  the $libc build. Try the other one by hand:" >&2
    other="monitorzinho-linux-x86_64-musl"
    [ "$libc" = "musl" ] && other="monitorzinho-linux-x86_64"
    echo "    curl -fsSL https://github.com/$REPO/releases/latest/download/$other -o $dest" >&2
    echo "    chmod 755 $dest" >&2
    echo >&2
    echo "  Or build from source: https://github.com/$REPO#build-from-source" >&2
    exit 1
fi

# One rename, so the binary is either the old one or the new one and never half of
# either — including for a shell that is running it at this moment.
mv -f "$tmp" "$dest"
trap - EXIT INT TERM

echo "Installed to $dest — $version"

case ":$PATH:" in
    *":$dest_dir:"*) ;;
    *)
        echo
        echo "$dest_dir is not in your PATH. Add this to your shell profile (~/.bashrc, ~/.zshrc, ...):"
        echo "  export PATH=\"$dest_dir:\$PATH\""
        ;;
esac

echo "Run 'monitorzinho' to start."
