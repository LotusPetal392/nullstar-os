# Developing NullStar OS

NullStar OS combines a host-side runner, freestanding kernel and userspace artifacts,
host-testable libraries, and host filesystem tools. Keeping host and target environments
separate is the most important part of a predictable development setup.

## Toolchain model

`rust-toolchain.toml` pins `nightly-2026-02-01` with `rust-src`,
`llvm-tools-preview`, `rustfmt`, `clippy`, `rust-analyzer`, and the
`x86_64-unknown-none` target. The pinned nightly is required for Cargo artifact
dependencies and the kernel's x86 interrupt ABI. `.cargo/config.toml` enables `bindeps`.

Run commands from the repository root so Rustup discovers the pinned toolchain:

```sh
rustup show active-toolchain
cargo --version
rustc --version
```

All should identify `nightly-2026-02-01`. An explicit
`+nightly-2026-02-01` override remains useful in scripts and diagnostics.

## Workspace and target layout

The root workspace contains more than the host runner, kernel, and bundled userspace.
It also includes the separately packaged NullFS service, host-testable NullFS crates,
and host tools such as `mkfs-nullfs`, `nullfs-image`, `nullfs-info`, `fsck-nullfs`, and
`nullfs-fuse`.

The root runner is built for the host. The kernel, bundled userspace binaries, and
freestanding NullFS service are built for `x86_64-unknown-none`. Host libraries and tools
use the host target unless an individual command explicitly selects otherwise.

Do not substitute `cargo build --workspace` for the release-build command used by the
repository checks. That mode attempts to link freestanding `_start` binaries as ordinary
host executables.

## Local and automated checks

GitHub Actions runs `.github/workflows/kernel-qemu.yml` for pull requests and pushes to
`main`. It checks formatting, builds the image, and runs QEMU smoke validation. Automated
results complement rather than replace local verification.

Use the checked-in wrapper:

```sh
./scripts/check-local.sh --quick
```

The quick path runs formatting, workspace tests, Clippy with warnings denied, and a
release build using the pinned toolchain and lockfile.

Before publishing, run:

```sh
./scripts/check-local.sh
```

The complete path additionally runs normal-boot readiness, the two-boot smoke suite,
the targeted NullFS replacement phase, and logging lifecycle convergence. The targeted
paths can be run directly:

```sh
cargo +nightly-2026-02-01 run --release --locked -- --nullfs-restart-check
cargo +nightly-2026-02-01 run --release --locked -- --logging-lifecycle-check
```

The logging lifecycle image validates live `start`, `stop`, and `restart`, immediate route
withdrawal, fresh generation objects, duplicate-restart `Busy` fencing, exact filesystem
`Start`/`Stop` `Unsupported` responses, escalation from cooperative termination to
uncatchable forced termination after a bounded grace period, and repeated no-readiness
failure through bounded restart/backoff into the terminal `Failed` state.

QEMU must be available for integrated checks.

## Rust-analyzer

The repository contains host and freestanding targets:

- the root runner and host tools use the host target;
- the kernel and freestanding userspace use `x86_64-unknown-none`.

Do not set `rust-analyzer.cargo.target` to `x86_64-unknown-none` for the entire root
workspace. When concentrating on kernel or userspace code, open that package as a
separate editor workspace and configure its target explicitly:

```json
{
  "rust-analyzer.cargo.target": "x86_64-unknown-none",
  "rust-analyzer.check.allTargets": false,
  "rust-analyzer.check.command": "check"
}
```

Restart rust-analyzer after changing toolchain or target settings. The checked-in local
verification script is authoritative when editor diagnostics disagree with Cargo.

## Adding a userspace program

Bundled programs are explicit artifacts. To add one:

1. Create `userspace/src/bin/<name>.rs` with `#![no_std]`, `#![no_main]`,
   `userspace::entry!(...)`, and `userspace::panic_handler!()`.
2. Add a `[[bin]]` entry to `userspace/Cargo.toml` with tests and benches disabled.
3. Read `CARGO_BIN_FILE_USERSPACE_<name>` in the root `build.rs` and include the artifact
   in the generated image.
4. Put target-independent logic in a testable library module where practical.
5. Run the complete local check before publishing.

## Path and executable resolution

Filesystem operations support both absolute paths and paths relative to the process's
validated current working directory. The resolver canonicalizes `.`, `..`, and repeated
separators.

Executable lookup currently has two modes:

```text
cat              bare command name; resolves in the root command namespace as /cat
./cat            explicit relative executable path
../tools/cat     explicit relative executable path
/System/bin/cat  absolute executable path
```

A bare name does **not** search a configurable `PATH` yet. An executable name containing
a slash is resolved relative to the current working directory unless it begins with
`/`. Redirection and ordinary filesystem paths may also be relative.

`PWD` is kernel-managed working-directory state rather than a caller-controlled claim.
See [Userspace ABI](syscall-abi.md) for the authoritative current behavior.

## Unsafe-code expectations

The kernel and userspace runtime deny unsafe operations inside `unsafe fn` bodies. Keep
unsafe blocks small and document the invariant that makes each valid. In particular:

- interrupt and context-switch assembly must preserve frame layout and stack alignment;
- page-table code must prove ranges and frame ownership;
- userspace pointers must be checked and copied while the owning address space is active;
- allocators must preserve alignment and prevent overlapping regions;
- filesystem and protocol parsers must use checked arithmetic and bounded allocation.

Prefer target-independent state machines that can be tested on the host.

## Common failures

### `unwinding panics are not supported without std`

Build freestanding packages through the pinned workspace and target. The repository
profiles use `panic = "abort"`; a standalone host-target invocation can bypass that.

### `duplicate symbol: _start`

Each freestanding binary defines exactly one entry point. The kernel uses
`bootloader_api::entry_point!`; userspace uses `userspace::entry!`. Do not add another
`_start` or link a freestanding binary as an ordinary host test executable.

### Bootloader or `rust-lld` failures

Confirm the pinned toolchain, target, `rust-src`, and `llvm-tools-preview` are installed.

### QEMU does not start

Confirm `qemu-system-x86_64` is available. The runner currently targets that executable
and the `q35` machine.

## Before opening a pull request

```sh
./scripts/check-local.sh
git diff --check
git status --short
```

Document new user-visible behavior in the root README or the appropriate current-system
guide, and update design documents separately when a change implements or revises an
accepted long-term direction.