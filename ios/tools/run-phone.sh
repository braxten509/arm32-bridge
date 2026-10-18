#!/bin/bash
# Runs a game on the phone over adb with no window (nothing appears on the
# screen): frames are saved as pictures and copied back with the log.
# usage: run-phone.sh <App.app> [ios-run options...]
#   env: ANDROID_SERIAL (which phone), IOS_RUN_SECONDS, IOS_FRAME_EVERY, IOS_TAPS,
#        IOS_SOUND=1 (save the sound as sound.wav instead of playing it),
#        OUT (where log and frames go, default ~/.cache/ios-phone), TIMEOUT (default 600)
# The game folder is copied once to /sdcard/Download/ios/<name>.app (only changed files after).
set -e
APP=$(realpath "$1"); shift
NAME=$(basename "$APP")
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
ADB=${ADB:-$HOME/Android/Sdk/platform-tools/adb}
OUT=${OUT:-$HOME/.cache/ios-phone}
REMOTE_APP=/sdcard/Download/ios/$NAME
WORK=/data/local/tmp/ios

(cd "$ROOT" && ./build-phone.sh -q -p gamebridge-ios --bin ios-run)
"$ADB" shell mkdir -p "$WORK" /sdcard/Download/ios
"$ADB" push -q "$ROOT/target/aarch64-linux-android/release/ios-run" "$WORK/ios-run"
"$ADB" push -q --sync "$APP" /sdcard/Download/ios/
"$ADB" shell rm -rf "$WORK/frames" "$WORK/ios-run.log" "$WORK/sound.wav"
SOUND=""
[ -n "${IOS_SOUND:-}" ] && SOUND="IOS_SOUND_FILE=$WORK/sound.wav"
set +e
timeout "${TIMEOUT:-600}" "$ADB" shell "cd $WORK && IOS_HOME=$WORK/home IOS_FRAMES_DIR=$WORK/frames \
  IOS_RUN_SECONDS=${IOS_RUN_SECONDS:-60} IOS_FRAME_EVERY=${IOS_FRAME_EVERY:-60} IOS_TAPS='${IOS_TAPS:-}' $SOUND \
  ./ios-run run $REMOTE_APP $* > ios-run.log 2>&1; echo exit \$?"
set -e
rm -rf "$OUT"
mkdir -p "$OUT"
"$ADB" pull -q "$WORK/ios-run.log" "$OUT/" || true
"$ADB" pull -q "$WORK/frames" "$OUT/" 2>/dev/null || true
[ -n "$SOUND" ] && "$ADB" pull -q "$WORK/sound.wav" "$OUT/" 2>/dev/null
grep -v -E 'OutPath|web request' "$OUT/ios-run.log" | tail -"${TAIL:-25}"
