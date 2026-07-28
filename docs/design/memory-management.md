# Memory-management direction

## Status

Typed memory objects, first-class address spaces, mapped shared memory, conservative
commit accounting, and W^X are **accepted direction**. The allocator algorithms and
pager implementation are **tentative design**.

## Layering

Memory management should be divided into four layers:

1. physical page-frame allocation;
2. kernel object and heap allocation;
3. address spaces, mappings, and page tables;
4. userspace allocators and runtime policy.

The kernel remains responsible for page ownership, mapping enforcement, fault
resolution, and accounting even when backing data is supplied by userspace services.

## Physical memory

The base allocation unit remains a 4 KiB page. A buddy allocator is the preferred
long-term physical allocator because it supports fast allocation, coalescing, and
contiguous runs for page tables and DMA. The design should reserve room for DMA-low,
DMA32, normal, and future node-local zones without requiring those zones immediately.

Physical pages need inspectable ownership and purpose, including free, kernel, user,
page-table, slab, shared, file-cache, device, pinned, and reserved classifications.
Per-CPU order-zero caches may be added after SMP.

## Kernel allocation

The existing coalescing heap remains suitable during early development. The longer
term kernel should add slab or object caches for common fixed-size objects such as
threads, capabilities, endpoints, messages, timers, and VM metadata.

Interrupt and realtime paths must not rely on unbounded heap allocation. They should
use preallocated pools or bounded object caches.

## Address spaces and mappings

An address space should be a first-class kernel object independent from a process.
Each address space owns an authoritative collection of virtual-memory areas. Page
tables implement those mappings in hardware but are not the sole high-level mapping
database.

A mapping records:

- virtual range;
- backing memory object and offset;
- read, write, and execute protection;
- private or shared behavior;
- commitment and pinning policy;
- guard and growth constraints where applicable.

The virtual layout must keep the null page unmapped, reserve room for thread stacks,
shared libraries, mappings, per-CPU regions, the kernel direct map, MMIO, and future
randomization. Exact addresses should not become a stable ABI unnecessarily.

## Memory objects

All userspace mappings should be backed by typed memory objects. The initial set is:

- anonymous zero-filled memory;
- mapped shared memory;
- executable-image memory;
- device or MMIO memory;
- copy-on-write backing.

Later additions include file-backed objects, pager-backed objects, DMA objects, and
swap-backed anonymous pages.

A memory object defines logical size, page lookup, sharing, commitment, dirty state,
resize behavior, and lifetime. This common abstraction should support process heaps,
`mmap`, graphics buffers, media ring buffers, executable mappings, and future
userspace drivers.

## Anonymous memory and copy-on-write

Anonymous mappings should reserve address space and allocate physical pages lazily.
Untouched reads may use a shared read-only zero page; the first write allocates a
private page.

`fork` copy-on-write should be expressed through shared backing and page references.
A write fault allocates and copies only the faulting page. If shadow objects are used,
deep chains must eventually be flattened or collapsed.

Thread stacks should be bounded, guarded, and committed lazily. Kernel stacks should
be fixed-size, measured, and protected by guard pages.

## Shared memory

Shared byte-copy objects should evolve into mapped-page objects. A shared-memory
handle may be transferred with reduced rights, and every mapping may apply stricter
protections than the object permits.

Planned features include sealing against resize, write, execution, or further handle
distribution. Shared memory is the required bulk-data path for IPC, graphics, audio,
network rings, and large filesystem transfers.

## File-backed memory and pagers

The preferred long-term model is hybrid:

- the kernel owns physical residency, mappings, protection, and accounting;
- a filesystem or pager service supplies page contents and accepts writeback;
- faults involving external backing block the faulting thread through normal
  scheduler mechanisms rather than performing service logic in exception context.

Service failure must have explicit semantics. Resident pages may remain usable where
safe; unresolved faults fail with a precise backing-store error rather than waiting
indefinitely. File reads and mapped pages should eventually share a unified cache.

## Protection and executable memory

NullStar should enforce write XOR execute by default. Ordinary applications may not
map writable executable memory. A future JIT permission should authorize an explicit
write-then-seal-and-execute transition rather than permanent RWX pages.

MMIO mappings require explicit device capabilities and correct cache attributes.
Userspace drivers should receive DMA allocator and IOMMU-domain capabilities instead
of arbitrary physical-memory access.

## Accounting, commit, and pressure

Memory should be attributable to a process, service, or job. Accounting should cover
reserved virtual space, committed pages, shared-memory creation, page tables, pinned
memory, and kernel resources.

NullStar should begin with conservative overcommit. Writable commitments must have a
credible backing guarantee; failure should occur according to documented mapping or
fault semantics rather than through unpredictable global exhaustion.

The pressure sequence should prefer reclaiming clean caches, shrinking reclaimable
kernel structures, notifying services, and enforcing job limits before terminating a
process. Critical services may hold reserved budgets. A userspace policy manager may
choose a victim, but the kernel supplies measurements and enforcement.

Memory-pressure notifications should expose normal, moderate, critical, and emergency
levels so applications can discard regenerable state before an OOM condition.

## Deferred features

Disk swap, compressed memory, huge-page promotion, NUMA placement, deduplication, and
hibernation are deferred until anonymous objects, accounting, reclaim, and failure
semantics are reliable. Pinned, realtime, device, and kernel-critical pages must never
become ordinary swap candidates.

## Open questions

- Buddy metadata representation and maximum supported physical memory.
- Interval tree versus a simpler bounded mapping structure during early development.
- Kernel-coordinated page cache details and pager restart behavior.
- Exact shared-memory sealing and resize semantics.
- Job-level charging rules for pages shared by unrelated applications.
