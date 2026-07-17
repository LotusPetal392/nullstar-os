use alloc::{string::String, vec::Vec};
use core::fmt;

use crate::vfs;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELF_HEADER_SIZE: usize = 64;
const ELF64_PROGRAM_HEADER_SIZE: usize = 56;
const MAX_PROGRAM_HEADERS: u16 = 128;
const MAX_LOAD_SEGMENTS: usize = 32;

const ELF_CLASS_64: u8 = 2;
const ELF_DATA_LITTLE_ENDIAN: u8 = 1;
const ELF_CURRENT_VERSION: u8 = 1;
const ELF_MACHINE_X86_64: u16 = 62;
const ELF_TYPE_EXECUTABLE: u16 = 2;
const ELF_TYPE_SHARED_OBJECT: u16 = 3;

const PROGRAM_TYPE_LOAD: u32 = 1;
const PROGRAM_TYPE_DYNAMIC: u32 = 2;
const PROGRAM_TYPE_INTERPRETER: u32 = 3;
const PROGRAM_TYPE_TLS: u32 = 7;
const PROGRAM_TYPE_GNU_STACK: u32 = 0x6474_e551;

const PROGRAM_FLAG_EXECUTE: u32 = 1;
const PROGRAM_FLAG_WRITE: u32 = 2;
const PROGRAM_FLAG_READ: u32 = 4;

const LOW_CANONICAL_MAX: u64 = 0x0000_7fff_ffff_ffff;
const HIGH_CANONICAL_MIN: u64 = 0xffff_8000_0000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    Executable,
    PositionIndependent,
}

