from pathlib import Path

# Branch-only helper: patch the first implementation cut before running the
# repository's pinned formatter/tests/build, then let the workflow commit only
# the validated source files.
path = Path("userspace/src/application_launch.rs")
text = path.read_text()
text = text.replace(
    "    handle::{ApplicationProcess, Endpoint, Job, OwnedHandle},\n",
    "    handle::{Endpoint, Job, OwnedHandle},\n",
)
text = text.replace(
    "        CapabilityRole, ProcessContext, StartupCapabilityPolicy, StartupMessage, StartupReceiveError,\n",
    "        ApplicationProcess, CapabilityRole, ProcessContext, StartupCapabilityPolicy, StartupMessage, StartupReceiveError,\n",
)
path.write_text(text)
