#!/bin/bash
# Compiles the made-up manifests here into Android's binary form with aapt2.
set -e
cd "$(dirname "$0")"
SDK=$HOME/Android/Sdk
AAPT2=$(ls $SDK/build-tools/*/aapt2 | tail -1)
JAR=$SDK/platforms/android-34/android.jar
tmp=$(mktemp -d)
for name in made-up-unity made-up-classic; do
    "$AAPT2" link --manifest $name.xml -I "$JAR" -o "$tmp/$name.apk"
    unzip -p "$tmp/$name.apk" AndroidManifest.xml > $name.axml
done
rm -r "$tmp"
