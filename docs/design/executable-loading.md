# Executable loading and linking direction

## Status

A 64-bit-native platform, ELF64 executable format, statically linked bootstrap and
recovery environment, strict segment permissions, and explicit deferral of 32-bit
compatibility are **accepted direction**.

PIE and ASLR are the preferred next security step after the current fixed-address static
loader is mature. A versioned dynamic loader and shared platform libraries are accepted
long-term direction, but exact relocation support, shared-library ABI, symbol-version
rules, and loader implementation remain **tentative design**.

## Native architecture

The native platform is consistently 64-bit:

```text
kernel                 x86_64
system services        x86_64
userspace drivers      x86_64
recovery environment   x86_64
native applications    x86_64
native libraries       x86_64
```

Architecture-independent assets and packages may use the package architecture `any`.
Native executables and libraries use `x86_64`.

The loader should recognize an ELF32/i386 file and return a specific unsupported-
architecture result rather than treating it as arbitrary corruption. Native execution
of ELF32 is a distant optional compatibility milestone and does not constrain the first
loader or syscall ABI.

## ELF role

ELF is the container for machine-code executables, relocatable link inputs, shared
objects, debug information, notes, and eventual core dumps. The kernel or process loader
needs only the program-header view of an executable; section headers primarily serve
linkers, debuggers, symbol tools, and package tooling.

The initial runtime loader should support a deliberately small profile rather than every
GNU or Linux ELF extension.

## Initial executable profile

The first accepted executable profile is:

```text
ELF class:       ELF64
encoding:        little endian
machine:         x86_64
file type:       statically linked executable
program loading: PT_LOAD segments
entry:           validated 64-bit virtual address
permissions:     read, write, execute derived from segment flags
```

The loader should not require section headers, dynamic symbol tables, a dynamic
interpreter, TLS, constructors, or runtime relocations for the initial profile.

## Header validation

Before mapping any page, the loader must validate:

- ELF magic, class, byte order, version, machine, and supported file type;
- header and program-header entry sizes;
- program-header table offset, count, and total bounds;
- every file offset, virtual address, size, and alignment using checked arithmetic;
- `file_size <= memory_size` for loadable segments;
- file ranges remain inside the executable object;
- virtual ranges are canonical user addresses and do not overlap reserved regions;
- segment alignments are powers of two and file/virtual alignment constraints match;
- loadable segments do not overlap with incompatible permissions;
- the entry point lies inside an executable mapped segment;
- no segment requests unsupported writable-and-executable policy.

Every malformed input must fail before committing a replacement image. `exec` continues
to build and validate a new address space transactionally so failure leaves the old
process intact.

## Program headers

The loader should understand program headers incrementally.

### Initial

- `PT_LOAD`: maps file-backed bytes and zero-fills the remainder;
- `PT_PHDR`: optional metadata describing the program-header table;
- `PT_NOTE`: inspected by tooling and, later, by the loader for defined NullStar notes;
- `PT_GNU_STACK` or a NullStar equivalent: may request stack properties, but executable
  stacks are denied by default.

### Later

- `PT_INTERP`: identifies the versioned NullStar dynamic loader;
- `PT_DYNAMIC`: describes dynamic dependencies, relocations, and symbol metadata;
- `PT_TLS`: supplies the initial thread-local-storage image;
- `PT_GNU_RELRO` or a NullStar equivalent: identifies pages made read-only after
  relocation;
- unwind and exception metadata required by supported language runtimes.

Unknown mandatory program-header types are rejected. Unknown ignorable metadata may be
skipped only when its specification permits that behavior.

## Segment mapping

For each `PT_LOAD` segment, the loader:

1. reserves the complete page-aligned virtual range;
2. maps fresh frames with temporary loader permissions where required;
3. copies exactly the file-backed bytes;
4. zeroes `memory_size - file_size`, including the tail of the last file page;
5. applies final page permissions;
6. records the authoritative VM-area description;
7. invalidates stale translations before execution.

Segment flags translate to page protections, but policy may further reduce them. The
native policy is W^X: a page is not writable and executable at the same time. Read-only
data remains non-executable; writable data and stacks remain non-executable.

The loader must define behavior for segments that share a page because ELF flags are
segment-oriented while hardware protection is page-oriented. The preferred rule is to
reject layouts that would require combining write and execute rights on one page.

## BSS and zero-fill

