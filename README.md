# c2rs

Transpile tree-sitter generated `parser.c` (ABI 15) into pure Rust: dense tables, a `match`-based
lexer, and a `#[repr(C)]` `TSLanguage` static that the existing tree-sitter runtime consumes.
External scanners (`scanner.c`) are ported by hand; the harness verifies them against the C.

```
c2rs <parser.c> <out-dir> [--export-name NAME]     # writes abi.rs + parser.rs
```

## Testing

- `cargo test`: unit tests for the transpiler (escapes, designated initializers, macros, lexer
  statements, and regressions such as 32-bit character ranges vs 16-bit fields).
- `run.sh <grammar> <src-dir> <corpus|-> <examples|-> <exts>`: differential test of one grammar.
  Builds the original C and the Rust output side by side (set `RS_SCANNER=<scanner.rs>` to swap in a
  Rust scanner) and compares:
  - every table (byte-for-byte or semantically),
  - syntax trees for the grammar corpus and examples,
  - optional knobs, all deterministic: `FILES=<list>` real-world files (`MAXFILES`), `FUZZ=<n>`
    byte-level mutants per input (`FUZZ_SEED`, `FUZZ_INPUTS`), `PREFIX=<n>` truncation points,
    `INCR=<n>` incremental edits and reparses (exercises scanner serialize/deserialize),
    `SOUP=<n>` random sentences built from the grammar's own literal tokens plus exotic Unicode,
    `DIRECT=1` calls the C and Rust lexer functions for every lex state and the scanners for every
    subset of valid symbols on a mock lexer, comparing every advance/skip/mark_end call and the
    serialized state, with no parser in between,
  - `GOLDEN=<file>` records `<hex input>\t<sexp>` cases from the C parser for use as golden tests,
  - `REPRO=<file>` parses one file with both and prints both trees.
- `coverage.sh`: same arguments plus a file list; reports line coverage of the Rust scanner and the
  transpiled lexers (needs `rustup component add llvm-tools --toolchain 1.95.0`).
