use alloc::{string::String, vec, vec::Vec};
use core::{char::decode_utf16, fmt};

use spin::Mutex;

use crate::ahci;

use super::partition::{Inventory as PartitionInventory, Partition};

const DIRECTORY_ENTRY_SIZE: usize = 32;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LONG_NAME: u8 = 0x0f;
const MAX_DIRECTORY_ENTRIES: usize = 16_384;
const MAX_PATH_COMPONENTS: usize = 32;
const MAX_LONG_NAME_SLOTS: usize = 20;
const MAX_FILE_READ_BYTES: usize = 1024 * 1024;

static VOLUME: Mutex<Option<FatVolume>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    AlreadyInitialized,
    NotInitialized,
    NoSupportedPartition,
    Ahci(ahci::Error),
    AddressOverflow,
    LbaOutOfRange,
    BlockSizeMismatch { disk: usize, fat: usize },
    InvalidBootSector(&'static str),
    InvalidCluster(u32),
    FreeCluster(u32),
    ReservedCluster(u32),
    BadCluster(u32),
    FatChainLoop,
    DirectoryTooLarge,
    CorruptDirectory,
    InvalidPath,
    DirectoryNotFound,
    FileNotFound,
    NotDirectory,
    IsDirectory,
    ReadLimitTooLarge(usize),
}

