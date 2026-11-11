"""Sets the formula and the cask to a release.

    python3 update.py VERSION SHA256SUMS.txt Formula/bigtext.rb Casks/bigtext-app.rb

The checksums come from the release's SHA256SUMS.txt: in the formula, the sha256 under each url
gets the checksum of the file that url names; in the cask, `arm:` and `intel:` get those of the
two app zips.
"""
import re
import sys


def replace(text, pattern, repl, count, path):
    new, n = re.subn(pattern, repl, text, flags=re.M)
    if n != count:
        raise SystemExit(f"{path}: expected {count} match(es) for {pattern!r}, found {n}")
    return new


def main(version, sums_path, formula, cask):
    sums = {}
    with open(sums_path, encoding="utf-8") as f:
        for line in f:
            if line.strip():
                digest, name = line.split()
                sums[name.lstrip("*")] = digest
    version_line = r'^(\s*version ")[^"]+(")'

    with open(formula, encoding="utf-8") as f:
        text = f.read()
    text = replace(text, version_line, rf"\g<1>{version}\2", 1, formula)
    text = replace(
        text,
        r'(url "[^"\n]*/(bigtext-[^"/\n]+)"\n\s*sha256 ")[0-9a-f]{64}(")',
        lambda m: m.group(1) + sums[m.group(2)] + m.group(3),
        3,
        formula,
    )
    with open(formula, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)

    with open(cask, encoding="utf-8") as f:
        text = f.read()
    text = replace(text, version_line, rf"\g<1>{version}\2", 1, cask)
    for key, name in (("arm", "bigtext-macos-arm64.app.zip"), ("intel", "bigtext-macos-x64.app.zip")):
        text = replace(text, rf'(\b{key}:\s*")[0-9a-f]{{64}}(")', rf"\g<1>{sums[name]}\2", 1, cask)
    with open(cask, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


if __name__ == "__main__":
    if len(sys.argv) != 5:
        raise SystemExit(__doc__)
    main(*sys.argv[1:])
