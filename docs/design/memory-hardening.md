# Memory-hardening policy

## Status

PIE for normal userspace executables, PIC for shared libraries, userspace ASLR,
guard pages, non-executable writable memory, non-writable executable memory,
non-executable stacks, null-page protection, stack canaries, and read-only
post-relocation data are **accepted direction**.

Kernel ASLR, hardware shadow stacks, control-flow enforcement, hardened allocator
sampling, and the exact JIT authorization model are **tentative design** pending
architecture support, threat analysis, and implementation experience.

This document defines the intended platform policy. The current implementation may
lag behind it. Implemented behavior remains authoritative until each mechanism is
landed and tested.

## Security invariant

NullStar should be designed around the following default rule:

> Writable memory is not executable, executable code is not writable, and normal
> software does not rely on predictable virtual addresses.

These mechanisms are defense in depth. They do not replace memory-safe code,
capability isolation, strict parsing, privilege separation, bounds checking, or
transactional failure handling.

## Baseline policy

| Mechanism | Platform policy |
| --- | --- |
| User/supervisor page separation | Mandatory |
| NX and W^X | Mandatory |
| Low-address protection | Mandatory |
| Kernel-stack guard pages | Mandatory |
| Userspace-stack guard pages | Default for every thread |
| Stack canaries | Enabled for supported kernel and userspace toolchains |
| PIE | Default for normal userspace executables |
| PIC | Required for shared libraries and dynamically loaded code |
| Userspace ASLR | Enabled by default once suitable entropy exists |
| RELRO-style protection | Apply final read-only permissions after relocation |
| KASLR | Planned after early boot and kernel relocation are mature |
| Writable executable mappings | Denied unless explicitly authorized |

Compatibility or development exceptions must be explicit, narrow, inspectable, and
unavailable to ordinary production applications by default.

## PIE policy

Normal userspace executables should be position-independent executables. Static
linking does not imply a fixed virtual address: statically linked programs should still
be eligible for PIE and ASLR.

Fixed-address, non-PIE executables may be retained only for narrowly scoped cases such
as early loader bring-up, architecture tests, debugging, or constrained compatibility.
They must require an explicit build or launch opt-out and must not become part of the
normal application ABI.

The executable loader must reject unsupported relocation types rather than ignoring
them. Exact load addresses are never a stable userspace contract.

## PIC policy

Shared libraries must be built as position-independent code. The same policy applies
to dynamically loaded trusted modules and plugin hosts.

NullStar should prefer isolated userspace services over general-purpose loadable kernel
modules. Where relocatable kernel code is supported, it must not weaken kernel W^X,
symbol isolation, or privilege boundaries.

## Userspace ASLR

Once the kernel has a trustworthy CSPRNG, each new executable image should receive a
fresh randomized layout. The loader should independently randomize, where practical:

- the main executable;
- shared libraries and the dynamic loader;
- the initial thread stack;
- additional thread stacks;
- heap and anonymous mapping bases;
- mapped files;
- shared-memory mapping locations;
- syscall-helper or vDSO-like pages if introduced.

Randomization must preserve canonical-address, alignment, guard-region, collision, and
reserved-range constraints. IPC and shared-memory protocols must not require two
processes to map an object at the same virtual address.

A fork-like operation may initially inherit its parent layout. An exec-like operation
must construct a fresh layout.

## Entropy requirements

ASLR and other security-sensitive randomization must use the kernel's initialized
cryptographically secure random-number generator. Raw CPU random instructions must not
be used directly as the sole policy interface.

The entropy pool may combine bootloader-provided entropy, validated CPU facilities,
hardware timing, and device events. If adequate entropy is unavailable, NullStar must
report degraded randomization explicitly rather than presenting deterministic placement
as strong ASLR.

## Debugging and deterministic execution

Debugging support should understand randomized layouts instead of depending on fixed
addresses. Panic records, core dumps, symbolizers, and debuggers should record or expose
module bases and relocation slides.

Development and test profiles may request deterministic randomization or disable ASLR
for a selected process. This must be controlled through a debugger or development
capability, not an unrestricted global production switch.

## Kernel ASLR

KASLR is accepted long-term direction but should follow reliable early paging, kernel
relocation, entropy initialization, and panic symbolization.

The kernel linker layout and startup code should avoid unnecessary absolute-address
assumptions so a future boot path can select a randomized higher-half virtual base.
KASLR is defense in depth and must not delay stronger invariants such as W^X, strict
page permissions, stack guards, or kernel/user isolation.

## Guard pages

### Kernel stacks

Every kernel stack should have at least one unmapped guard page at its overflow edge.
Interrupt and exception stacks require equivalent protection. The architecture must
provide a separate emergency path, such as a dedicated double-fault stack on x86-64,
so a stack overflow can still produce a controlled diagnostic rather than recursive
fault corruption.

Kernel stacks should remain fixed-size and measurable unless later evidence justifies a
bounded growth scheme.

### Userspace stacks

The initial process stack and every additional thread stack should have an unmapped
guard region at the growth edge. Lazy growth may be supported only with:

