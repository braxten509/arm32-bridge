#!/bin/bash
# Builds the phone (arm64 Android) binary. Android has no libpthread, but
# Unicorn's CMake links it, so an empty stand-in archive is put on the path.
set -e
cd "$(dirname "$0")"
export LDFLAGS="-L$PWD/android-link-stubs"
cargo build --release --target aarch64-linux-android "$@"