ELF represents zero-initialized storage by making a load segment's in-memory size larger
than its file size. The loader must zero the complete difference and never expose stale
physical-page contents. Future lazy zero-fill may reserve pages and materialize them on
fault without changing the executable ABI.

## Sections, symbols, and debug data

Section headers are not authoritative runtime mappings. The kernel loader should not
map sections by name or depend on `.text`, `.data`, or `.bss` labels.

Symbol and DWARF information are useful for:

- kernel and userspace stack traces;
- debugging and profiling;
- crash reports and symbol servers;
- package ownership and build-ID lookup.

Production packages may place debug data in separate `debug-symbols` packages. Stripped
runtime binaries should retain the build ID and any mandatory NullStar ABI note.

## Static linking policy

Static linking remains the default for:

- the bootstrap image;
- the recovery environment;
- PID 1 bootstrap components;
- NullFS checking and repair tools;
- boot-generation selection and package recovery tools;
- early services while their internal library ABIs are unstable;
- small security-critical utilities that must run when `/System/lib` is damaged.

Static executables are self-contained, easier to load, independently rollbackable, and
avoid a dynamic-loader dependency in the repair path. Magnetar should record embedded
component versions so statically linked vulnerable code can still be audited and
rebuilt.

## PIE and ASLR

Position-independent executables should be introduced after fixed-address ELF64 loading
is reliable. A PIE executable is represented as a dynamically relocatable ELF image but
may still be otherwise statically linked.

The loader should choose randomized bases for:

- executable images;
- shared libraries later;
- anonymous mappings;
- heaps;
- thread stacks;
- selected kernel mappings where architecture policy permits.

Randomization uses cryptographically suitable entropy and preserves alignment,
canonical-address, guard, and collision constraints. Exact addresses are never part of
the stable userspace ABI.

PIE support requires a deliberately bounded relocation subset. Unsupported relocation
records fail loading rather than being ignored.

## Dynamic linking direction

A future dynamic executable identifies a NullStar interpreter through `PT_INTERP`, for
example a versioned loader under `/System/lib`. The kernel maps the executable and
interpreter, supplies a versioned startup block, and transfers control to the
interpreter. The interpreter validates dependencies and performs userspace relocation.

The dynamic loader must support:

- ELF shared objects built for the same architecture and NullStar ABI;
- dependency graph cycle and depth limits;
- deterministic library identity and ABI-major selection;
- eager relocation for privileged or boot-critical services;
- thread-local-storage allocation;
- constructors and destructors under documented ordering;
- RELRO and final W^X protection transitions;
- ASLR and position-independent libraries;
- application-private libraries;
- package-generation and build-ID attribution;
- actionable diagnostics when dependencies are unavailable or incompatible.

Lazy binding may be added only if it provides a measured benefit and does not weaken
security or failure determinism. Immediate binding is the safer initial policy.

## Library resolution

Library resolution is driven by verified package and deployment metadata, not arbitrary
ambient search paths. The default order should be conceptually:

1. application-private libraries declared by the active application generation;
2. versioned libraries in the active verified system generation;
3. an explicitly selected compatibility runtime.

The loader should not search the current working directory. Privileged services must
not load libraries from ordinary user-writable paths. An `LD_LIBRARY_PATH`-style escape
hatch, if provided for development, is disabled for privileged and production launches.

Multiple incompatible ABI-major versions may coexist. Applications depend on a library
identity and ABI major, not merely a filename found first in a directory.

## Static and dynamic use

The preferred mature hybrid is:

- statically link application-specific Rust crates and small private dependencies;
- dynamically link large stable platform libraries such as the native renderer, text
  stack, UI toolkit, accessibility client, libc compatibility layer, and common service
  clients;
- keep bootstrap and recovery fully static;
- keep untrusted extensions, drivers, applets, codecs, and LV2 plugins out of process
  even when dynamic linking exists.

A fully static GUI application remains valid and communicates with the compositor and
other services through the same protocols.

## Library architecture matching

A process may directly link or dynamically load only machine-code libraries built for
its own architecture and ABI:

```text
64-bit process -> 64-bit libraries
32-bit compatibility process -> 32-bit compatibility libraries
```

A 64-bit process cannot load a normal 32-bit `.so` or static object into the same
address space. Architecture-neutral assets, bytecode, and shared-memory protocols are
not native libraries and remain usable across process architectures when their formats
are explicitly word-size independent.

## Thread-local storage

TLS is deferred until the thread ABI is stable. The design must define:

