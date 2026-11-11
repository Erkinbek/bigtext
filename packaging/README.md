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

## Scoop and Homebrew

`scoop-bucket/` and `homebrew-tap/` are the contents of the repositories `Erkinbek/scoop-bigtext`
and `Erkinbek/homebrew-tap`, file for file, at version 0.7.0. Once those repositories exist they
keep themselves up to date, so the copies here are only where they started:

- **Scoop**: the repository is made from the template `ScoopInstaller/BucketTemplate`, with
  `bucket/bigtext.json`, `README.md` and `.github/workflows/install-test.yml` from here (and the
  template's `bucket/app-name.json.template` removed). The template's Excavator workflow checks
  for a new release every four hours and updates the manifest from the release's
  `SHA256SUMS.txt`; the install test installs BigText with a fresh Scoop and runs it.
  `scoop bucket add bigtext https://github.com/Erkinbek/scoop-bigtext`, then
  `scoop install bigtext/bigtext`.
- **Homebrew**: `Formula/bigtext.rb` installs the release binaries (macOS arm64 and x64, Linux
  x64; Homebrew does not quarantine them, so macOS opens them without the unsigned-app warning),
  `Casks/bigtext.rb` the macOS app. The update workflow brings both to the latest release every
  six hours (`.github/update.py` takes the checksums from `SHA256SUMS.txt`); the test workflow
  installs both on macOS and the formula on Linux. `brew install erkinbek/tap/bigtext` or
  `brew install --cask erkinbek/tap/bigtext`.

## AUR

`aur/PKGBUILD` is the package `bigtext-bin` (the Linux release binaries). The workflow
`.github/workflows/aur.yml` sets its version and checksums after each release (or by hand from
the Actions tab), builds it in an Arch Linux container, installs and runs it, and pushes it to
the AUR when the repository variable `AUR_PUBLISH` is `true` and the secret
`AUR_SSH_PRIVATE_KEY` holds a key added to the AUR account. One-time setup:

1. Make an account on [aur.archlinux.org](https://aur.archlinux.org/register) and add the public
   half of an SSH key (`ssh-keygen -t ed25519 -f aur -C bigtext-aur`) under *My Account*.
2. `gh secret set AUR_SSH_PRIVATE_KEY < aur` and `gh variable set AUR_PUBLISH --body true`.
3. Run the `aur` workflow by hand once: the first push creates the package.
