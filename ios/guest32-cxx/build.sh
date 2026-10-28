#!/usr/bin/env bash
# Builds libguest32cxx.so: LLVM libc++, libc++abi and libunwind (SjLj), and
# Apple's GCC 4.2.1 libstdc++ for games built with GCC's headers, for
# 32-bit iPhone games, as a 32-bit ARM ELF library the translator loads into
# guest memory. See README.md for the ABI it matches and why.
#
#   build.sh         the library -> $OUT/libguest32cxx.so (+ .imports, .exports)
#   build.sh test    also the test libraries -> $OUT/test/
#
# Each source is compiled by clang as Apple armv7 code (armv7-apple-ios5: Thumb-2,
# so the front end gives Apple's types, mangled names and SjLj exceptions)
# to LLVM bitcode, then made into ELF code by llc with Apple's argument rules
# (apcs-gnu) and SjLj exception tables, and linked by ld.lld.
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
CACHE=${GUEST32_CXX_CACHE:-$HOME/.cache/gamebridge/guest32-cxx}
OUT=${GUEST32_CXX_OUT:-$CACHE/out}
SRC=$("$HERE/fetch.sh")
GNU=$("$HERE/fetch.sh" libstdcxx)
JOBS=${JOBS:-$(nproc)}
WORK=$OUT/obj
mkdir -p "$WORK" "$OUT/include"

# libc++'s two generated headers.
cp "$HERE/config/__config_site" "$OUT/include/__config_site"
cp "$SRC/libcxx/vendor/llvm/default_assertion_handler.in" "$OUT/include/__assertion_handler"

COMMON=(-target armv7-apple-ios5.0 -O2 -g0 -DNDEBUG -fno-stack-protector -fno-common -nostdinc)
C_HEADERS=(-isystem "$(clang -print-resource-dir)/include" -isystem "$HERE/darwin-include")
FRONT=(
    "${COMMON[@]}" -isystem "$OUT/include" -isystem "$SRC/libcxx/include"
    -isystem "$SRC/libcxxabi/include" -isystem "$SRC/libunwind/include" "${C_HEADERS[@]}"
    -fvisibility=hidden -fvisibility-inlines-hidden
    -Wno-user-defined-literals -Wno-deprecated-declarations
)
CXX=(-x c++ -std=c++23 -nostdinc++ -fexceptions -frtti)
CC=(-x c -std=c11 -fexceptions)
# .cc files are GNU libstdc++ code (Apple's GCC 4.2.1 one, README "GNU
# libstdc++"): its own headers, C++98, and default visibility with inline
# functions kept (Apple's build: GCC_INLINES_ARE_PRIVATE_EXTERN = NO), since
# games import the members GCC didn't inline from the library.
GNU_FRONT=(
    "${COMMON[@]}" -isystem "$GNU/include/c++/4.2.1" -isystem "$GNU/include/c++/4.2.1/backward" "${C_HEADERS[@]}"
)
GNU_CXX=(-x c++ -std=gnu++98 -nostdinc++ -fexceptions -frtti)
# reserve-r9: Apple's armv7 code doesn't keep r9 across calls (iPhone OS 3+),
# while this triple's rules would keep values there across calls into game
# code (virtual functions, callbacks); with r9 reserved this code never uses it.
LLC=(-mtriple=thumbv7-unknown-unknown -mattr=+reserve-r9 -target-abi=apcs-gnu -exception-model=sjlj -relocation-model=pic -O2 -filetype=obj)

# compile <source> <object> [flags...]
compile() {
    local source=$1 object=$2
    shift 2
    local front=("${FRONT[@]}") language=("${CXX[@]}")
    [[ $source == *.c ]] && language=("${CC[@]}")
    [[ $source == *.cc ]] && front=("${GNU_FRONT[@]}") language=("${GNU_CXX[@]}")
    if ! clang "${front[@]}" "${language[@]}" "$@" -emit-llvm -c "$source" -o "$object.bc" 2>"$object.log" ||
        ! llc "${LLC[@]}" "$object.bc" -o "$object" 2>>"$object.log" ||
        ! init_array "$object" 2>>"$object.log"; then
        echo "FAILED: $source" >&2
        head -40 "$object.log" >&2
        return 1
    fi
    rm -f "$object.bc" "$object.log"
}

