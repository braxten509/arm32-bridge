#!/bin/bash
# Rewrites ios/cxxrt/exports.txt: the C++ runtime names games may import,
# one per line as "F name" (function) or "D name" (data), from a built
# runtime build's objects. Left out on purpose:
#  - names taking the host's FILE (a game's FILE is not the host's),
#  - long double entry points and numeric_limits<long double>: Apple's
#    long double is 64-bit, the host's 128-bit (their type_info stays),
#  - variadic functions and ones taking a va_list (only
#    std::__convert_from_v, which no game we know imports): Apple's arm64
#    passes variadic arguments on the stack in 8-byte slots, the host in
#    registers, and its va_list is a different type,
#  - Game Bridge's own glue (gamebridge_*, gb_*, __gamebridge::) and compiler helpers (DW.ref.*).
# usage: cxxrt-exports.sh OUT_DIR/cxxrt/{src,libsupcxx}-*.o
#   (the objects ios/build.rs compiles, before they are joined)
set -e
BIN=$HOME/Android/Sdk/ndk/30.0.16248370/toolchains/llvm/prebuilt/linux-x86_64/bin
OUT="$(dirname "$0")/../cxxrt/exports.txt"
LIST=$(mktemp -p "$(dirname "$OUT")" .exports.XXXXXX)
"$BIN/llvm-nm" -P --defined-only --extern-only "$@" 2>/dev/null \
  | awk 'NF >= 2 && $1 !~ /:$/ { print $2, $1 }' | sort -u -k2,2 > "$LIST"
{
  echo "# C++ runtime names registered for games (made by ios/tools/cxxrt-exports.sh)."
  while read -r kind name; do
    case "$kind" in T|W|i) k=F ;; *) k=D ;; esac
    plain=$("$BIN/llvm-cxxfilt" "$name")
    case "$plain" in *_IO_FILE*|*__sFILE*|gamebridge_*|gb_*|*__gamebridge::*|DW.ref.*) continue ;; esac
    # Apple's arm64 passes variadic arguments on the stack, the host in
    # registers; va_list differs too. None is imported by a game we know.
    case "$plain" in *"...)"*|*va_list*) continue ;; esac
    if [[ "$plain" == *"long double"* && "$name" != _ZTI* && "$name" != _ZTS* ]]; then continue; fi
    echo "$k $name"
  done < "$LIST"
} > "$OUT"
rm -f "$LIST"
echo "$(grep -c '^[FD] ' "$OUT") names in $OUT"
