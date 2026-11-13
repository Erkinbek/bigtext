# Security policy

## Supported versions

Only the latest release gets fixes. BigText offers new versions itself (**Help → Check for
updates…**), and Scoop, Homebrew and `cargo install bigtext` update to them.

## Reporting a vulnerability

Please report it privately: **Security → Report a vulnerability** on
[github.com/Erkinbek/bigtext](https://github.com/Erkinbek/bigtext/security/advisories/new).
Do not open a public issue for it.

Say which version and system you use, what an attacker can do, and how to reproduce it. If a
file triggers it, attach a small one that does, made for the report: never a file with real or
confidential data.

This is a one-person project: you can expect an answer within a week. Once it is fixed, the
release notes and a GitHub security advisory describe it, with credit to you if you want it.

## What counts

BigText opens files from anywhere, so these matter most:

- A file (text, CSV, JSON, a log, an SQL dump, a compressed file or an archive) that makes
  BigText run code, write outside the place you chose, or read or send anything you did not ask
  for.
- Crashes or unbounded memory use caused by a crafted file, when they can be reproduced.
- A file name, remote path, container or service name that ends up running a different command
  than the one shown (BigText runs `ssh`, `docker`, `kubectl` and `journalctl` for remote files
  and logs, and your own command for **Run command on selection…**).
- The updater installing something other than the release it shows. It downloads only from the
  project's GitHub releases and checks the file's SHA-256 against the digest GitHub reports.

A crash on a file you made by hand is welcome as a normal bug report too.
