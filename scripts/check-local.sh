#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

case "${1:-}" in
    "") run_qemu=true ;;
    --quick) run_qemu=false ;;
    -h|--help)
        echo "Usage: scripts/check-local.sh [--quick]"
        echo "  --quick  Skip the normal-boot and full QEMU smoke checks"
        exit 0
        ;;
    *)
        echo "Unknown argument: $1" >&2
        exit 2
        ;;
esac

if (( $# > 1 )); then
    echo "Expected at most one argument" >&2
    exit 2
fi

toolchain=+nightly-2026-02-01

cargo "$toolchain" fmt --all -- --check
cargo "$toolchain" test --workspace --locked
cargo "$toolchain" clippy --workspace --all-targets --locked -- -D warnings
# The root package builds kernel/userspace artifacts for x86_64-unknown-none.
# `build --workspace` would additionally try to link freestanding `_start`
# binaries as Linux executables, which is not a meaningful build mode here.
cargo "$toolchain" build --release --locked

if [[ "$run_qemu" == true ]]; then
    cargo "$toolchain" run --release --locked --quiet -- --boot-check
    cargo "$toolchain" run --release --locked --quiet -- --test
fi
