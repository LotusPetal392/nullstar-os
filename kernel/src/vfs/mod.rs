use alloc::{
    format,
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
pub const TMPFS_MOUNT_PATH: &str = "/tmp";
pub const TMPFS_MAX_FILES: usize = 32;
pub const TMPFS_MAX_FILE_BYTES: usize = 64 * 1024;
pub const TMPFS_MAX_TOTAL_BYTES: usize = 256 * 1024;
pub const TMPFS_MAX_NAME_BYTES: usize = 128;

static ROOT_MOUNT: Mutex<Option<MountedRoot>> = Mutex::new(None);
static TMPFS: Mutex<TmpFileSystem> = Mutex::new(TmpFileSystem::new());

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

#[derive(Debug, Clone, Copy)]
pub struct TmpfsInfo {
    pub mount_path: &'static str,
    pub file_count: usize,
    pub total_bytes: usize,
    pub maximum_files: usize,
    pub maximum_file_bytes: usize,
    pub maximum_total_bytes: usize,
    pub creates: u64,
    pub truncates: u64,
    pub writes: u64,
    pub bytes_written: u64,
    pub rejected_writes: u64,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub truncate: bool,
    pub append: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    AlreadyMounted,
    NotMounted,
    InvalidPath,
    PathTooLong,
    TooManyPathComponents,
    NameTooLong,
    NotFound,
    NotDirectory,
    IsDirectory,
    ReadOnly,
    InvalidOpenOptions,
    TooManyFiles,
    FileTooLarge,
    NoSpace,
    AddressOverflow,
    ReadWindowTooLarge { end: u64, maximum: usize },
    ShortRead { expected: usize, actual: usize },
    Fat(fat::Error),
}

impl Error {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::AlreadyMounted => "a filesystem is already mounted at that path",
            Self::NotMounted => "the requested filesystem is not mounted",
            Self::InvalidPath => "filesystem path is invalid",
            Self::PathTooLong => "filesystem path exceeds the configured bound",
            Self::TooManyPathComponents => "filesystem path has too many components",
            Self::NameTooLong => "tmpfs file name exceeds the configured bound",
            Self::NotFound => "filesystem path was not found",
            Self::NotDirectory => "filesystem path is not a directory",
            Self::IsDirectory => "filesystem path identifies a directory",
            Self::ReadOnly => "filesystem is read-only",
            Self::InvalidOpenOptions => "file open options are invalid",
            Self::TooManyFiles => "tmpfs file-count limit was reached",
            Self::FileTooLarge => "tmpfs file-size limit was exceeded",
            Self::NoSpace => "tmpfs aggregate capacity was exceeded",
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
                "read ending at byte {end} exceeds the {maximum}-byte filesystem prefix bound"
            ),
            Self::ShortRead { expected, actual } => write!(
                formatter,
                "short filesystem read: expected {expected}, received {actual}"
            ),
            Self::Fat(error) => write!(formatter, "FAT error: {error}"),
            _ => formatter.write_str(self.description()),
        }
    }
}

