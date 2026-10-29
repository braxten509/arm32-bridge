#!/bin/bash
# Builds converter/assets/offline.dex: Game Bridge's offline HTTP classes (offline/java):
# OfflineHttpClient (what the converter's fix 7 turns a game's DefaultHttpClient into),
# OfflineDirector and Offline (the bridge's answers, src/offline). The converter adds it to an
# offline game as its next classesN.dex. Run after changing those classes; the result is
# checked in.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SDK=${ANDROID_HOME:-$HOME/Android/Sdk}
TOOLS=$SDK/build-tools/37.0.0
PLATFORM=$SDK/platforms/android-36
CP="$PLATFORM/android.jar:$PLATFORM/optional/org.apache.http.legacy.jar"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
javac --release 8 -nowarn -cp "$CP" -d "$WORK/classes" $(find "$ROOT/offline/java" -name '*.java')
"$TOOLS/d8" --release --min-api 21 --lib "$PLATFORM/android.jar" --classpath "$PLATFORM/optional/org.apache.http.legacy.jar" \
    --output "$WORK" $(find "$WORK/classes" -name '*.class')
cp "$WORK/classes.dex" "$ROOT/converter/assets/offline.dex"
ls -la "$ROOT/converter/assets/offline.dex"
