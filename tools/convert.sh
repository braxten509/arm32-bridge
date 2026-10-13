#!/bin/bash
# Converts a 32-bit-only Android game APK so it runs on 64-bit-only phones,
# then installs it on the phone (over Wi-Fi ADB).
#
# - The game's main 32-bit library (the largest one, or --main) is replaced by
#   the bridge (the same build for every game) and kept next to it as
#   lib<main>-arm32.so.
# - Other 32-bit libraries are replaced by the shared stand-in (tools/standin),
#   with their originals kept next to them the same way.
# - Known Java-side bugs are fixed in the dex files (tools/patch_dex.py).
# - The APK is re-signed with the debug key (the original signature can't survive).
# - Phone functions the game imports that the bridge has no handler for yet are listed.
#
# usage: tools/convert.sh <game.apk> [--out <converted.apk>] [--main <library name>] [--no-install]
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}
BUILD_TOOLS=${BUILD_TOOLS:-$HOME/Android/Sdk/build-tools/37.0.0}
CLANG=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang

usage() { echo "usage: tools/convert.sh <game.apk> [--out <converted.apk>] [--main <library name>] [--no-install]" >&2; exit 2; }
APK="" OUT="" MAIN="" INSTALL=1
while [ $# -gt 0 ]; do
    case $1 in
        --out) OUT=$2; shift 2 ;;
        --main) MAIN=${2#lib}; MAIN=${MAIN%.so}; shift 2 ;;
        --no-install) INSTALL=0; shift ;;
        -*) usage ;;
        *) [ -z "$APK" ] || usage; APK=$1; shift ;;
    esac
done
[ -n "$APK" ] || usage
APK=$(realpath "$APK")
OUT=$(realpath -m "${OUT:-$(dirname "$APK")/$(basename "$APK" .apk)-arm64.apk}")

ENTRIES=$(unzip -Z1 "$APK")
if grep -q '^lib/arm64-v8a/.*\.so$' <<<"$ENTRIES"; then
    echo "This game already has 64-bit code, so it doesn't need converting. Install it as it is."
    exit 0
fi
ABI=""
for candidate in armeabi-v7a armeabi; do
    if grep -q "^lib/$candidate/.*\.so$" <<<"$ENTRIES"; then ABI=$candidate; break; fi
done
[ -n "$ABI" ] || { echo "This APK has no 32-bit ARM code, so there is nothing to convert." >&2; exit 1; }

WORK=$ROOT/target/apk/$(basename "$APK" .apk)
rm -rf "$WORK"
mkdir -p "$WORK/stage/lib/arm64-v8a"
unzip -q -o "$APK" "lib/$ABI/*" 'classes*.dex' -d "$WORK/unpacked"
LIB32=$WORK/unpacked/lib/$ABI
if [ -z "$MAIN" ]; then
    MAIN=$(basename "$(ls -S "$LIB32"/*.so | head -1)" .so)
    MAIN=${MAIN#lib}
fi
[ -f "$LIB32/lib$MAIN.so" ] || { echo "No lib$MAIN.so in lib/$ABI." >&2; exit 1; }
echo "== game code: lib$MAIN.so ($ABI)"

echo "== bridge"
systemd-run --user --wait --pipe -q -p AllowedCPUs=6-11,18-23 -d bash -c \
    "cd '$ROOT' && ./build-phone.sh --lib -q && cargo build --release -q" >&2
cp "$ROOT/target/aarch64-linux-android/release/libarm32_bridge.so" "$WORK/stage/lib/arm64-v8a/lib$MAIN.so"

echo "== phone functions the bridge doesn't have yet"
"$ROOT/target/release/arm32-bridge" imports "$LIB32/lib$MAIN.so" > "$WORK/missing.txt" 2>/dev/null || true
if [ -s "$WORK/missing.txt" ]; then
    echo "$(wc -l < "$WORK/missing.txt") missing (the game may fail when it uses them):"
    column -c 110 "$WORK/missing.txt" | sed 's/^/  /'
else
    echo "none"
fi

echo "== stand-ins for other 32-bit libraries"
"$CLANG" -O2 -shared -fPIC -Wl,--build-id=none -o "$WORK/libstandin.so" "$ROOT/tools/standin/standin.c" -llog -ldl
for library in "$LIB32"/*.so; do
    name=$(basename "$library" .so)
    cp "$library" "$WORK/stage/lib/arm64-v8a/$name-arm32.so"
    [ "$name" = "lib$MAIN" ] && continue
    cp "$WORK/libstandin.so" "$WORK/stage/lib/arm64-v8a/$name.so"
    echo "$name.so"
done

echo "== APK"
cp "$APK" "$WORK/unsigned.apk"
zip -q -d "$WORK/unsigned.apk" 'lib/*' 'META-INF/MANIFEST.MF' 'META-INF/*.SF' 'META-INF/*.RSA' 'META-INF/*.DSA' >/dev/null || true
(cd "$WORK/stage" && zip -q -r "$WORK/unsigned.apk" lib)

echo "== Java fixes"
mkdir -p "$WORK/dex"
cp "$WORK"/unpacked/classes*.dex "$WORK/dex/"
python3 -I "$ROOT/tools/patch_dex.py" "$BUILD_TOOLS/dexdump" "$WORK"/dex/classes*.dex
for dex in "$WORK"/dex/classes*.dex; do
    if ! cmp -s "$dex" "$WORK/unpacked/$(basename "$dex")"; then
        zip -q -d "$WORK/unsigned.apk" "$(basename "$dex")"
        (cd "$WORK/dex" && zip -q "$WORK/unsigned.apk" "$(basename "$dex")")
    fi
done
"$BUILD_TOOLS/zipalign" -f -p 4 "$WORK/unsigned.apk" "$WORK/aligned.apk"
"$BUILD_TOOLS/apksigner" sign --ks "$HOME/.android/debug.keystore" --ks-pass pass:android --key-pass pass:android \
    --ks-key-alias androiddebugkey --out "$OUT" "$WORK/aligned.apk" 2>/dev/null
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
