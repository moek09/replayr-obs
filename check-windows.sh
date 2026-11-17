#!/usr/bin/env bash
# Type-checks the Windows code from Linux (no linking, nothing runs).
#
# Needs: rustup target x86_64-pc-windows-msvc, clang, llvm-ar and the MinGW-w64
# C headers (mingw-w64-x86-64-dev) for the small C files in build scripts.
set -euo pipefail
cd "$(dirname "$0")"

export CC_x86_64_pc_windows_msvc=clang
export CFLAGS_x86_64_pc_windows_msvc="--target=x86_64-w64-mingw32"
export AR_x86_64_pc_windows_msvc=llvm-ar
# CARGO_CMD=clippy ./check-windows.sh -- -D warnings runs Clippy instead.
exec cargo "${CARGO_CMD:-check}" --target x86_64-pc-windows-msvc --workspace --all-targets "$@"
