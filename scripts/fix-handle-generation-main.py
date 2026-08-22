#!/usr/bin/env python3
from pathlib import Path

path = Path("userspace/src/main.rs")
text = path.read_text()

success_pattern = ")\n    .ok()\n        == Some(PROCESS_START_BOOTSTRAP_SLOT);"
success_replacement = ")\n    .is_ok();"
count = text.count(success_pattern)
if count != 1:
    raise RuntimeError(f"expected one managed-tool success comparison, found {count}")
text = text.replace(success_pattern, success_replacement, 1)

success_pattern = ")\n        .ok()\n            == Some(PROCESS_START_BOOTSTRAP_SLOT)"
success_replacement = ")\n        .is_ok()"
count = text.count(success_pattern)
if count != 1:
    raise RuntimeError(f"expected one definition-service success comparison, found {count}")
text = text.replace(success_pattern, success_replacement, 1)

failure_pattern = ")\n    .ok()\n        != Some(PROCESS_START_BOOTSTRAP_SLOT)"
failure_replacement = ")\n    .is_err()"
count = text.count(failure_pattern)
if count != 1:
    raise RuntimeError(f"expected one logging failure comparison, found {count}")
text = text.replace(failure_pattern, failure_replacement, 1)

failure_pattern = ")\n        .ok()\n            != Some(PROCESS_START_BOOTSTRAP_SLOT)"
failure_replacement = ")\n        .is_err()"
count = text.count(failure_pattern)
if count != 1:
    raise RuntimeError(f"expected one contained-service failure comparison, found {count}")
text = text.replace(failure_pattern, failure_replacement, 1)

if "Some(PROCESS_START_BOOTSTRAP_SLOT)" in text:
    raise RuntimeError("raw bootstrap slot comparison remains")

path.write_text(text)
print("PID 1 bootstrap grant comparisons fixed")
