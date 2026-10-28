#!/usr/bin/env bash
# Fetches a pinned source tree into $CACHE and checks it against the recorded
# git tree hashes. Prints the source folder. Safe to run again: a checked
# copy is kept. Needs git and network access the first time only.
#
#   fetch.sh             LLVM (libc++, libc++abi, libunwind, compiler-rt's builtins)
#   fetch.sh libstdcxx   Apple's GCC 4.2.1 libstdc++ (iPhone OS 5/6's libstdc++.6.dylib)
set -euo pipefail

CACHE=${GUEST32_CXX_CACHE:-$HOME/.cache/gamebridge/guest32-cxx}

case ${1:-llvm} in
llvm)
    NAME=llvm-19.1.7
    URL=https://github.com/llvm/llvm-project
    COMMIT=cd708029e0b2869e80abe31ddb175f7c35361f90 # llvmorg-19.1.7^{}
    # `git rev-parse $COMMIT:<folder>`: a content checksum of each folder.
    declare -A TREES=(
        [libcxx]=7bcf87ad3b5284406c816d9038759dddb5ef5bfe
        [libcxxabi]=c3aa69d7f2e91b7f5d70c403e91aefb01ff44be3
        [libunwind]=d5269f75a5dd964b1a64554d20c73b443f7b09da
        [compiler-rt/lib/builtins]=49411bce0ebb9484848b1edebfc7a88d0b891805
    )
    ;;
libstdcxx)
    NAME=libstdcxx-53.4
    URL=https://github.com/apple-oss-distributions/libstdcxx
    COMMIT=b12878ae37c4be87e85a4ce5ccd2799ffc26e269 # libstdcxx-53.4^{}
    declare -A TREES=(
        [src]=b197881de693adb033af64376b640e783c8b9782
        [include]=775b142f608a204d92e4f268af12c074c6ec1407
        [exports]=feb0c27190d0649fc3d7fec58387b228afa82617
    )
    ;;
*) echo "fetch.sh: unknown source $1" >&2; exit 1 ;;
esac
SRC=$CACHE/$NAME

if [ ! -d "$SRC/.git" ]; then
    mkdir -p "$CACHE"
    rm -rf "$SRC.partial"
    git init -q "$SRC.partial"
    git -C "$SRC.partial" remote add origin "$URL"
    git -C "$SRC.partial" fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
    mv "$SRC.partial" "$SRC"
fi
# Only these folders are checked out (their files are fetched on demand).
folders=("${!TREES[@]}")
if [ "$(git -C "$SRC" sparse-checkout list 2>/dev/null | sort)" != "$(printf '/%s\n' "${folders[@]}" | sort)" ]; then
    git -C "$SRC" sparse-checkout set --no-cone "${folders[@]/#//}"
    git -C "$SRC" -c advice.detachedHead=false checkout -q "$COMMIT"
fi

[ "$(git -C "$SRC" rev-parse HEAD)" = "$COMMIT" ] || { echo "fetch.sh: $SRC is not at $COMMIT" >&2; exit 1; }
for folder in "${folders[@]}"; do
    have=$(git -C "$SRC" rev-parse "HEAD:$folder")
    [ "$have" = "${TREES[$folder]}" ] || { echo "fetch.sh: $folder tree $have, expected ${TREES[$folder]}" >&2; exit 1; }
done
# Nothing in the checked-out folders may differ from the commit: no changed
# files, and no extra ones (build.sh compiles whole folders by pattern).
extra=$(git -C "$SRC" status --porcelain --untracked-files=all --ignored -- "${folders[@]}")
[ -z "$extra" ] || { echo "fetch.sh: $SRC differs from $COMMIT:" >&2; echo "$extra" >&2; exit 1; }
echo "$SRC"
