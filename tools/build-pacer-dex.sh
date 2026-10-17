#!/bin/bash
# Builds converter/assets/pacer.dex: the Java class that loads Game Bridge's
# frame helper into 64-bit games (pacer/java). The converter carries it and
# adds it to each 64-bit game as its next classesN.dex. Run after changing
# Pacer.java; the result is checked in.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SDK=${ANDROID_HOME:-$HOME/Android/Sdk}
TOOLS=$SDK/build-tools/37.0.0
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
javac --release 8 -nowarn -cp "$SDK/platforms/android-36/android.jar" -d "$WORK/classes" \
    "$ROOT/pacer/java/dev/braxten/gamebridge/Pacer.java"
# Android 5 (API 21) is the oldest that loads extra classesN.dex files by itself.
"$TOOLS/d8" --release --min-api 21 --lib "$SDK/platforms/android-36/android.jar" --output "$WORK" \
    "$WORK/classes/dev/braxten/gamebridge/Pacer.class"
cp "$WORK/classes.dex" "$ROOT/converter/assets/pacer.dex"
ls -la "$ROOT/converter/assets/pacer.dex"
