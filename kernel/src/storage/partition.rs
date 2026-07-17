use alloc::{string::String, vec, vec::Vec};
use core::{char::decode_utf16, fmt};

use crate::ahci;

const MBR_SIGNATURE_OFFSET: usize = 510;
const MBR_PARTITION_TABLE_OFFSET: usize = 446;
const MBR_PARTITION_ENTRY_SIZE: usize = 16;
const MBR_PARTITION_COUNT: usize = 4;
const MAX_LOGICAL_PARTITIONS: usize = 128;
const GPT_SIGNATURE: &[u8; 8] = b"EFI PART";
const GPT_MIN_HEADER_SIZE: usize = 92;
const GPT_MIN_ENTRY_SIZE: usize = 128;
const MAX_GPT_ENTRY_ARRAY_BYTES: usize = 1024 * 1024;
const MAX_RECORDED_GPT_PARTITIONS: usize = 256;

const GPT_EFI_SYSTEM: Guid = Guid::from_raw([
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e,
    0xc9, 0x3b,
]);
const GPT_MICROSOFT_BASIC_DATA: Guid = Guid::from_raw([
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26,
    0x99, 0xc7,
]);
const GPT_BIOS_BOOT: Guid = Guid::from_raw([
    0x48, 0x61, 0x68, 0x21, 0x49, 0x64, 0x6f, 0x6e, 0x74, 0x4e, 0x65, 0x65, 0x64, 0x45,
    0x46, 0x49,
]);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    DiskUnavailable,
    Ahci(ahci::Error),
    BlockSizeTooSmall(usize),
    AddressOverflow,
    LbaOutOfRange,
    MissingMbrSignature,
    InvalidExtendedPartition,
    ExtendedPartitionLoop,
    InvalidGptSignature,
    InvalidGptHeaderSize(u32),
    InvalidGptHeaderCrc { expected: u32, actual: u32 },
    InvalidGptEntrySize(u32),
    GptEntryArrayTooLarge(u64),
    InvalidGptEntryArrayCrc { expected: u32, actual: u32 },
    NoPartitions,
}

impl Error {
    pub const fn description(self) -> &'static str {
        match self {
            Self::DiskUnavailable => "AHCI disk is unavailable",
            Self::Ahci(_) => "AHCI block read failed",
            Self::BlockSizeTooSmall(_) => "disk logical block size is smaller than 512 bytes",
            Self::AddressOverflow => "partition address calculation overflowed",
            Self::LbaOutOfRange => "partition metadata references an LBA outside the disk",
            Self::MissingMbrSignature => "disk does not contain an MBR signature or FAT boot sector",
            Self::InvalidExtendedPartition => "extended partition chain is malformed",
            Self::ExtendedPartitionLoop => "extended partition chain contains a loop",
            Self::InvalidGptSignature => "protective MBR is not followed by a GPT header",
            Self::InvalidGptHeaderSize(_) => "GPT header size is invalid",
            Self::InvalidGptHeaderCrc { .. } => "GPT header CRC32 is invalid",
            Self::InvalidGptEntrySize(_) => "GPT partition-entry size is invalid",
            Self::GptEntryArrayTooLarge(_) => "GPT partition-entry array exceeds the scan bound",
            Self::InvalidGptEntryArrayCrc { .. } => "GPT partition-entry-array CRC32 is invalid",
            Self::NoPartitions => "partition table does not contain any usable partitions",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ahci(error) => write!(formatter, "AHCI error: {error}"),
            Self::BlockSizeTooSmall(size) => {
                write!(formatter, "disk block size is {size} bytes; at least 512 are required")
            }
            Self::InvalidGptHeaderSize(size) => write!(formatter, "invalid GPT header size: {size}"),
            Self::InvalidGptHeaderCrc { expected, actual } => write!(
                formatter,
                "GPT header CRC mismatch: expected {expected:#010x}, calculated {actual:#010x}"
            ),
            Self::InvalidGptEntrySize(size) => {
                write!(formatter, "invalid GPT partition-entry size: {size}")
            }
            Self::GptEntryArrayTooLarge(bytes) => write!(
                formatter,
                "GPT partition-entry array is {bytes} bytes; the scan bound is {MAX_GPT_ENTRY_ARRAY_BYTES}"
            ),
            Self::InvalidGptEntryArrayCrc { expected, actual } => write!(
                formatter,
                "GPT entry-array CRC mismatch: expected {expected:#010x}, calculated {actual:#010x}"
            ),
            _ => formatter.write_str(self.description()),
        }
    }
}

