from pathlib import Path
import base64
import zlib

payload = Path("tools/userspace_shell_payload.txt").read_text()
source = zlib.decompress(base64.b64decode(payload)).decode()
source = source.replace(
    "pipe <producer> | <consumer>  run a blocking userspace pipeline",
    "pipe <a> | <b>   run a userspace pipeline",
)
source = source.replace(
    "pipes            show kernel pipe statistics",
    "pipes            show pipe buffers, blocking, and wakeups",
)
exec(compile(source, "userspace-shell-integration", "exec"))
