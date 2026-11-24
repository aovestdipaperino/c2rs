#!/bin/zsh
# usage: run.sh <gname> <src-dir> <corpus-dir|-> <examples-dir|-> <exts>
S=${LITE_DIR:?set LITE_DIR to a work dir holding up-<grammar> clones and out/}
C2RS=${C2RS:-$(cd "$(dirname "$0")" && pwd)}
g=$1; src=$2
$C2RS/target/release/c2rs $src/parser.c $S/out/$g --export-name tree_sitter_${g}_rs || exit 1
cp $S/out/$g/abi.rs $S/out/$g/parser.rs $C2RS/harness/gen/
cd $C2RS/harness && rm -f target/release/diffh && C_SRC=$src GNAME=$g cargo build --release 2>&1 | grep -E "^error" -A10 | head -40
[ -x target/release/diffh ] || { echo "BUILD FAILED for $g"; exit 1; }
REPS=${REPS:-1} ./target/release/diffh "$3" "$4" "$5" 2>&1 | grep -v "^  ok"