- a hard maximum size;
- a persistent guard region;
- collision checks against adjacent mappings;
- rejection of arbitrary long-distance automatic growth.

### Allocator and mapping guards

Guarding every allocation is too expensive as a universal production policy. Kernel
and userspace allocators should nevertheless support:

- guard pages around large allocations;
- randomized large-allocation placement;
- quarantine of recently freed objects;
- sampled guarded allocations;
- stronger debugging and hardening profiles.

Guard regions are also useful around selected IPC rings, shared-memory buffers, DMA
windows, per-CPU data, critical metadata arenas, and JIT mappings.

## W^X and executable memory

No page may be writable and executable at the same time under the normal mapping API.
The loader may use temporary writable mappings only when relocation requires them, and
must apply final read-execute or read-only permissions before control reaches the new
image.

Ordinary processes may not create executable anonymous memory. A future JIT facility
must require an explicit capability and provide a write-then-seal transition:

1. allocate read-write, non-executable memory;
2. generate code;
3. seal or remap it read-execute;
4. prevent mutation while executable.

A dual-mapping design may be considered, but it must still prevent any writable mapping
from also being executable and must be restricted to authorized runtimes.

## Stack and control-flow protection

Guard pages detect stack exhaustion but not every overwrite. NullStar should also use,
where supported:

- compiler stack canaries;
- non-executable stacks;
- randomized stack locations;
- bounded argument, environment, signal-frame, and exception-frame construction;
- hardware shadow stacks and control-flow enforcement when architecture support and
  toolchains are mature.

Rust reduces many classes of memory corruption, but unsafe code, assembly, foreign-code
compatibility, parsers, drivers, and device interaction still justify these defenses.

## Low-address protection

Normal processes must never map address zero. NullStar should reserve a larger low
range, initially at least the first 64 KiB, as permanently inaccessible to normal
userspace. This converts null and small-offset pointer dereferences into immediate
faults.

Any future compatibility exception must require explicit privilege and a demonstrated
need.

## Loader protections

The ELF loader and future dynamic linker must enforce:

- checked arithmetic for all file and virtual ranges;
- rejection of overlapping or malformed load segments;
- canonical userspace addresses and reserved-range exclusion;
- non-executable stacks by default;
- no writable executable final segment layout;
- bounded relocation, dependency, symbol, TLS, and constructor processing;
- immediate binding for privileged and boot-critical services;
- RELRO or equivalent final read-only relocation data;
- transactional exec with no partial image replacement;
- build-ID and mapping metadata suitable for symbolization.

Parsing complex dynamic-linker metadata should eventually occur in a restricted
userspace component where practical. The kernel should retain only the minimum mapping
and validation authority required by the executable ABI.

## Shared memory and IPC

Shared-memory objects are referred to by capabilities or handles, not process pointers.
Each recipient maps an object independently according to its own address-space policy.
Cross-process data structures must use explicit lengths, offsets, fixed-width fields,
and validated descriptors.

This rule preserves ASLR and prevents service protocols from depending on another
process's virtual layout.

## Runtime profiles

### Development

- PIE and PIC enabled;
- W^X and guard pages enabled;
- stack canaries enabled;
- deterministic ASLR available for selected tests;
- aggressive allocator guards and assertions;
- detailed fault records and symbols.

### Production

- full userspace ASLR;
- W^X and non-executable stacks enforced;
- guard pages for kernel and userspace stacks;
- stack canaries;
- RELRO-style final permissions;
- executable-memory creation capability-gated;
- debug controls restricted.

### Maximum hardening

- strongest supported ASLR and KASLR;
- no fixed-address legacy executable opt-outs;
- eager relocation;
- guarded-allocation sampling or hardened heaps;
- hardware shadow stacks or control-flow enforcement where available;
- reduced address disclosure and restricted core dumps.

## Implementation sequence

1. Enforce user/supervisor, writable, and executable page permissions.
2. Enforce NX and W^X.
3. Keep the null page and low-address guard range unmapped.
4. Add guard pages to kernel, interrupt, exception, and userspace stacks.
5. Enable compiler stack canaries where the freestanding toolchain supports them.
6. Complete per-process address-space and VM-area tracking.
7. Add static PIE and a bounded relocation subset.
8. Initialize the kernel CSPRNG from suitable entropy sources.
9. Add userspace ASLR for executables, stacks, heaps, and mappings.
10. Add dynamic PIC libraries, immediate relocation, and RELRO.
11. Add KASLR and slide-aware debugging.
12. Evaluate hardened allocator sampling and hardware control-flow protection.

The immediate architectural requirement is to avoid APIs, protocols, linker layouts,
and debugger assumptions that make fixed virtual addresses permanent.

## Open questions

- The first accepted PIE relocation subset.
- The initial userspace virtual-address layout and entropy budget.
- The size and placement of stack and low-address guard regions.
- The exact kernel and userspace stack-canary implementation for freestanding Rust.
- The JIT capability, sealing, and cache-coherency contract.
- Whether sampled guarded allocation belongs in the kernel, userspace runtimes, or both.
- The first architecture and toolchain combination suitable for shadow stacks or
  control-flow enforcement.