- initial TLS image and alignment from `PT_TLS`;
- per-module TLS identifiers;
- static and dynamic TLS allocation;
- thread-pointer register convention;
- startup and thread-creation initialization;
- destructor ordering and process-exit behavior;
- limits that prevent unbounded module or per-thread growth.

The native Rust and C compatibility runtimes should share one documented TLS ABI rather
than inventing unrelated layouts.

## Constructors and destructors

Language-runtime initialization is a userspace runtime or dynamic-loader responsibility,
not a kernel policy. The order for pre-initialization, constructors, main entry, thread
cleanup, destructors, and process termination must be explicit and bounded.

The bootstrap and recovery environment should avoid depending on elaborate constructor
chains even after dynamic linking is available.

## NullStar ELF notes

NullStar may define one or more vendor notes for diagnostics and compatibility. Useful
fields include:

```text
native ABI family and minimum version
required loader ABI
build ID or package build identity
expected package ID and version
required service-protocol major versions
security or launch-profile hints
```

Embedded package identity is not authority. Magnetar and the launcher verify it against
the signed package manifest and active generation. A binary cannot grant itself package
identity, capabilities, or a trusted launch profile by writing a note.

Notes must use assigned names and types, fixed-width encoding, explicit sizes, and
forward-compatible unknown-field behavior.

## Build IDs and symbols

Every packaged executable and shared library should carry a stable build ID derived from
or bound to its exact content. Logging, crash reporting, debugging, and Magnetar use the
build ID to locate symbols and package provenance.

Separate symbol packages may be installed without changing the executable generation.
A symbol server or repository lookup must verify that symbols match the requested build
ID before use.

## Stable ABI boundary

Rust's compiler-private ABI is not the system shared-library contract. Native dynamic
libraries should expose a stable C-compatible or explicitly specified NullStar ABI, with
safe Rust crates wrapping that boundary.

Service protocols remain preferable when components need independent replacement,
strong isolation, language neutrality, or failure containment. Dynamic linking is for
large trusted code that genuinely benefits from in-process sharing.

## Security requirements

The loader and dynamic linker should enforce:

- W^X and non-executable stacks by default;
- PIE/ASLR for normal dynamic applications;
- RELRO after relocation;
- checked relocation types and target ranges;
- immutable verified library inputs from the active deployment;
- bounded dependency, relocation, symbol, TLS, and constructor processing;
- no ambient current-directory or user-writable search for privileged programs;
- package-generation and build-ID attribution in crash records;
- transactional `exec` and deterministic failure without partial image replacement.

Future control-flow enforcement, shadow stacks, CET notes, signed-code policies, or
advanced GNU properties should be added only when hardware support and threat analysis
justify them.

## Deferred 32-bit compatibility

A possible distant `org.nullstar.compat32` deployment may provide:

- ELF32/i386 loading;
- 32-bit syscall argument translation;
- 32-bit runtime, libc, TLS, and dynamic linker;
- selected 32-bit libraries;
- debugger and crash-report support;
- isolated 32-bit hosts for legacy plugins or codecs.

The kernel remains 64-bit. Compatibility processes communicate with native services
through fixed-width, pointer-free protocols and shared buffers. Legacy 32-bit plugins
run in a separate host process; they are never loaded into a 64-bit process.

NullStar will not add an x32-style third native ABI unless a future measured requirement
justifies its substantial compiler, loader, package, and test complexity.

## Implementation phases

1. Harden the existing static ELF64/x86-64 loader and malformed-input tests.
2. Centralize program-header validation, VM-area construction, final permissions, and
   precise loader errors.
3. Add non-executable stacks, build IDs, NullStar notes, and separate symbol-package
   conventions.
4. Add static PIE, a bounded relocation subset, and userspace ASLR.
5. Define the stable platform-library ABI and dynamic-loader startup contract.
6. Add shared objects, immediate relocation, dependency resolution, TLS, constructors,
   and RELRO.
7. Integrate library selection with Magnetar generations and application-private
   deployment metadata.
8. Add libc and external compatibility runtimes.
9. Consider optional 32-bit compatibility only after the 64-bit loader, runtime,
   debugger, package, and protocol contracts are mature.

## Open questions

- The first supported PIE relocation subset.
- The path and identity of the native dynamic loader.
- The stable platform-library ABI and symbol-version representation.
- Whether a NullStar-specific program-header type is necessary or notes are sufficient.
- The precise executable-signing relationship between package verification and runtime
  enforcement.
- Constructor, TLS, and unload support required by the first libc and C++ ports.
