# Homebrew formula for the reproit CLI.
#
# Drop this into a tap repo (github.com/ReproIt/homebrew-tap) so users can:
#
#     brew install ReproIt/tap/reproit
#
# This installs the PREBUILT binary from the GitHub Release produced by
# .github/workflows/release.yml (no Rust toolchain needed). On each release the
# `update-homebrew-tap` job in that workflow regenerates this file with the real
# sha256 values and pushes it to the tap, so you don't hand-edit it. To do it
# manually, bump `version` and fill the sha256 values below, printed by the
# release workflow's "Package" step or via:
#
#     shasum -a 256 reproit-vX.Y.Z-<target>.tar.gz
class Reproit < Formula
  desc "Deterministic UI fuzzer: find a bug once, reproduce it forever"
  homepage "https://reproit.com"
  version "0.1.0"
  license "Elastic-2.0"

  on_macos do
    on_arm do
      url "https://github.com/ReproIt/reproit/releases/download/v#{version}/reproit-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/ReproIt/reproit/releases/download/v#{version}/reproit-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/ReproIt/reproit/releases/download/v#{version}/reproit-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "reproit"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/reproit --version")
  end
end
