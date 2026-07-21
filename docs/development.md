# Developing GalacticOS

GalacticOS mixes a host-side runner with bare-metal kernel and userspace
artifacts. Keeping those targets separate is the most important part of a
predictable development environment.

## Toolchain model

`rust-toolchain.toml` pins `nightly-2026-02-01` with:

- `rust-src`
- `llvm-tools-preview`
- `rustfmt`
- `clippy`
- `rust-analyzer`
- the `x86_64-unknown-none` target

The pinned nightly is required for Cargo artifact dependencies and the kernel's
x86 interrupt ABI. `.cargo/config.toml` enables `bindeps` for the workspace.

Run commands from the repository root so Rustup discovers the pinned toolchain:

```sh
rustup show active-toolchain
cargo --version
rustc --version
```

All three should identify `nightly-2026-02-01` when invoked here. An explicit
`+nightly-2026-02-01` override is useful in scripts and diagnostics.

## Local checks

GitHub Actions are temporarily disabled. The preserved workflow is parked at
`.github/workflows-disabled/kernel-qemu.yml`; move it back to
`.github/workflows/kernel-qemu.yml` to restore automatic pull-request and
`main`-branch checks. While it is disabled, the complete local check below is
the required pre-push verification path.

Use the checked-in wrapper rather than maintaining a separate command list:

```sh
./scripts/check-local.sh --quick
```

The quick path runs:

```sh
cargo +nightly-2026-02-01 fmt --all -- --check
cargo +nightly-2026-02-01 test --workspace --locked
cargo +nightly-2026-02-01 clippy --workspace --all-targets --locked -- -D warnings
cargo +nightly-2026-02-01 build --release --locked
```

Run the complete pre-push check with:

```sh
./scripts/check-local.sh
```

It additionally executes:

```sh
cargo +nightly-2026-02-01 run --locked --quiet -- --boot-check
cargo +nightly-2026-02-01 run --locked --quiet -- --test
```

The first command verifies normal boot. The second creates a temporary smoke
image and boots it twice to verify persistent FAT writes as well as the complete
subsystem suite. QEMU must be installed for the full path.

Do not substitute `cargo build --workspace` for the release-build command. That
mode also tries to link the freestanding `_start` binaries as host executables,
which is not a meaningful workspace build.

## Rust-analyzer

The repository root contains two target kinds:

- `galactic-os` must be analyzed for the host target because it launches QEMU.
- `kernel` and userspace binaries must be analyzed for
  `x86_64-unknown-none`.

Do not set `rust-analyzer.cargo.target` to `x86_64-unknown-none` for the entire
root workspace; that makes the host runner use the bare-metal target. If the
editor cannot infer the artifact target for kernel work, open `kernel/` as a
separate editor workspace and configure:

```json
{
  "rust-analyzer.cargo.target": "x86_64-unknown-none",
  "rust-analyzer.check.allTargets": false,
  "rust-analyzer.check.command": "check"
}
```

Use the same approach with `userspace/` when concentrating on a userspace
binary. Restart rust-analyzer after changing the toolchain or target. The local
check script is authoritative when editor diagnostics and Cargo disagree.

## Adding a userspace program

Bundled programs are explicit build artifacts. To add one:

1. Create `userspace/src/bin/<name>.rs` with `#![no_std]`, `#![no_main]`,
   `userspace::entry!(...)`, and `userspace::panic_handler!()`.
2. Add a `[[bin]]` entry to `userspace/Cargo.toml` with tests and benches
   disabled for the freestanding binary.
3. Read `CARGO_BIN_FILE_USERSPACE_<name>` in the root `build.rs` and add the
   artifact to `build_image`.
4. Add host-testable logic to a library module where practical; reserve QEMU
   tests for behavior that depends on the kernel or emulated hardware.
5. Run the complete local check before publishing the change.

Program names embedded at the image root are resolved with or without a leading
slash by the process-spawn command parser. Filesystem paths themselves must be
absolute.

## Unsafe-code expectations

The kernel and userspace runtime deny unsafe operations inside `unsafe fn`
bodies. Keep unsafe blocks small and document the invariant that makes each one
valid. In particular:

- interrupt and context-switch assembly must preserve its documented frame
  layout and stack alignment
- page-table code must prove mapped ranges and frame ownership
- raw userspace pointers must be range-checked and copied while the owning
  address space is active
- global allocator changes must preserve alignment and avoid overlapping free
  regions

Prefer extracting target-independent state machines into library modules so
they can be covered by host tests.

## Common failures

### `unwinding panics are not supported without std`

Build the freestanding crates through this workspace and pinned target. The root
profiles set `panic = "abort"`. A standalone host-target invocation can bypass
that setup and produce this error.

### `duplicate symbol: _start`

Each freestanding binary must define exactly one entry point. Kernel entry comes
from `bootloader_api::entry_point!`; userspace entry comes from
`userspace::entry!`. Do not add a second `_start` function or link a freestanding
binary as a normal host test executable.

### Bootloader or `rust-lld` build failures

Confirm the active pinned nightly and installed components:

```sh
rustup show active-toolchain
rustup component list --installed --toolchain nightly-2026-02-01
rustup target list --installed --toolchain nightly-2026-02-01
```

The required target is `x86_64-unknown-none`; the required low-level components
include `rust-src` and `llvm-tools-preview`.

### QEMU does not start

Confirm that the executable used by the runner is available:

```sh
qemu-system-x86_64 --version
```

The current runner assumes that exact executable name and the `q35` machine.

## Before opening a pull request

```sh
./scripts/check-local.sh
git diff --check
git status --short
```

Document new user-visible behavior in the root README or the appropriate guide,
and keep serial smoke markers synchronized with the host runner when an
integration test changes.