# With APCS the ARM ELF backend always puts static constructors in .ctors
# (ARMElfTargetObjectFile: init_array only for AAPCS), which src/elf.rs
# doesn't run. Same order as .init_array: .ctors.N runs before
# .init_array.(65535-N). Within one object the order of a section's entries
# turns around (.ctors runs last to first), so only one ordered entry is
# allowed: the file's own initializer, besides those of template static
# members (GNU libstdc++'s locale::id instances, guarded), which C++ leaves
# unordered.
init_array() {
    local object=$1 renames=() section size ordered
    while read -r section size; do
        ordered=$(llvm-objdump -r "$object" | awk -v records="RELOCATION RECORDS FOR [$section]:" \
            '$0 == records { inside = 1; next } /^RELOCATION RECORDS/ { inside = 0 } inside && /_GLOBAL__sub_I/' | wc -l)
        ((ordered <= 1)) || { echo "$object: more than one ordered constructor in $section" >&2; return 1; }
        case $section in
        .ctors) renames+=(--rename-section "$section=.init_array") ;;
        .ctors.*) renames+=(--rename-section "$section=.init_array.$(printf '%05d' $((65535 - 10#${section#.ctors.})))") ;;
        .dtors*) echo "$object: $section (static destructors should use __cxa_atexit)" >&2; return 1 ;;
        esac
    done < <(llvm-objdump -h "$object" | awk '$2 ~ /^\.(ctors|dtors)/ { print $2, $3 }')
    [ ${#renames[@]} = 0 ] || llvm-objcopy "${renames[@]}" "$object"
}

# Runs up to $JOBS compiles at once; any failure fails the build.
pending=0
failed=0
spawn() {
    if ((pending >= JOBS)); then
        wait -n || failed=1
        pending=$((pending - 1))
    fi
    compile "$@" &
    pending=$((pending + 1))
}
finish() {
    while ((pending > 0)); do
        wait -n || failed=1
        pending=$((pending - 1))
    done
    [ $failed = 0 ] || { echo "build.sh: some sources failed" >&2; exit 1; }
}

LIBCXX=(-D_LIBCPP_BUILDING_LIBRARY -DLIBCXX_BUILDING_LIBCXXABI -I"$SRC/libcxx/src")
LIBCXXABI=(-D_LIBCXXABI_BUILDING_LIBRARY -D_LIBCPP_BUILDING_LIBRARY -DHAVE___CXA_THREAD_ATEXIT_IMPL=0 -I"$SRC/libcxxabi/src" -I"$SRC/libcxx/src")

# Left out: new.cpp (libc++abi defines new/delete), C++20 waiting atomics and
# barriers (Apple's private __ulock calls), std::print.
objects=()
for f in "$SRC"/libcxx/src/*.cpp "$SRC"/libcxx/src/ryu/*.cpp; do
    case $(basename "$f") in new.cpp | atomic.cpp | barrier.cpp | print.cpp) continue ;; esac
    object=$WORK/cxx_$(basename "$f" .cpp).o
    objects+=("$object")
    extra=()
    # std::__throw_bad_alloc() is GNU libstdc++'s (the same function): libc++'s
    # own mode for sitting on libstdc++ keeps only std::nothrow from this file.
    [ "$(basename "$f")" = new_helpers.cpp ] && extra=(-DLIBSTDCXX)
    spawn "$f" "$object" "${LIBCXX[@]}" "${extra[@]}"
done
for f in "$SRC"/libcxxabi/src/*.cpp; do
    # cxa_thread_atexit: C++11 thread_local, which needs iPhone OS 8.
    case $(basename "$f") in cxa_noexception.cpp | cxa_thread_atexit.cpp) continue ;; esac
    object=$WORK/abi_$(basename "$f" .cpp).o
    objects+=("$object")
    spawn "$f" "$object" "${LIBCXXABI[@]}"
done
# Apple's GCC 4.2.1 libstdc++ (std::, beside libc++'s std::__1::): all but
# its libsupc++ files (new/delete, exceptions, type_info: libc++abi's here,
# one definition each) and stdexcept.cc (the exception classes are
# libc++abi's too; glue/libstdcxx.cpp adds GCC's constructors from a string).
for f in "$GNU"/src/*.cc; do
    case $(basename "$f") in del_op*.cc | new_op*.cc | new_handler.cc | eh_*.cc | tinfo.cc | stdexcept.cc) continue ;; esac
    object=$WORK/gnu_$(basename "$f" .cc).o
    objects+=("$object")
    spawn "$f" "$object" -DBUILDING_LIBSTDCXX -w
done
objects+=("$WORK/unwind_sjlj.o")
spawn "$SRC/libunwind/src/Unwind-sjlj.c" "$WORK/unwind_sjlj.o" -I"$SRC/libunwind/src" -I"$HERE/darwin-include/unwind"
# The helpers llc calls for division (armv7 has no divide instruction) and
# 64-bit integer/float conversions, kept inside the library (hidden).
for name in udivsi3 divsi3 umodsi3 modsi3 udivmodsi4 divmodsi4 udivdi3 divdi3 umoddi3 moddi3 udivmoddi4 divmoddi4 \
    fixdfdi fixsfdi fixunsdfdi fixunssfdi floatdidf floatdisf floatundidf floatundisf; do
    object=$WORK/rt_$name.o
    objects+=("$object")
    spawn "$SRC/compiler-rt/lib/builtins/$name.c" "$object" -fno-builtin
done
for f in "$HERE"/glue/*.c "$HERE"/glue/*.cpp; do
    [ -e "$f" ] || continue
    object=$WORK/glue_$(basename "${f%.*}").o
    objects+=("$object")
    spawn "$f" "$object" "${LIBCXXABI[@]}"
done
for f in "$HERE"/glue/*.cc; do
    object=$WORK/glue_$(basename "$f" .cc).o
    objects+=("$object")
    spawn "$f" "$object" -DBUILDING_LIBSTDCXX
done
finish

LIB=$OUT/libguest32cxx.so
ld.lld -shared -soname libguest32cxx.so --hash-style=sysv -z now -z noexecstack --gc-sections \
    -o "$LIB" "${objects[@]}"
"$HERE/check-library.sh" "$LIB"
echo "built $LIB"

if [ "${1:-}" = test ]; then
    mkdir -p "$OUT/test"
    compile "$HERE/test/testc.c" "$WORK/test_testc.o" -fno-builtin
    compile "$HERE/test/cxxtest.cpp" "$WORK/test_cxxtest.o"
    ld.lld -shared -soname libtestc.so --hash-style=sysv -z now -o "$OUT/test/libtestc.so" "$WORK/test_testc.o" "$WORK"/rt_*.o
    cp "$LIB" "$OUT/test/"
    # libtestc.so first: its exports are known before the library's imports are bound.
    ld.lld -shared -soname libcxxtest.so --hash-style=sysv -z now -o "$OUT/test/libcxxtest.so" \
        "$WORK/test_cxxtest.o" "$OUT/test/libtestc.so" "$OUT/test/libguest32cxx.so"
    # The GNU libstdc++ checks: GNU code, plus a libc++ file catching its exceptions.
    compile "$HERE/test/gnutest.cc" "$WORK/test_gnutest.o"
    compile "$HERE/test/gnucatch.cpp" "$WORK/test_gnucatch.o"
    ld.lld -shared -soname libgnutest.so --hash-style=sysv -z now -o "$OUT/test/libgnutest.so" \
        "$WORK/test_gnutest.o" "$WORK/test_gnucatch.o" "$OUT/test/libguest32cxx.so"
    echo "built $OUT/test"
fi
