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
trap 'kill "$server" 2>/dev/null; wait "$server" 2>/dev/null; rm -rf "$work"' EXIT

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
python3 "$work/serve.py" "$work" "$version" > "$work/port" &
server=$!
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -s "$work/port" ] && break; sleep 0.5; done
base=http://127.0.0.1:$(cat "$work/port")/releases

echo "== latest release"
RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bin sh "$root/install.sh"
test "$("$work/bin/ruckus" --version)" = "ruckus $version"

echo "== pinned version, over the existing binary"
RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bin RUCKUS_VERSION=v$version sh "$root/install.sh"
test "$("$work/bin/ruckus" --version)" = "ruckus $version"

echo "== a corrupted archive is refused"
printf '%s  %s\n' 0000000000000000000000000000000000000000000000000000000000000000 "$name.tar.gz" > "$release/SHA256SUMS"
if RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bad sh "$root/install.sh" 2> "$work/err"; then
    echo "install.sh accepted a bad checksum" >&2
    exit 1
fi
grep -q "checksum mismatch" "$work/err"
test ! -e "$work/bad/ruckus"

echo "== a missing version is reported"
if RUCKUS_BASE_URL=$base RUCKUS_INSTALL_DIR=$work/bad RUCKUS_VERSION=9.9.9 sh "$root/install.sh" 2> "$work/err"; then
    echo "install.sh installed a version that does not exist" >&2
    exit 1
fi
grep -q "no release archive" "$work/err"

echo "install.sh: all checks passed"
