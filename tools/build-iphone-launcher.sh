#!/bin/bash
# Builds android/app/src/main/assets/iphone-launcher.apk: the template of an
# iPhone game's own app (android/iphone-launcher). Game Bridge copies it for
# each iPhone game with the game's name, folder and icon put in, signs it and
# installs it (IphoneApps.kt). tools/build-app.sh runs it (the assets folder
# isn't checked in); the same tools make the same file, so the games' apps
# aren't remade for nothing. It is left unsigned: the phone signs each copy.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
SDK=${ANDROID_HOME:-$HOME/Android/Sdk}
TOOLS=$SDK/build-tools/37.0.0
JAR=$SDK/platforms/android-36/android.jar
SRC=$ROOT/android/iphone-launcher
OUT=$ROOT/android/app/src/main/assets/iphone-launcher.apk
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
"$TOOLS/aapt2" compile --dir "$SRC/res" -o "$WORK/res.zip"
"$TOOLS/aapt2" link -I "$JAR" --manifest "$SRC/AndroidManifest.xml" --min-sdk-version 31 --target-sdk-version 36 \
    -o "$WORK/base.apk" "$WORK/res.zip"
javac --release 8 -nowarn -cp "$JAR" -d "$WORK/classes" "$SRC/java/dev/braxten/gamebridge/launcher/Launcher.java"
"$TOOLS/d8" --release --min-api 31 --lib "$JAR" --output "$WORK" "$WORK/classes/dev/braxten/gamebridge/launcher/Launcher.class"
python3 -I - "$WORK/base.apk" "$WORK/classes.dex" "$OUT" <<'PY'
import sys, zipfile
base, dex, out = sys.argv[1:]
with zipfile.ZipFile(base) as source, zipfile.ZipFile(out, "w") as target:
    for item in source.infolist():
        # resources.arsc stays uncompressed (Android 11+ maps it straight from the APK).
        kind = zipfile.ZIP_STORED if item.filename == "resources.arsc" else zipfile.ZIP_DEFLATED
        target.writestr(zipfile.ZipInfo(item.filename, date_time=(2026, 1, 1, 0, 0, 0)), source.read(item), compress_type=kind)
    with open(dex, "rb") as code:
        target.writestr(zipfile.ZipInfo("classes.dex", date_time=(2026, 1, 1, 0, 0, 0)), code.read(), compress_type=zipfile.ZIP_DEFLATED)
PY
ls -la "$OUT"