impl fmt::Display for ImageType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Executable => formatter.write_str("executable"),
            Self::PositionIndependent => formatter.write_str("position-independent executable"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadSegment {
    pub program_header_index: u16,
    pub file_offset: u64,
    pub virtual_address: u64,
    pub file_size: u64,
    pub memory_size: u64,
    pub alignment: u64,
    pub readable: bool,
    pub writable: bool,
    pub executable: bool,
}

impl LoadSegment {
    pub fn end_virtual_address(&self) -> u64 {
        self.virtual_address.saturating_add(self.memory_size)
    }

    pub const fn permissions(&self) -> &'static str {
        match (self.readable, self.writable, self.executable) {
            (false, false, false) => "---",
            (false, false, true) => "--x",
            (false, true, false) => "-w-",
            (false, true, true) => "-wx",
            (true, false, false) => "r--",
            (true, false, true) => "r-x",
            (true, true, false) => "rw-",
            (true, true, true) => "rwx",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Image {
    pub path: String,
    pub image_type: ImageType,
    pub entry_point: u64,
    pub file_size: u64,
    pub program_header_count: u16,
    pub has_dynamic_segment: bool,
    pub has_tls_segment: bool,
    pub executable_stack_requested: bool,
    load_segments: Vec<LoadSegment>,
}

impl Image {
    pub fn load_segments(&self) -> &[LoadSegment] {
        &self.load_segments
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Vfs(vfs::Error),
    NoElfCandidate,
    NotAFile,
    FileTooSmall(u64),
    InvalidMagic,
    UnsupportedClass(u8),
    UnsupportedEncoding(u8),
    InvalidIdentificationVersion(u8),
    UnsupportedImageType(u16),
    UnsupportedMachine(u16),
    InvalidVersion(u32),
    InvalidHeaderSize(u16),
    MissingProgramHeaders,
    InvalidProgramHeaderSize(u16),
    TooManyProgramHeaders(u16),
    ProgramHeaderTableOutOfRange,
    ProgramHeaderTableOutsideReadWindow(u64),
    ShortRead { expected: usize, actual: usize },
    UnsupportedInterpreter,
    TooManyLoadSegments,
    NoLoadableSegments,
    SegmentFileLargerThanMemory(u16),
    SegmentFileOutOfRange(u16),
    SegmentAddressOverflow(u16),
    NonCanonicalSegment(u16),
    InvalidSegmentAlignment(u16),
    WritableExecutableSegment(u16),
    OverlappingSegments { first: u16, second: u16 },
    NonCanonicalEntryPoint(u64),
    EntryPointNotExecutable(u64),
}

impl Error {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::Vfs(_) => "virtual filesystem operation failed",
            Self::NoElfCandidate => "directory does not contain an ELF image",
            Self::NotAFile => "ELF path is not a regular file",
            Self::FileTooSmall(_) => "file is too small to contain an ELF64 header",
            Self::InvalidMagic => "file does not contain the ELF magic",
            Self::UnsupportedClass(_) => "ELF class is not 64-bit",
            Self::UnsupportedEncoding(_) => "ELF byte order is not little-endian",
            Self::InvalidIdentificationVersion(_) => "ELF identification version is invalid",
            Self::UnsupportedImageType(_) => "ELF image type is unsupported",
            Self::UnsupportedMachine(_) => "ELF machine is not x86-64",
            Self::InvalidVersion(_) => "ELF header version is invalid",
            Self::InvalidHeaderSize(_) => "ELF64 header size is invalid",
            Self::MissingProgramHeaders => "ELF image does not contain program headers",
            Self::InvalidProgramHeaderSize(_) => "ELF64 program-header size is invalid",
            Self::TooManyProgramHeaders(_) => {
                "ELF program-header count exceeds the configured bound"
            }
            Self::ProgramHeaderTableOutOfRange => "ELF program-header table is outside the file",
            Self::ProgramHeaderTableOutsideReadWindow(_) => {
                "ELF program-header table is outside the bounded VFS read window"
            }
            Self::ShortRead { .. } => "ELF parser received a short file read",
            Self::UnsupportedInterpreter => "dynamically interpreted ELF images are unsupported",
            Self::TooManyLoadSegments => "ELF load-segment count exceeds the configured bound",
            Self::NoLoadableSegments => "ELF image does not contain a loadable segment",
            Self::SegmentFileLargerThanMemory(_) => {
                "ELF segment file size exceeds its in-memory size"
            }
            Self::SegmentFileOutOfRange(_) => "ELF segment references bytes outside the file",
            Self::SegmentAddressOverflow(_) => "ELF segment virtual-address range overflowed",
            Self::NonCanonicalSegment(_) => {
                "ELF segment uses a non-canonical virtual-address range"
            }
            Self::InvalidSegmentAlignment(_) => "ELF segment alignment is invalid",
            Self::WritableExecutableSegment(_) => {
                "ELF segment requests write and execute permissions"
            }
            Self::OverlappingSegments { .. } => "ELF loadable segments overlap",
            Self::NonCanonicalEntryPoint(_) => "ELF entry point is not a canonical x86-64 address",
            Self::EntryPointNotExecutable(_) => {
                "ELF entry point is not inside an executable load segment"
            }
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vfs(error) => write!(formatter, "VFS error: {error}"),
            Self::FileTooSmall(size) => write!(
                formatter,
                "ELF file is {size} bytes; at least {ELF_HEADER_SIZE} are required"
            ),
            Self::UnsupportedClass(value) => write!(formatter, "unsupported ELF class {value}"),
            Self::UnsupportedEncoding(value) => {
                write!(formatter, "unsupported ELF data encoding {value}")
            }
            Self::InvalidIdentificationVersion(value) => {
                write!(formatter, "invalid ELF identification version {value}")
            }
            Self::UnsupportedImageType(value) => {
                write!(formatter, "unsupported ELF image type {value:#06x}")
            }
            Self::UnsupportedMachine(value) => {
                write!(formatter, "unsupported ELF machine {value:#06x}")
            }
            Self::InvalidVersion(value) => write!(formatter, "invalid ELF version {value}"),
            Self::InvalidHeaderSize(value) => {
                write!(formatter, "invalid ELF64 header size {value}")
            }
            Self::InvalidProgramHeaderSize(value) => {
                write!(formatter, "invalid ELF64 program-header size {value}")
            }
            Self::TooManyProgramHeaders(value) => write!(
                formatter,
                "ELF declares {value} program headers; maximum is {MAX_PROGRAM_HEADERS}"
            ),
            Self::ProgramHeaderTableOutsideReadWindow(end) => write!(
                formatter,
                "ELF program-header table ends at byte {end}, beyond the {}-byte VFS read window",
                vfs::MAX_READ_WINDOW_BYTES
            ),
            Self::ShortRead { expected, actual } => {
                write!(
                    formatter,
                    "short ELF read: expected {expected}, received {actual}"
                )
            }
            Self::SegmentFileLargerThanMemory(index) => {
                write!(formatter, "ELF LOAD segment {index} has p_filesz > p_memsz")
            }
            Self::SegmentFileOutOfRange(index) => {
                write!(
                    formatter,
                    "ELF LOAD segment {index} extends beyond the file"
                )
            }
            Self::SegmentAddressOverflow(index) => {
                write!(
                    formatter,
                    "ELF LOAD segment {index} address range overflowed"
                )
            }
            Self::NonCanonicalSegment(index) => {
                write!(
                    formatter,
                    "ELF LOAD segment {index} is outside canonical address space"
                )
            }
            Self::InvalidSegmentAlignment(index) => {
                write!(formatter, "ELF LOAD segment {index} has invalid alignment")
            }
            Self::WritableExecutableSegment(index) => {
                write!(
                    formatter,
                    "ELF LOAD segment {index} requests writable executable memory"
                )
            }
            Self::OverlappingSegments { first, second } => {
                write!(formatter, "ELF LOAD segments {first} and {second} overlap")
            }
            Self::NonCanonicalEntryPoint(entry) => {
                write!(formatter, "ELF entry point {entry:#018x} is non-canonical")
            }
            Self::EntryPointNotExecutable(entry) => write!(
                formatter,
                "ELF entry point {entry:#018x} is outside executable LOAD segments"
            ),
            _ => formatter.write_str(self.description()),
        }
    }
}

impl From<vfs::Error> for Error {
    fn from(error: vfs::Error) -> Self {
        Self::Vfs(error)
    }
}

pub fn validate(path: &str) -> Result<Image, Error> {
    let metadata = vfs::metadata(path)?;
    if !metadata.is_file() {
        return Err(Error::NotAFile);
    }
    if metadata.size < ELF_HEADER_SIZE as u64 {
        return Err(Error::FileTooSmall(metadata.size));
    }

    let mut header = [0_u8; ELF_HEADER_SIZE];
    read_exact(path, 0, &mut header)?;
    if header.get(..4) != Some(ELF_MAGIC) {
        return Err(Error::InvalidMagic);
    }
    if header[4] != ELF_CLASS_64 {
        return Err(Error::UnsupportedClass(header[4]));
    }
    if header[5] != ELF_DATA_LITTLE_ENDIAN {
        return Err(Error::UnsupportedEncoding(header[5]));
    }
    if header[6] != ELF_CURRENT_VERSION {
        return Err(Error::InvalidIdentificationVersion(header[6]));
    }

    let image_type = match read_u16(&header, 16)? {
        ELF_TYPE_EXECUTABLE => ImageType::Executable,
        ELF_TYPE_SHARED_OBJECT => ImageType::PositionIndependent,
        other => return Err(Error::UnsupportedImageType(other)),
    };
    let machine = read_u16(&header, 18)?;
    if machine != ELF_MACHINE_X86_64 {
        return Err(Error::UnsupportedMachine(machine));
    }
    let version = read_u32(&header, 20)?;
    if version != u32::from(ELF_CURRENT_VERSION) {
        return Err(Error::InvalidVersion(version));
    }

    let entry_point = read_u64(&header, 24)?;
    if !is_canonical_address(entry_point) {
        return Err(Error::NonCanonicalEntryPoint(entry_point));
    }

    let program_header_offset = read_u64(&header, 32)?;
    let header_size = read_u16(&header, 52)?;
    if usize::from(header_size) != ELF_HEADER_SIZE {
        return Err(Error::InvalidHeaderSize(header_size));
    }

    let program_header_size = read_u16(&header, 54)?;
    if usize::from(program_header_size) != ELF64_PROGRAM_HEADER_SIZE {
        return Err(Error::InvalidProgramHeaderSize(program_header_size));
    }
    let program_header_count = read_u16(&header, 56)?;
    if program_header_count == 0 {
        return Err(Error::MissingProgramHeaders);
    }
    if program_header_count > MAX_PROGRAM_HEADERS {
        return Err(Error::TooManyProgramHeaders(program_header_count));
    }

    let table_bytes = u64::from(program_header_size)
        .checked_mul(u64::from(program_header_count))
        .ok_or(Error::ProgramHeaderTableOutOfRange)?;
    let table_end = program_header_offset
        .checked_add(table_bytes)
        .ok_or(Error::ProgramHeaderTableOutOfRange)?;
    if program_header_offset < u64::from(header_size) || table_end > metadata.size {
        return Err(Error::ProgramHeaderTableOutOfRange);
    }
    if table_end > vfs::MAX_READ_WINDOW_BYTES as u64 {
        return Err(Error::ProgramHeaderTableOutsideReadWindow(table_end));
    }

    let mut load_segments = Vec::new();
    let mut has_dynamic_segment = false;
    let mut has_tls_segment = false;
    let mut executable_stack_requested = false;

    for index in 0..program_header_count {
        let offset = program_header_offset
            .checked_add(
                u64::from(index)
                    .checked_mul(u64::from(program_header_size))
                    .ok_or(Error::ProgramHeaderTableOutOfRange)?,
            )
            .ok_or(Error::ProgramHeaderTableOutOfRange)?;
        let mut program_header = [0_u8; ELF64_PROGRAM_HEADER_SIZE];
        read_exact(path, offset, &mut program_header)?;

        let program_type = read_u32(&program_header, 0)?;
        let flags = read_u32(&program_header, 4)?;
        match program_type {
            PROGRAM_TYPE_LOAD => {
                if load_segments.len() >= MAX_LOAD_SEGMENTS {
                    return Err(Error::TooManyLoadSegments);
                }
                let segment = parse_load_segment(
                    index,
                    flags,
                    &program_header,
                    metadata.size,
                    &load_segments,
                )?;
                if segment.memory_size != 0 {
                    load_segments.push(segment);
                }
            }
            PROGRAM_TYPE_DYNAMIC => has_dynamic_segment = true,
            PROGRAM_TYPE_INTERPRETER => return Err(Error::UnsupportedInterpreter),
            PROGRAM_TYPE_TLS => has_tls_segment = true,
            PROGRAM_TYPE_GNU_STACK => {
                executable_stack_requested = flags & PROGRAM_FLAG_EXECUTE != 0;
            }
            _ => {}
        }
    }

    if load_segments.is_empty() {
        return Err(Error::NoLoadableSegments);
    }
    load_segments.sort_unstable_by_key(|segment| segment.virtual_address);

    let entry_is_executable = load_segments.iter().any(|segment| {
        segment.executable
            && entry_point >= segment.virtual_address
            && entry_point < segment.end_virtual_address()
    });
    if !entry_is_executable {
        return Err(Error::EntryPointNotExecutable(entry_point));
    }

    Ok(Image {
        path: vfs::normalize_path(path)?,
        image_type,
        entry_point,
        file_size: metadata.size,
        program_header_count,
        has_dynamic_segment,
        has_tls_segment,
        executable_stack_requested,
        load_segments,
    })
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

fn parse_load_segment(
    index: u16,
    flags: u32,
    program_header: &[u8; ELF64_PROGRAM_HEADER_SIZE],
    file_size: u64,
    existing: &[LoadSegment],
) -> Result<LoadSegment, Error> {
    let file_offset = read_u64(program_header, 8)?;
    let virtual_address = read_u64(program_header, 16)?;
    let segment_file_size = read_u64(program_header, 32)?;
    let memory_size = read_u64(program_header, 40)?;
    let alignment = read_u64(program_header, 48)?;

    if segment_file_size > memory_size {
        return Err(Error::SegmentFileLargerThanMemory(index));
    }
    let file_end = file_offset
        .checked_add(segment_file_size)
        .ok_or(Error::SegmentFileOutOfRange(index))?;
    if file_end > file_size {
        return Err(Error::SegmentFileOutOfRange(index));
    }

    if alignment > 1
        && (!alignment.is_power_of_two() || virtual_address % alignment != file_offset % alignment)
    {
        return Err(Error::InvalidSegmentAlignment(index));
    }

    let virtual_end = virtual_address
        .checked_add(memory_size)
        .ok_or(Error::SegmentAddressOverflow(index))?;
    if memory_size != 0 && !is_canonical_range(virtual_address, virtual_end - 1) {
        return Err(Error::NonCanonicalSegment(index));
    }

    let readable = flags & PROGRAM_FLAG_READ != 0;
    let writable = flags & PROGRAM_FLAG_WRITE != 0;
    let executable = flags & PROGRAM_FLAG_EXECUTE != 0;
    if writable && executable {
        return Err(Error::WritableExecutableSegment(index));
    }

    if memory_size != 0 {
        for other in existing {
            let other_end = other.end_virtual_address();
            if virtual_address < other_end && other.virtual_address < virtual_end {
                return Err(Error::OverlappingSegments {
                    first: other.program_header_index,
                    second: index,
                });
            }
        }
    }

    Ok(LoadSegment {
        program_header_index: index,
        file_offset,
        virtual_address,
        file_size: segment_file_size,
        memory_size,
        alignment,
        readable,
        writable,
        executable,
    })
}

fn read_exact(path: &str, offset: u64, buffer: &mut [u8]) -> Result<(), Error> {
    let actual = vfs::read_at(path, offset, buffer)?;
    if actual != buffer.len() {
        return Err(Error::ShortRead {
            expected: buffer.len(),
            actual,
        });
    }
    Ok(())
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let value: [u8; 2] = bytes
        .get(offset..offset.saturating_add(2))
        .and_then(|value| value.try_into().ok())
        .ok_or(Error::ProgramHeaderTableOutOfRange)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let value: [u8; 4] = bytes
        .get(offset..offset.saturating_add(4))
        .and_then(|value| value.try_into().ok())
        .ok_or(Error::ProgramHeaderTableOutOfRange)?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let value: [u8; 8] = bytes
        .get(offset..offset.saturating_add(8))
        .and_then(|value| value.try_into().ok())
        .ok_or(Error::ProgramHeaderTableOutOfRange)?;
    Ok(u64::from_le_bytes(value))
}

fn is_canonical_address(address: u64) -> bool {
    address <= LOW_CANONICAL_MAX || address >= HIGH_CANONICAL_MIN
}

fn is_canonical_range(start: u64, end_inclusive: u64) -> bool {
    (start <= LOW_CANONICAL_MAX && end_inclusive <= LOW_CANONICAL_MAX)
        || (start >= HIGH_CANONICAL_MIN && end_inclusive >= HIGH_CANONICAL_MIN)
}
