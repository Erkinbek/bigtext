# Packaging

The release workflow (`.github/workflows/build.yml`) builds these from the release binaries:

| File | Built by | What it is |
| --- | --- | --- |
| `bigtext-setup-windows-x64.exe` | `installer/bigtext.iss` (Inno Setup) | Windows installer with "Open with" and the Explorer menu |
| `bigtext-linux-x64.deb` | `linux/build-deb.sh` | Debian / Ubuntu package: `/usr/bin/bigtext`, `bigtext-cli`, menu entry, icon |
| `bigtext-linux-x64.AppImage` | `linux/build-appimage.sh` | One file that runs on most Linux systems |
| `bigtext-macos-arm64.app.zip`, `…-x64.app.zip` | `macos/build-app.sh` | `BigText.app` for the Applications folder (unsigned) |
| `SHA256SUMS.txt` | the release job | Checksums of all files |

`linux/bigtext.desktop` is the menu entry (also in the Linux tarball, with `icon-256.png`). The
window's app id is `bigtext`, so desktops match it to this entry.

`winget/` holds the winget manifests; the release workflow's winget job submits new versions
once the package is in `microsoft/winget-pkgs`.

## Homebrew

`homebrew/bigtext-cask.rb` (the macOS app) and `homebrew/bigtext-formula.rb` (built from source,
Linux and macOS) are ready for a tap. To publish one:

1. Create the repository `Erkinbek/homebrew-tap` on GitHub.
2. Copy the cask to `Casks/bigtext.rb` and the formula to `Formula/bigtext.rb`.
3. For each release, set `version` (and the formula's `url`) and the `sha256` values: the
   app zips' are in the release's `SHA256SUMS.txt`; the source tarball's is
   `curl -L https://github.com/Erkinbek/bigtext/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256`.

Then: `brew install --cask erkinbek/tap/bigtext` (the app) or `brew install erkinbek/tap/bigtext`.
