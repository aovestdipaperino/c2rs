# c2rs

[![CI](https://github.com/aovestdipaperino/c2rs/actions/workflows/ci.yml/badge.svg)](https://github.com/aovestdipaperino/c2rs/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024-orange.svg)](https://www.rust-lang.org)

Turn a tree-sitter generated `parser.c` into pure Rust.

Every tree-sitter grammar ships a generated `parser.c`: a flat list of tables, two lexer state
machines and one `TSLanguage` struct. `c2rs` reads that file and emits the same thing as Rust, so a
grammar crate needs no C compiler and no `build.rs`. The generated code plugs into the existing
tree-sitter runtime through the usual `tree_sitter_<name>()` symbol.

```
c2rs <parser.c> <out-dir> [--export-name NAME]     # writes abi.rs + parser.rs
```

## What it produces

- Dense Rust arrays for every table: parse table, small parse table, parse actions, symbol names and
  metadata, field maps, lex modes, alias and supertype maps, reserved words, external scanner states.
- The two lexers (`ts_lex` and `ts_lex_keywords`) as `match`-based state machines, with the same
  `advance`, `skip` and `mark_end` calls the C makes.
- A `#[repr(C)]` `TSLanguage` static, exported as `#[no_mangle] extern "C" fn tree_sitter_<name>()`.

It does not generate a grammar from `grammar.js` (use the tree-sitter CLI for that), and it does
not port external scanners (`scanner.c`): those are hand-written C and are ported by hand. The
generated code calls them through `extern "C"`, so a C scanner or a Rust port works unchanged.
The tree-sitter runtime that drives the parser is still C.

## Used by

[`tree-sitter-lite-rs`](https://github.com/aovestdipaperino/tree-sitter-lite-rs): pure-Rust
javascript, typescript, tsx, python, rust, go, java and c grammars, checked against the original C
and against tokensave's full integration suite.

## How it is tested

A transpiler that is "almost right" is worse than none, so the harness compares the Rust output with
the original C in several independent ways. `run.sh <grammar> <src-dir> <corpus|-> <examples|-> <exts>`
builds both side by side and compares:

- every table, byte for byte or semantically;
- syntax trees for the grammar's corpus and examples;
- optional, deterministic knobs: `FILES=<list>` real-world files, `FUZZ=<n>` byte-level mutants,
  `PREFIX=<n>` truncation points, `INCR=<n>` incremental edits and reparses (exercises scanner
  serialize and deserialize), `SOUP=<n>` random sentences built from the grammar's own literal
  tokens plus exotic Unicode;
- `DIRECT=1`: calls the C and Rust lexers for every lex state, and the scanners for every subset of
  valid symbols, on a mock lexer, comparing every `advance`, `skip` and `mark_end` call and the
  serialized state, with no parser in between;
- `RS_SCANNER=<scanner.rs>` swaps a Rust scanner in for the C one, so ports are checked the same way;
- `GOLDEN=<file>` records `<hex input>\t<sexp>` cases from the C parser for use as golden tests;
- `REPRO=<file>` parses one file with both and prints both trees.

`cargo test` runs the transpiler's own unit tests (escapes, designated initializers, macros, lexer
statements, and regressions such as 32-bit character ranges versus 16-bit fields). `coverage.sh`
reports line coverage of the Rust scanner and the transpiled lexers; it needs
`rustup component add llvm-tools`.

The harness has found real bugs: a 16-bit truncation of character ranges that made emoji in
identifiers a syntax error, found by token soup and then pinned by a unit test.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | the transpiler and its unit tests |
| `src/abi.rs.tmpl` | the ABI 15 types written next to the generated parser |
| `harness/` | the differential test harness |
| `run.sh`, `coverage.sh` | one grammar end to end, and the coverage report |

## Star history

[![Star History Chart](https://api.star-history.com/svg?repos=aovestdipaperino/c2rs&type=Date)](https://star-history.com/#aovestdipaperino/c2rs&Date)

## License

[MIT](LICENSE). The ABI layouts mirror tree-sitter's `parser.h`; see [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
