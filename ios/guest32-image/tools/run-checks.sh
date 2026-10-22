#!/usr/bin/env bash
# All checks for guest32-image in one go: unit tests, the Android build check,
# then compare-llvm.sh for each real program given as "<label>=<path>[@arch]".
# EXPECT_<LABEL> (label upper-cased) is passed on as EXPECT_PLAN.
#
# usage: run-checks.sh [label=path[@arch] ...]
set -uo pipefail
here=$(cd "$(dirname "$0")/.." && pwd)
status=0
step() { echo "== $*"; "$@" || { echo "FAILED: $*"; status=1; }; }

step cargo test --manifest-path "$here/Cargo.toml"
step cargo check --all-targets --target aarch64-linux-android --manifest-path "$here/Cargo.toml"
for input in "$@"; do
  label=${input%%=*}
  rest=${input#*=}
  path=${rest%@*}
  arch=""
  [[ $rest == *@* ]] && arch=${rest##*@}
  expect_var="EXPECT_${label^^}"
  expect_var=${expect_var//-/_}
  EXPECT_PLAN=${!expect_var:-} step "$here/tools/compare-llvm.sh" "$path" "$label" $arch
done
echo "== overall: $([[ $status == 0 ]] && echo PASS || echo FAIL)"
exit $status
