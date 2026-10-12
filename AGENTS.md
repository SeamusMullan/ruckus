# Notes for coding agents

ruckus is a Rust CLI (edition 2024) for ROS 2: `ruckus install` (ready-built ROS
from RoboStack via the rattler crates), environments (`env`, `shell`, `run`),
workspace dependencies (`deps`) and tmux layouts (`up`). See README.md for
what it does and docs/configuration.md for its settings.

## Checks

Run these before every commit; CI runs the same on Linux and macOS:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
RUCKUS_NETWORK_TESTS=1 cargo test --locked --test env   # resolves real RoboStack packages
cargo build --release && sh packaging/test-install.sh target/release/ruckus
```

Never touch the user's real `~/.config/ruckus`, `~/.local/share/ruckus` or tmux
sessions: set `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` to a
temporary directory, and `RUCKUS_TMUX_SOCKET` to a private tmux socket.

## Releasing a new version

Publishing is public and cannot be fully undone: a pushed tag and a GitHub
release are seen and downloaded at once. **Do not bump, tag or publish without
the user asking for that release.** Steps 1–5 are local and safe; step 6
publishes.

### 1. Pick the version

ruckus follows [semantic versioning](https://semver.org). While it is 0.x:

- **0.x.Y (patch)**: bug fixes only.
- **0.X.0 (minor)**: new features, or any change that breaks existing configs,
  environment files, `ruckus.lock` files, command names or flags.

Tags are the version with a `v` prefix: `v0.2.0`.

### 2. Start from a green `main`

```sh
git switch main && git pull
git status                                   # must be clean
gh run list --workflow ci.yml --limit 1      # latest run must be success
```

### 3. Bump the version

The version lives only in `Cargo.toml`; `Cargo.lock` must match, or the
`--locked` release build fails.

```sh
# edit Cargo.toml: version = "0.2.0"
cargo build                                  # updates the ruckus entry in Cargo.lock
git diff --stat                              # exactly Cargo.toml and Cargo.lock
```

Nothing else holds the version. The Homebrew formula template uses
placeholders, and `install.sh` installs the latest release.

### 4. Check, commit and push

Run every check in [Checks](#checks), then:

```sh
git commit -am "Release 0.2.0"
git push origin main
gh run watch "$(gh run list --workflow ci.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

Wait until all CI jobs pass, including the real Jazzy and Lyrical installs.

### 5. Dry-run the release (recommended)

A manual run of the release workflow builds and packages everything for all
four targets but publishes nothing:

```sh
gh workflow run release.yml --ref main
gh run watch "$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

### 6. Tag and publish

```sh
git tag -a v0.2.0 -m "ruckus 0.2.0"
git push origin v0.2.0
gh run watch "$(gh run list --workflow release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

The release workflow (`.github/workflows/release.yml`):

1. Fails if the tag does not match the version in `Cargo.toml`.
2. Builds and checks `ruckus --version` on native runners for
   aarch64/x86_64 macOS and aarch64/x86_64 Linux (Ubuntu 22.04, for an older glibc).
3. Packages each as `ruckus-<version>-<target>.tar.gz`, with the binary,
   LICENSE, README.md and shell completions.
4. Writes `SHA256SUMS`, fills in `packaging/homebrew/ruckus.rb` as `ruckus.rb`,
   and creates the GitHub release with all of them and generated notes.
5. Pushes `ruckus.rb` to the tap, `SeamusMullan/homebrew-ruckus`, if the
   `HOMEBREW_TAP_TOKEN` secret is set (see step 7).

Check the release has 6 assets: four archives, `SHA256SUMS` and `ruckus.rb`:

```sh
gh release view v0.2.0 --json assets -q '.assets[].name'
```

### 7. Update the Homebrew tap

If the workflow's "Update the Homebrew tap" step says no `HOMEBREW_TAP_TOKEN`
secret is set, update the tap by hand:

```sh
git clone https://github.com/SeamusMullan/homebrew-ruckus.git /tmp/homebrew-ruckus
gh release download v0.2.0 -R SeamusMullan/ruckus -p ruckus.rb -D /tmp/homebrew-ruckus/Formula --clobber
cd /tmp/homebrew-ruckus && ruby -c Formula/ruckus.rb
git commit -am "ruckus 0.2.0" && git push
```

To automate this, the user (not an agent) creates a fine-grained token with
"Contents: read and write" on `SeamusMullan/homebrew-ruckus` only, and adds it
to this repo as the Actions secret `HOMEBREW_TAP_TOKEN`.

### 8. Verify both install paths

```sh
# The install script, into a throwaway directory:
curl -fsSL https://raw.githubusercontent.com/SeamusMullan/ruckus/main/install.sh \
  | RUCKUS_INSTALL_DIR="$(mktemp -d)" RUCKUS_INSTALL_TMUX=0 sh

# Homebrew:
brew update && brew upgrade ruckus && brew test ruckus && ruckus --version
```

Both must report the new version.

### If something goes wrong

- **The release workflow failed before publishing** (no GitHub release exists):
  fix the problem on `main`, then move the tag to the fixed commit:
  `git tag -d v0.2.0 && git push origin :refs/tags/v0.2.0`, and redo step 6.
- **A release was published and is broken:** never move or reuse a published
  tag, since people may already have downloaded it. Fix the problem on `main`
  and release the next patch version (`v0.2.1`). If the broken release is
  dangerous, ask the user before marking it as a pre-release or deleting it
  (`gh release edit v0.2.0 --prerelease`).
- **The tap points at a broken release:** point it back at the previous good
  `ruckus.rb` (from that release's assets) until the fix is out.
