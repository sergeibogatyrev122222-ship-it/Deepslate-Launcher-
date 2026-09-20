#!/usr/bin/env bash
# Every gate that must pass before a milestone is called done.
# Run from anywhere:  ./scripts/check.sh
set -euo pipefail

cd "$(dirname "$0")/.."
# shellcheck source=../env.sh
source ./env.sh

fail=0
run() {
  local name="$1"; shift
  printf '\n\033[1m== %s ==\033[0m\n' "$name"
  if "$@"; then
    printf '\033[32mPASS\033[0m %s\n' "$name"
  else
    printf '\033[31mFAIL\033[0m %s\n' "$name"
    fail=1
  fi
}

run "cargo fmt"    cargo fmt --all -- --check
run "cargo clippy" cargo clippy --all-targets --all-features -- -D warnings
run "cargo test"   cargo test --all
run "tsc"          npm --prefix ui run typecheck

printf '\n'
if [ "$fail" -ne 0 ]; then
  printf '\033[31mone or more gates failed\033[0m\n'
  exit 1
fi
printf '\033[32mall gates passed\033[0m\n'
