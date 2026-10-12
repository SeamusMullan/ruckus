#!/bin/sh
# Install ruckus from its GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/SeamusMullan/ruckus/main/install.sh | sh
#
# Settings, as environment variables:
#   RUCKUS_VERSION      version to install, e.g. 0.1.0 [default: the latest release]
#   RUCKUS_INSTALL_DIR  where to put the binary [default: ~/.local/bin]
#   RUCKUS_BASE_URL     where releases live [default: GitHub]; used by the tests
set -eu

repo=SeamusMullan/ruckus
base=${RUCKUS_BASE_URL:-https://github.com/$repo/releases}
dir=${RUCKUS_INSTALL_DIR:-$HOME/.local/bin}

say() { printf 'ruckus-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    final_url() { curl -fsSLI -o /dev/null -w '%{url_effective}' "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    final_url() { wget -S --spider "$1" 2>&1 | sed -n 's/^ *Location: //p' | tail -n 1; }
else
    die "needs curl or wget"
fi

case "$(uname -s)" in
    Darwin) os=apple-darwin ;;
    Linux) os=unknown-linux-gnu ;;
    *) die "ruckus runs on macOS and Linux, not $(uname -s)" ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) die "no ruckus build for $(uname -m)" ;;
esac
# An x86_64 shell under Rosetta on an Apple Silicon Mac: use the native build.
if [ "$os" = apple-darwin ] && [ "$arch" = x86_64 ] &&
    [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
    arch=aarch64
fi
target=$arch-$os

version=${RUCKUS_VERSION:-}
if [ -z "$version" ]; then
    # .../releases/latest redirects to .../releases/tag/v<version>.
    tag=$(final_url "$base/latest" | sed -n 's#.*/tag/##p')
    [ -n "$tag" ] || die "cannot find the latest release at $base"
    version=${tag#v}
fi
version=${version#v}

name=ruckus-$version-$target
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading ruckus $version for $target"
fetch "$base/download/v$version/$name.tar.gz" "$tmp/$name.tar.gz" ||
    die "no release archive $name.tar.gz (see $base)"
fetch "$base/download/v$version/SHA256SUMS" "$tmp/SHA256SUMS" ||
    die "no SHA256SUMS in release v$version"

expected=$(awk -v file="$name.tar.gz" '$2 == file { print $1 }' "$tmp/SHA256SUMS")
[ -n "$expected" ] || die "SHA256SUMS has no entry for $name.tar.gz"
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$tmp/$name.tar.gz" | awk '{ print $1 }')
else
    actual=$(shasum -a 256 "$tmp/$name.tar.gz" | awk '{ print $1 }')
fi
[ "$actual" = "$expected" ] || die "checksum mismatch for $name.tar.gz"

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$dir"
# Install beside the old binary, then rename over it, so a running ruckus is never half written.
cp "$tmp/$name/ruckus" "$dir/.ruckus.new"
chmod 755 "$dir/.ruckus.new"
mv "$dir/.ruckus.new" "$dir/ruckus"
say "installed $("$dir/ruckus" --version) to $dir/ruckus"

case ":$PATH:" in
    *":$dir:"*) ;;
    *) say "note: $dir is not on your PATH; add this to your shell's rc file:"
       say "  export PATH=\"$dir:\$PATH\"" ;;
esac
if ! command -v tmux >/dev/null 2>&1; then
    say "note: layouts (ruckus up) need tmux: brew install tmux, or sudo apt install tmux"
fi
say "shell completions: see https://github.com/$repo#shell-completions"
say "next: ruckus install jazzy"
