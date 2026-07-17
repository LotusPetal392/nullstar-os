from pathlib import Path

required_markers = {
    "kernel/src/main.rs": [
        "pub(crate) use process::{elf, userspace};",
        'elf::validate("/init")',
        "userspace process exited:",
        "First ring-3 process exited",
    ],
    "kernel/src/shell.rs": [
        '"process" | "userspace" => self.print_userspace()',
        "fn print_userspace(&self)",
        "show the completed ring-3 process",
    ],
    "src/main.rs": [
        "USERSPACE_TEST_MARKER",
        "userspace process exited: path=/init, exit_code=42",
        "&& userspace_ready",
    ],
}

for path, markers in required_markers.items():
    source = Path(path).read_text()
    for marker in markers:
        if marker not in source:
            raise SystemExit(f"{path}: missing integrated marker {marker!r}")

print("ring-3 source integration is complete")
