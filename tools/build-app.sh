#!/bin/bash
# Builds the Game Bridge phone app (android/): the bridge, the shared stand-in
# and the converter (apk-convert) go into its library folder, the PC's debug
# signing key into its assets (so phone and PC conversions replace each other),
# then Gradle builds the APK. With --install it is installed over ADB.
# --public leaves the PC's key out: that build makes its own key on each phone
# (Android's key store), for sharing the app with other people.
#
# usage: tools/build-app.sh [--public] [--install]
set -euo pipefail
PUBLIC=0 INSTALL=0
for option in "$@"; do
    case $option in
        --public) PUBLIC=1 ;;
        --install) INSTALL=1 ;;
        *) echo "unknown option $option" >&2; exit 2 ;;
    esac
done
ROOT=$(cd "$(dirname "$0")/.." && pwd)
NDK=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}
BIN=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
LIBS=$ROOT/android/app/src/main/jniLibs/arm64-v8a
ASSETS=$ROOT/android/app/src/main/assets
mkdir -p "$LIBS" "$ASSETS"

echo "== bridge and converter"
(cd "$ROOT" && ./build-phone.sh -q --lib -p arm32-bridge && ./build-phone.sh -q -p apk-convert)
OUT=$ROOT/target/aarch64-linux-android/release
cp "$OUT/libarm32_bridge.so" "$LIBS/libarm32_bridge.so"
# Android only unpacks lib*.so files, so the converter program travels under that name.
"$BIN/llvm-strip" -o "$LIBS/libapkconvert.so" "$OUT/apk-convert"
"$BIN/aarch64-linux-android30-clang" -O2 -shared -fPIC -Wl,--build-id=none -o "$LIBS/libstandin.so" \
    "$ROOT/tools/standin/standin.c" -llog -ldl
if [ $PUBLIC = 1 ]; then
    rm -f "$ASSETS/debug.keystore"
else
    cp "$HOME/.android/debug.keystore" "$ASSETS/debug.keystore"
fi

echo "== app"
(cd "$ROOT/android" && ./gradlew -q assembleRelease)
APK=$ROOT/android/app/build/outputs/apk/release/app-release.apk
ls -la "$APK"
if [ $INSTALL = 1 ]; then
    adb install -r "$APK"
fi
