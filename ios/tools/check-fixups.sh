#!/bin/bash
# Compares ios-run's fix-up lists with llvm-objdump's for one program.
# usage: check-fixups.sh <program (universal or 64-bit)> [ios-run binary]
set -euo pipefail
PROGRAM=$1
RUN=${2:-$(dirname "$0")/../../target/release/ios-run}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
llvm-lipo "$PROGRAM" -thin arm64 -output "$WORK/thin" 2>/dev/null || cp "$PROGRAM" "$WORK/thin"
"$RUN" fixups "$WORK/thin" > "$WORK/ours"
# Apple's tool: rebase rows end in "pointer"; bind rows hold address, type, addend, dylib, symbol.
llvm-objdump --macho --rebase "$WORK/thin" | awk '$NF=="pointer"{print "rebase " $3}' | sort > "$WORK/apple-rebase"
llvm-objdump --macho --bind "$WORK/thin" | awk '$4=="pointer"{print "bind " $3 " " $5 " " $6 " " $7}' | sort > "$WORK/apple-bind"
llvm-objdump --macho --lazy-bind "$WORK/thin" | awk '$3 ~ /^0x/ && NF>=5{print "lazy " $3 " 0 " $4 " " $5}' | sort > "$WORK/apple-lazy"
llvm-objdump --macho --weak-bind "$WORK/thin" | awk '$4=="pointer"{print "weak " $3 " " $5 " " $6}' | sort > "$WORK/apple-weak"
normalize() { awk '{ $2 = sprintf("0x%x", strtonum($2)); if (NF >= 4 && $1 != "weak") sub(/\..*/, "", $4); print }' | sort; }
status=0
for kind in rebase bind lazy weak; do
  case $kind in
    weak) grep "^weak " "$WORK/ours" | awk '{print $1, $2, $3, $5}' | normalize > "$WORK/ours-$kind" ;;
    *) grep "^$kind " "$WORK/ours" | normalize > "$WORK/ours-$kind" ;;
  esac
  normalize < "$WORK/apple-$kind" > "$WORK/apple-$kind.n"
  ours=$(wc -l < "$WORK/ours-$kind"); apple=$(wc -l < "$WORK/apple-$kind.n")
  if cmp -s "$WORK/ours-$kind" "$WORK/apple-$kind.n"; then
    echo "$kind: $ours match"
  else
    echo "$kind: DIFFERENT (ours $ours, Apple's tool $apple)"; { diff "$WORK/ours-$kind" "$WORK/apple-$kind.n" || true; } | head -5
    status=1
  fi
done
exit $status
