#!/usr/bin/env bash
# check-game-imports.sh <App.app | Mach-O> ...: lists every C++ runtime
# import (libc++, libc++abi, SjLj unwinder) of each 32-bit game that
# libguest32cxx.so does not export. Reads the games where they are; nothing
# from them is kept. Exit 1 if any is missing.
set -euo pipefail
CACHE=${GUEST32_CXX_CACHE:-$HOME/.cache/gamebridge/guest32-cxx}
EXPORTS=${GUEST32_CXX_OUT:-$CACHE/out}/libguest32cxx.so.exports
[ -f "$EXPORTS" ] || { echo "no $EXPORTS: run build.sh first" >&2; exit 1; }

missing_total=0
for game in "$@"; do
    binary=$game
    if [ -d "$game" ]; then
        name=$(plutil -extract CFBundleExecutable raw "$game/Info.plist" 2>/dev/null || basename "$game" .app)
        binary=$game/$name
    fi
    arch=armv7
    llvm-lipo -archs "$binary" 2>/dev/null | grep -qw armv7 || arch=$(llvm-lipo -archs "$binary" | awk '{ print $1 }')
    # Mach-O names carry one more leading underscore than C names.
    imports=$(llvm-nm -u --arch="$arch" "$binary" | awk '{ print $NF }' |
        grep -E '^(__Z|___cxa_|___gxx_|__Unwind_|___dynamic_cast$)' | sed 's/^_//' | sort -u)
    missing=$(comm -23 <(echo "$imports") "$EXPORTS")
    count=$(echo "$imports" | grep -c . || true)
    if [ -n "$missing" ]; then
        echo "$(basename "$game"): $(echo "$missing" | wc -l) of $count C++ runtime imports missing:"
        echo "$missing" | sed 's/^/  /'
        missing_total=1
    else
        echo "$(basename "$game"): all $count C++ runtime imports exported"
    fi
done
exit $missing_total
