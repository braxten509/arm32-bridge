#!/bin/bash
# Builds the 32-bit test libraries (and the native 64-bit speed test) with the NDK.
set -e
cd "$(dirname "$0")"
NDK=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}
BIN=$NDK/toolchains/llvm/prebuilt/linux-x86_64/bin
mkdir -p build
"$BIN/armv7a-linux-androideabi21-clang" -O2 -fPIC -shared -o build/bench.so bench.c
"$BIN/armv7a-linux-androideabi21-clang" -O2 -fPIC -shared -o build/libsecond.so second.c
"$BIN/armv7a-linux-androideabi21-clang" -O2 -fPIC -shared -o build/libslowinit.so slowinit.c
"$BIN/armv7a-linux-androideabi21-clang++" -O2 -fPIC -shared -static-libstdc++ -o build/selftest.so selftest.cpp -llog -lz -lEGL -lGLESv3 -lOpenSLES
"$BIN/aarch64-linux-android30-clang" -O2 -o build/bench64 bench.c bench_main.c
cc -O2 -o build/bench-pc bench.c bench_main.c
echo built: build/bench.so build/libsecond.so build/libslowinit.so build/selftest.so build/bench64 build/bench-pc
