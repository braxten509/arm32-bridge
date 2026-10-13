#!/bin/bash
# Rebuilds a 32-bit-only Android game APK so it runs on 64-bit-only phones.
#
# - lib<main>.so (the game's 32-bit code) is replaced by the bridge (the same
#   build for every game) and kept next to it as lib<main>-arm32.so.
# - Other 32-bit libraries are replaced by the shared stand-in (tools/standin),
#   with their originals kept next to them the same way.
# - Known Java-side bugs are fixed in the dex files (tools/patch_dex.py).
# - The APK is re-signed with the debug key (the original signature can't survive).
#
# usage: tools/package_apk.sh <original.apk> <main library name, e.g. cocos2dcpp> <output.apk>
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
APK=$(realpath "$1")
MAIN=$2
OUT=$(realpath -m "$3")
NDK=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}
BUILD_TOOLS=${BUILD_TOOLS:-$HOME/Android/Sdk/build-tools/37.0.0}
CLANG=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android30-clang

WORK=$ROOT/target/apk/$(basename "$APK" .apk)
rm -rf "$WORK"
mkdir -p "$WORK/stage/lib/arm64-v8a"
unzip -q -o "$APK" 'lib/armeabi-v7a/*' 'classes*.dex' -d "$WORK/unpacked"
LIB32=$WORK/unpacked/lib/armeabi-v7a

echo "== bridge library"
"$ROOT/build-phone.sh" --lib
cp "$ROOT/target/aarch64-linux-android/release/libarm32_bridge.so" "$WORK/stage/lib/arm64-v8a/lib$MAIN.so"

echo "== stand-ins for other 32-bit libraries"
"$CLANG" -O2 -shared -fPIC -Wl,--build-id=none -o "$WORK/libstandin.so" "$ROOT/tools/standin/standin.c" -llog -ldl
for library in "$LIB32"/*.so; do
    name=$(basename "$library" .so)
    cp "$library" "$WORK/stage/lib/arm64-v8a/$name-arm32.so"
    [ "$name" = "lib$MAIN" ] && continue
    cp "$WORK/libstandin.so" "$WORK/stage/lib/arm64-v8a/$name.so"
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
    --ks-key-alias androiddebugkey --out "$OUT" "$WORK/aligned.apk"
ls -la "$OUT"
