# Contributing

Issues and pull requests are welcome.

- `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test` must pass.
- Any change to the generated output needs a differential run on at least one grammar (see the
  README, "Testing"). If you fix a transpiler bug, add a unit test in `src/main.rs` that pins it.
- Keep the generated code free of `unsafe` beyond what the C ABI requires.
