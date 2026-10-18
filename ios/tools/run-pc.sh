#!/bin/bash
# Builds the iPhone layer for ARM64 Linux and runs a game under qemu on the PC.
# usage: run-pc.sh <App.app> [ios-run options...]
#   env: IOS_HOME (game's writable folder), IOS_RUN_SECONDS, IOS_LIST_MISSING,
#        LOG (log file, default ~/.cache/ios-run.log), TAIL (lines shown, default 20),
#        TIMEOUT (seconds, default 300)
# Exits with the run's own exit code (124 = timed out); stops if the build fails.
APP=$(realpath "$1"); shift
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
LOG=${LOG:-$HOME/.cache/ios-run.log}
BUILD_LOG=$(mktemp)
trap 'rm -f "$BUILD_LOG"' EXIT
(cd "$ROOT" && ~/.cache/aarch64-cross/build.sh -p gamebridge-ios) > "$BUILD_LOG" 2>&1
BUILD_STATUS=$?
grep -E '^(error|warning)' -A6 "$BUILD_LOG" | head -60
if [ "$BUILD_STATUS" -ne 0 ]; then
  echo "build failed (exit $BUILD_STATUS); not running"
  exit "$BUILD_STATUS"
fi
timeout "${TIMEOUT:-300}" qemu-aarch64 -L ~/.cache/aarch64-sysroot \
  ~/.cache/aarch64-cross/target/aarch64-unknown-linux-gnu/release/ios-run run "$APP" "$@" > "$LOG" 2>&1
RUN_STATUS=$?
grep -v -E 'fields moved up|signal [0-9]+: ignored' "$LOG" | tail -"${TAIL:-20}"
echo "exit $RUN_STATUS"
exit "$RUN_STATUS"
