#!/usr/bin/env bash
# Diffs what guest32-image decodes from a real 32-bit program against LLVM's
# own tools: every rebase/bind/weak/lazy record (segment, section, address,
# type, addend, library, symbol), every symbol table entry and every section.
# Fails on any difference. Optional checks: EXPECT_PLAN='slide=0x0 entry=0x... thumb=true kind=main'.
#
# usage: compare-llvm.sh <program> <label> [arch]   (arch e.g. armv7 for universal files)
set -euo pipefail
program=$1
label=$2
arch=${3:-}
here=$(cd "$(dirname "$0")/.." && pwd)
out=${OUT_DIR:-$HOME/.cache/ios-debug/missing/scratch-guest32-image/compare}/$label
mkdir -p "$out"
arch_flag=()
[[ -n $arch ]] && arch_flag=(--arch="$arch")

cargo build --quiet --release --example inspect --manifest-path "$here/Cargo.toml"
inspect=$(cargo metadata --format-version 1 --no-deps --manifest-path "$here/Cargo.toml" \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release/examples/inspect

"$inspect" "$program" > "$out/summary.txt"
cat "$out/summary.txt"

status=0
compare() { # name ours theirs
  if diff -u "$3" "$2" > "$out/$1.diff"; then
    echo "MATCH $1: $(wc -l < "$2") lines"
  else
    echo "DIFF $1: see $out/$1.diff ($(grep -c '^[-+][^-+]' "$out/$1.diff") changed lines)"
    status=1
  fi
}

"$inspect" "$program" --fixups > "$out/fixups.ours"
llvm-objdump --macho "${arch_flag[@]}" --rebase --bind --lazy-bind --weak-bind "$program" \
  | sed -n '/^Rebase table:/,$p' | grep -v '^$' > "$out/fixups.llvm"
compare fixups "$out/fixups.ours" "$out/fixups.llvm"

"$inspect" "$program" --symbols > "$out/symbols.ours"
llvm-nm "${arch_flag[@]}" -a -p -x "$program" | grep -E '^[0-9a-f]{8} ' > "$out/symbols.llvm"
compare symbols "$out/symbols.ours" "$out/symbols.llvm"

"$inspect" "$program" --sections > "$out/sections.ours"
llvm-objdump --macho "${arch_flag[@]}" -h "$program" \
  | awk '$1 ~ /^[0-9]+$/ {print $1, $2, $3, $4}' > "$out/sections.llvm"
compare sections "$out/sections.ours" "$out/sections.llvm"

# Both sorted: llvm prints in trie walk order, ours by name.
"$inspect" "$program" --exports | LC_ALL=C sort > "$out/exports.ours"
llvm-objdump --macho "${arch_flag[@]}" --exports-trie "$program" \
  | { grep -E '^0x' || true; } | LC_ALL=C sort > "$out/exports.llvm"
compare exports "$out/exports.ours" "$out/exports.llvm"

if [[ -n ${EXPECT_PLAN:-} ]]; then
  if grep -q "^plan: ${EXPECT_PLAN} " "$out/summary.txt"; then
    echo "MATCH plan: $EXPECT_PLAN"
  else
    echo "DIFF plan: wanted '$EXPECT_PLAN', got '$(grep '^plan' "$out/summary.txt")'"
    status=1
  fi
fi
exit $status
