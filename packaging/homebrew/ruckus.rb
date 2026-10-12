# Template for the formula in a future tap, SeamusMullan/homebrew-ruckus.
# The release workflow (.github/workflows/release.yml) fills in the version and
# checksums and attaches the result to each GitHub release as ruckus.rb.
class Ruckus < Formula
  desc "Install ROS 2, manage its environments and start sessions in tmux"
  homepage "https://github.com/SeamusMullan/ruckus"
  version "@VERSION@"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/SeamusMullan/ruckus/releases/download/v#{version}/ruckus-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "@SHA256:aarch64-apple-darwin@"
    end
    on_intel do
      url "https://github.com/SeamusMullan/ruckus/releases/download/v#{version}/ruckus-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "@SHA256:x86_64-apple-darwin@"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/SeamusMullan/ruckus/releases/download/v#{version}/ruckus-#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA256:aarch64-unknown-linux-gnu@"
    end
    on_intel do
      url "https://github.com/SeamusMullan/ruckus/releases/download/v#{version}/ruckus-#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "@SHA256:x86_64-unknown-linux-gnu@"
    end
  end

  depends_on "tmux"

  def install
    bin.install "ruckus"
    bash_completion.install "completions/ruckus.bash" => "ruckus"
    zsh_completion.install "completions/_ruckus"
    fish_completion.install "completions/ruckus.fish"
  end

  test do
    assert_equal "ruckus #{version}", shell_output("#{bin}/ruckus --version").strip
    assert_match "turtle", shell_output("#{bin}/ruckus ls")
    assert_match "#compdef ruckus", shell_output("#{bin}/ruckus completions zsh")
    system bin/"ruckus", "up", "turtle", "--dry-run"
  end
end