impl From<ahci::Error> for Error {
    fn from(error: ahci::Error) -> Self {
        Self::Ahci(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableKind {
    SuperFloppy,
    Mbr,
    Gpt,
}

impl fmt::Display for TableKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SuperFloppy => formatter.write_str("superfloppy"),
            Self::Mbr => formatter.write_str("MBR"),
            Self::Gpt => formatter.write_str("GPT"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guid([u8; 16]);

impl Guid {
    pub const fn from_raw(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    fn from_slice(bytes: &[u8]) -> Option<Self> {
        let bytes: [u8; 16] = bytes.get(..16)?.try_into().ok()?;
        Some(Self(bytes))
    }

    fn is_zero(self) -> bool {
        self.0.iter().all(|byte| *byte == 0)
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let data1 = u32::from_le_bytes(self.0[0..4].try_into().unwrap());
        let data2 = u16::from_le_bytes(self.0[4..6].try_into().unwrap());
        let data3 = u16::from_le_bytes(self.0[6..8].try_into().unwrap());
        write!(
            formatter,
            "{data1:08x}-{data2:04x}-{data3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            self.0[8],
            self.0[9],
            self.0[10],
            self.0[11],
            self.0[12],
            self.0[13],
            self.0[14],
            self.0[15]
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKind {
    BootloaderStage,
    Fat12,
    Fat16,
    Fat32,
    EfiSystem,
    MicrosoftBasicData,
    BiosBoot,
    Extended,
    ProtectiveMbr,
    UnknownMbr(u8),
    UnknownGpt(Guid),
}

impl PartitionKind {
    pub const fn may_contain_filesystem(self) -> bool {
        !matches!(
            self,
            Self::BootloaderStage | Self::BiosBoot | Self::Extended | Self::ProtectiveMbr
        )
    }
}

impl fmt::Display for PartitionKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BootloaderStage => formatter.write_str("bootloader stage"),
            Self::Fat12 => formatter.write_str("FAT12"),
            Self::Fat16 => formatter.write_str("FAT16"),
            Self::Fat32 => formatter.write_str("FAT32"),
            Self::EfiSystem => formatter.write_str("EFI system"),
            Self::MicrosoftBasicData => formatter.write_str("Microsoft basic data"),
            Self::BiosBoot => formatter.write_str("BIOS boot"),
            Self::Extended => formatter.write_str("extended"),
            Self::ProtectiveMbr => formatter.write_str("protective MBR"),
            Self::UnknownMbr(value) => write!(formatter, "MBR type {value:#04x}"),
            Self::UnknownGpt(guid) => write!(formatter, "GPT type {guid}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub index: u32,
    pub start_lba: u64,
    pub block_count: u64,
    pub kind: PartitionKind,
    pub bootable: bool,
    pub name: String,
    pub type_guid: Option<Guid>,
    pub unique_guid: Option<Guid>,
    pub attributes: u64,
}

impl Partition {
    pub fn end_lba_inclusive(&self) -> u64 {
        self.start_lba
            .saturating_add(self.block_count.saturating_sub(1))
    }
}

#[derive(Debug, Clone)]
pub struct Inventory {
    pub table_kind: TableKind,
    pub disk_block_size: usize,
    pub disk_block_count: u64,
    pub protective_mbr: bool,
    pub header_crc_valid: bool,
    pub entry_array_crc_valid: bool,
    pub truncated: bool,
    partitions: Vec<Partition>,
}

impl Inventory {
    pub fn partitions(&self) -> &[Partition] {
        &self.partitions
    }

    pub fn filesystem_candidates(&self) -> impl Iterator<Item = &Partition> {
        self.partitions
            .iter()
            .filter(|partition| partition.kind.may_contain_filesystem())
    }
}

pub fn scan() -> Result<Inventory, Error> {
    let disk = ahci::info().ok_or(Error::DiskUnavailable)?;
    let block_size = usize::try_from(disk.logical_block_size).map_err(|_| Error::AddressOverflow)?;
    if block_size < 512 {
        return Err(Error::BlockSizeTooSmall(block_size));
    }

    let first_block = read_block(0, block_size, disk.logical_block_count)?;
    if first_block.get(MBR_SIGNATURE_OFFSET..MBR_SIGNATURE_OFFSET + 2) != Some(&[0x55, 0xaa]) {
        if looks_like_fat_boot_sector(&first_block) {
            return Ok(superfloppy_inventory(
                first_block,
                block_size,
                disk.logical_block_count,
            ));
        }
        return Err(Error::MissingMbrSignature);
    }

    let mut primary_entries = Vec::new();
    let mut protective_mbr = false;
    let mut extended_bases = Vec::new();

    for slot in 0..MBR_PARTITION_COUNT {
        let offset = MBR_PARTITION_TABLE_OFFSET + slot * MBR_PARTITION_ENTRY_SIZE;
        let entry = &first_block[offset..offset + MBR_PARTITION_ENTRY_SIZE];
        let partition_type = entry[4];
        let start_lba = u64::from(read_u32(entry, 8).ok_or(Error::AddressOverflow)?);
        let block_count = u64::from(read_u32(entry, 12).ok_or(Error::AddressOverflow)?);
        if partition_type == 0 || block_count == 0 {
            continue;
        }

        validate_range(start_lba, block_count, disk.logical_block_count)?;
        protective_mbr |= partition_type == 0xee;
        let kind = mbr_partition_kind(partition_type);
        let index = u32::try_from(slot + 1).unwrap();
        primary_entries.push(Partition {
            index,
            start_lba,
            block_count,
            kind,
            bootable: entry[0] == 0x80,
            name: mbr_partition_name(kind),
            type_guid: None,
            unique_guid: None,
            attributes: 0,
        });

        if matches!(kind, PartitionKind::Extended) {
            extended_bases.push(start_lba);
        }
    }

    if protective_mbr {
        return scan_gpt(block_size, disk.logical_block_count);
    }

    let mut partitions = primary_entries;
    let mut next_logical_index = 5_u32;
    for extended_base in extended_bases {
        scan_extended_partition(
            extended_base,
            block_size,
            disk.logical_block_count,
            &mut next_logical_index,
            &mut partitions,
        )?;
    }

    if partitions.is_empty() {
        if looks_like_fat_boot_sector(&first_block) {
            return Ok(superfloppy_inventory(
                first_block,
                block_size,
                disk.logical_block_count,
            ));
        }
        return Err(Error::NoPartitions);
    }

    Ok(Inventory {
        table_kind: TableKind::Mbr,
        disk_block_size: block_size,
        disk_block_count: disk.logical_block_count,
        protective_mbr: false,
        header_crc_valid: false,
        entry_array_crc_valid: false,
        truncated: false,
        partitions,
    })
}

fn scan_extended_partition(
    base_lba: u64,
    block_size: usize,
    disk_block_count: u64,
    next_index: &mut u32,
    partitions: &mut Vec<Partition>,
) -> Result<(), Error> {
    let mut relative_ebr_lba = 0_u64;
    let mut visited = Vec::new();

    for _ in 0..MAX_LOGICAL_PARTITIONS {
        let ebr_lba = base_lba
            .checked_add(relative_ebr_lba)
            .ok_or(Error::AddressOverflow)?;
        if visited.contains(&ebr_lba) {
            return Err(Error::ExtendedPartitionLoop);
        }
        visited.push(ebr_lba);

        let block = read_block(ebr_lba, block_size, disk_block_count)?;
        if block.get(MBR_SIGNATURE_OFFSET..MBR_SIGNATURE_OFFSET + 2) != Some(&[0x55, 0xaa]) {
            return Err(Error::InvalidExtendedPartition);
        }

        let logical = &block[MBR_PARTITION_TABLE_OFFSET
            ..MBR_PARTITION_TABLE_OFFSET + MBR_PARTITION_ENTRY_SIZE];
        let logical_type = logical[4];
        let logical_relative = u64::from(read_u32(logical, 8).ok_or(Error::AddressOverflow)?);
        let logical_blocks = u64::from(read_u32(logical, 12).ok_or(Error::AddressOverflow)?);
        if logical_type != 0 && logical_blocks != 0 {
            let start_lba = ebr_lba
                .checked_add(logical_relative)
                .ok_or(Error::AddressOverflow)?;
            validate_range(start_lba, logical_blocks, disk_block_count)?;
            let kind = mbr_partition_kind(logical_type);
            partitions.push(Partition {
                index: *next_index,
                start_lba,
                block_count: logical_blocks,
                kind,
                bootable: logical[0] == 0x80,
                name: mbr_partition_name(kind),
                type_guid: None,
                unique_guid: None,
                attributes: 0,
            });
            *next_index = next_index.saturating_add(1);
        }

        let link_offset = MBR_PARTITION_TABLE_OFFSET + MBR_PARTITION_ENTRY_SIZE;
        let link = &block[link_offset..link_offset + MBR_PARTITION_ENTRY_SIZE];
        let link_type = link[4];
        let next_relative = u64::from(read_u32(link, 8).ok_or(Error::AddressOverflow)?);
        let link_blocks = u64::from(read_u32(link, 12).ok_or(Error::AddressOverflow)?);
        if link_type == 0 || link_blocks == 0 {
            return Ok(());
        }
        if !matches!(mbr_partition_kind(link_type), PartitionKind::Extended) {
            return Err(Error::InvalidExtendedPartition);
        }
        relative_ebr_lba = next_relative;
    }

    Err(Error::ExtendedPartitionLoop)
}

fn scan_gpt(block_size: usize, disk_block_count: u64) -> Result<Inventory, Error> {
    let header_block = read_block(1, block_size, disk_block_count)?;
    if header_block.get(..8) != Some(GPT_SIGNATURE) {
        return Err(Error::InvalidGptSignature);
    }

    let header_size = read_u32(&header_block, 12).ok_or(Error::AddressOverflow)?;
    let header_size_usize = usize::try_from(header_size).map_err(|_| Error::AddressOverflow)?;
    if !(GPT_MIN_HEADER_SIZE..=block_size).contains(&header_size_usize) {
        return Err(Error::InvalidGptHeaderSize(header_size));
    }

    let expected_header_crc = read_u32(&header_block, 16).ok_or(Error::AddressOverflow)?;
    let mut header_bytes = header_block[..header_size_usize].to_vec();
    header_bytes[16..20].fill(0);
    let actual_header_crc = crc32(&header_bytes);
    if actual_header_crc != expected_header_crc {
        return Err(Error::InvalidGptHeaderCrc {
            expected: expected_header_crc,
            actual: actual_header_crc,
        });
    }

    let current_lba = read_u64(&header_block, 24).ok_or(Error::AddressOverflow)?;
    let backup_lba = read_u64(&header_block, 32).ok_or(Error::AddressOverflow)?;
    let first_usable_lba = read_u64(&header_block, 40).ok_or(Error::AddressOverflow)?;
    let last_usable_lba = read_u64(&header_block, 48).ok_or(Error::AddressOverflow)?;
    if current_lba >= disk_block_count
        || backup_lba >= disk_block_count
        || first_usable_lba > last_usable_lba
        || last_usable_lba >= disk_block_count
    {
        return Err(Error::LbaOutOfRange);
    }

    let entries_lba = read_u64(&header_block, 72).ok_or(Error::AddressOverflow)?;
    let entry_count = read_u32(&header_block, 80).ok_or(Error::AddressOverflow)?;
    let entry_size = read_u32(&header_block, 84).ok_or(Error::AddressOverflow)?;
    let expected_entries_crc = read_u32(&header_block, 88).ok_or(Error::AddressOverflow)?;
    if entry_size < GPT_MIN_ENTRY_SIZE as u32 || entry_size % 8 != 0 {
        return Err(Error::InvalidGptEntrySize(entry_size));
    }

    let entry_array_bytes = u64::from(entry_count)
        .checked_mul(u64::from(entry_size))
        .ok_or(Error::AddressOverflow)?;
    if entry_array_bytes > MAX_GPT_ENTRY_ARRAY_BYTES as u64 {
        return Err(Error::GptEntryArrayTooLarge(entry_array_bytes));
    }
    let entry_array_len = usize::try_from(entry_array_bytes).map_err(|_| Error::AddressOverflow)?;
    let entry_bytes = read_bytes(
        entries_lba,
        entry_array_len,
        block_size,
        disk_block_count,
    )?;
    let actual_entries_crc = crc32(&entry_bytes);
    if actual_entries_crc != expected_entries_crc {
        return Err(Error::InvalidGptEntryArrayCrc {
            expected: expected_entries_crc,
            actual: actual_entries_crc,
        });
    }

    let entry_size_usize = usize::try_from(entry_size).map_err(|_| Error::AddressOverflow)?;
    let mut partitions = Vec::new();
    let mut truncated = false;
    for index in 0..usize::try_from(entry_count).map_err(|_| Error::AddressOverflow)? {
        let offset = index
            .checked_mul(entry_size_usize)
            .ok_or(Error::AddressOverflow)?;
        let entry = entry_bytes
            .get(offset..offset + entry_size_usize)
            .ok_or(Error::AddressOverflow)?;
        let type_guid = Guid::from_slice(entry).ok_or(Error::AddressOverflow)?;
        if type_guid.is_zero() {
            continue;
        }

        let unique_guid = Guid::from_slice(&entry[16..]).ok_or(Error::AddressOverflow)?;
        let start_lba = read_u64(entry, 32).ok_or(Error::AddressOverflow)?;
        let end_lba = read_u64(entry, 40).ok_or(Error::AddressOverflow)?;
        let attributes = read_u64(entry, 48).ok_or(Error::AddressOverflow)?;
        if start_lba > end_lba || end_lba >= disk_block_count {
            return Err(Error::LbaOutOfRange);
        }
        let block_count = end_lba
            .checked_sub(start_lba)
            .and_then(|value| value.checked_add(1))
            .ok_or(Error::AddressOverflow)?;

        if partitions.len() >= MAX_RECORDED_GPT_PARTITIONS {
            truncated = true;
            continue;
        }

        let kind = gpt_partition_kind(type_guid);
        let name_end = entry_size_usize.min(128);
        let name = if name_end > 56 {
            decode_utf16_name(&entry[56..name_end])
        } else {
            String::new()
        };
        partitions.push(Partition {
            index: u32::try_from(index + 1).unwrap_or(u32::MAX),
            start_lba,
            block_count,
            kind,
            bootable: attributes & (1 << 2) != 0,
            name,
            type_guid: Some(type_guid),
            unique_guid: Some(unique_guid),
            attributes,
        });
    }

    if partitions.is_empty() {
        return Err(Error::NoPartitions);
    }

    Ok(Inventory {
        table_kind: TableKind::Gpt,
        disk_block_size: block_size,
        disk_block_count,
        protective_mbr: true,
        header_crc_valid: true,
        entry_array_crc_valid: true,
        truncated,
        partitions,
    })
}

fn superfloppy_inventory(
    boot_sector: Vec<u8>,
    block_size: usize,
    disk_block_count: u64,
) -> Inventory {
    let kind = fat_kind_from_boot_sector(&boot_sector).unwrap_or(PartitionKind::UnknownMbr(0));
    Inventory {
        table_kind: TableKind::SuperFloppy,
        disk_block_size: block_size,
        disk_block_count,
        protective_mbr: false,
        header_crc_valid: false,
        entry_array_crc_valid: false,
        truncated: false,
        partitions: vec![Partition {
            index: 0,
            start_lba: 0,
            block_count: disk_block_count,
            kind,
            bootable: true,
            name: String::from("whole disk"),
            type_guid: None,
            unique_guid: None,
            attributes: 0,
        }],
    }
}

fn mbr_partition_kind(value: u8) -> PartitionKind {
    match value {
        0x01 => PartitionKind::Fat12,
        0x04 | 0x06 | 0x0e => PartitionKind::Fat16,
        0x0b | 0x0c => PartitionKind::Fat32,
        0x05 | 0x0f | 0x85 => PartitionKind::Extended,
        0x07 => PartitionKind::MicrosoftBasicData,
        0x20 => PartitionKind::BootloaderStage,
        0xee => PartitionKind::ProtectiveMbr,
        0xef => PartitionKind::EfiSystem,
        other => PartitionKind::UnknownMbr(other),
    }
}

fn mbr_partition_name(kind: PartitionKind) -> String {
    match kind {
        PartitionKind::BootloaderStage => String::from("bootloader second stage"),
        PartitionKind::Fat12 | PartitionKind::Fat16 | PartitionKind::Fat32 => {
            String::from("FAT volume")
        }
        PartitionKind::EfiSystem => String::from("EFI system partition"),
        PartitionKind::MicrosoftBasicData => String::from("basic data partition"),
        PartitionKind::Extended => String::from("extended partition"),
        PartitionKind::ProtectiveMbr => String::from("protective MBR entry"),
        _ => String::new(),
    }
}

fn gpt_partition_kind(guid: Guid) -> PartitionKind {
    if guid == GPT_EFI_SYSTEM {
        PartitionKind::EfiSystem
    } else if guid == GPT_MICROSOFT_BASIC_DATA {
        PartitionKind::MicrosoftBasicData
    } else if guid == GPT_BIOS_BOOT {
        PartitionKind::BiosBoot
    } else {
        PartitionKind::UnknownGpt(guid)
    }
}

fn validate_range(start_lba: u64, block_count: u64, disk_block_count: u64) -> Result<(), Error> {
    let end = start_lba
        .checked_add(block_count)
        .ok_or(Error::AddressOverflow)?;
    if start_lba >= disk_block_count || end > disk_block_count {
        Err(Error::LbaOutOfRange)
    } else {
        Ok(())
    }
}

fn read_block(lba: u64, block_size: usize, disk_block_count: u64) -> Result<Vec<u8>, Error> {
    if lba >= disk_block_count {
        return Err(Error::LbaOutOfRange);
    }
    let mut block = vec![0_u8; block_size];
    ahci::read_block(lba, &mut block)?;
    Ok(block)
}

fn read_bytes(
    start_lba: u64,
    byte_len: usize,
    block_size: usize,
    disk_block_count: u64,
) -> Result<Vec<u8>, Error> {
    if byte_len == 0 {
        return Ok(Vec::new());
    }
    let block_count = byte_len
        .checked_add(block_size - 1)
        .ok_or(Error::AddressOverflow)?
        / block_size;
    let end_lba = start_lba
        .checked_add(u64::try_from(block_count).map_err(|_| Error::AddressOverflow)?)
        .ok_or(Error::AddressOverflow)?;
    if start_lba >= disk_block_count || end_lba > disk_block_count {
        return Err(Error::LbaOutOfRange);
    }

    let allocation_len = block_count
        .checked_mul(block_size)
        .ok_or(Error::AddressOverflow)?;
    let mut bytes = vec![0_u8; allocation_len];
    for block_index in 0..block_count {
        let offset = block_index
            .checked_mul(block_size)
            .ok_or(Error::AddressOverflow)?;
        let lba = start_lba
            .checked_add(u64::try_from(block_index).map_err(|_| Error::AddressOverflow)?)
            .ok_or(Error::AddressOverflow)?;
        ahci::read_block(lba, &mut bytes[offset..offset + block_size])?;
    }
    bytes.truncate(byte_len);
    Ok(bytes)
}

fn looks_like_fat_boot_sector(block: &[u8]) -> bool {
    if block.len() < 512 || block[510..512] != [0x55, 0xaa] {
        return false;
    }
    let bytes_per_sector = read_u16(block, 11).unwrap_or(0);
    let sectors_per_cluster = block[13];
    let reserved_sectors = read_u16(block, 14).unwrap_or(0);
    let fat_count = block[16];
    let total_sectors = u32::from(read_u16(block, 19).unwrap_or(0))
        .max(read_u32(block, 32).unwrap_or(0));
    let sectors_per_fat = u32::from(read_u16(block, 22).unwrap_or(0))
        .max(read_u32(block, 36).unwrap_or(0));

    matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096)
        && sectors_per_cluster != 0
        && sectors_per_cluster.is_power_of_two()
        && reserved_sectors != 0
        && (1..=2).contains(&fat_count)
        && total_sectors != 0
        && sectors_per_fat != 0
}

fn fat_kind_from_boot_sector(block: &[u8]) -> Option<PartitionKind> {
    if !looks_like_fat_boot_sector(block) {
        return None;
    }
    let bytes_per_sector = u32::from(read_u16(block, 11)?);
    let sectors_per_cluster = u32::from(*block.get(13)?);
    let reserved_sectors = u32::from(read_u16(block, 14)?);
    let fat_count = u32::from(*block.get(16)?);
    let root_entry_count = u32::from(read_u16(block, 17)?);
    let total_sectors = u32::from(read_u16(block, 19)?).max(read_u32(block, 32)?);
    let sectors_per_fat = u32::from(read_u16(block, 22)?).max(read_u32(block, 36)?);
    let root_dir_sectors = root_entry_count
        .checked_mul(32)?
        .checked_add(bytes_per_sector - 1)?
        / bytes_per_sector;
    let first_data_sector = reserved_sectors
        .checked_add(fat_count.checked_mul(sectors_per_fat)?)?
        .checked_add(root_dir_sectors)?;
    let data_sectors = total_sectors.checked_sub(first_data_sector)?;
    let cluster_count = data_sectors / sectors_per_cluster;
    Some(if cluster_count < 4_085 {
        PartitionKind::Fat12
    } else if cluster_count < 65_525 {
        PartitionKind::Fat16
    } else {
        PartitionKind::Fat32
    })
}

fn decode_utf16_name(bytes: &[u8]) -> String {
    let mut words = Vec::new();
    for pair in bytes.chunks_exact(2) {
        let value = u16::from_le_bytes([pair[0], pair[1]]);
        if value == 0 || value == 0xffff {
            break;
        }
        words.push(value);
    }

    let mut value = String::new();
    for character in decode_utf16(words) {
        value.push(character.unwrap_or('?'));
    }
    value
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let bytes: [u8; 8] = bytes.get(offset..offset.checked_add(8)?)?.try_into().ok()?;
    Some(u64::from_le_bytes(bytes))
}
