# NullStar OS

NullStar OS is an experimental x86-64 operating system written in Rust. It
combines a freestanding `no_std` kernel with a small `no_std` ring-3 userspace
and boots as a BIOS disk image in QEMU.

The project is under active development. It is intended for operating-system
experimentation, not production use or untrusted workloads.

## Current capabilities

- x86-64 paging, physical-frame allocation, a coalescing kernel heap, GDT/TSS,
  an IDT, exception handling, and timer-driven scheduling
- ACPI discovery, PCIe ECAM enumeration, APIC/IOAPIC interrupts with legacy
  PIC/PIT fallback, a framebuffer console, serial diagnostics, and PS/2 keyboard
  input
- AHCI block access, MBR and GPT discovery, FAT12/16/32 reads, constrained
  FAT16 writes, a root VFS mount, and a bounded `/tmp` tmpfs
- ELF64 ring-3 processes, file descriptors, pipes, `fork` with copy-on-write,
  transactional `exec`, parent/child waiting, environments, process groups,
  terminal ownership, and a focused signal implementation
- a documented userspace platform ABI with system discovery, file metadata,
  paged directory reads, per-process working directories, descriptor
  duplication, parent-process lookup, direct child signaling, and controlled
  process-group reassignment
- a PID 1 userspace supervisor, a userspace shell (`ush`) with pipelines,
  redirection, variables, background jobs and basic job control, and an
  emergency kernel diagnostic shell
- separate normal-boot and destructive smoke-test images, plus host-side unit
  tests and a local pre-push check script

See [Architecture](docs/architecture.md) for how these pieces fit together,
[Userspace ABI](docs/syscall-abi.md) for the ring-3 contract, and
[Development](docs/development.md) for the toolchain, test workflow, and common
build issues.

## Requirements

- [Rustup](https://rustup.rs/) and Cargo
- `qemu-system-x86_64` available on `PATH`
- Bash for the local check script

The repository pins `nightly-2026-02-01` and its required Rust components in
`rust-toolchain.toml`. Rustup installs them when needed. The first build also
needs network access to download Rust components and Cargo dependencies.

Development is currently tested on Linux. Hardware boot and other QEMU machine
models are not supported targets yet.

## Quick start

```sh
git clone https://github.com/LotusPetal392/nullstar-os.git
cd nullstar-os
cargo run
```

QEMU opens a display for the framebuffer and PS/2 keyboard. The host terminal
shows the serial log. Focus the QEMU window and enter `help` when the NullStar OS
prompt appears. Stop QEMU with `Ctrl-C` in the host terminal.

### Run modes

| Command | Behavior |
| --- | --- |
| `cargo run` | Boot the normal image with the QEMU display enabled. |
| `cargo run -- --headless` | Boot normally without a display; serial output only. |
| `cargo run -- --boot-check` | Boot headlessly and exit after PID 1 launches the userspace shell. |
| `cargo run -- --test` | Run the full headless smoke suite, including persistent FAT verification across two boots. |
| `cargo run -- --help` | Show all runner options. |

The smoke runner copies its dedicated image to a temporary file before testing,
so its persistence checks do not modify the normal boot image.

## Using the shells

Normal boot starts `/init` as PID 1. Init launches `ush` as the foreground
userspace shell and supervises it. Program names may omit the leading slash. A
short session might look like this:

```text
ush> cat /hello.txt | upper
ush> pipe-producer > /tmp/message.txt
ush> cat /tmp/message.txt
ush> FILE=/hello.txt
ush> export FILE
ush> cat $FILE
ush> delay &
ush> jobs
ush> wait
ush> exit
```

After `exit`, init starts a fresh shell. Run `help` for the authoritative
userspace command list.

If init cannot start or terminates unexpectedly, the kernel enters its emergency
diagnostic shell. That shell exposes hardware and kernel-state commands such as
`memory`, `interrupts`, `pci`, `disk`, and `process`.

## Testing locally

GitHub Actions are temporarily disabled. Until the workflow is restored, these
local checks are the required verification path for every change.

Run the fast checks while iterating:

```sh
./scripts/check-local.sh --quick
```

This checks formatting, runs the host-side workspace tests, runs Clippy with
warnings denied, and creates a release build. Before publishing a change, run
the complete suite:

```sh
./scripts/check-local.sh
```

The full suite adds a normal-boot readiness check and the two-boot QEMU smoke
test. It can take several minutes. Individual Cargo commands should generally
include `--locked` so local results use the committed dependency graph.

## Repository layout

```text
.
├── build.rs          Builds normal and smoke-test BIOS disk images
├── src/              Host-side QEMU runner and smoke-test harness
├── kernel/           Freestanding x86-64 kernel
├── userspace/        Shared no_std runtime and bundled ring-3 programs
├── shared/           ABI definitions included by kernel and userspace
├── scripts/          Local verification commands
└── docs/             Architecture and development guides
```

The root package is a host executable. It uses Cargo artifact dependencies to
build the kernel and userspace for `x86_64-unknown-none`, assembles the disk
images, and launches QEMU.

## Current limitations

- The launcher produces BIOS images for QEMU's `q35` machine with one CPU and
  128 MiB of memory; UEFI and SMP are not wired into the current boot path.
- There is no networking stack or network driver.
- FAT writes are limited to regular files in the FAT16 root directory with 8.3
  names and a 1 MiB per-file bound. `/tmp` is volatile and intentionally small.
- Userspace has no standard library, libc, dynamic linker, package manager, or
  general POSIX compatibility. Programs are statically built into the image.
- Metadata, directory, working-directory, `open`, `spawn_command`, and `execve`
  operations accept explicit relative paths. Bare executable names still use
  the root-directory command namespace rather than a configurable search path.
- Descriptor-free new-process-group launches use `fork`, process-group control,
  and `execve`. Descriptor-bearing launches, joined pipeline stages, and the
  `/exec` compatibility launcher still use the legacy atomic spawn syscall.
- Process, descriptor, pipe, environment, job, and filesystem resources have
  fixed bounds. These keep failure behavior deterministic while the kernel is
  still evolving.
- Security hardening and broad hardware compatibility are incomplete.
