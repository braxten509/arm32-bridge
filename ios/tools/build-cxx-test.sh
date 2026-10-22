#!/bin/bash
# Builds ios/cxxrt/test/throw_test, a small arm64 iPhone program that throws
# and catches C++ exceptions (compact unwind info, LSDAs, personality through
# the GOT, as Apple's linker lays them out), from throw_test.cc with the
# NDK's clang and ld64.lld. Imports are left for the loader to resolve by
# name (flat lookup). The built file is kept in the repository.
set -e
BIN=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}/toolchains/llvm/prebuilt/linux-x86_64/bin
DIR=$(cd "$(dirname "$0")/../cxxrt/test" && pwd)
OBJ=$(mktemp -p "$DIR" .throw_test.XXXXXX.o)
trap 'rm -f "$OBJ"' EXIT
"$BIN/clang++" --target=arm64-apple-ios9.0 -O1 -fexceptions -fcxx-exceptions -frtti \
  -nostdinc++ -ffreestanding -c "$DIR/throw_test.cc" -o "$OBJ"
"$BIN/ld64.lld" -arch arm64 -platform_version ios 9.0 9.0 -undefined dynamic_lookup \
  -o "$DIR/throw_test" "$OBJ"
echo "built $DIR/throw_test"
