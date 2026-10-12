#!/bin/bash
# Rebuilds a 32-bit-only Android game APK so it runs on 64-bit-only phones.
#
# - lib<main>.so (the game's 32-bit code) is packed as lib/arm64-v8a/libgame32.so
#   and replaced by the bridge, built with entry points for this game.
# - Other 32-bit libraries get 64-bit stand-ins (tools/gen_stub_lib.py).
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
"$BUILD_TOOLS/dexdump" "$WORK"/unpacked/classes*.dex > "$WORK/dexdump.txt" 2>/dev/null

echo "== entry points"
python3 "$ROOT/tools/gen_exports.py" "$WORK/dexdump.txt" "$LIB32/lib$MAIN.so" "$WORK/exports.rs"

echo "== bridge library"
ARM32_EXPORTS_RS="$WORK/exports.rs" "$ROOT/build-phone.sh" --lib
cp "$ROOT/target/aarch64-linux-android/release/libarm32_bridge.so" "$WORK/stage/lib/arm64-v8a/lib$MAIN.so"
cp "$LIB32/lib$MAIN.so" "$WORK/stage/lib/arm64-v8a/libgame32.so"

echo "== stand-ins for other 32-bit libraries"
for library in "$LIB32"/*.so; do
    name=$(basename "$library")
    [ "$name" = "lib$MAIN.so" ] && continue
    python3 "$ROOT/tools/gen_stub_lib.py" "$WORK/dexdump.txt" "$library" "$WORK/${name%.so}.c"
    "$CLANG" -O2 -shared -fPIC -Wl,-soname,"$name" -o "$WORK/stage/lib/arm64-v8a/$name" "$WORK/${name%.so}.c"
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
