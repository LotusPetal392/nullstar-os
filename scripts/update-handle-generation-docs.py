#!/usr/bin/env python3
from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    count = text.count(old)
    if count == 0:
        # Idempotent reruns are allowed after the generated documentation commit.
        if new in text:
            return text
        raise RuntimeError(f"{label}: source text not found")
    if count != 1:
        raise RuntimeError(f"{label}: expected one source match, found {count}")
    return text.replace(old, new, 1)


path = Path("docs/protection-model.md")
text = path.read_text()
text = replace_once(
    text,
    "ABI 1.30 adds bounded manual-reset events with independently delegable signal and wait authority.\n",
    "ABI 1.30 adds bounded manual-reset events with independently delegable signal and wait authority.\n"
    "ABI 1.31 makes live capability values opaque generation-checked handles. Each newly installed\n"
    "handle receives a fresh nonzero generation combined with its bounded process-local table slot;\n"
    "generation exhaustion fails closed rather than wrapping. A caller may resolve the current opaque\n"
    "handle installed at one of its own slots for managed bootstrap and bounded cleanup, but the slot\n"
    "number itself is not authority and the handle bit layout is not a userspace contract.\n",
    "protection ABI 1.31 status",
)
text = replace_once(
    text,
    "The capability namespace is separate from the file-descriptor namespace. Both use small\n"
    "integers today, but handles are valid only for capability operations and descriptors are\n"
    "valid only for descriptor and filesystem I/O.\n",
    "The capability namespace is separate from the file-descriptor namespace. Capability handles\n"
    "are opaque generation-checked `u64` values; process-local table slots are bounded discovery\n"
    "coordinates rather than authority. File descriptors remain small integers and are valid only\n"
    "for descriptor and filesystem I/O.\n",
    "protection handle namespace",
)
text = replace_once(
    text,
    "child. The source must carry `TRANSFER`, and the child receives only a requested subset\n"
    "of rights. A deterministic child slot can be requested so parent and child agree on the\n"
    "initial handle across `fork` and `exec`.\n",
    "child. The source must carry `TRANSFER`, and the child receives only a requested subset\n"
    "of rights. A deterministic child slot can be requested for managed bootstrap. The grant\n"
    "returns the child's actual opaque generation-checked handle; the child resolves that handle\n"
    "from the agreed slot rather than treating the slot number as authority.\n",
    "protection direct-child bootstrap",
)
path.write_text(text)

path = Path("docs/syscall-abi.md")
text = path.read_text()
text = replace_once(
    text,
    "The ABI is experimental, but callers can query the current version, 1.30, and a\n",
    "The ABI is experimental, but callers can query the current version, 1.31, and a\n",
    "ABI current version",
)
text = replace_once(
    text,
    "| 48 | `capability_grant_child` | child PID, source handle, reduced rights, requested child handle | child handle |\n",
    "| 48 | `capability_grant_child` | child PID, source handle, reduced rights, requested child slot | child opaque handle |\n",
    "grant-child table row",
)
text = replace_once(
    text,
    "Capability handles occupy a namespace separate from file descriptors. Handles\n"
    "are process local, begin at one, and refer to an object plus an explicit rights\n"
    "mask. Duplication and delegation require the corresponding authority and accept\n"
    "only a nonempty subset of the source rights.\n",
    "Capability handles occupy a namespace separate from file descriptors. Handles\n"
    "are process-local opaque `u64` values and refer to an object plus an explicit\n"
    "rights mask. ABI 1.31 generation-checks those values so later reuse of the same\n"
    "bounded table slot cannot revive a stale handle. Duplication and delegation require\n"
    "the corresponding authority and accept only a nonempty subset of the source rights.\n",
    "capability handle description",
)
text = replace_once(
    text,
    "rights must be a subset of the source rights. A requested child handle of zero\n"
    "allocates the lowest free slot; a nonzero value requests that exact slot. This\n"
    "allows recently forked processes to agree on a bootstrap endpoint without a\n"
    "global service namespace. Capability tables are not implicitly cloned by\n"
    "`fork`, but they remain attached to a process across `exec`.\n",
    "rights must be a subset of the source rights. A requested child slot of zero\n"
    "allocates the lowest free slot; a nonzero value requests that exact slot. The\n"
    "return value is the child's opaque generation-checked handle, not the slot number.\n"
    "This lets recently forked processes agree on a bootstrap slot without a global\n"
    "service namespace. Capability tables are not implicitly cloned by `fork`, but\n"
    "they remain attached to a process across `exec`.\n",
    "direct-child ABI description",
)
marker = "See [Capability and IPC protection model](protection-model.md) for lifetime,\nsecurity-boundary, testing, and migration details.\n\n"
section = """## Version 1.31 generation-checked capability handles

| Number | Name | Arguments | Result |
| ---: | --- | --- | --- |
| 91 | `capability_handle_at_slot` | caller-local capability slot | current opaque handle |

ABI 1.31 advertises `capability::GENERATION_CHECKED_HANDLES`. Every newly installed
live capability receives a nonzero generation. Closing or moving a handle makes that
exact opaque value stale; later reuse of its table slot receives a different generation.
Generation exhaustion fails with `ENOSPC` rather than wrapping to an earlier value.

`capability_handle_at_slot` is intentionally process-local and bounded. It resolves the
opaque handle currently installed at one of the caller's own slots and returns `ENOENT`
when that slot is empty. The slot is a discovery coordinate used by managed bootstrap
and cleanup code; knowing a slot does not grant authority, and the packed handle layout
is not part of the public ABI contract.

"""
if section not in text:
    if marker not in text:
        raise RuntimeError("1.31 insertion marker not found")
    text = text.replace(marker, marker + section, 1)
path.write_text(text)

path = Path("docs/design/ipc-and-object-model.md")
text = path.read_text()
text = replace_once(
    text,
    "Handle values are opaque and meaningful only in the owning process. A generation plus\n"
    "slot index is the preferred implementation so a stale value cannot accidentally refer\n"
    "to a newly allocated object after table-slot reuse. The exact width and bit layout are\n"
    "not a public promise until the ABI is specified.\n",
    "Handle values are opaque and meaningful only in the owning process. ABI 1.31 implements\n"
    "generation-checked handle values so a stale value cannot accidentally refer to newly\n"
    "allocated authority after table-slot reuse. A bounded slot may be used as a local\n"
    "discovery coordinate, but the generation/slot encoding and exact bit layout remain\n"
    "private implementation details rather than a public ABI promise.\n",
    "IPC handle model status",
)
path.write_text(text)

print("handle-generation documentation updated")
