use alloc::{vec, vec::Vec};
use core::fmt;

use crate::vfs;

pub use nullstar_executable::{Image, ImageType, LoadSegment, MAX_EXECUTABLE_FILE_BYTES};

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELF_HEADER_SIZE: usize = 64;

/// One validated executable and the immutable bytes from which it was parsed.
///
/// Address-space construction must use this byte buffer rather than reopening
/// the path, so validation and execution can never observe different content.
pub struct LoadedExecutable {
    image: Image,
    bytes: Vec<u8>,
}

impl LoadedExecutable {
    pub fn from_bytes(path: &str, bytes: Vec<u8>) -> Result<Self, Error> {
        let image = nullstar_executable::validate_bytes(path, &bytes).map_err(Error::Format)?;
        Ok(Self { image, bytes })
    }

    pub const fn image(&self) -> &Image {
        &self.image
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_image(self) -> Image {
        self.image
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Vfs(vfs::Error),
    Format(nullstar_executable::Error),
    NoElfCandidate,
    NotAFile,
}

impl Error {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::Vfs(_) => "virtual filesystem operation failed",
            Self::Format(error) => error.description(),
            Self::NoElfCandidate => "directory does not contain an ELF image",
            Self::NotAFile => "ELF path is not a regular file",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vfs(error) => write!(formatter, "VFS error: {error}"),
            Self::Format(error) => error.fmt(formatter),
            _ => formatter.write_str(self.description()),
        }
    }
}

impl From<vfs::Error> for Error {
    fn from(error: vfs::Error) -> Self {
        Self::Vfs(error)
    }
}

/// Materializes and validates a bootstrap-VFS executable within the global
/// executable-size bound.
pub fn load(path: &str) -> Result<LoadedExecutable, Error> {
    let metadata = vfs::metadata(path)?;
    if !metadata.is_file() {
        return Err(Error::NotAFile);
    }
    if metadata.size > MAX_EXECUTABLE_FILE_BYTES as u64 {
        return Err(Error::Format(nullstar_executable::Error::FileTooLarge(
            metadata.size,
        )));
    }

    let byte_count = usize::try_from(metadata.size)
        .map_err(|_| Error::Format(nullstar_executable::Error::FileTooLarge(metadata.size)))?;
    let normalized_path = vfs::normalize_path(path)?;
    let mut bytes = vec![0_u8; byte_count];
    vfs::read_exact_at(&normalized_path, 0, &mut bytes)?;
    LoadedExecutable::from_bytes(&normalized_path, bytes)
}

pub fn validate(path: &str) -> Result<Image, Error> {
    load(path).map(LoadedExecutable::into_image)
}

pub fn validate_first_in_directory(directory: &str) -> Result<Image, Error> {
    for entry in vfs::read_directory(directory)? {
        if !entry.is_file() || entry.size < ELF_HEADER_SIZE as u64 {
            continue;
        }

        let path = vfs::join(directory, &entry.name)?;
        let mut magic = [0_u8; 4];
        if vfs::read_at(&path, 0, &mut magic)? != magic.len() {
            continue;
        }
        if magic == *ELF_MAGIC {
            return validate(&path);
        }
    }

    Err(Error::NoElfCandidate)
}
