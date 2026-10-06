# Homebrew formula for Mellow. Builds from the tagged source release, so the
# same formula works on macOS and Linux.
#
# Publish by copying this file to Formula/mellow.rb in the tap repository
# (ritru-labs/homebrew-mellow), then: brew install ritru-labs/mellow/mellow
#
# After tagging a release, refresh url and sha256 with:
#   scripts/update-homebrew-formula.sh vX.Y.Z
class Mellow < Formula
  desc "Modern, visual-first terminal text editor"
  homepage "https://github.com/ritru-labs/mellow"
  url "https://github.com/ritru-labs/mellow/archive/refs/tags/v0.1.2.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "Apache-2.0"
  head "https://github.com/ritru-labs/mellow.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match "mellow #{version}", shell_output("#{bin}/mellow --version")
  end
end
