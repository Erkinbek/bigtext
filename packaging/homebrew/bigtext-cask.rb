# Homebrew cask for the macOS app (goes to Casks/bigtext.rb in a tap repository).
# For a new release: set `version`, and the two sha256 values from the release's SHA256SUMS.txt.
cask "bigtext" do
  arch arm: "arm64", intel: "x64"

  version "0.4.1"
  sha256 arm:   "REPLACE_WITH_SHA256_OF_bigtext-macos-arm64.app.zip",
         intel: "REPLACE_WITH_SHA256_OF_bigtext-macos-x64.app.zip"

  url "https://github.com/Erkinbek/bigtext/releases/download/v#{version}/bigtext-macos-#{arch}.app.zip"
  name "BigText"
  desc "Text editor for huge files that never freezes"
  homepage "https://github.com/Erkinbek/bigtext"

  depends_on macos: ">= :big_sur"

  app "BigText.app"
  binary "#{appdir}/BigText.app/Contents/MacOS/bigtext-cli"

  caveats <<~EOS
    BigText is not signed. If macOS refuses to open it, run once:
      xattr -dr com.apple.quarantine "#{appdir}/BigText.app"
  EOS
end
