<!-- What does this change, and why? Link the issue if there is one. -->

## Checklist

- [ ] `cargo fmt` and `cargo clippy --all-targets -- -D warnings` (latest stable Rust) are clean.
- [ ] `cargo test --lib --bins` passes; for a change in the window, also `bigtext --self-test`.
- [ ] Files are still never read whole into memory: large inputs stream, with progress and Cancel.
- [ ] New interface texts are translated in all eight languages (`src/app/i18n_*.rs`).
- [ ] FEATURES.md, USAGE.md and CHANGELOG.md (under `## Unreleased`) describe a user-facing change.

<!-- A change for speed: numbers before and after (bigtext-bench or --bench-ui), with the
machine, the file, and whether the OS cache was cold or warm. -->
