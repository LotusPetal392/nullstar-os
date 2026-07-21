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
pub const MAX_FILE_WRITE_BYTES: usize = 1024 * 1024;
const ATTR_ARCHIVE: u8 = 0x20;
const FAT16_END_OF_CHAIN: u16 = 0xffff;

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
    ReadOnly,
    WriteUnsupported,
    RootOnly,
    InvalidShortName,
    RootDirectoryFull,
    FileTooLarge(usize),
    NoSpace,
    ReadLimitTooLarge(usize),
}

impl Error {
    pub const fn description(self) -> &'static str {
        match self {
            Self::AlreadyInitialized => "FAT filesystem is already initialized",
            Self::NotInitialized => "FAT filesystem is not initialized",
            Self::NoSupportedPartition => "no readable FAT volume was found",
            Self::Ahci(_) => "AHCI block I/O failed",
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
            Self::ReadOnly => "FAT directory entry is read-only",
            Self::WriteUnsupported => "writes are supported only on FAT16 volumes",
            Self::RootOnly => "FAT writes are limited to root-directory regular files",
            Self::InvalidShortName => "FAT write path is not a supported 8.3 short name",
            Self::RootDirectoryFull => "FAT fixed root directory contains no free entry",
            Self::FileTooLarge(_) => "FAT file exceeds the configured write bound",
            Self::NoSpace => "FAT volume contains too few free clusters",
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
            Self::FileTooLarge(size) => write!(
                formatter,
                "FAT file size {size} exceeds {MAX_FILE_WRITE_BYTES} bytes"
            ),
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
    pub writable: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct WriteInfo {
    pub writable: bool,
    pub maximum_file_bytes: usize,
    pub creates: u64,
    pub truncates: u64,
    pub writes: u64,
    pub bytes_written: u64,
    pub clusters_allocated: u64,
    pub clusters_freed: u64,
    pub fat_entry_updates: u64,
    pub directory_updates: u64,
    pub flushes: u64,
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
    write_info: WriteInfo,
}

#[derive(Debug, Clone, Copy)]
struct RootEntryLocation {
    relative_sector: u64,
    offset: usize,
}

#[derive(Debug, Clone)]
struct RootFileRecord {
    entry: DirectoryEntry,
    location: RootEntryLocation,
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
                writable: fat_type == FatType::Fat16,
            },
            first_fat_sector,
            root_dir_first_sector,
            root_dir_sector_count,
            first_data_sector,
            root_cluster,
            write_info: WriteInfo {
                writable: fat_type == FatType::Fat16,
                maximum_file_bytes: MAX_FILE_WRITE_BYTES,
                creates: 0,
                truncates: 0,
                writes: 0,
                bytes_written: 0,
                clusters_allocated: 0,
                clusters_freed: 0,
                fat_entry_updates: 0,
                directory_updates: 0,
                flushes: 0,
            },
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
            truncated: target_len < entry.size as usize,
        })
    }

    fn open_root_file(
        &mut self,
        path: &str,
        create: bool,
        truncate: bool,
    ) -> Result<DirectoryEntry, Error> {
        self.require_writable()?;
        let short_name = encode_root_short_name(path)?;
        let (record, free_location) = self.find_root_file(&short_name)?;
        match record {
            Some(record) => {
                if record.entry.is_directory() {
                    return Err(Error::IsDirectory);
                }
                if record.entry.is_read_only() {
                    return Err(Error::ReadOnly);
                }
                if truncate {
                    let entry = self.replace_root_file(&record, &[])?;
                    self.write_info.truncates = self.write_info.truncates.saturating_add(1);
                    Ok(entry)
                } else {
                    Ok(record.entry)
                }
            }
            None if create => {
                let location = free_location.ok_or(Error::RootDirectoryFull)?;
                self.create_root_file(short_name, location)
            }
            None => Err(Error::FileNotFound),
        }
    }

    fn write_root_file_at(
        &mut self,
        path: &str,
        offset: u64,
        bytes: &[u8],
    ) -> Result<usize, Error> {
        self.require_writable()?;
        if bytes.is_empty() {
            return Ok(0);
        }
        let short_name = encode_root_short_name(path)?;
        let (record, _) = self.find_root_file(&short_name)?;
        let record = record.ok_or(Error::FileNotFound)?;
        if record.entry.is_directory() {
            return Err(Error::IsDirectory);
        }
        if record.entry.is_read_only() {
            return Err(Error::ReadOnly);
        }
        let offset = usize::try_from(offset).map_err(|_| Error::AddressOverflow)?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or(Error::AddressOverflow)?;
        if end > MAX_FILE_WRITE_BYTES {
            return Err(Error::FileTooLarge(end));
        }
        let old_size = usize::try_from(record.entry.size).map_err(|_| Error::AddressOverflow)?;
        if old_size > MAX_FILE_WRITE_BYTES {
            return Err(Error::FileTooLarge(old_size));
        }
        let mut contents = if old_size == 0 {
            Vec::new()
        } else {
            let data = self.read_file(path, MAX_FILE_WRITE_BYTES)?;
            if data.truncated || data.bytes.len() != old_size {
                return Err(Error::CorruptDirectory);
            }
            data.bytes
        };
        if end > contents.len() {
            contents.resize(end, 0);
        }
        contents[offset..end].copy_from_slice(bytes);
        self.replace_root_file(&record, &contents)?;
        self.write_info.writes = self.write_info.writes.saturating_add(1);
        self.write_info.bytes_written = self
            .write_info
            .bytes_written
            .saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn append_root_file(&mut self, path: &str, bytes: &[u8]) -> Result<(u64, usize), Error> {
        self.require_writable()?;
        let short_name = encode_root_short_name(path)?;
        let (record, _) = self.find_root_file(&short_name)?;
        let record = record.ok_or(Error::FileNotFound)?;
        let offset = u64::from(record.entry.size);
        let count = self.write_root_file_at(path, offset, bytes)?;
        Ok((offset, count))
    }

    fn require_writable(&self) -> Result<(), Error> {
        if self.info.fat_type != FatType::Fat16 {
            Err(Error::WriteUnsupported)
        } else {
            Ok(())
        }
    }

    fn find_root_file(
        &self,
        wanted_short_name: &[u8; 11],
    ) -> Result<(Option<RootFileRecord>, Option<RootEntryLocation>), Error> {
        if self.info.fat_type != FatType::Fat16 || self.root_dir_sector_count == 0 {
            return Err(Error::WriteUnsupported);
        }
        let mut free_location = None;
        let end = self
            .root_dir_first_sector
            .checked_add(self.root_dir_sector_count)
            .ok_or(Error::AddressOverflow)?;
        for relative_sector in self.root_dir_first_sector..end {
            let block = self.read_volume_sector(relative_sector)?;
            for (index, entry) in block.chunks_exact(DIRECTORY_ENTRY_SIZE).enumerate() {
                let location = RootEntryLocation {
                    relative_sector,
                    offset: index
                        .checked_mul(DIRECTORY_ENTRY_SIZE)
                        .ok_or(Error::AddressOverflow)?,
                };
                match entry[0] {
                    0x00 => return Ok((None, free_location.or(Some(location)))),
                    0xe5 => {
                        free_location.get_or_insert(location);
                        continue;
                    }
                    _ => {}
                }
                let attributes = entry[11];
                if attributes == ATTR_LONG_NAME || attributes & ATTR_VOLUME_ID != 0 {
                    continue;
                }
                let mut raw_short_name = [0_u8; 11];
                raw_short_name.copy_from_slice(&entry[..11]);
                if raw_short_name[0] == 0x05 {
                    raw_short_name[0] = 0xe5;
                }
                if &raw_short_name == wanted_short_name {
                    return Ok((
                        Some(RootFileRecord {
                            entry: parse_short_directory_entry(entry)?,
                            location,
                        }),
                        free_location,
                    ));
                }
            }
        }
        Ok((None, free_location))
    }

    fn create_root_file(
        &mut self,
        short_name: [u8; 11],
        location: RootEntryLocation,
    ) -> Result<DirectoryEntry, Error> {
        let mut entry = [0_u8; DIRECTORY_ENTRY_SIZE];
        entry[..11].copy_from_slice(&short_name);
        entry[11] = ATTR_ARCHIVE;
        // A deterministic 1980-01-01 timestamp keeps the early kernel independent
        // of wall-clock support while remaining valid FAT metadata.
        entry[16..18].copy_from_slice(&0x0021_u16.to_le_bytes());
        entry[18..20].copy_from_slice(&0x0021_u16.to_le_bytes());
        entry[24..26].copy_from_slice(&0x0021_u16.to_le_bytes());
        self.write_root_entry_bytes(location, &entry)?;
        self.flush_storage()?;
        self.info.root_entry_count = self.info.root_entry_count.saturating_add(1);
        self.write_info.creates = self.write_info.creates.saturating_add(1);
        parse_short_directory_entry(&entry)
    }

    fn replace_root_file(
        &mut self,
        record: &RootFileRecord,
        contents: &[u8],
    ) -> Result<DirectoryEntry, Error> {
        if contents.len() > MAX_FILE_WRITE_BYTES {
            return Err(Error::FileTooLarge(contents.len()));
        }
        let old_chain = self.cluster_chain(record.entry.first_cluster)?;
        let cluster_count = contents
            .len()
            .checked_add(self.info.bytes_per_cluster - 1)
            .ok_or(Error::AddressOverflow)?
            / self.info.bytes_per_cluster;
        let new_chain = self.find_free_clusters(cluster_count)?;

        for (index, cluster) in new_chain.iter().copied().enumerate() {
            let start = index
                .checked_mul(self.info.bytes_per_cluster)
                .ok_or(Error::AddressOverflow)?;
            let end = contents.len().min(
                start
                    .checked_add(self.info.bytes_per_cluster)
                    .ok_or(Error::AddressOverflow)?,
            );
            self.write_cluster(cluster, contents.get(start..end).unwrap_or(&[]))?;
        }
        if !new_chain.is_empty() {
            self.flush_storage()?;
            if let Err(error) = self.link_fat16_chain(&new_chain) {
                let _ = self.clear_fat16_chain(&new_chain);
                let _ = self.flush_storage();
                return Err(error);
            }
            self.flush_storage()?;
        }

        let first_cluster = new_chain.first().copied().unwrap_or(0);
        let size =
            u32::try_from(contents.len()).map_err(|_| Error::FileTooLarge(contents.len()))?;
        let updated_entry = match self.update_root_entry(record, first_cluster, size) {
            Ok(entry) => entry,
            Err(error) => {
                if !new_chain.is_empty() {
                    let _ = self.clear_fat16_chain(&new_chain);
                    let _ = self.flush_storage();
                }
                return Err(error);
            }
        };
        self.flush_storage()?;

        if !old_chain.is_empty() {
            self.clear_fat16_chain(&old_chain)?;
            self.flush_storage()?;
        }
        self.write_info.clusters_allocated = self
            .write_info
            .clusters_allocated
            .saturating_add(new_chain.len() as u64);
        self.write_info.clusters_freed = self
            .write_info
            .clusters_freed
            .saturating_add(old_chain.len() as u64);
        Ok(updated_entry)
    }

    fn update_root_entry(
        &mut self,
        record: &RootFileRecord,
        first_cluster: u32,
        size: u32,
    ) -> Result<DirectoryEntry, Error> {
        if first_cluster > u32::from(u16::MAX) {
            return Err(Error::InvalidCluster(first_cluster));
        }
        let mut block = self.read_volume_sector(record.location.relative_sector)?;
        let end = record
            .location
            .offset
            .checked_add(DIRECTORY_ENTRY_SIZE)
            .ok_or(Error::AddressOverflow)?;
        let entry = block
            .get_mut(record.location.offset..end)
            .ok_or(Error::CorruptDirectory)?;
        entry[11] |= ATTR_ARCHIVE;
        entry[20..22].copy_from_slice(&0_u16.to_le_bytes());
        entry[26..28].copy_from_slice(&(first_cluster as u16).to_le_bytes());
        entry[28..32].copy_from_slice(&size.to_le_bytes());
        let parsed = parse_short_directory_entry(entry)?;
        self.write_volume_sector(record.location.relative_sector, &block)?;
        self.write_info.directory_updates = self.write_info.directory_updates.saturating_add(1);
        Ok(parsed)
    }

    fn write_root_entry_bytes(
        &mut self,
        location: RootEntryLocation,
        entry: &[u8; DIRECTORY_ENTRY_SIZE],
    ) -> Result<(), Error> {
        let mut block = self.read_volume_sector(location.relative_sector)?;
        let end = location
            .offset
            .checked_add(DIRECTORY_ENTRY_SIZE)
            .ok_or(Error::AddressOverflow)?;
        block
            .get_mut(location.offset..end)
            .ok_or(Error::CorruptDirectory)?
            .copy_from_slice(entry);
        self.write_volume_sector(location.relative_sector, &block)?;
        self.write_info.directory_updates = self.write_info.directory_updates.saturating_add(1);
        Ok(())
    }

    fn cluster_chain(&self, first_cluster: u32) -> Result<Vec<u32>, Error> {
        if first_cluster == 0 {
            return Ok(Vec::new());
        }
        self.validate_cluster(first_cluster)?;
        let mut chain = Vec::new();
        let mut cluster = first_cluster;
        loop {
            if chain.len() >= self.info.cluster_count as usize {
                return Err(Error::FatChainLoop);
            }
            chain.push(cluster);
            let Some(next) = self.next_cluster(cluster)? else {
                break;
            };
            if chain.contains(&next) {
                return Err(Error::FatChainLoop);
            }
            cluster = next;
        }
        Ok(chain)
    }

    fn find_free_clusters(&self, count: usize) -> Result<Vec<u32>, Error> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut clusters = Vec::with_capacity(count);
        for cluster in 2..=self.info.cluster_count.saturating_add(1) {
            if self.read_fat16_entry(cluster)? == 0 {
                clusters.push(cluster);
                if clusters.len() == count {
                    return Ok(clusters);
                }
            }
        }
        Err(Error::NoSpace)
    }

    fn read_fat16_entry(&self, cluster: u32) -> Result<u16, Error> {
        self.read_fat16_entry_from_copy(cluster, 0)
    }

    fn read_fat16_entry_from_copy(&self, cluster: u32, fat_copy: u8) -> Result<u16, Error> {
        self.require_writable()?;
        self.validate_cluster(cluster)?;
        if fat_copy >= self.info.fat_count {
            return Err(Error::InvalidPath);
        }
        let offset = u64::from(cluster)
            .checked_mul(2)
            .ok_or(Error::AddressOverflow)?;
        let fat_start = self
            .first_fat_sector
            .checked_add(
                u64::from(fat_copy)
                    .checked_mul(u64::from(self.info.sectors_per_fat))
                    .ok_or(Error::AddressOverflow)?,
            )
            .ok_or(Error::AddressOverflow)?;
        let fat_byte_offset = fat_start
            .checked_mul(self.info.bytes_per_sector as u64)
            .and_then(|value| value.checked_add(offset))
            .ok_or(Error::AddressOverflow)?;
        let bytes = self.read_volume_bytes(fat_byte_offset, 2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn verify_root_file_fat_copies(&self, path: &str) -> Result<bool, Error> {
        self.require_writable()?;
        let short_name = encode_root_short_name(path)?;
        let (record, _) = self.find_root_file(&short_name)?;
        let record = record.ok_or(Error::FileNotFound)?;
        let chain = self.cluster_chain(record.entry.first_cluster)?;
        for cluster in chain {
            let expected = self.read_fat16_entry_from_copy(cluster, 0)?;
            for fat_copy in 1..self.info.fat_count {
                if self.read_fat16_entry_from_copy(cluster, fat_copy)? != expected {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn link_fat16_chain(&mut self, chain: &[u32]) -> Result<(), Error> {
        for (index, cluster) in chain.iter().copied().enumerate() {
            let value = chain
                .get(index.saturating_add(1))
                .copied()
                .map(|next| next as u16)
                .unwrap_or(FAT16_END_OF_CHAIN);
            self.write_fat16_entry(cluster, value)?;
        }
        Ok(())
    }

    fn clear_fat16_chain(&mut self, chain: &[u32]) -> Result<(), Error> {
        for cluster in chain.iter().copied() {
            self.write_fat16_entry(cluster, 0)?;
        }
        Ok(())
    }

    fn write_fat16_entry(&mut self, cluster: u32, value: u16) -> Result<(), Error> {
        self.require_writable()?;
        self.validate_cluster(cluster)?;
        let byte_offset = u64::from(cluster)
            .checked_mul(2)
            .ok_or(Error::AddressOverflow)?;
        let sector_delta = byte_offset / self.info.bytes_per_sector as u64;
        let within_sector = usize::try_from(byte_offset % self.info.bytes_per_sector as u64)
            .map_err(|_| Error::AddressOverflow)?;
        if within_sector + 2 > self.info.bytes_per_sector {
            return Err(Error::CorruptDirectory);
        }
        for fat_copy in 0..self.info.fat_count {
            let relative_sector = self
                .first_fat_sector
                .checked_add(
                    u64::from(fat_copy)
                        .checked_mul(u64::from(self.info.sectors_per_fat))
                        .ok_or(Error::AddressOverflow)?,
                )
                .and_then(|sector| sector.checked_add(sector_delta))
                .ok_or(Error::AddressOverflow)?;
            let mut block = self.read_volume_sector(relative_sector)?;
            block[within_sector..within_sector + 2].copy_from_slice(&value.to_le_bytes());
            self.write_volume_sector(relative_sector, &block)?;
            self.write_info.fat_entry_updates = self.write_info.fat_entry_updates.saturating_add(1);
        }
        Ok(())
    }

    fn write_cluster(&mut self, cluster: u32, bytes: &[u8]) -> Result<(), Error> {
        self.validate_cluster(cluster)?;
        if bytes.len() > self.info.bytes_per_cluster {
            return Err(Error::FileTooLarge(bytes.len()));
        }
        let relative_sector = self.cluster_first_sector(cluster)?;
        let mut cluster_bytes = vec![0_u8; self.info.bytes_per_cluster];
        cluster_bytes[..bytes.len()].copy_from_slice(bytes);
        for sector_index in 0..self.info.sectors_per_cluster {
            let start = usize::try_from(sector_index)
                .map_err(|_| Error::AddressOverflow)?
                .checked_mul(self.info.bytes_per_sector)
                .ok_or(Error::AddressOverflow)?;
            let end = start
                .checked_add(self.info.bytes_per_sector)
                .ok_or(Error::AddressOverflow)?;
            self.write_volume_sector(
                relative_sector
                    .checked_add(u64::from(sector_index))
                    .ok_or(Error::AddressOverflow)?,
                &cluster_bytes[start..end],
            )?;
        }
        Ok(())
    }

    fn cluster_first_sector(&self, cluster: u32) -> Result<u64, Error> {
        self.validate_cluster(cluster)?;
        let relative_sector = self
            .first_data_sector
            .checked_add(
                u64::from(cluster - 2)
                    .checked_mul(u64::from(self.info.sectors_per_cluster))
                    .ok_or(Error::AddressOverflow)?,
            )
            .ok_or(Error::AddressOverflow)?;
        let end = relative_sector
            .checked_add(u64::from(self.info.sectors_per_cluster))
            .ok_or(Error::AddressOverflow)?;
        if end > self.info.total_sectors {
            return Err(Error::LbaOutOfRange);
        }
        Ok(relative_sector)
    }

    fn write_volume_sector(&self, relative_sector: u64, bytes: &[u8]) -> Result<(), Error> {
        if relative_sector >= self.info.total_sectors || bytes.len() != self.info.bytes_per_sector {
            return Err(Error::LbaOutOfRange);
        }
        let lba = self
            .partition
            .start_lba
            .checked_add(relative_sector)
            .ok_or(Error::AddressOverflow)?;
        ahci::write_block(lba, bytes)?;
        Ok(())
    }

    fn flush_storage(&mut self) -> Result<(), Error> {
        ahci::flush()?;
        self.write_info.flushes = self.write_info.flushes.saturating_add(1);
        Ok(())
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

fn encode_root_short_name(path: &str) -> Result<[u8; 11], Error> {
    let components = path_components(path)?;
    if components.len() != 1 {
        return Err(Error::RootOnly);
    }
    let name = components[0];
    let mut parts = name.split('.');
    let base = parts.next().ok_or(Error::InvalidShortName)?;
    let extension = parts.next().unwrap_or("");
    if parts.next().is_some()
        || base.is_empty()
        || base.len() > 8
        || extension.len() > 3
        || !base.as_bytes().iter().copied().all(valid_short_name_byte)
        || !extension
            .as_bytes()
            .iter()
            .copied()
            .all(valid_short_name_byte)
    {
        return Err(Error::InvalidShortName);
    }
    let mut encoded = [b' '; 11];
    for (destination, source) in encoded[..8].iter_mut().zip(base.as_bytes()) {
        *destination = source.to_ascii_uppercase();
    }
    for (destination, source) in encoded[8..].iter_mut().zip(extension.as_bytes()) {
        *destination = source.to_ascii_uppercase();
    }
    Ok(encoded)
}

fn valid_short_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn parse_short_directory_entry(entry: &[u8]) -> Result<DirectoryEntry, Error> {
    if entry.len() != DIRECTORY_ENTRY_SIZE {
        return Err(Error::CorruptDirectory);
    }
    let short_name = decode_short_name(entry)?;
    let cluster_high = u32::from(read_u16(entry, 20).ok_or(Error::CorruptDirectory)?);
    let cluster_low = u32::from(read_u16(entry, 26).ok_or(Error::CorruptDirectory)?);
    let first_cluster = (cluster_high << 16) | cluster_low;
    let size = read_u32(entry, 28).ok_or(Error::CorruptDirectory)?;
    Ok(DirectoryEntry {
        name: short_name.clone(),
        short_name,
        attributes: entry[11],
        first_cluster,
        size,
    })
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
        if !bytes.len().is_multiple_of(DIRECTORY_ENTRY_SIZE) {
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

pub fn write_info() -> Option<WriteInfo> {
    VOLUME.lock().as_ref().map(|volume| volume.write_info)
}

pub fn open_file(path: &str, create: bool, truncate: bool) -> Result<DirectoryEntry, Error> {
    let mut mounted = VOLUME.lock();
    let volume = mounted.as_mut().ok_or(Error::NotInitialized)?;
    volume.open_root_file(path, create, truncate)
}

pub fn write_file_at(path: &str, offset: u64, bytes: &[u8]) -> Result<usize, Error> {
    let mut mounted = VOLUME.lock();
    let volume = mounted.as_mut().ok_or(Error::NotInitialized)?;
    volume.write_root_file_at(path, offset, bytes)
}

pub fn append_file(path: &str, bytes: &[u8]) -> Result<(u64, usize), Error> {
    let mut mounted = VOLUME.lock();
    let volume = mounted.as_mut().ok_or(Error::NotInitialized)?;
    volume.append_root_file(path, bytes)
}

pub fn verify_file_fat_copies(path: &str) -> Result<bool, Error> {
    let mounted = VOLUME.lock();
    let volume = mounted.as_ref().ok_or(Error::NotInitialized)?;
    volume.verify_root_file_fat_copies(path)
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
