#!/bin/sh
# Install ruckus from its GitHub releases.
#
#   curl -fsSL https://raw.githubusercontent.com/SeamusMullan/ruckus/main/install.sh | sh
#
# Settings, as environment variables:
#   RUCKUS_VERSION      version to install, e.g. 0.1.0 [default: the latest release]
#   RUCKUS_INSTALL_DIR  where to put the binary [default: ~/.local/bin]
#   RUCKUS_INSTALL_TMUX 1 to install tmux if it is missing without asking, 0 never to
#                       [default: ask, or skip when there is no terminal to ask on]
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

# Layouts (ruckus up) need tmux. Offer to install it with the system's package manager.
install_tmux() {
    command -v tmux >/dev/null 2>&1 && return 0
    # Running as root, sudo is not needed (and may not exist, as in containers).
    sudo=sudo
    [ "$(id -u)" = 0 ] && sudo=
    if command -v brew >/dev/null 2>&1; then
        pm=brew cmd="brew install tmux"
    elif command -v apt-get >/dev/null 2>&1; then
        pm=apt cmd="${sudo:+sudo }apt-get install -y tmux"
    elif command -v dnf >/dev/null 2>&1; then
        pm=dnf cmd="${sudo:+sudo }dnf install -y tmux"
    elif command -v pacman >/dev/null 2>&1; then
        pm=pacman cmd="${sudo:+sudo }pacman -S --noconfirm tmux"
    else
        say "note: layouts (ruckus up) need tmux; install it with your package manager"
        return 0
    fi
    case "${RUCKUS_INSTALL_TMUX:-ask}" in
        0) answer=n ;;
        1) answer=y ;;
        *)
            # stdin is the script itself under `curl | sh`, so ask on the terminal.
            if { : </dev/tty; } 2>/dev/null; then
                printf 'ruckus-install: layouts need tmux, which is not installed. Run `%s` now? [Y/n] ' "$cmd" >&2
                read -r answer </dev/tty || answer=n
                [ -n "$answer" ] || answer=y
            else
                answer=n
            fi
            ;;
    esac
    case "$answer" in
        [Yy]*) ;;
        *) say "note: layouts (ruckus up) need tmux; install it with: $cmd"; return 0 ;;
    esac
    say "running: $cmd"
    case $pm in
        brew) brew install tmux ;;
        # A fresh system or container may have no package lists yet.
        apt) $sudo apt-get install -y tmux || { $sudo apt-get update && $sudo apt-get install -y tmux; } ;;
        dnf) $sudo dnf install -y tmux ;;
        pacman) $sudo pacman -S --noconfirm tmux ;;
    esac && say "installed tmux" || say "warning: could not install tmux; run \`$cmd\` yourself"
}

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
install_tmux
say "shell completions: see https://github.com/$repo#shell-completions"
say "next: ruckus install jazzy"
