#!/usr/bin/env bash
# Source this before any cargo/rustup command:  source ./env.sh
#
# Rust is installed portably rather than system-wide, so it stays out of PATH
# and removing it is a folder delete (see TEARDOWN.md). Override DEEPSLATE_RUST_HOME
# if yours lives elsewhere.
: "${DEEPSLATE_RUST_HOME:=C:/Users/serge/tools/rust}"

export RUSTUP_HOME="${DEEPSLATE_RUST_HOME}/rustup"
export CARGO_HOME="${DEEPSLATE_RUST_HOME}/cargo"
export PATH="$(cygpath -u "${DEEPSLATE_RUST_HOME}" 2>/dev/null || echo "${DEEPSLATE_RUST_HOME}")/cargo/bin:$PATH"
