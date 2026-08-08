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
- AHCI block access, MBR and GPT discovery, checked partition-relative read-only
  endpoints, narrowly scoped writable raw endpoints for discovered NullFS
  partitions, FAT12/16/32 reads, constrained FAT16 writes, a root VFS mount, and
  a bounded `/tmp` tmpfs
- a host-testable NullFS 1.2 stack with explicit little-endian records,
  authoritative allocation maps, bounded redo recovery, deterministic crash
  testing, formatter, image, inspection, and read-only checking tools, plus a
  userspace service with negotiated direct writable sessions and bounded public
  VFS writes
- ELF64 ring-3 processes, file descriptors, pipes, `fork` with copy-on-write,
  transactional `exec`, parent/child waiting, environments, process groups,
  terminal ownership, and a focused signal implementation including uncatchable,
  unblockable forced termination
- bounded per-process capability tables with rights-reduced duplication and
  delegation, message endpoints, counted notifications, shared byte-memory
  objects, and explicit direct-child bootstrap grants
- a documented userspace platform ABI with system discovery, file metadata,
  paged directory reads, per-process working directories, descriptor
  duplication, parent-process lookup, direct child signaling, and controlled
  process-group reassignment
- a PID 1 userspace supervisor, a userspace shell (`ush`) with pipelines,
  redirection, variables, background jobs and basic job control, and an
  emergency kernel diagnostic shell
- separate normal-boot and destructive smoke-test images, including non-destructive
  NullFS raw-endpoint identity, bounds, and flush checks during normal boot, plus
  host-side unit tests and a local pre-push check script

See [Architecture](docs/architecture.md) for how the implemented pieces fit
together, [Design direction](docs/design/README.md) and the
[architecture design roadmap](docs/design/roadmap.md) for accepted long-term
planning, [Userspace ABI](docs/syscall-abi.md) for the ring-3 contract,
[Protection model](docs/protection-model.md) for the capability and IPC
foundation, [Identity and access-control design](docs/identity-and-access.md) and
[device filesystem design](docs/devfs.md) for planned policy and `/dev`
architecture, [NullFS roadmap](docs/filesystems/nullfs-roadmap.md) for the native
persistent filesystem plan, and [Development](docs/development.md) for the
toolchain, test workflow, and common build issues.

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
| `cargo run -- --test` | Run the full headless suite, including persistent FAT, NullFS replacement, and logging lifecycle verification. |
| `cargo run -- --nullfs-restart-check` | Run only the targeted NullFS replacement and stale-descriptor check. |
| `cargo run -- --logging-lifecycle-check` | Run only logging start/stop/restart, route replacement, restart fencing, forced termination, and readiness-timeout recovery checks. |
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

GitHub Actions automatically run the kernel QEMU smoke workflow for pull
requests and pushes to `main`. Local checks remain the required pre-push
verification path because they exercise the complete development suite before a
change reaches GitHub.

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

The full suite adds a normal-boot readiness check, the two-boot QEMU smoke
test, NullFS replacement, and logging lifecycle convergence. It can take several
minutes. Individual Cargo commands should generally include `--locked` so local
results use the committed dependency graph.

## Repository layout

```text
.
├── build.rs          Builds normal, smoke-test, and targeted fault-test BIOS images
├── src/              Host-side QEMU runner and smoke-test harness
├── crates/           Host-testable shared libraries, including NullFS format code
├── kernel/           Freestanding x86-64 kernel
├── tools/            Host utilities for formatting, checking, inspecting, and mounting NullFS
├── userspace/        Shared no_std runtime and bundled ring-3 programs
├── shared/           ABI definitions included by kernel and userspace
├── scripts/          Local verification commands
└── docs/             Architecture, format, and development guides
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
  NullFS supports host-side writable images and recovery. PID 1 launches
  `nullfs-service --writable` with a partition-scoped raw `READ | WRITE | FLUSH`
  endpoint. The kernel proxy negotiates exactly `WRITE`, requires the returned
  `WRITE` session feature, and exposes bounded create, write, truncate, append,
  and unlink through the UUID-selected 4 MiB primary volume at
  `/Volumes/NullStar`; stat, read, open, `fstat`, seek, directory reads, and
  `chdir` also remain available. The volume contains `System/`, `Applications/`,
  and `Users/`; all three are implemented namespace bindings that retain
  canonical paths while using the volume's matching backing nodes.
  The System subtree is read-only through both canonical and raw public views.
  A statically linked fixture now executes through `/System/bin`, while PID 1 and
  recovery utilities remain independent bootstrap-image programs. The writable
  `/Users` binding contains a fixture home with the accepted managed `Profile`
  layout. Direct flags-zero sessions stay read-only, and raw block authority,
  session authority, and public VFS policy remain separate. The bounded version 1
  service-definition parser and one policy-pinned `/System/services` activation pilot are
  implemented; general discovery, enablement, dependencies, a separate manager, public
  `mkdir`, `rmdir`, rename, and offline repair remain future work.
- Userspace has no standard library, libc, dynamic linker, package manager, or
  general POSIX compatibility. Programs are statically built into the image.
- Metadata, directory, working-directory, `open`, `spawn_command`, and `execve`
  operations accept explicit relative paths. Bare executable names still use
  the root-directory command namespace rather than a configurable search path.
- Generic userspace launches use `fork`, descriptor duplication, process-group
  control, and `execve`. Pipeline stages wait on a close-on-exec launch barrier
  until the shell has finalized inherited pipe endpoints and group membership.
  The `/exec` helper follows the same path, so no bundled program calls syscall
  7's legacy atomic spawn operation. The kernel retains that entry point only
  to preserve the version-1 ABI contract.
- The capability and IPC layer is a migration foundation: existing drivers,
  filesystems, VFS routing, terminals, and pipes remain kernel-resident. Shared
  memory currently uses bounded copy operations rather than mapped pages, and
  there are no MMIO, IRQ, DMA, revocation, or service-discovery capabilities.
- Process, descriptor, pipe, capability, environment, job, and filesystem
  resources have fixed bounds. These keep failure behavior deterministic while
  the kernel is still evolving.
- Security hardening and broad hardware compatibility are incomplete.

## AI-assisted development

NullStar OS is developed and maintained by Natalie Rockot with substantial
assistance from OpenAI's ChatGPT for design discussion, implementation,
debugging, testing guidance, review, and documentation.

AI-assisted changes are reviewed and accepted by the project maintainer, who
remains responsible for the project's direction and published contents.

## License

NullStar OS is free software licensed under the GNU General Public License,
version 3 or (at your option) any later version (`GPL-3.0-or-later`). See
[`LICENSE`](LICENSE) for the complete license text.

Cargo dependencies and any other third-party components retain their respective
licenses. See [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md) for the
project's third-party licensing record.

Contributions are accepted under the same `GPL-3.0-or-later` terms. See
[`CONTRIBUTING.md`](CONTRIBUTING.md) before submitting material.
