#!/bin/bash
# Runs the iPhone layer's unit tests on the PC: built for ARM64 Linux and
# run under qemu (most of the code only exists on 64-bit ARM).
# usage: test-pc.sh [test name filter]
set -e
X=~/.cache/aarch64-cross
export CC_aarch64_unknown_linux_gnu=$X/cc CXX_aarch64_unknown_linux_gnu=$X/cxx
export AR_aarch64_unknown_linux_gnu=$HOME/Android/Sdk/ndk/30.0.16248370/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-ar
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=$X/cc
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER="qemu-aarch64 -L $HOME/.cache/aarch64-sysroot"
# Unicorn (the translator's engine, used for 32-bit games) is built for the target too.
export CMAKE_SYSTEM_NAME=Linux CMAKE_SYSTEM_PROCESSOR=aarch64
export BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu="--target=aarch64-linux-gnu --sysroot=$HOME/.cache/aarch64-sysroot"
cd "$(dirname "$0")/../.."
cargo test --release --target aarch64-unknown-linux-gnu --target-dir "${TARGET_DIR:-$X/target}" -p gamebridge-ios --lib -- "$@"
# The ignored tests are GIL mode's (Unicorn, the checker): one thread runs game code at a time.
ARM32_PARALLEL=0 cargo test --release --target aarch64-unknown-linux-gnu --target-dir "${TARGET_DIR:-$X/target}" -p gamebridge-ios --lib -- --ignored "$@"
