#!/bin/bash
# Builds ios/cxxrt/test/throw_test, a small arm64 iPhone program that throws
# and catches C++ and Objective-C exceptions, from throw_test.cc and
# throw_test_objc.mm with the NDK's clang and ld64.lld. Imports are left for
# the loader to resolve by name (flat lookup). The built file is kept in the
# repository. Its frames cover the three ways a function's unwind info is
# found:
#  - throw_test.cc: compact unwind info, LSDAs and the personality through
#    the GOT, as Apple's linker lays them out;
#  - throw_test_objc.mm: DWARF FDEs named by the compact table (a no-op
#    remember/restore pair is something compact unwind can't describe);
#  - remembered_frame: an FDE and no compact entry at all, as code linked
#    with -no_compact_unwind (its entry is cleared after linking), behind
#    a malformed FDE (broken_fde_frame's, broken after linking).
set -e
BIN=${NDK:-$HOME/Android/Sdk/ndk/30.0.16248370}/toolchains/llvm/prebuilt/linux-x86_64/bin
DIR=$(cd "$(dirname "$0")/../cxxrt/test" && pwd)
WORK=$(mktemp -d -p "$DIR" .throw_test.XXXXXX)
trap 'rm -rf "$WORK"' EXIT
FLAGS=(--target=arm64-apple-ios9.0 -O1 -fexceptions -fcxx-exceptions -frtti -nostdinc++ -ffreestanding)
"$BIN/clang++" "${FLAGS[@]}" -c "$DIR/throw_test.cc" -o "$WORK/throw_test.o"
"$BIN/clang++" "${FLAGS[@]}" -fobjc-runtime=ios-9.0 -fobjc-exceptions -S "$DIR/throw_test_objc.mm" -o "$WORK/objc.s"
sed -i 's/^\(\s*\.cfi_startproc.*\)$/\1\n\t.cfi_remember_state\n\t.cfi_restore_state/' "$WORK/objc.s"
"$BIN/clang" --target=arm64-apple-ios9.0 -c "$WORK/objc.s" -o "$WORK/objc.o"
"$BIN/ld64.lld" -arch arm64 -platform_version ios 9.0 9.0 -undefined dynamic_lookup \
  -o "$DIR/throw_test" "$WORK/throw_test.o" "$WORK/objc.o"
python3 -I "$(dirname "$0")/patch-test-unwind.py" zero-compact "$DIR/throw_test" _remembered_frame
python3 -I "$(dirname "$0")/patch-test-unwind.py" break-fde "$DIR/throw_test" _broken_fde_frame
echo "built $DIR/throw_test"
