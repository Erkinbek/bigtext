# Homebrew tap for BigText

[BigText](https://github.com/Erkinbek/bigtext) opens, searches and edits text and log files of
any size (1–50+ GB) without freezing.

## Install

The command-line build, for macOS and Linux (`bigtext` opens the window, `bigtext-cli` is for
scripts):

```
brew install erkinbek/tap/bigtext
```

The app for the Applications folder (macOS):

```
brew install --cask erkinbek/tap/bigtext-app
```

The app is not signed, so macOS may refuse to open it the first time; the cask's notes show the
command that lets it run. The command-line build opens without asking. Both put `bigtext-cli` on
the PATH, so install one of them.

## Update

```
brew upgrade bigtext          # or: brew upgrade --cask bigtext-app
```

A workflow brings the formula and the cask to each new BigText release within six hours.
