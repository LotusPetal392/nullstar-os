use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::fmt;

use spin::Mutex;

use crate::fat;

pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_PATH_COMPONENTS: usize = 32;
pub const MAX_READ_WINDOW_BYTES: usize = 1024 * 1024;

static ROOT_MOUNT: Mutex<Option<MountedRoot>> = Mutex::new(None);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSystemKind {
    Fat,
}

impl fmt::Display for FileSystemKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fat => formatter.write_str("fat"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
}

impl fmt::Display for NodeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File => formatter.write_str("file"),
            Self::Directory => formatter.write_str("directory"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MountInfo {
    pub mount_path: &'static str,
    pub filesystem: FileSystemKind,
    pub volume_label: String,
    pub volume_id: u32,
    pub partition_index: u32,
    pub partition_start_lba: u64,
    pub read_only: bool,
}

#[derive(Debug, Clone)]
pub struct Metadata {
    pub path: String,
    pub kind: NodeKind,
    pub size: u64,
    pub read_only: bool,
    pub hidden: bool,
    pub system: bool,
}

impl Metadata {
    pub fn is_file(&self) -> bool {
        self.kind == NodeKind::File
    }

    pub fn is_directory(&self) -> bool {
        self.kind == NodeKind::Directory
    }
}

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub name: String,
    pub kind: NodeKind,
    pub size: u64,
    pub read_only: bool,
    pub hidden: bool,
    pub system: bool,
}

impl DirectoryEntry {
    pub fn is_file(&self) -> bool {
        self.kind == NodeKind::File
    }

    pub fn is_directory(&self) -> bool {
        self.kind == NodeKind::Directory
    }
}

#[derive(Debug, Clone)]
pub struct FileData {
    pub bytes: Vec<u8>,
    pub total_size: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    AlreadyMounted,
    NotMounted,
    InvalidPath,
    PathTooLong,
    TooManyPathComponents,
    NotFound,
    NotDirectory,
    IsDirectory,
    AddressOverflow,
    ReadWindowTooLarge { end: u64, maximum: usize },
    ShortRead { expected: usize, actual: usize },
    Fat(fat::Error),
}

impl Error {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::AlreadyMounted => "a root filesystem is already mounted",
            Self::NotMounted => "no root filesystem is mounted",
            Self::InvalidPath => "filesystem path is invalid",
            Self::PathTooLong => "filesystem path exceeds the configured bound",
            Self::TooManyPathComponents => "filesystem path has too many components",
            Self::NotFound => "filesystem path was not found",
            Self::NotDirectory => "filesystem path is not a directory",
            Self::IsDirectory => "filesystem path identifies a directory",
            Self::AddressOverflow => "filesystem offset calculation overflowed",
            Self::ReadWindowTooLarge { .. } => {
                "filesystem read requires a prefix larger than the configured bound"
            }
            Self::ShortRead { .. } => "filesystem returned fewer bytes than requested",
            Self::Fat(_) => "FAT filesystem operation failed",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadWindowTooLarge { end, maximum } => write!(
                formatter,
                "read ending at byte {end} exceeds the {maximum}-byte FAT prefix bound"
            ),
            Self::ShortRead { expected, actual } => {
                write!(formatter, "short filesystem read: expected {expected}, received {actual}")
            }
            Self::Fat(error) => write!(formatter, "FAT error: {error}"),
            _ => formatter.write_str(self.description()),
        }
    }
}

impl From<fat::Error> for Error {
    fn from(error: fat::Error) -> Self {
        Self::Fat(error)
    }
}

#[derive(Debug, Clone, Copy)]
enum BackendKind {
    Fat,
}

#[derive(Debug, Clone)]
struct MountedRoot {
    backend: BackendKind,
    info: MountInfo,
}

trait FileSystem {
    fn metadata(&self, path: &str) -> Result<Metadata, Error>;
    fn read_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>, Error>;
    fn read_at(&self, path: &str, offset: u64, buffer: &mut [u8]) -> Result<usize, Error>;
}

#[derive(Debug, Clone, Copy)]
struct FatFileSystem;

impl FileSystem for FatFileSystem {
    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        if path == "/" {
            return Ok(Metadata {
                path: String::from("/"),
                kind: NodeKind::Directory,
                size: 0,
                read_only: true,
                hidden: false,
                system: false,
            });
        }

        let (parent, name) = parent_and_name(path)?;
        let entries = fat::list_directory(parent)?;
        let entry = entries
            .into_iter()
            .find(|entry| {
                entry.name.eq_ignore_ascii_case(name)
                    || entry.short_name.eq_ignore_ascii_case(name)
            })
            .ok_or(Error::NotFound)?;

