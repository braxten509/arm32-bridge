#!/bin/bash
# Converts a 32-bit-only Android game APK so it runs on 64-bit-only phones,
# then installs it on the phone (over Wi-Fi ADB). A 64-bit game instead keeps
# its own code and gets Game Bridge's frame helper (pacer/), which lifts its
# frame caps.
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
# - A bundle (.xapk, .apkm, .apks) becomes a folder of pieces, all signed with
#   the same key and installed together; its game data (OBB) is copied to
#   /sdcard/Android/obb/.
#
# usage: tools/convert.sh <game.apk|bundle> [--out <converted.apk|folder>] [--no-install] [--abi x86_64] [--device <serial>] [--min-target-sdk N]
#   --abi x86_64: a copy for the PC's Android emulator (x86_64 bridge, Unicorn only)
#   --device: install on this adb device (e.g. emulator-5554) instead of the phone
#   --min-target-sdk: raise an old game's target Android version to N, as the phone app does
#     (Android refuses old targets from installers other than adb)
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}
BUILD_TOOLS=${BUILD_TOOLS:-$HOME/Android/Sdk/build-tools/37.0.0}

usage() {
    echo "usage: tools/convert.sh <game.apk|bundle> [--out <converted.apk|folder>] [--no-install] [--abi x86_64] [--device <serial>] [--min-target-sdk N]" >&2
    exit 2
}
APK="" OUT="" INSTALL=1 ABI=arm64-v8a DEVICE="" MIN_TARGET=()
while [ $# -gt 0 ]; do
    case $1 in
        --out) OUT=$2; shift 2 ;;
        --no-install) INSTALL=0; shift ;;
        --abi) ABI=$2; shift 2 ;;
        --device) DEVICE=$2; shift 2 ;;
        --min-target-sdk) MIN_TARGET=(--min-target-sdk "$2"); shift 2 ;;
        -*) usage ;;
        *) [ -z "$APK" ] || usage; APK=$1; shift ;;
    esac
done
[ -n "$APK" ] || usage
case $ABI in
    arm64-v8a) TARGET=aarch64-linux-android CLANG_ARCH=aarch64 ;;
    x86_64) TARGET=x86_64-linux-android CLANG_ARCH=x86_64 ;;
    *) usage ;;
esac
CLANG=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/$CLANG_ARCH-linux-android30-clang
ADB=(adb)
[ -z "$DEVICE" ] || ADB=(adb -s "$DEVICE")
APK=$(realpath "$APK")
STEM=$(basename "$APK")
STEM=${STEM%.*}
BUNDLE=0
case ${APK,,} in *.xapk|*.apkm|*.apks) BUNDLE=1 ;; esac
if [ $BUNDLE = 1 ]; then
    OUT=$(realpath -m "${OUT:-$(dirname "$APK")/$STEM-${ABI%%-*}}")
else
    OUT=$(realpath -m "${OUT:-$(dirname "$APK")/$STEM-${ABI%%-*}.apk}")
fi

WORK=$ROOT/target/apk/$STEM-$ABI
rm -rf "$WORK"
mkdir -p "$WORK"

echo "== building the bridge"
systemd-run --user --wait --pipe -q -p AllowedCPUs=6-11,18-23 -d -E TARGET=$TARGET bash -c \
    "cd '$ROOT' && ./build-phone.sh --lib -q && TARGET=aarch64-linux-android ./build-phone.sh --lib -q -p gamebridge-pacer && cargo build --release -q -p apk-convert" >&2
# The frame helper for 64-bit games is always the ARM build (the emulator translates ARM games).
"$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-strip" -o "$WORK/libgamebridge_pacer.so" "$ROOT/target/aarch64-linux-android/release/libgamebridge_pacer.so"
"$CLANG" -O2 -shared -fPIC -Wl,--build-id=none -o "$WORK/libstandin.so" "$ROOT/tools/standin/standin.c" -llog -ldl

echo "== converting"
set +e
UNSIGNED=$WORK/unsigned.apk
[ $BUNDLE = 0 ] || UNSIGNED=$WORK/unsigned
"$ROOT/target/release/apk-convert" "$APK" "$UNSIGNED" \
    --bridge "$ROOT/target/$TARGET/release/libarm32_bridge.so" --standin "$WORK/libstandin.so" \
    --pacer "$WORK/libgamebridge_pacer.so" \
    --work "$WORK" --abi "$ABI" "${MIN_TARGET[@]}" > "$WORK/convert.log"
STATUS=$?
set -e
grep -v -e '^missing ' -e '^piece ' -e '^obb ' "$WORK/convert.log" | sed -e 's/^step /  /' -e 's/^error /  /' -e 's/^warn /  warning: /'
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
sign() {
    "$BUILD_TOOLS/apksigner" sign --ks "$HOME/.android/debug.keystore" --ks-pass pass:android --key-pass pass:android \
        --ks-key-alias androiddebugkey --out "$2" "$1" 2>/dev/null
}
if [ $BUNDLE = 1 ]; then
    mkdir -p "$OUT"
    rm -f "$OUT"/*.apk "$OUT"/*.idsig
    rm -rf "$OUT/obb"
    # Every piece gets the same key: Android only installs pieces signed alike.
    for piece in "$UNSIGNED"/*.apk; do
        sign "$piece" "$OUT/$(basename "$piece")"
    done
    [ ! -d "$UNSIGNED/obb" ] || cp -r "$UNSIGNED/obb" "$OUT/obb"
else
    sign "$UNSIGNED" "$OUT"
fi
echo "converted: $OUT"

[ "$INSTALL" = 1 ] || exit 0
echo "== install"
if [ -z "$DEVICE" ] && ! "${ADB[@]}" get-state >/dev/null 2>&1; then
    # Wireless debugging changes its port; look for the phone on the network.
    for address in $(adb mdns services 2>/dev/null | awk '/_adb-tls-connect/ {print $NF}'); do
        adb connect "$address" >/dev/null 2>&1 && break
    done
fi
if ! "${ADB[@]}" get-state >/dev/null 2>&1; then
    echo "No phone found. Turn on Wireless debugging on the phone (same Wi-Fi), then run:" >&2
    echo "  adb install -r --no-incremental --bypass-low-target-sdk-block '$OUT'" >&2
    exit 1
fi
# Not streamed: an incremental install lets the game start before all of it has
# arrived, and every read of a missing part waits for the cable (Broken Age's
# start-up script took 9 s instead of 0.1 s and Android called it not responding).
INSTALL_COMMAND=(install -r --no-incremental --bypass-low-target-sdk-block "$OUT")
[ $BUNDLE = 0 ] || INSTALL_COMMAND=(install-multiple -r --no-incremental --bypass-low-target-sdk-block "$OUT"/*.apk)
if ! result=$("${ADB[@]}" "${INSTALL_COMMAND[@]}" 2>&1); then
    if grep -q "UPDATE_INCOMPATIBLE" <<<"$result"; then
        echo "The phone already has this game signed by someone else, so it can't be replaced in place." >&2
        echo "Uninstall it on the phone first (this deletes its saved progress), then run this again." >&2
    else
        echo "$result" >&2
    fi
    exit 1
fi
if [ -d "$OUT/obb" ]; then
    echo "== copying game data"
    (cd "$OUT/obb" && find . -type f) | while read -r file; do
        "${ADB[@]}" push "$OUT/obb/${file#./}" "/sdcard/Android/obb/${file#./}" >/dev/null 2>&1 || echo "  couldn't copy ${file#./}" >&2
    done
fi
echo "installed"
