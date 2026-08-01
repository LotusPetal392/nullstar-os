// Stable identity and development layout for NullStar's primary NullFS volume.
//
// The UUID is authoritative for stable selection and capability delegation on
// trusted boot media; it is not a cryptographic media identity. The label and
// mount name are human-facing and may change without changing volume identity.

pub const FILESYSTEM_UUID: [u8; 16] = [
    0x4e, 0x75, 0x6c, 0x6c, 0x53, 0x74, 0x61, 0x72, 0x2d, 0x4e, 0x75, 0x6c, 0x6c, 0x46, 0x53, 0x02,
];
pub const CAPACITY_BLOCKS: u64 = 1024;
pub const DISPLAY_NAME: &str = "NullStar";
pub const MOUNT_PATH: &str = "/Volumes/NullStar";
