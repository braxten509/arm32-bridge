#!/usr/bin/env bash
# check-library.sh <lib.so>: refuses what the translator's ELF loader
# (src/elf.rs) can't load, and writes <lib.so>.imports and <lib.so>.exports
# (C names, one per line): the library's contract with the runner's C table.
set -euo pipefail
LIB=$1

problems=0
if llvm-readelf -l "$LIB" | grep -q ' TLS '; then
    echo "check-library: $LIB has thread-local storage (src/elf.rs can't load it)" >&2
    problems=1
fi
# Relocations src/elf.rs applies: R_ARM_RELATIVE, ABS32, GLOB_DAT, JUMP_SLOT.
others=$(llvm-readelf -r "$LIB" | awk '$3 ~ /^R_ARM_/ { print $3 }' | sort -u |
    grep -v -x -E 'R_ARM_(RELATIVE|ABS32|GLOB_DAT|JUMP_SLOT)' || true)
if [ -n "$others" ]; then
    echo "check-library: $LIB needs relocations src/elf.rs doesn't apply: $others" >&2
    problems=1
fi
if ! llvm-readelf -d "$LIB" | grep -q '(HASH)'; then
    echo "check-library: $LIB has no DT_HASH (link with --hash-style=sysv)" >&2
    problems=1
fi
if llvm-readelf -d "$LIB" | grep -q '(NEEDED)'; then
    echo "check-library: $LIB needs other libraries; it may only import from the C table" >&2
    problems=1
fi
[ $problems = 0 ] || exit 1

llvm-nm -D --undefined-only "$LIB" | awk '{ print $NF }' | sort -u >"$LIB.imports"
llvm-nm -D --defined-only "$LIB" | awk '{ print $NF }' | sort -u >"$LIB.exports"
echo "check-library: $(wc -l <"$LIB.exports") exports, $(wc -l <"$LIB.imports") imports from the C table"
