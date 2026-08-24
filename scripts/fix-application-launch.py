from pathlib import Path

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