        Ok(Metadata {
            path: path.to_string(),
            kind: if entry.is_directory() {
                NodeKind::Directory
            } else {
                NodeKind::File
            },
            size: u64::from(entry.size),
            read_only: entry.is_read_only(),
            hidden: entry.is_hidden(),
            system: entry.is_system(),
        })
    }

    fn read_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>, Error> {
        if !self.metadata(path)?.is_directory() {
            return Err(Error::NotDirectory);
        }

        let entries = fat::list_directory(path)?;
        Ok(entries
            .into_iter()
            .map(|entry| DirectoryEntry {
                name: entry.name,
                kind: if entry.is_directory() {
                    NodeKind::Directory
                } else {
                    NodeKind::File
                },
                size: u64::from(entry.size),
                read_only: entry.is_read_only(),
                hidden: entry.is_hidden(),
                system: entry.is_system(),
            })
            .collect())
    }

    fn read_at(&self, path: &str, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        let metadata = self.metadata(path)?;
        if metadata.is_directory() {
            return Err(Error::IsDirectory);
        }
        if buffer.is_empty() || offset >= metadata.size {
            return Ok(0);
        }

        let remaining = metadata
            .size
            .checked_sub(offset)
            .ok_or(Error::AddressOverflow)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| Error::AddressOverflow)?;
        let end = offset
            .checked_add(requested as u64)
            .ok_or(Error::AddressOverflow)?;
        if end > MAX_READ_WINDOW_BYTES as u64 {
            return Err(Error::ReadWindowTooLarge {
                end,
                maximum: MAX_READ_WINDOW_BYTES,
            });
        }

        let prefix = fat::read_file(
            path,
            usize::try_from(end).map_err(|_| Error::AddressOverflow)?,
        )?;
        let start = usize::try_from(offset).map_err(|_| Error::AddressOverflow)?;
        let end = start
            .checked_add(requested)
            .ok_or(Error::AddressOverflow)?;
        let source = prefix.bytes.get(start..end).ok_or(Error::ShortRead {
            expected: end,
            actual: prefix.bytes.len(),
        })?;
        buffer[..requested].copy_from_slice(source);
        Ok(requested)
    }
}

pub fn mount_fat_root() -> Result<MountInfo, Error> {
    let volume = fat::info().ok_or(Error::NotMounted)?;
    let mut root = ROOT_MOUNT.lock();
    if root.is_some() {
        return Err(Error::AlreadyMounted);
    }

    let info = MountInfo {
        mount_path: "/",
        filesystem: FileSystemKind::Fat,
        volume_label: volume.volume_label,
        volume_id: volume.volume_id,
        partition_index: volume.partition_index,
        partition_start_lba: volume.partition_start_lba,
        read_only: true,
    };
    *root = Some(MountedRoot {
        backend: BackendKind::Fat,
        info: info.clone(),
    });
    Ok(info)
}

pub fn info() -> Option<MountInfo> {
    ROOT_MOUNT.lock().as_ref().map(|root| root.info.clone())
}

pub fn metadata(path: &str) -> Result<Metadata, Error> {
    let path = normalize_path(path)?;
    with_backend(|filesystem| filesystem.metadata(&path))
}

pub fn read_directory(path: &str) -> Result<Vec<DirectoryEntry>, Error> {
    let path = normalize_path(path)?;
    with_backend(|filesystem| filesystem.read_directory(&path))
}

pub fn read_at(path: &str, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
    let path = normalize_path(path)?;
    with_backend(|filesystem| filesystem.read_at(&path, offset, buffer))
}

pub fn read_exact_at(path: &str, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
    let actual = read_at(path, offset, buffer)?;
    if actual != buffer.len() {
        return Err(Error::ShortRead {
            expected: buffer.len(),
            actual,
        });
    }
    Ok(())
}

pub fn read_file(path: &str, maximum_bytes: usize) -> Result<FileData, Error> {
    if maximum_bytes > MAX_READ_WINDOW_BYTES {
        return Err(Error::ReadWindowTooLarge {
            end: maximum_bytes as u64,
            maximum: MAX_READ_WINDOW_BYTES,
        });
    }

    let metadata = metadata(path)?;
    if metadata.is_directory() {
        return Err(Error::IsDirectory);
    }

    let requested = usize::try_from(metadata.size.min(maximum_bytes as u64))
        .map_err(|_| Error::AddressOverflow)?;
    let mut bytes = vec![0_u8; requested];
    read_exact_at(path, 0, &mut bytes)?;
    Ok(FileData {
        bytes,
        total_size: metadata.size,
        truncated: requested as u64 < metadata.size,
    })
}

pub fn join(parent: &str, child: &str) -> Result<String, Error> {
    if child.is_empty() || child.contains('/') || child.as_bytes().contains(&0) {
        return Err(Error::InvalidPath);
    }

    let parent = normalize_path(parent)?;
    let candidate = if parent == "/" {
        alloc::format!("/{child}")
    } else {
        alloc::format!("{parent}/{child}")
    };
    normalize_path(&candidate)
}

pub fn normalize_path(path: &str) -> Result<String, Error> {
    if !path.starts_with('/') || path.as_bytes().contains(&0) {
        return Err(Error::InvalidPath);
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(Error::PathTooLong);
    }

    let mut normalized = String::from("/");
    let mut component_count = 0usize;
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." {
            return Err(Error::InvalidPath);
        }

        component_count = component_count.saturating_add(1);
        if component_count > MAX_PATH_COMPONENTS {
            return Err(Error::TooManyPathComponents);
        }
        if normalized.len() > 1 {
            normalized.push('/');
        }
        normalized.push_str(component);
        if normalized.len() > MAX_PATH_BYTES {
            return Err(Error::PathTooLong);
        }
    }

    Ok(normalized)
}

fn parent_and_name(path: &str) -> Result<(&str, &str), Error> {
    if path == "/" {
        return Err(Error::InvalidPath);
    }

    let separator = path.rfind('/').ok_or(Error::InvalidPath)?;
    let name = path.get(separator + 1..).ok_or(Error::InvalidPath)?;
    if name.is_empty() {
        return Err(Error::InvalidPath);
    }
    let parent = if separator == 0 {
        "/"
    } else {
        path.get(..separator).ok_or(Error::InvalidPath)?
    };
    Ok((parent, name))
}

fn with_backend<T>(
    operation: impl FnOnce(&dyn FileSystem) -> Result<T, Error>,
) -> Result<T, Error> {
    let backend = ROOT_MOUNT
        .lock()
        .as_ref()
        .map(|root| root.backend)
        .ok_or(Error::NotMounted)?;

    match backend {
        BackendKind::Fat => operation(&FatFileSystem),
    }
}
