#!/bin/bash
# Converts a 32-bit-only Android game APK so it runs on 64-bit-only phones,
# then installs it on the phone (over Wi-Fi ADB).
#
# - Every 32-bit library lib<name>.so is replaced by the shared stand-in
#   (tools/standin) and kept next to it as lib<name>-arm32.so. The bridge (the
#   same build for every game) is added once as libarm32_bridge.so; each
#   stand-in hands its original to it.
# - Known Java-side problems are fixed in the dex files.
# The conversion itself is done by apk-convert (converter/), the same program
# the phone app uses; this script builds it, signs the result and installs it.
# - The APK is re-signed with the debug key (the original signature can't survive).
# - Phone functions the game imports that the bridge has no handler for yet are listed.
#
# usage: tools/convert.sh <game.apk> [--out <converted.apk>] [--no-install]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}
BUILD_TOOLS=${BUILD_TOOLS:-$HOME/Android/Sdk/build-tools/37.0.0}
CLANG=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang

usage() { echo "usage: tools/convert.sh <game.apk> [--out <converted.apk>] [--no-install]" >&2; exit 2; }
APK="" OUT="" INSTALL=1
while [ $# -gt 0 ]; do
    case $1 in
        --out) OUT=$2; shift 2 ;;
        --no-install) INSTALL=0; shift ;;
        -*) usage ;;
        *) [ -z "$APK" ] || usage; APK=$1; shift ;;
    esac
done
[ -n "$APK" ] || usage
APK=$(realpath "$APK")
OUT=$(realpath -m "${OUT:-$(dirname "$APK")/$(basename "$APK" .apk)-arm64.apk}")

WORK=$ROOT/target/apk/$(basename "$APK" .apk)
rm -rf "$WORK"
mkdir -p "$WORK"

echo "== building the bridge"
systemd-run --user --wait --pipe -q -p AllowedCPUs=6-11,18-23 -d bash -c \
    "cd '$ROOT' && ./build-phone.sh --lib -q && cargo build --release -q -p apk-convert" >&2
"$CLANG" -O2 -shared -fPIC -Wl,--build-id=none -o "$WORK/libstandin.so" "$ROOT/tools/standin/standin.c" -llog -ldl

echo "== converting"
set +e
"$ROOT/target/release/apk-convert" "$APK" "$WORK/unsigned.apk" \
    --bridge "$ROOT/target/aarch64-linux-android/release/libarm32_bridge.so" --standin "$WORK/libstandin.so" \
    --work "$WORK" > "$WORK/convert.log"
STATUS=$?
set -e
grep -v '^missing ' "$WORK/convert.log" | sed -e 's/^step /  /' -e 's/^error /  /' -e 's/^warn /  warning: /'
case $STATUS in
    0) ;;
    3) exit 0 ;;
    *) exit 1 ;;
esac
MISSING=$(grep '^missing ' "$WORK/convert.log" | cut -d' ' -f2- || true)
if [ -n "$MISSING" ]; then
    echo "== $(wc -l <<<"$MISSING") phone functions the bridge doesn't have yet (the game may fail if it uses them):"
    column -c 110 <<<"$MISSING" | sed 's/^/  /'
fi

echo "== signing"
"$BUILD_TOOLS/apksigner" sign --ks "$HOME/.android/debug.keystore" --ks-pass pass:android --key-pass pass:android \
    --ks-key-alias androiddebugkey --out "$OUT" "$WORK/unsigned.apk" 2>/dev/null
echo "converted: $OUT"

[ "$INSTALL" = 1 ] || exit 0
echo "== install"
if ! adb get-state >/dev/null 2>&1; then
    # Wireless debugging changes its port; look for the phone on the network.
    for address in $(adb mdns services 2>/dev/null | awk '/_adb-tls-connect/ {print $NF}'); do
        adb connect "$address" >/dev/null 2>&1 && break
    done
fi
if ! adb get-state >/dev/null 2>&1; then
    echo "No phone found. Turn on Wireless debugging on the phone (same Wi-Fi), then run:" >&2
    echo "  adb install -r --bypass-low-target-sdk-block '$OUT'" >&2
    exit 1
fi
if ! result=$(adb install -r --bypass-low-target-sdk-block "$OUT" 2>&1); then
    if grep -q "UPDATE_INCOMPATIBLE" <<<"$result"; then
        echo "The phone already has this game signed by someone else, so it can't be replaced in place." >&2
        echo "Uninstall it on the phone first (this deletes its saved progress), then run this again." >&2
    else
        echo "$result" >&2
    fi
    exit 1
fi
echo "installed"