impl From<fat::Error> for Error {
    fn from(error: fat::Error) -> Self {
        match error {
            fat::Error::FileNotFound => Self::NotFound,
            fat::Error::DirectoryNotFound => Self::NotFound,
            fat::Error::NotDirectory => Self::NotDirectory,
            fat::Error::IsDirectory => Self::IsDirectory,
            fat::Error::ReadOnly | fat::Error::WriteUnsupported => Self::ReadOnly,
            fat::Error::FileTooLarge(_) => Self::FileTooLarge,
            fat::Error::NoSpace | fat::Error::RootDirectoryFull => Self::NoSpace,
            fat::Error::InvalidPath | fat::Error::RootOnly | fat::Error::InvalidShortName => {
                Self::InvalidPath
            }
            other => Self::Fat(other),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum BackendKind {
    Fat,
    Tmpfs,
}

#[derive(Debug, Clone)]
struct MountedRoot {
    info: MountInfo,
}

#[derive(Debug, Clone)]
struct TmpFile {
    name: String,
    bytes: Vec<u8>,
}

struct TmpFileSystem {
    mounted: bool,
    files: Vec<TmpFile>,
    total_bytes: usize,
    creates: u64,
    truncates: u64,
    writes: u64,
    bytes_written: u64,
    rejected_writes: u64,
}

impl TmpFileSystem {
    const fn new() -> Self {
        Self {
            mounted: false,
            files: Vec::new(),
            total_bytes: 0,
            creates: 0,
            truncates: 0,
            writes: 0,
            bytes_written: 0,
            rejected_writes: 0,
        }
    }

    fn info(&self) -> Option<TmpfsInfo> {
        self.mounted.then_some(TmpfsInfo {
            mount_path: TMPFS_MOUNT_PATH,
            file_count: self.files.len(),
            total_bytes: self.total_bytes,
            maximum_files: TMPFS_MAX_FILES,
            maximum_file_bytes: TMPFS_MAX_FILE_BYTES,
            maximum_total_bytes: TMPFS_MAX_TOTAL_BYTES,
            creates: self.creates,
            truncates: self.truncates,
            writes: self.writes,
            bytes_written: self.bytes_written,
            rejected_writes: self.rejected_writes,
        })
    }

    fn metadata(&self, path: &str) -> Result<Metadata, Error> {
        self.require_mounted()?;
        if path == TMPFS_MOUNT_PATH {
            return Ok(Metadata {
                path: String::from(TMPFS_MOUNT_PATH),
                kind: NodeKind::Directory,
                size: 0,
                read_only: false,
                hidden: false,
                system: false,
            });
        }
        let name = tmpfs_name(path)?;
        let file = self
            .files
            .iter()
            .find(|file| file.name == name)
            .ok_or(Error::NotFound)?;
        Ok(Metadata {
            path: format!("{TMPFS_MOUNT_PATH}/{}", file.name),
            kind: NodeKind::File,
            size: file.bytes.len() as u64,
            read_only: false,
            hidden: false,
            system: false,
        })
    }

    fn read_directory(&self, path: &str) -> Result<Vec<DirectoryEntry>, Error> {
        self.require_mounted()?;
        if path != TMPFS_MOUNT_PATH {
            return Err(Error::NotDirectory);
        }
        Ok(self
            .files
            .iter()
            .map(|file| DirectoryEntry {
                name: file.name.clone(),
                kind: NodeKind::File,
                size: file.bytes.len() as u64,
                read_only: false,
                hidden: false,
                system: false,
            })
            .collect())
    }

    fn open(&mut self, path: &str, options: OpenOptions) -> Result<Metadata, Error> {
        self.require_mounted()?;
        validate_open_options(options)?;
        if path == TMPFS_MOUNT_PATH {
            return Err(Error::IsDirectory);
        }
        let name = tmpfs_name(path)?;
        let index = self.files.iter().position(|file| file.name == name);
        let index = match index {
            Some(index) => index,
            None if options.create => {
                if self.files.len() >= TMPFS_MAX_FILES {
                    return Err(Error::TooManyFiles);
                }
                self.files.push(TmpFile {
                    name: name.to_string(),
                    bytes: Vec::new(),
                });
                self.creates = self.creates.saturating_add(1);
                self.files.len() - 1
            }
            None => return Err(Error::NotFound),
        };
        if options.truncate {
            let old_length = self.files[index].bytes.len();
            self.files[index].bytes.clear();
            self.total_bytes = self.total_bytes.saturating_sub(old_length);
            self.truncates = self.truncates.saturating_add(1);
        }
        self.metadata(path)
    }

    fn read_at(&self, path: &str, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
        self.require_mounted()?;
        let name = tmpfs_name(path)?;
        let file = self
            .files
            .iter()
            .find(|file| file.name == name)
            .ok_or(Error::NotFound)?;
        let offset = usize::try_from(offset).map_err(|_| Error::AddressOverflow)?;
        if buffer.is_empty() || offset >= file.bytes.len() {
            return Ok(0);
        }
        let count = buffer.len().min(file.bytes.len() - offset);
        buffer[..count].copy_from_slice(&file.bytes[offset..offset + count]);
        Ok(count)
    }

    fn write_at(&mut self, path: &str, offset: u64, bytes: &[u8]) -> Result<usize, Error> {
        self.require_mounted()?;
        let name = tmpfs_name(path)?;
        let index = self
            .files
            .iter()
            .position(|file| file.name == name)
            .ok_or(Error::NotFound)?;
        let offset = usize::try_from(offset).map_err(|_| Error::AddressOverflow)?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or(Error::AddressOverflow)?;
        let old_length = self.files[index].bytes.len();
        let new_length = old_length.max(end);
        if new_length > TMPFS_MAX_FILE_BYTES {
            self.rejected_writes = self.rejected_writes.saturating_add(1);
            return Err(Error::FileTooLarge);
        }
        let new_total = self
            .total_bytes
            .checked_sub(old_length)
            .and_then(|total| total.checked_add(new_length))
            .ok_or(Error::AddressOverflow)?;
        if new_total > TMPFS_MAX_TOTAL_BYTES {
            self.rejected_writes = self.rejected_writes.saturating_add(1);
            return Err(Error::NoSpace);
        }
        if end > old_length {
            self.files[index].bytes.resize(end, 0);
        }
        self.files[index].bytes[offset..end].copy_from_slice(bytes);
        self.total_bytes = new_total;
        self.writes = self.writes.saturating_add(1);
        self.bytes_written = self.bytes_written.saturating_add(bytes.len() as u64);
        Ok(bytes.len())
    }

    fn append(&mut self, path: &str, bytes: &[u8]) -> Result<(u64, usize), Error> {
        self.require_mounted()?;
        let name = tmpfs_name(path)?;
        let offset = self
            .files
            .iter()
            .find(|file| file.name == name)
            .map(|file| file.bytes.len())
            .ok_or(Error::NotFound)?;
        let count = self.write_at(path, offset as u64, bytes)?;
        Ok((offset as u64, count))
    }

    fn require_mounted(&self) -> Result<(), Error> {
        self.mounted.then_some(()).ok_or(Error::NotMounted)
    }
}

#[derive(Debug, Clone, Copy)]
struct FatFileSystem;

impl FatFileSystem {
    fn metadata(self, path: &str) -> Result<Metadata, Error> {
        if path == "/" {
            return Ok(Metadata {
                path: String::from("/"),
                kind: NodeKind::Directory,
                size: 0,
                read_only: fat::info().is_none_or(|volume| !volume.writable),
                hidden: false,
                system: false,
            });
        }

        let (parent, name) = parent_and_name(path)?;
        let entry = fat::list_directory(parent)?
            .into_iter()
            .find(|entry| {
                entry.name.eq_ignore_ascii_case(name) || entry.short_name.eq_ignore_ascii_case(name)
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
            read_only: entry.is_directory() || entry.is_read_only(),
            hidden: entry.is_hidden(),
            system: entry.is_system(),
        })
    }

    fn open(self, path: &str, options: OpenOptions) -> Result<Metadata, Error> {
        let entry = fat::open_file(path, options.create, options.truncate)?;
        if entry.is_directory() {
            return Err(Error::IsDirectory);
        }
        Ok(Metadata {
            path: path.to_string(),
            kind: NodeKind::File,
            size: u64::from(entry.size),
            read_only: entry.is_read_only(),
            hidden: entry.is_hidden(),
            system: entry.is_system(),
        })
    }

    fn write_at(self, path: &str, offset: u64, bytes: &[u8]) -> Result<usize, Error> {
        fat::write_file_at(path, offset, bytes).map_err(Into::into)
    }

    fn append(self, path: &str, bytes: &[u8]) -> Result<(u64, usize), Error> {
        fat::append_file(path, bytes).map_err(Into::into)
    }

    fn read_directory(self, path: &str) -> Result<Vec<DirectoryEntry>, Error> {
        if !self.metadata(path)?.is_directory() {
            return Err(Error::NotDirectory);
        }

        Ok(fat::list_directory(path)?
            .into_iter()
            .map(convert_fat_directory_entry)
            .collect())
    }

    fn read_at(self, path: &str, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
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
        let prefix_end = offset
            .checked_add(requested as u64)
            .ok_or(Error::AddressOverflow)?;
        if prefix_end > MAX_READ_WINDOW_BYTES as u64 {
            return Err(Error::ReadWindowTooLarge {
                end: prefix_end,
                maximum: MAX_READ_WINDOW_BYTES,
            });
        }

        let prefix = fat::read_file(
            path,
            usize::try_from(prefix_end).map_err(|_| Error::AddressOverflow)?,
        )?;
        let start = usize::try_from(offset).map_err(|_| Error::AddressOverflow)?;
        let end = start.checked_add(requested).ok_or(Error::AddressOverflow)?;
        let source = prefix.bytes.get(start..end).ok_or(Error::ShortRead {
            expected: end,
            actual: prefix.bytes.len(),
        })?;
        buffer[..requested].copy_from_slice(source);
        Ok(requested)
    }
}

fn convert_fat_directory_entry(entry: fat::DirectoryEntry) -> DirectoryEntry {
    let kind = if entry.is_directory() {
        NodeKind::Directory
    } else {
        NodeKind::File
    };
    let hidden = entry.is_hidden();
    let system = entry.is_system();
    let read_only = entry.is_directory() || entry.is_read_only();
    DirectoryEntry {
        name: entry.name,
        kind,
        size: u64::from(entry.size),
        read_only,
        hidden,
        system,
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
        read_only: !volume.writable,
    };
    *root = Some(MountedRoot { info: info.clone() });
    Ok(info)
}

pub fn mount_tmpfs() -> Result<TmpfsInfo, Error> {
    let mut tmpfs = TMPFS.lock();
    if tmpfs.mounted {
        return Err(Error::AlreadyMounted);
    }
    tmpfs.mounted = true;
    tmpfs.info().ok_or(Error::NotMounted)
}

pub fn info() -> Option<MountInfo> {
    ROOT_MOUNT.lock().as_ref().map(|root| root.info.clone())
}

pub fn tmpfs_info() -> Option<TmpfsInfo> {
    TMPFS.lock().info()
}

pub fn metadata(path: &str) -> Result<Metadata, Error> {
    let path = normalize_path(path)?;
    match backend_for_path(&path)? {
        BackendKind::Fat => FatFileSystem.metadata(&path),
        BackendKind::Tmpfs => TMPFS.lock().metadata(&path),
    }
}

pub fn open(path: &str, options: OpenOptions) -> Result<Metadata, Error> {
    validate_open_options(options)?;
    let path = normalize_path(path)?;
    match backend_for_path(&path)? {
        BackendKind::Fat => {
            let metadata = if options.write || options.create || options.truncate || options.append
            {
                FatFileSystem.open(&path, options)?
            } else {
                FatFileSystem.metadata(&path)?
            };
            if metadata.is_directory() {
                return Err(Error::IsDirectory);
            }
            Ok(metadata)
        }
        BackendKind::Tmpfs => TMPFS.lock().open(&path, options),
    }
}

pub fn read_directory(path: &str) -> Result<Vec<DirectoryEntry>, Error> {
    let path = normalize_path(path)?;
    match backend_for_path(&path)? {
        BackendKind::Fat => {
            let mut entries = FatFileSystem.read_directory(&path)?;
            if path == "/"
                && TMPFS.lock().mounted
                && !entries.iter().any(|entry| entry.name == "tmp")
            {
                entries.push(DirectoryEntry {
                    name: String::from("tmp"),
                    kind: NodeKind::Directory,
                    size: 0,
                    read_only: false,
                    hidden: false,
                    system: false,
                });
            }
            Ok(entries)
        }
        BackendKind::Tmpfs => TMPFS.lock().read_directory(&path),
    }
}

pub fn read_at(path: &str, offset: u64, buffer: &mut [u8]) -> Result<usize, Error> {
    let path = normalize_path(path)?;
    match backend_for_path(&path)? {
        BackendKind::Fat => FatFileSystem.read_at(&path, offset, buffer),
        BackendKind::Tmpfs => TMPFS.lock().read_at(&path, offset, buffer),
    }
}

pub fn write_at(path: &str, offset: u64, bytes: &[u8]) -> Result<usize, Error> {
    let path = normalize_path(path)?;
    match backend_for_path(&path)? {
        BackendKind::Fat => FatFileSystem.write_at(&path, offset, bytes),
        BackendKind::Tmpfs => TMPFS.lock().write_at(&path, offset, bytes),
    }
}

pub fn append(path: &str, bytes: &[u8]) -> Result<(u64, usize), Error> {
    let path = normalize_path(path)?;
    match backend_for_path(&path)? {
        BackendKind::Fat => FatFileSystem.append(&path, bytes),
        BackendKind::Tmpfs => TMPFS.lock().append(&path, bytes),
    }
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
        truncated: (requested as u64) < metadata.size,
    })
}

pub fn join(parent: &str, child: &str) -> Result<String, Error> {
    if child.is_empty() || child.contains('/') || child.as_bytes().contains(&0) {
        return Err(Error::InvalidPath);
    }

    let parent = normalize_path(parent)?;
    let candidate = if parent == "/" {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
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

fn backend_for_path(path: &str) -> Result<BackendKind, Error> {
    if path == TMPFS_MOUNT_PATH || path.starts_with("/tmp/") {
        if TMPFS.lock().mounted {
            Ok(BackendKind::Tmpfs)
        } else {
            Err(Error::NotMounted)
        }
    } else if ROOT_MOUNT.lock().is_some() {
        Ok(BackendKind::Fat)
    } else {
        Err(Error::NotMounted)
    }
}

fn tmpfs_name(path: &str) -> Result<&str, Error> {
    let name = path.strip_prefix("/tmp/").ok_or(Error::InvalidPath)?;
    if name.is_empty() || name.contains('/') || name.as_bytes().contains(&0) {
        return Err(Error::InvalidPath);
    }
    if name.len() > TMPFS_MAX_NAME_BYTES {
        return Err(Error::NameTooLong);
    }
    Ok(name)
}

fn validate_open_options(options: OpenOptions) -> Result<(), Error> {
    if !options.read && !options.write {
        return Err(Error::InvalidOpenOptions);
    }
    if (options.create || options.truncate || options.append) && !options.write {
        return Err(Error::InvalidOpenOptions);
    }
    if options.truncate && options.append {
        return Err(Error::InvalidOpenOptions);
    }
    Ok(())
}
