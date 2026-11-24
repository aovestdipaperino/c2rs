#!/bin/zsh
# Line coverage of the Rust scanner and the transpiled lexers under the differential harness.
# usage: coverage.sh <gname> <src-dir> <corpus-dir|-> <examples-dir|-> <exts> <files-list|->
# Env: LITE_DIR, RS_SCANNER (as run.sh); any harness knobs (FUZZ, PREFIX, INCR, MAXFILES, ...).
S=${LITE_DIR:?set LITE_DIR}
C2RS=${C2RS:-$(cd "$(dirname "$0")" && pwd)}
g=$1; src=$2
BIN=${LLVM_BIN:-$(ls -d $HOME/.rustup/toolchains/1.95.0-aarch64-apple-darwin/lib/rustlib/*/bin)}
LLVM_COV=${LLVM_COV:-$BIN/llvm-cov}; LLVM_PROFDATA=${LLVM_PROFDATA:-$BIN/llvm-profdata}
$C2RS/target/release/c2rs $src/parser.c $S/out/$g --export-name tree_sitter_${g}_rs || exit 1
cp $S/out/$g/abi.rs $S/out/$g/parser.rs $C2RS/harness/gen/
cd $C2RS/harness
export CARGO_TARGET_DIR=$C2RS/harness/target-cov
rm -rf $CARGO_TARGET_DIR/cov && mkdir -p $CARGO_TARGET_DIR/cov
rm -f $CARGO_TARGET_DIR/release/diffh
C_SRC=$src GNAME=$g RUSTFLAGS="-C instrument-coverage" cargo +1.95.0-aarch64-apple-darwin build --release 2>&1 | grep -E "^error" -A10 | head -30
[ -x $CARGO_TARGET_DIR/release/diffh ] || { echo "BUILD FAILED"; exit 1; }
[ "$6" != "-" ] && export FILES=$6
LLVM_PROFILE_FILE=$CARGO_TARGET_DIR/cov/p-%p.profraw REPS=1 $CARGO_TARGET_DIR/release/diffh "$3" "$4" "$5" 2>&1 | grep -v "^  ok" | tail -8
$LLVM_PROFDATA merge -sparse $CARGO_TARGET_DIR/cov/*.profraw -o $CARGO_TARGET_DIR/cov/all.profdata || exit 1
echo "== coverage (Rust scanner + transpiled lexers; excludes the harness itself)"
$LLVM_COV report $CARGO_TARGET_DIR/release/diffh -instr-profile=$CARGO_TARGET_DIR/cov/all.profdata \
  -ignore-filename-regex='(main\.rs|/rustc/|\.cargo/registry|abi\.rs)' 2>&1 | awk 'NR<=2 || /scanner|parser|TOTAL/ {print}' | cut -c1-170
