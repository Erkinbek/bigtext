# Homebrew formula that builds BigText from source, for Linux and macOS (goes to
# Formula/bigtext.rb in a tap repository). For a new release: set `url` and `sha256` (of the tag's
# source tarball: curl -L <url> | shasum -a 256).
class Bigtext < Formula
  desc "Text editor for huge files that never freezes"
  homepage "https://github.com/Erkinbek/bigtext"
  url "https://github.com/Erkinbek/bigtext/archive/refs/tags/v0.4.0.tar.gz"
  sha256 "REPLACE_WITH_SHA256_OF_THE_SOURCE_TARBALL"
  license "MIT"

  depends_on "rust" => :build

  on_linux do
    depends_on "libxkbcommon"
    depends_on "mesa"
    depends_on "wayland"
  end

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    (testpath/"app.log").write "2024-05-01 10:00:00 INFO a\n2024-05-01 10:00:01 ERROR b\n"
    assert_equal "1", shell_output("#{bin}/bigtext-cli count ERROR #{testpath}/app.log").strip
  end
end