impl Error {
    pub const fn description(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "FAT filesystem is already initialized",
            Self::NotInitialized => "FAT filesystem is not initialized",
            Self::NoSupportedPartition => "no readable FAT volume was found",
            Self::Ahci(_) => "AHCI block read failed",
            Self::AddressOverflow => "FAT address calculation overflowed",
            Self::LbaOutOfRange => "FAT metadata references an LBA outside the partition",
            Self::BlockSizeMismatch { .. } => {
                "FAT sector size does not match the disk logical-block size"
            }
            Self::InvalidBootSector(_) => "FAT boot sector is invalid",
            Self::InvalidCluster(_) => "FAT cluster number is invalid",
            Self::FreeCluster(_) => "FAT chain unexpectedly references a free cluster",
            Self::ReservedCluster(_) => "FAT chain references a reserved cluster",
            Self::BadCluster(_) => "FAT chain references a bad cluster",
            Self::FatChainLoop => "FAT cluster chain is cyclic or exceeds its volume bound",
            Self::DirectoryTooLarge => "FAT directory exceeds the configured entry bound",
            Self::CorruptDirectory => "FAT directory entry is malformed",
            Self::InvalidPath => "filesystem path is invalid",
            Self::DirectoryNotFound => "directory was not found",
            Self::FileNotFound => "file was not found",
            Self::NotDirectory => "path component is not a directory",
            Self::IsDirectory => "path identifies a directory rather than a file",
            Self::ReadLimitTooLarge(_) => "requested file read exceeds the configured bound",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ahci(error) => write!(formatter, "AHCI error: {error}"),
            Self::BlockSizeMismatch { disk, fat } => write!(
                formatter,
                "disk blocks are {disk} bytes but the FAT volume uses {fat}-byte sectors"
            ),
            Self::InvalidBootSector(reason) => {
                write!(formatter, "invalid FAT boot sector: {reason}")
            }
            Self::InvalidCluster(cluster) => write!(formatter, "invalid FAT cluster {cluster}"),
            Self::FreeCluster(cluster) => write!(formatter, "cluster {cluster} is marked free"),
            Self::ReservedCluster(cluster) => {
                write!(formatter, "cluster {cluster} has a reserved FAT value")
            }
            Self::BadCluster(cluster) => write!(formatter, "cluster {cluster} is marked bad"),
            Self::ReadLimitTooLarge(limit) => write!(
                formatter,
                "requested file-read limit {limit} exceeds {MAX_FILE_READ_BYTES} bytes"
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
pub enum FatType {
    Fat12,
    Fat16,
    Fat32,
}

impl fmt::Display for FatType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fat12 => formatter.write_str("FAT12"),
            Self::Fat16 => formatter.write_str("FAT16"),
            Self::Fat32 => formatter.write_str("FAT32"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct VolumeInfo {
    pub partition_index: u32,
    pub partition_start_lba: u64,
    pub partition_block_count: u64,
    pub fat_type: FatType,
    pub volume_label: String,
    pub volume_id: u32,
    pub bytes_per_sector: usize,
    pub sectors_per_cluster: u32,
    pub bytes_per_cluster: usize,
    pub reserved_sectors: u32,
    pub fat_count: u8,
    pub sectors_per_fat: u32,
    pub total_sectors: u64,
    pub cluster_count: u32,
    pub root_entry_count: usize,
}

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub name: String,
    pub short_name: String,
    pub attributes: u8,
    pub first_cluster: u32,
    pub size: u32,
}

impl DirectoryEntry {
    pub const fn is_directory(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    pub const fn is_hidden(&self) -> bool {
        self.attributes & 0x02 != 0
    }

    pub const fn is_system(&self) -> bool {
        self.attributes & 0x04 != 0
    }

    pub const fn is_read_only(&self) -> bool {
        self.attributes & 0x01 != 0
    }
}

#[derive(Debug, Clone)]
pub struct FileData {
    pub bytes: Vec<u8>,
    pub total_size: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy)]
enum DirectoryLocation {
    FixedRoot,
    Cluster(u32),
}

#[derive(Debug)]
struct FatVolume {
    partition: Partition,
    info: VolumeInfo,
    first_fat_sector: u64,
    root_dir_first_sector: u64,
    root_dir_sector_count: u64,
    first_data_sector: u64,
    root_cluster: u32,
}

impl FatVolume {
    fn mount(partition: &Partition) -> Result<Self, Error> {
        let disk = ahci::info().ok_or(Error::NotInitialized)?;
        let disk_block_size =
            usize::try_from(disk.logical_block_size).map_err(|_| Error::AddressOverflow)?;
        let boot_sector = read_disk_block(partition.start_lba, disk_block_size)?;
        if boot_sector.len() < 512 || boot_sector.get(510..512) != Some(&[0x55_u8, 0xaa][..]) {
            return Err(Error::InvalidBootSector("missing 0x55aa signature"));
        }

        let bytes_per_sector = usize::from(
            read_u16(&boot_sector, 11)
                .ok_or(Error::InvalidBootSector("missing bytes-per-sector field"))?,
        );
        if !(512..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() {
            return Err(Error::InvalidBootSector(
                "unsupported bytes-per-sector value",
            ));
        }
        if bytes_per_sector != disk_block_size {
            return Err(Error::BlockSizeMismatch {
                disk: disk_block_size,
                fat: bytes_per_sector,
            });
        }

        let sectors_per_cluster = u32::from(boot_sector[13]);
        if sectors_per_cluster == 0
            || !sectors_per_cluster.is_power_of_two()
            || sectors_per_cluster > 128
        {
            return Err(Error::InvalidBootSector(
                "invalid sectors-per-cluster value",
            ));
        }

        let reserved_sectors = u32::from(
            read_u16(&boot_sector, 14)
                .ok_or(Error::InvalidBootSector("missing reserved-sector count"))?,
        );
        if reserved_sectors == 0 {
            return Err(Error::InvalidBootSector("reserved-sector count is zero"));
        }

        let fat_count = boot_sector[16];
        if !(1..=2).contains(&fat_count) {
            return Err(Error::InvalidBootSector("unsupported FAT copy count"));
        }

        let root_directory_entries = u32::from(
            read_u16(&boot_sector, 17)
                .ok_or(Error::InvalidBootSector("missing root-directory size"))?,
        );
        let total_sectors_16 = u64::from(
            read_u16(&boot_sector, 19)
                .ok_or(Error::InvalidBootSector("missing total-sector field"))?,
        );
        let total_sectors_32 = u64::from(read_u32(&boot_sector, 32).ok_or(
            Error::InvalidBootSector("missing extended total-sector field"),
        )?);
        let total_sectors = if total_sectors_16 != 0 {
            total_sectors_16
        } else {
            total_sectors_32
        };
        if total_sectors == 0 || total_sectors > partition.block_count {
            return Err(Error::InvalidBootSector(
                "total-sector count is zero or exceeds the partition",
            ));
        }

        let sectors_per_fat_16 = u32::from(
            read_u16(&boot_sector, 22).ok_or(Error::InvalidBootSector("missing FAT-size field"))?,
        );
        let sectors_per_fat_32 = read_u32(&boot_sector, 36)
            .ok_or(Error::InvalidBootSector("missing FAT32 size field"))?;
        let sectors_per_fat = if sectors_per_fat_16 != 0 {
            sectors_per_fat_16
        } else {
            sectors_per_fat_32
        };
        if sectors_per_fat == 0 {
            return Err(Error::InvalidBootSector("FAT size is zero"));
        }

        let root_dir_bytes = u64::from(root_directory_entries)
            .checked_mul(DIRECTORY_ENTRY_SIZE as u64)
            .ok_or(Error::AddressOverflow)?;
        let root_dir_sector_count = root_dir_bytes
            .checked_add(bytes_per_sector as u64 - 1)
            .ok_or(Error::AddressOverflow)?
            / bytes_per_sector as u64;
        let first_fat_sector = u64::from(reserved_sectors);
        let root_dir_first_sector = first_fat_sector
            .checked_add(
                u64::from(fat_count)
                    .checked_mul(u64::from(sectors_per_fat))
                    .ok_or(Error::AddressOverflow)?,
            )
            .ok_or(Error::AddressOverflow)?;
        let first_data_sector = root_dir_first_sector
            .checked_add(root_dir_sector_count)
            .ok_or(Error::AddressOverflow)?;
        if first_data_sector >= total_sectors {
            return Err(Error::InvalidBootSector(
                "data region starts outside the volume",
            ));
        }

        let data_sectors = total_sectors
            .checked_sub(first_data_sector)
            .ok_or(Error::AddressOverflow)?;
        let cluster_count_u64 = data_sectors / u64::from(sectors_per_cluster);
        let cluster_count = u32::try_from(cluster_count_u64)
            .map_err(|_| Error::InvalidBootSector("cluster count exceeds FAT32 limits"))?;
        if cluster_count == 0 {
            return Err(Error::InvalidBootSector("volume contains no data clusters"));
        }
        let fat_type = if cluster_count < 4_085 {
            FatType::Fat12
        } else if cluster_count < 65_525 {
            FatType::Fat16
        } else {
            FatType::Fat32
        };

        if fat_type == FatType::Fat32 && root_directory_entries != 0 {
            return Err(Error::InvalidBootSector(
                "FAT32 volume has a fixed-root directory entry count",
            ));
        }
        if fat_type != FatType::Fat32 && root_directory_entries == 0 {
            return Err(Error::InvalidBootSector(
                "FAT12/16 volume has no fixed-root directory",
            ));
        }

        let root_cluster = if fat_type == FatType::Fat32 {
            let cluster = read_u32(&boot_sector, 44)
                .ok_or(Error::InvalidBootSector("missing FAT32 root cluster"))?;
            if cluster < 2 || cluster > cluster_count.saturating_add(1) {
                return Err(Error::InvalidBootSector("FAT32 root cluster is invalid"));
            }
            cluster
        } else {
            0
        };

        let fat_bytes = u64::from(sectors_per_fat)
            .checked_mul(bytes_per_sector as u64)
            .ok_or(Error::AddressOverflow)?;
        let fat_entries = u64::from(cluster_count)
            .checked_add(2)
            .ok_or(Error::AddressOverflow)?;
        let required_fat_bytes = match fat_type {
            FatType::Fat12 => {
                fat_entries
                    .checked_mul(3)
                    .and_then(|value| value.checked_add(1))
                    .ok_or(Error::AddressOverflow)?
                    / 2
            }
            FatType::Fat16 => fat_entries.checked_mul(2).ok_or(Error::AddressOverflow)?,
            FatType::Fat32 => fat_entries.checked_mul(4).ok_or(Error::AddressOverflow)?,
        };
        if fat_bytes < required_fat_bytes {
            return Err(Error::InvalidBootSector(
                "FAT is too small for the data region",
            ));
        }

        let bytes_per_cluster = bytes_per_sector
            .checked_mul(usize::try_from(sectors_per_cluster).map_err(|_| Error::AddressOverflow)?)
            .ok_or(Error::AddressOverflow)?;
        let (volume_id_offset, volume_label_offset) = if fat_type == FatType::Fat32 {
            (67, 71)
        } else {
            (39, 43)
        };
        let volume_id = read_u32(&boot_sector, volume_id_offset).unwrap_or(0);
        let volume_label = decode_label(
            boot_sector
                .get(volume_label_offset..volume_label_offset + 11)
                .unwrap_or(&[]),
        );

        let mut volume = Self {
            partition: partition.clone(),
            info: VolumeInfo {
                partition_index: partition.index,
                partition_start_lba: partition.start_lba,
                partition_block_count: partition.block_count,
                fat_type,
                volume_label,
                volume_id,
                bytes_per_sector,
                sectors_per_cluster,
                bytes_per_cluster,
                reserved_sectors,
                fat_count,
                sectors_per_fat,
                total_sectors,
                cluster_count,
                root_entry_count: 0,
            },
            first_fat_sector,
            root_dir_first_sector,
            root_dir_sector_count,
            first_data_sector,
            root_cluster,
        };
        volume.info.root_entry_count = volume.scan_directory(volume.root_location())?.len();
        Ok(volume)
    }

    fn root_location(&self) -> DirectoryLocation {
        match self.info.fat_type {
            FatType::Fat32 => DirectoryLocation::Cluster(self.root_cluster),
            FatType::Fat12 | FatType::Fat16 => DirectoryLocation::FixedRoot,
        }
    }

    fn list_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>, Error> {
        let components = path_components(path)?;
        let mut location = self.root_location();
        for component in components {
            let entry = self
                .find_entry(location, component)?
                .ok_or(Error::DirectoryNotFound)?;
            if !entry.is_directory() {
                return Err(Error::NotDirectory);
            }
            if entry.first_cluster < 2 {
                return Err(Error::CorruptDirectory);
            }
            location = DirectoryLocation::Cluster(entry.first_cluster);
        }
        self.scan_directory(location)
    }

    fn read_file(&self, path: &str, max_bytes: usize) -> Result<FileData, Error> {
        if max_bytes > MAX_FILE_READ_BYTES {
            return Err(Error::ReadLimitTooLarge(max_bytes));
        }
        let components = path_components(path)?;
        let (file_name, directories) = components.split_last().ok_or(Error::InvalidPath)?;
        let mut location = self.root_location();
        for component in directories {
            let entry = self
                .find_entry(location, component)?
                .ok_or(Error::DirectoryNotFound)?;
            if !entry.is_directory() {
                return Err(Error::NotDirectory);
            }
            if entry.first_cluster < 2 {
                return Err(Error::CorruptDirectory);
            }
            location = DirectoryLocation::Cluster(entry.first_cluster);
        }

        let entry = self
            .find_entry(location, file_name)?
            .ok_or(Error::FileNotFound)?;
        if entry.is_directory() {
            return Err(Error::IsDirectory);
        }

        let target_len = usize::try_from(u64::from(entry.size).min(max_bytes as u64))
            .map_err(|_| Error::AddressOverflow)?;
        let mut bytes = Vec::with_capacity(target_len);
        if target_len == 0 {
            return Ok(FileData {
                bytes,
                total_size: entry.size,
                truncated: false,
            });
        }
        if entry.first_cluster < 2 {
            return Err(Error::InvalidCluster(entry.first_cluster));
        }

        let mut cluster = entry.first_cluster;
        let mut traversed = 0_u32;
        while bytes.len() < target_len {
            traversed = traversed.saturating_add(1);
            if traversed > self.info.cluster_count.saturating_add(1) {
                return Err(Error::FatChainLoop);
            }

            let cluster_bytes = self.read_cluster(cluster)?;
            let remaining = target_len - bytes.len();
            bytes.extend_from_slice(&cluster_bytes[..remaining.min(cluster_bytes.len())]);
            if bytes.len() >= target_len {
                break;
            }
            cluster = self.next_cluster(cluster)?.ok_or(Error::CorruptDirectory)?;
        }

        Ok(FileData {
            bytes,
            total_size: entry.size,
            truncated: target_len < entry.size as usize,
        })
    }

    fn find_entry(
        &self,
        location: DirectoryLocation,
        name: &str,
    ) -> Result<Option<DirectoryEntry>, Error> {
        Ok(self
            .scan_directory(location)?
            .into_iter()
            .find(|entry| path_name_matches(entry, name)))
    }

    fn scan_directory(&self, location: DirectoryLocation) -> Result<Vec<DirectoryEntry>, Error> {
        let mut parser = DirectoryParser::new();
        match location {
            DirectoryLocation::FixedRoot => {
                for relative_sector in self.root_dir_first_sector
                    ..self
                        .root_dir_first_sector
                        .checked_add(self.root_dir_sector_count)
                        .ok_or(Error::AddressOverflow)?
                {
                    let block = self.read_volume_sector(relative_sector)?;
                    if !parser.consume(&block)? {
                        break;
                    }
                }
            }
            DirectoryLocation::Cluster(mut cluster) => {
                let mut traversed = 0_u32;
                loop {
                    traversed = traversed.saturating_add(1);
                    if traversed > self.info.cluster_count.saturating_add(1) {
                        return Err(Error::FatChainLoop);
                    }
                    let bytes = self.read_cluster(cluster)?;
                    if !parser.consume(&bytes)? {
                        break;
                    }
                    let Some(next) = self.next_cluster(cluster)? else {
                        break;
                    };
                    cluster = next;
                }
            }
        }
        Ok(parser.entries)
    }

    fn read_cluster(&self, cluster: u32) -> Result<Vec<u8>, Error> {
        self.validate_cluster(cluster)?;
        let relative_sector = self
            .first_data_sector
            .checked_add(
                u64::from(cluster - 2)
                    .checked_mul(u64::from(self.info.sectors_per_cluster))
                    .ok_or(Error::AddressOverflow)?,
            )
            .ok_or(Error::AddressOverflow)?;
        let end_sector = relative_sector
            .checked_add(u64::from(self.info.sectors_per_cluster))
            .ok_or(Error::AddressOverflow)?;
        if end_sector > self.info.total_sectors {
            return Err(Error::LbaOutOfRange);
        }

        let mut bytes = vec![0_u8; self.info.bytes_per_cluster];
        for sector_index in 0..self.info.sectors_per_cluster {
            let block = self.read_volume_sector(
                relative_sector
                    .checked_add(u64::from(sector_index))
                    .ok_or(Error::AddressOverflow)?,
            )?;
            let offset = usize::try_from(sector_index)
                .map_err(|_| Error::AddressOverflow)?
                .checked_mul(self.info.bytes_per_sector)
                .ok_or(Error::AddressOverflow)?;
            bytes[offset..offset + self.info.bytes_per_sector].copy_from_slice(&block);
        }
        Ok(bytes)
    }

    fn next_cluster(&self, cluster: u32) -> Result<Option<u32>, Error> {
        self.validate_cluster(cluster)?;
        let value = match self.info.fat_type {
            FatType::Fat12 => {
                let offset = u64::from(cluster)
                    .checked_add(u64::from(cluster / 2))
                    .ok_or(Error::AddressOverflow)?;
                let bytes = self.read_fat_bytes(offset, 2)?;
                let word = u16::from_le_bytes([bytes[0], bytes[1]]);
                if cluster & 1 == 0 {
                    u32::from(word & 0x0fff)
                } else {
                    u32::from(word >> 4)
                }
            }
            FatType::Fat16 => {
                let offset = u64::from(cluster)
                    .checked_mul(2)
                    .ok_or(Error::AddressOverflow)?;
                let bytes = self.read_fat_bytes(offset, 2)?;
                u32::from(u16::from_le_bytes([bytes[0], bytes[1]]))
            }
            FatType::Fat32 => {
                let offset = u64::from(cluster)
                    .checked_mul(4)
                    .ok_or(Error::AddressOverflow)?;
                let bytes = self.read_fat_bytes(offset, 4)?;
                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) & 0x0fff_ffff
            }
        };

        match self.info.fat_type {
            FatType::Fat12 => classify_fat_value(value, 0x0ff0, 0x0ff7, 0x0ff8),
            FatType::Fat16 => classify_fat_value(value, 0xfff0, 0xfff7, 0xfff8),
            FatType::Fat32 => classify_fat_value(value, 0x0fff_fff0, 0x0fff_fff7, 0x0fff_fff8),
        }
    }

    fn read_fat_bytes(&self, offset: u64, length: usize) -> Result<Vec<u8>, Error> {
        let fat_byte_offset = self
            .first_fat_sector
            .checked_mul(self.info.bytes_per_sector as u64)
            .and_then(|value| value.checked_add(offset))
            .ok_or(Error::AddressOverflow)?;
        self.read_volume_bytes(fat_byte_offset, length)
    }

    fn read_volume_bytes(&self, offset: u64, length: usize) -> Result<Vec<u8>, Error> {
        let volume_bytes = self
            .info
            .total_sectors
            .checked_mul(self.info.bytes_per_sector as u64)
            .ok_or(Error::AddressOverflow)?;
        let end = offset
            .checked_add(u64::try_from(length).map_err(|_| Error::AddressOverflow)?)
            .ok_or(Error::AddressOverflow)?;
        if end > volume_bytes {
            return Err(Error::LbaOutOfRange);
        }

        let mut output = vec![0_u8; length];
        let mut copied = 0_usize;
        while copied < length {
            let absolute_offset = offset
                .checked_add(u64::try_from(copied).map_err(|_| Error::AddressOverflow)?)
                .ok_or(Error::AddressOverflow)?;
            let relative_sector = absolute_offset / self.info.bytes_per_sector as u64;
            let within_sector =
                usize::try_from(absolute_offset % self.info.bytes_per_sector as u64)
                    .map_err(|_| Error::AddressOverflow)?;
            let block = self.read_volume_sector(relative_sector)?;
            let chunk = (length - copied).min(self.info.bytes_per_sector - within_sector);
            output[copied..copied + chunk]
                .copy_from_slice(&block[within_sector..within_sector + chunk]);
            copied += chunk;
        }
        Ok(output)
    }

    fn read_volume_sector(&self, relative_sector: u64) -> Result<Vec<u8>, Error> {
        if relative_sector >= self.info.total_sectors {
            return Err(Error::LbaOutOfRange);
        }
        let lba = self
            .partition
            .start_lba
            .checked_add(relative_sector)
            .ok_or(Error::AddressOverflow)?;
        read_disk_block(lba, self.info.bytes_per_sector)
    }

    fn validate_cluster(&self, cluster: u32) -> Result<(), Error> {
        if cluster < 2 || cluster > self.info.cluster_count.saturating_add(1) {
            Err(Error::InvalidCluster(cluster))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct DirectoryParser {
    entries: Vec<DirectoryEntry>,
    long_name: LongNameState,
    finished: bool,
}

impl DirectoryParser {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            long_name: LongNameState::new(),
            finished: false,
        }
    }

    fn consume(&mut self, bytes: &[u8]) -> Result<bool, Error> {
        if bytes.len() % DIRECTORY_ENTRY_SIZE != 0 {
            return Err(Error::CorruptDirectory);
        }
        for entry in bytes.chunks_exact(DIRECTORY_ENTRY_SIZE) {
            if self.entries.len() >= MAX_DIRECTORY_ENTRIES {
                return Err(Error::DirectoryTooLarge);
            }

            match entry[0] {
                0x00 => {
                    self.finished = true;
                    self.long_name.clear();
                    return Ok(false);
                }
                0xe5 => {
                    self.long_name.clear();
                    continue;
                }
                _ => {}
            }

            let attributes = entry[11];
            if attributes == ATTR_LONG_NAME {
                self.long_name.consume(entry)?;
                continue;
            }
            if attributes & ATTR_VOLUME_ID != 0 {
                self.long_name.clear();
                continue;
            }

            let short_name = decode_short_name(entry)?;
            if short_name == "." || short_name == ".." {
                self.long_name.clear();
                continue;
            }
            let name = self
                .long_name
                .take(entry)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| short_name.clone());
            let cluster_high = u32::from(read_u16(entry, 20).ok_or(Error::CorruptDirectory)?);
            let cluster_low = u32::from(read_u16(entry, 26).ok_or(Error::CorruptDirectory)?);
            let first_cluster = (cluster_high << 16) | cluster_low;
            let size = read_u32(entry, 28).ok_or(Error::CorruptDirectory)?;
            self.entries.push(DirectoryEntry {
                name,
                short_name,
                attributes,
                first_cluster,
                size,
            });
        }
        Ok(!self.finished)
    }
}

#[derive(Debug)]
struct LongNameState {
    slots: Vec<Option<[u16; 13]>>,
    checksum: Option<u8>,
}

impl LongNameState {
    fn new() -> Self {
        Self {
            slots: Vec::new(),
            checksum: None,
        }
    }

    fn clear(&mut self) {
        self.slots.clear();
        self.checksum = None;
    }

    fn consume(&mut self, entry: &[u8]) -> Result<(), Error> {
        if entry.len() != DIRECTORY_ENTRY_SIZE || entry[12] != 0 {
            self.clear();
            return Err(Error::CorruptDirectory);
        }
        let ordinal = entry[0];
        let sequence = usize::from(ordinal & 0x1f);
        let last = ordinal & 0x40 != 0;
        if sequence == 0 || sequence > MAX_LONG_NAME_SLOTS {
            self.clear();
            return Err(Error::CorruptDirectory);
        }

        if last {
            self.slots.clear();
            self.slots.resize(sequence, None);
            self.checksum = Some(entry[13]);
        }
        if self.slots.len() < sequence || self.checksum != Some(entry[13]) {
            self.clear();
            return Ok(());
        }

        self.slots[sequence - 1] = Some(long_name_words(entry)?);
        Ok(())
    }

    fn take(&mut self, short_entry: &[u8]) -> Option<String> {
        let valid = !self.slots.is_empty()
            && self.slots.iter().all(Option::is_some)
            && self.checksum == Some(short_name_checksum(&short_entry[..11]));
        let result = valid.then(|| {
            let mut words = Vec::new();
            for slot in &self.slots {
                words.extend_from_slice(slot.as_ref().unwrap());
            }
            decode_long_name(&words)
        });
        self.clear();
        result
    }
}

pub fn init(partitions: &PartitionInventory) -> Result<VolumeInfo, Error> {
    let mut mounted = VOLUME.lock();
    if mounted.is_some() {
        return Err(Error::AlreadyInitialized);
    }

    let mut last_error = None;
    for partition in partitions.filesystem_candidates() {
        match FatVolume::mount(partition) {
            Ok(volume) => {
                let info = volume.info.clone();
                *mounted = Some(volume);
                return Ok(info);
            }
            Err(error) => last_error = Some(error),
        }
    }

    Err(last_error.unwrap_or(Error::NoSupportedPartition))
}

pub fn info() -> Option<VolumeInfo> {
    VOLUME.lock().as_ref().map(|volume| volume.info.clone())
}

pub fn list_directory(path: &str) -> Result<Vec<DirectoryEntry>, Error> {
    let mounted = VOLUME.lock();
    let volume = mounted.as_ref().ok_or(Error::NotInitialized)?;
    volume.list_directory(path)
}

pub fn read_file(path: &str, max_bytes: usize) -> Result<FileData, Error> {
    let mounted = VOLUME.lock();
    let volume = mounted.as_ref().ok_or(Error::NotInitialized)?;
    volume.read_file(path, max_bytes)
}

fn path_components(path: &str) -> Result<Vec<&str>, Error> {
    let mut components = Vec::new();
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." || component.as_bytes().contains(&0) {
            return Err(Error::InvalidPath);
        }
        components.push(component);
        if components.len() > MAX_PATH_COMPONENTS {
            return Err(Error::InvalidPath);
        }
    }
    Ok(components)
}

fn path_name_matches(entry: &DirectoryEntry, name: &str) -> bool {
    entry.name.eq_ignore_ascii_case(name) || entry.short_name.eq_ignore_ascii_case(name)
}

fn classify_fat_value(
    value: u32,
    reserved_start: u32,
    bad_value: u32,
    end_of_chain_start: u32,
) -> Result<Option<u32>, Error> {
    if value == 0 {
        Err(Error::FreeCluster(value))
    } else if value == 1 || (reserved_start..bad_value).contains(&value) {
        Err(Error::ReservedCluster(value))
    } else if value == bad_value {
        Err(Error::BadCluster(value))
    } else if value >= end_of_chain_start {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn read_disk_block(lba: u64, block_size: usize) -> Result<Vec<u8>, Error> {
    let mut block = vec![0_u8; block_size];
    ahci::read_block(lba, &mut block)?;
    Ok(block)
}

fn decode_label(bytes: &[u8]) -> String {
    let mut value = String::new();
    for byte in bytes.iter().copied() {
        if byte == b' ' || byte == 0 {
            value.push(' ');
        } else if byte.is_ascii_graphic() {
            value.push(char::from(byte));
        } else {
            value.push('?');
        }
    }
    while value.ends_with(' ') {
        value.pop();
    }
    value
}

fn decode_short_name(entry: &[u8]) -> Result<String, Error> {
    if entry.len() != DIRECTORY_ENTRY_SIZE {
        return Err(Error::CorruptDirectory);
    }
    let mut raw = [0_u8; 11];
    raw.copy_from_slice(&entry[..11]);
    if raw[0] == 0x05 {
        raw[0] = 0xe5;
    }

    let mut base = decode_short_component(&raw[..8]);
    let mut extension = decode_short_component(&raw[8..]);
    let case_flags = entry[12];
    if case_flags & 0x08 != 0 {
        base.make_ascii_lowercase();
    }
    if case_flags & 0x10 != 0 {
        extension.make_ascii_lowercase();
    }

    if base.is_empty() {
        return Err(Error::CorruptDirectory);
    }
    if extension.is_empty() {
        Ok(base)
    } else {
        base.push('.');
        base.push_str(&extension);
        Ok(base)
    }
}

fn decode_short_component(bytes: &[u8]) -> String {
    let mut value = String::new();
    for byte in bytes.iter().copied() {
        if byte == b' ' {
            continue;
        }
        value.push(if byte.is_ascii_graphic() {
            char::from(byte)
        } else {
            '?'
        });
    }
    value
}

fn long_name_words(entry: &[u8]) -> Result<[u16; 13], Error> {
    let mut words = [0_u16; 13];
    let mut index = 0;
    for range in [1..11, 14..26, 28..32] {
        for pair in entry
            .get(range)
            .ok_or(Error::CorruptDirectory)?
            .chunks_exact(2)
        {
            words[index] = u16::from_le_bytes([pair[0], pair[1]]);
            index += 1;
        }
    }
    Ok(words)
}

fn decode_long_name(words: &[u16]) -> String {
    let words = words
        .iter()
        .copied()
        .take_while(|word| *word != 0 && *word != 0xffff);
    let mut value = String::new();
    for character in decode_utf16(words) {
        let character = character.unwrap_or('?');
        value.push(if character == '/' || character.is_control() {
            '?'
        } else {
            character
        });
    }
    value
}

fn short_name_checksum(bytes: &[u8]) -> u8 {
    bytes.iter().copied().fold(0_u8, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(byte)
    })
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let bytes: [u8; 2] = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(u16::from_le_bytes(bytes))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
    Some(u32::from_le_bytes(bytes))
}
