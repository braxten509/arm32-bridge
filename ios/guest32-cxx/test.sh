#!/usr/bin/env bash
# The libguest32cxx.so test: builds the library and the test libraries
# (build.sh test), then runs test/cxxtest.cpp's run_all on the translator
# (ARM64 arm32-bridge; on the PC under qemu-aarch64). Passes when run_all
# returns 0 with no translator fallbacks; otherwise run_all's value has one
# bit per failed CHECK.
#
#   BIN=<arm32-bridge for ARM64> test.sh        (default: the PC cross build)
#   test.sh --no-build                          reuse the last build
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
CACHE=${GUEST32_CXX_CACHE:-$HOME/.cache/gamebridge/guest32-cxx}
OUT=${GUEST32_CXX_OUT:-$CACHE/out}
BIN=${BIN:-$HOME/.cache/aarch64-cross/target-g32cxx/aarch64-unknown-linux-gnu/release/arm32-bridge}

[ "${1:-}" = --no-build ] || "$HERE/build.sh" test

run=("$BIN")
[ "$(uname -m)" = aarch64 ] || run=(qemu-aarch64 -L "$HOME/.cache/aarch64-sysroot" "$BIN")
output=$(ARM32_STATS=1 timeout 600 "${run[@]}" call "$OUT/test/libcxxtest.so" run_all 2>&1) || true
echo "$output" | grep -E 'run_all|fallbacks|handler|FAIL' || echo "$output" | tail -20
# Every instruction translated: no Unicorn fallbacks (32-bit iPhone code has no Unicorn).
if echo "$output" | grep -q '^run_all returned 0 (0x0)$' && echo "$output" | grep -q ' 0 fallbacks at 0 sites'; then
    echo "test.sh: PASS"
else
    echo "test.sh: FAIL" >&2
    exit 1
fi
