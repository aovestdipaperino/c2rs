#!/usr/bin/env bash
# usage: run.sh <gname> <src-dir> <corpus-dir|-> <examples-dir|-> <exts>
# Env: LITE_DIR, RS_SCANNER, PREGEN=<dir with abi.rs + parser.rs to verify instead of transpiling>,
#      and the harness knobs (see README).
S=${LITE_DIR:?set LITE_DIR to a work dir holding up-<grammar> clones and out/}
C2RS=${C2RS:-$(cd "$(dirname "$0")" && pwd)}
g=$1; src=$2
mkdir -p $C2RS/harness/gen
if [ -n "$PREGEN" ]; then
  # verify already-generated files: the crate's own abi.rs and parser.rs (exporting tree_sitter_<g>),
  # with the export renamed to tree_sitter_<g>_rs so both can be linked side by side.
  cp $PREGEN/abi.rs $C2RS/harness/gen/abi.rs
  sed "s/fn tree_sitter_${g}()/fn tree_sitter_${g}_rs()/" $PREGEN/parser.rs > $C2RS/harness/gen/parser.rs
  grep -q "tree_sitter_${g}_rs" $C2RS/harness/gen/parser.rs || { echo "PREGEN: export rename failed"; exit 1; }
else
  $C2RS/target/release/c2rs $src/parser.c $S/out/$g --export-name tree_sitter_${g}_rs || exit 1
  cp $S/out/$g/abi.rs $S/out/$g/parser.rs $C2RS/harness/gen/
fi
cd $C2RS/harness && rm -f target/release/diffh && C_SRC=$src GNAME=$g cargo build --release 2>&1 | grep -E "^error" -A10 | head -40
[ -x target/release/diffh ] || { echo "BUILD FAILED for $g"; exit 1; }
REPS=${REPS:-1} ./target/release/diffh "$3" "$4" "$5" 2>&1 | grep -v "^  ok"
