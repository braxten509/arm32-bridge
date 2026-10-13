#!/bin/bash
# Builds the phone (arm64 Android) binary. Android has no libpthread, but
# Unicorn's CMake links it, so an empty stand-in archive is put on the path.
# TARGET=x86_64-linux-android builds the copy for the PC's Android emulator.
set -e
cd "$(dirname "$0")"
export LDFLAGS="-L$PWD/android-link-stubs"
cargo build --release --target "${TARGET:-aarch64-linux-android}" "$@"
