#!/bin/sh
# Test install.sh against a fake release, served locally the way GitHub
# serves releases (including the /latest redirect).
#
#   packaging/test-install.sh target/release/ruckus
set -eu

bin=$1
root=$(cd "$(dirname "$0")/.." && pwd)
version=$("$bin" --version | awk '{ print $2 }')
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) target=aarch64-apple-darwin ;;
    Darwin-x86_64) target=x86_64-apple-darwin ;;
    Linux-aarch64) target=aarch64-unknown-linux-gnu ;;
    Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
esac

work=$(mktemp -d)
trap 'kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true; rm -rf "$work"' EXIT

# A release laid out like the release workflow's.
release=$work/releases/download/v$version
name=ruckus-$version-$target
mkdir -p "$release" "$work/pkg/$name/completions"
cp "$bin" "$root/LICENSE" "$root/README.md" "$work/pkg/$name/"
tar -C "$work/pkg" -czf "$release/$name.tar.gz" "$name"
(cd "$release" && { sha256sum "$name.tar.gz" 2>/dev/null || shasum -a 256 "$name.tar.gz"; } > SHA256SUMS)

cat > "$work/serve.py" <<'PY'
import http.server, os, sys
version = sys.argv[2]
class Handler(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path.rstrip("/").endswith("/releases/latest"):
            self.send_response(302)
            self.send_header("Location", f"/releases/tag/v{version}")
            self.end_headers()
        elif "/releases/tag/" in self.path:
            self.send_response(200)
            self.end_headers()
        else:
            super().do_GET()
    do_HEAD = do_GET
    def log_message(self, *args):
        pass
os.chdir(sys.argv[1])
server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
PY
python3 -u "$work/serve.py" "$work" "$version" > "$work/port" 2> "$work/server.err" &
server=$!
# Python can take a while to start on a cold CI runner.
tries=0
until [ -s "$work/port" ]; do
    tries=$((tries + 1))
    if [ "$tries" -gt 60 ]; then
        echo "the test server did not start ($(command -v python3), $(python3 --version 2>&1)):" >&2
        cat "$work/server.err" >&2
        exit 1
    fi
    sleep 0.5
done
base=http://127.0.0.1:$(cat "$work/port")/releases

echo "== latest release"
RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bin sh "$root/install.sh"
test "$("$work/bin/ruckus" --version)" = "ruckus $version"

echo "== pinned version, over the existing binary"
RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bin RUCKUS_VERSION=v$version sh "$root/install.sh"
test "$("$work/bin/ruckus" --version)" = "ruckus $version"

echo "== a corrupted archive is refused"
cp "$release/SHA256SUMS" "$work/SHA256SUMS.good"
printf '%s  %s\n' 0000000000000000000000000000000000000000000000000000000000000000 "$name.tar.gz" > "$release/SHA256SUMS"
if RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bad sh "$root/install.sh" 2> "$work/err"; then
    echo "install.sh accepted a bad checksum" >&2
    exit 1
fi
grep -q "checksum mismatch" "$work/err"
test ! -e "$work/bad/ruckus"
cp "$work/SHA256SUMS.good" "$release/SHA256SUMS"

echo "== a missing version is reported"
if RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bad RUCKUS_VERSION=9.9.9 sh "$root/install.sh" 2> "$work/err"; then
    echo "install.sh installed a version that does not exist" >&2
    exit 1
fi
grep -q "no release archive" "$work/err"

# A PATH with the tools install.sh uses but no tmux, plus a fake package manager.
sandbox() {
    sb=$work/path-$1
    mkdir -p "$sb"
    for tool in uname curl sed mktemp rm awk sha256sum shasum perl tar gzip mkdir cp chmod mv id sysctl cat touch; do
        real=$(command -v "$tool" 2>/dev/null) && ln -sf "$real" "$sb/$tool"
    done
    printf '#!/bin/sh\nexec "$@"\n' > "$sb/sudo"
    chmod +x "$sb/sudo"
    echo "$sb"
}
fake() { # fake NAME SANDBOX: records its arguments; `apt-get install` fails until `update`
    cat > "$2/$1" <<SH
#!/bin/sh
echo "$1 \$*" >> "$work/calls"
if [ "$1 \$1" = "apt-get install" ] && [ ! -e "$work/updated" ]; then exit 100; fi
if [ "\$1" = update ]; then touch "$work/updated"; fi
SH
    chmod +x "$2/$1"
}

echo "== tmux missing, apt: installs it, updating package lists when needed"
sb=$(sandbox apt); fake apt-get "$sb"
PATH=$sb RUCKUS_INSTALL_TMUX=1 RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bin RUCKUS_VERSION=$version \
    /bin/sh "$root/install.sh" 2> "$work/err" || { cat "$work/err"; exit 1; }
printf 'apt-get install -y tmux\napt-get update\napt-get install -y tmux\n' | diff - "$work/calls" || { cat "$work/err"; exit 1; }
grep -q "installed tmux" "$work/err"

echo "== tmux missing, brew"
rm -f "$work/calls"
sb=$(sandbox brew); fake brew "$sb"
PATH=$sb RUCKUS_INSTALL_TMUX=1 RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bin RUCKUS_VERSION=$version \
    /bin/sh "$root/install.sh" 2> "$work/err" || { cat "$work/err"; exit 1; }
test "$(cat "$work/calls")" = "brew install tmux"

echo "== tmux missing, told not to: only a note"
rm -f "$work/calls"
PATH=$sb RUCKUS_INSTALL_TMUX=0 RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bin RUCKUS_VERSION=$version \
    /bin/sh "$root/install.sh" 2> "$work/err" || { cat "$work/err"; exit 1; }
test ! -e "$work/calls"
grep -q "install it with: brew install tmux" "$work/err"

echo "install.sh: all checks passed"
