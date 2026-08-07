#![no_std]

//! Bounded, allocation-backed validation of ELF64 x86-64 executable images.

extern crate alloc;

use alloc::{string::String, vec::Vec};
use core::fmt;

/// Maximum accepted executable file size.
pub const MAX_EXECUTABLE_FILE_BYTES: usize = 1024 * 1024;
/// Maximum number of bytes covered by unique mapped 4 KiB pages.
pub const MAX_EXECUTABLE_MAPPED_BYTES: u64 = 16 * 1024 * 1024;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const ELF_HEADER_SIZE: usize = 64;
const ELF64_PROGRAM_HEADER_SIZE: usize = 56;
const MAX_PROGRAM_HEADERS: u16 = 128;
const MAX_LOAD_SEGMENTS: usize = 32;
const PAGE_SIZE: u64 = 4096;

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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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
    FileTooSmall(u64),
    FileTooLarge(u64),
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
    MappedMemoryTooLarge(u64),
    NonCanonicalEntryPoint(u64),
    EntryPointNotExecutable(u64),
}

impl Error {
    pub const fn description(&self) -> &'static str {
        match self {
            Self::FileTooSmall(_) => "file is too small to contain an ELF64 header",
            Self::FileTooLarge(_) => "ELF file exceeds the configured size bound",
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
            Self::OverlappingSegments { .. } => {
                "ELF loadable segments overlap in virtual address pages"
            }
            Self::MappedMemoryTooLarge(_) => {
                "ELF mapped-page footprint exceeds the configured bound"
            }
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
            Self::FileTooSmall(size) => write!(
                formatter,
                "ELF file is {size} bytes; at least {ELF_HEADER_SIZE} are required"
            ),
            Self::FileTooLarge(size) => write!(
                formatter,
                "ELF file is {size} bytes; maximum is {MAX_EXECUTABLE_FILE_BYTES}"
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
            Self::NonCanonicalSegment(index) => write!(
                formatter,
                "ELF LOAD segment {index} is outside canonical address space"
            ),
            Self::InvalidSegmentAlignment(index) => {
                write!(formatter, "ELF LOAD segment {index} has invalid alignment")
            }
            Self::WritableExecutableSegment(index) => write!(
                formatter,
                "ELF LOAD segment {index} requests writable executable memory"
            ),
            Self::OverlappingSegments { first, second } => write!(
                formatter,
                "ELF LOAD segments {first} and {second} overlap in mapped pages"
            ),
            Self::MappedMemoryTooLarge(bytes) => write!(
                formatter,
                "ELF LOAD segments map {bytes} bytes; maximum is {MAX_EXECUTABLE_MAPPED_BYTES}"
            ),
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

impl core::error::Error for Error {}

/// Validates an in-memory ELF64 x86-64 executable image.
///
/// The returned image owns `path` and its load-segment list. All integer fields
/// are decoded explicitly as little-endian values.
pub fn validate_bytes(path: &str, bytes: &[u8]) -> Result<Image, Error> {
    let file_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if bytes.len() > MAX_EXECUTABLE_FILE_BYTES {
        return Err(Error::FileTooLarge(file_size));
    }
    if bytes.len() < ELF_HEADER_SIZE {
        return Err(Error::FileTooSmall(file_size));
    }

    if bytes.get(0..4) != Some(&ELF_MAGIC[..]) {
        return Err(Error::InvalidMagic);
    }
    if bytes[4] != ELF_CLASS_64 {
        return Err(Error::UnsupportedClass(bytes[4]));
    }
    if bytes[5] != ELF_DATA_LITTLE_ENDIAN {
        return Err(Error::UnsupportedEncoding(bytes[5]));
    }
    if bytes[6] != ELF_CURRENT_VERSION {
        return Err(Error::InvalidIdentificationVersion(bytes[6]));
    }

    let image_type = match read_u16(bytes, 16)? {
        ELF_TYPE_EXECUTABLE => ImageType::Executable,
        ELF_TYPE_SHARED_OBJECT => ImageType::PositionIndependent,
        other => return Err(Error::UnsupportedImageType(other)),
    };
    let machine = read_u16(bytes, 18)?;
    if machine != ELF_MACHINE_X86_64 {
        return Err(Error::UnsupportedMachine(machine));
    }
    let version = read_u32(bytes, 20)?;
    if version != u32::from(ELF_CURRENT_VERSION) {
        return Err(Error::InvalidVersion(version));
    }

    let entry_point = read_u64(bytes, 24)?;
    if !is_canonical_address(entry_point) {
        return Err(Error::NonCanonicalEntryPoint(entry_point));
    }

    let program_header_offset = read_u64(bytes, 32)?;
    let header_size = read_u16(bytes, 52)?;
    if usize::from(header_size) != ELF_HEADER_SIZE {
        return Err(Error::InvalidHeaderSize(header_size));
    }

    let program_header_size = read_u16(bytes, 54)?;
    if usize::from(program_header_size) != ELF64_PROGRAM_HEADER_SIZE {
        return Err(Error::InvalidProgramHeaderSize(program_header_size));
    }
    let program_header_count = read_u16(bytes, 56)?;
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
    if program_header_offset < u64::from(header_size) || table_end > file_size {
        return Err(Error::ProgramHeaderTableOutOfRange);
    }

    let mut load_segments = Vec::new();
    let mut mapped_bytes = 0_u64;
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
        let offset = usize::try_from(offset).map_err(|_| Error::ProgramHeaderTableOutOfRange)?;
        let end = offset
            .checked_add(ELF64_PROGRAM_HEADER_SIZE)
            .ok_or(Error::ProgramHeaderTableOutOfRange)?;
        let program_header = bytes
            .get(offset..end)
            .ok_or(Error::ProgramHeaderTableOutOfRange)?;

        let program_type = read_u32(program_header, 0)?;
        let flags = read_u32(program_header, 4)?;
        match program_type {
            PROGRAM_TYPE_LOAD => {
                let segment = parse_load_segment(index, flags, program_header, file_size)?;
                if segment.memory_size == 0 {
                    continue;
                }
                if load_segments.len() >= MAX_LOAD_SEGMENTS {
                    return Err(Error::TooManyLoadSegments);
                }

                let (first_page, last_page, segment_mapped_bytes) = mapped_page_span(&segment)?;
                for other in &load_segments {
                    let (other_first_page, other_last_page, _) = mapped_page_span(other)?;
                    if first_page <= other_last_page && other_first_page <= last_page {
                        return Err(Error::OverlappingSegments {
                            first: other.program_header_index,
                            second: index,
                        });
                    }
                }

                mapped_bytes = mapped_bytes
                    .checked_add(segment_mapped_bytes)
                    .ok_or(Error::MappedMemoryTooLarge(u64::MAX))?;
                if mapped_bytes > MAX_EXECUTABLE_MAPPED_BYTES {
                    return Err(Error::MappedMemoryTooLarge(mapped_bytes));
                }
                load_segments.push(segment);
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
        path: String::from(path),
        image_type,
        entry_point,
        file_size,
        program_header_count,
        has_dynamic_segment,
        has_tls_segment,
        executable_stack_requested,
        load_segments,
    })
}

fn parse_load_segment(
    index: u16,
    flags: u32,
    program_header: &[u8],
    file_size: u64,
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

fn mapped_page_span(segment: &LoadSegment) -> Result<(u64, u64, u64), Error> {
    let virtual_end = segment
        .virtual_address
        .checked_add(segment.memory_size)
        .ok_or(Error::SegmentAddressOverflow(segment.program_header_index))?;
    let first_page = segment.virtual_address / PAGE_SIZE;
    let last_page = (virtual_end - 1) / PAGE_SIZE;
    let page_count = last_page
        .checked_sub(first_page)
        .and_then(|difference| difference.checked_add(1))
        .ok_or(Error::SegmentAddressOverflow(segment.program_header_index))?;
    let bytes = page_count
        .checked_mul(PAGE_SIZE)
        .ok_or(Error::MappedMemoryTooLarge(u64::MAX))?;
    Ok((first_page, last_page, bytes))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let value: [u8; 2] = read_array(bytes, offset)?;
    Ok(u16::from_le_bytes(value))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let value: [u8; 4] = read_array(bytes, offset)?;
    Ok(u32::from_le_bytes(value))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let value: [u8; 8] = read_array(bytes, offset)?;
    Ok(u64::from_le_bytes(value))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], Error> {
    let end = offset
        .checked_add(N)
        .ok_or(Error::ProgramHeaderTableOutOfRange)?;
    bytes
        .get(offset..end)
        .and_then(|value| value.try_into().ok())
        .ok_or(Error::ProgramHeaderTableOutOfRange)
}

fn is_canonical_address(address: u64) -> bool {
    address <= LOW_CANONICAL_MAX || address >= HIGH_CANONICAL_MIN
}

fn is_canonical_range(start: u64, end_inclusive: u64) -> bool {
    (start <= LOW_CANONICAL_MAX && end_inclusive <= LOW_CANONICAL_MAX)
        || (start >= HIGH_CANONICAL_MIN && end_inclusive >= HIGH_CANONICAL_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{vec, vec::Vec};

    const BASE: u64 = 0x0040_0000;

    #[derive(Clone, Copy)]
    struct Program {
        program_type: u32,
        flags: u32,
        file_offset: u64,
        virtual_address: u64,
        file_size: u64,
        memory_size: u64,
        alignment: u64,
    }

    fn load() -> Program {
        Program {
            program_type: PROGRAM_TYPE_LOAD,
            flags: PROGRAM_FLAG_READ | PROGRAM_FLAG_EXECUTE,
            file_offset: 0,
            virtual_address: BASE,
            file_size: 512,
            memory_size: PAGE_SIZE,
            alignment: PAGE_SIZE,
        }
    }

    fn build(entry: u64, image_type: u16, programs: &[Program]) -> Vec<u8> {
        let table_end = ELF_HEADER_SIZE + programs.len() * ELF64_PROGRAM_HEADER_SIZE;
        let mut bytes = vec![0_u8; 512_usize.max(table_end)];
        bytes[0..4].copy_from_slice(ELF_MAGIC);
        bytes[4] = ELF_CLASS_64;
        bytes[5] = ELF_DATA_LITTLE_ENDIAN;
        bytes[6] = ELF_CURRENT_VERSION;
        put_u16(&mut bytes, 16, image_type);
        put_u16(&mut bytes, 18, ELF_MACHINE_X86_64);
        put_u32(&mut bytes, 20, u32::from(ELF_CURRENT_VERSION));
        put_u64(&mut bytes, 24, entry);
        put_u64(&mut bytes, 32, ELF_HEADER_SIZE as u64);
        put_u16(&mut bytes, 52, ELF_HEADER_SIZE as u16);
        put_u16(&mut bytes, 54, ELF64_PROGRAM_HEADER_SIZE as u16);
        put_u16(&mut bytes, 56, programs.len() as u16);

        for (index, program) in programs.iter().enumerate() {
            let offset = ELF_HEADER_SIZE + index * ELF64_PROGRAM_HEADER_SIZE;
            put_u32(&mut bytes, offset, program.program_type);
            put_u32(&mut bytes, offset + 4, program.flags);
            put_u64(&mut bytes, offset + 8, program.file_offset);
            put_u64(&mut bytes, offset + 16, program.virtual_address);
            put_u64(&mut bytes, offset + 32, program.file_size);
            put_u64(&mut bytes, offset + 40, program.memory_size);
            put_u64(&mut bytes, offset + 48, program.alignment);
        }
        bytes
    }

    fn valid_bytes() -> Vec<u8> {
        build(BASE + 0x100, ELF_TYPE_EXECUTABLE, &[load()])
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn accepts_valid_executable_and_owns_metadata() {
        let bytes = valid_bytes();
        let image = validate_bytes("/bin/init", &bytes).unwrap();
        assert_eq!(image.path, "/bin/init");
        assert_eq!(image.image_type, ImageType::Executable);
        assert_eq!(image.entry_point, BASE + 0x100);
        assert_eq!(image.file_size, 512);
        assert_eq!(image.program_header_count, 1);
        assert_eq!(image.load_segments().len(), 1);
        assert_eq!(image.load_segments()[0].permissions(), "r-x");
    }

    #[test]
    fn accepts_position_independent_image_and_metadata_segments() {
        let programs = [
            load(),
            Program {
                program_type: PROGRAM_TYPE_DYNAMIC,
                ..load()
            },
            Program {
                program_type: PROGRAM_TYPE_TLS,
                ..load()
            },
            Program {
                program_type: PROGRAM_TYPE_GNU_STACK,
                flags: PROGRAM_FLAG_EXECUTE,
                ..load()
            },
        ];
        let image = validate_bytes("pie", &build(BASE, ELF_TYPE_SHARED_OBJECT, &programs)).unwrap();
        assert_eq!(image.image_type, ImageType::PositionIndependent);
        assert!(image.has_dynamic_segment);
        assert!(image.has_tls_segment);
        assert!(image.executable_stack_requested);
    }

    #[test]
    fn rejects_too_small_and_too_large_files() {
        assert_eq!(validate_bytes("small", &[]), Err(Error::FileTooSmall(0)));
        let bytes = vec![0_u8; MAX_EXECUTABLE_FILE_BYTES + 1];
        assert_eq!(
            validate_bytes("large", &bytes),
            Err(Error::FileTooLarge((MAX_EXECUTABLE_FILE_BYTES + 1) as u64))
        );
    }

    #[test]
    fn rejects_invalid_identification_and_header_fields() {
        let mut bytes = valid_bytes();
        bytes[0] = 0;
        assert_eq!(validate_bytes("bad", &bytes), Err(Error::InvalidMagic));

        let mut bytes = valid_bytes();
        bytes[4] = 1;
        assert_eq!(
            validate_bytes("bad", &bytes),
            Err(Error::UnsupportedClass(1))
        );

        let mut bytes = valid_bytes();
        bytes[5] = 2;
        assert_eq!(
            validate_bytes("bad", &bytes),
            Err(Error::UnsupportedEncoding(2))
        );

        let mut bytes = valid_bytes();
        bytes[6] = 0;
        assert_eq!(
            validate_bytes("bad", &bytes),
            Err(Error::InvalidIdentificationVersion(0))
        );

        let mut bytes = valid_bytes();
        put_u16(&mut bytes, 18, 3);
        assert_eq!(
            validate_bytes("bad", &bytes),
            Err(Error::UnsupportedMachine(3))
        );

        let mut bytes = valid_bytes();
        put_u16(&mut bytes, 52, 63);
        assert_eq!(
            validate_bytes("bad", &bytes),
            Err(Error::InvalidHeaderSize(63))
        );

        let mut bytes = valid_bytes();
        put_u16(&mut bytes, 54, 55);
        assert_eq!(
            validate_bytes("bad", &bytes),
            Err(Error::InvalidProgramHeaderSize(55))
        );
    }

    #[test]
    fn rejects_missing_excessive_and_out_of_range_program_header_tables() {
        let mut bytes = valid_bytes();
        put_u16(&mut bytes, 56, 0);
        assert_eq!(
            validate_bytes("missing", &bytes),
            Err(Error::MissingProgramHeaders)
        );

        let mut bytes = valid_bytes();
        put_u16(&mut bytes, 56, MAX_PROGRAM_HEADERS + 1);
        assert_eq!(
            validate_bytes("many", &bytes),
            Err(Error::TooManyProgramHeaders(MAX_PROGRAM_HEADERS + 1))
        );

        let mut bytes = valid_bytes();
        put_u64(&mut bytes, 32, 500);
        assert_eq!(
            validate_bytes("range", &bytes),
            Err(Error::ProgramHeaderTableOutOfRange)
        );
    }

    #[test]
    fn rejects_interpreter_and_absence_of_nonempty_load_segments() {
        let interpreter = Program {
            program_type: PROGRAM_TYPE_INTERPRETER,
            ..load()
        };
        assert_eq!(
            validate_bytes("interp", &build(BASE, ELF_TYPE_EXECUTABLE, &[interpreter])),
            Err(Error::UnsupportedInterpreter)
        );

        let empty = Program {
            file_size: 0,
            memory_size: 0,
            ..load()
        };
        assert_eq!(
            validate_bytes("empty", &build(BASE, ELF_TYPE_EXECUTABLE, &[empty])),
            Err(Error::NoLoadableSegments)
        );
    }

    #[test]
    fn rejects_segment_file_errors() {
        let larger = Program {
            file_size: 2,
            memory_size: 1,
            ..load()
        };
        assert_eq!(
            validate_bytes("larger", &build(BASE, ELF_TYPE_EXECUTABLE, &[larger])),
            Err(Error::SegmentFileLargerThanMemory(0))
        );

        let out_of_range = Program {
            file_offset: 500,
            file_size: 13,
            memory_size: 13,
            alignment: 1,
            ..load()
        };
        assert_eq!(
            validate_bytes(
                "out-of-range",
                &build(BASE, ELF_TYPE_EXECUTABLE, &[out_of_range])
            ),
            Err(Error::SegmentFileOutOfRange(0))
        );
    }

    #[test]
    fn rejects_invalid_alignment_and_writable_executable_segment() {
        let invalid_alignment = Program {
            alignment: 3,
            ..load()
        };
        assert_eq!(
            validate_bytes(
                "alignment",
                &build(BASE, ELF_TYPE_EXECUTABLE, &[invalid_alignment])
            ),
            Err(Error::InvalidSegmentAlignment(0))
        );

        let writable_executable = Program {
            flags: PROGRAM_FLAG_READ | PROGRAM_FLAG_WRITE | PROGRAM_FLAG_EXECUTE,
            ..load()
        };
        assert_eq!(
            validate_bytes(
                "wx",
                &build(BASE, ELF_TYPE_EXECUTABLE, &[writable_executable])
            ),
            Err(Error::WritableExecutableSegment(0))
        );
    }

    #[test]
    fn rejects_noncanonical_and_overflowing_addresses() {
        let noncanonical = Program {
            virtual_address: LOW_CANONICAL_MAX,
            file_offset: 0,
            file_size: 0,
            memory_size: 2,
            alignment: 1,
            ..load()
        };
        assert_eq!(
            validate_bytes(
                "noncanonical",
                &build(0, ELF_TYPE_EXECUTABLE, &[noncanonical])
            ),
            Err(Error::NonCanonicalSegment(0))
        );

        let overflow = Program {
            virtual_address: u64::MAX,
            file_offset: 0,
            file_size: 0,
            memory_size: 2,
            alignment: 1,
            ..load()
        };
        assert_eq!(
            validate_bytes("overflow", &build(0, ELF_TYPE_EXECUTABLE, &[overflow])),
            Err(Error::SegmentAddressOverflow(0))
        );

        let noncanonical_entry = LOW_CANONICAL_MAX + 1;
        assert_eq!(
            validate_bytes(
                "entry",
                &build(noncanonical_entry, ELF_TYPE_EXECUTABLE, &[load()])
            ),
            Err(Error::NonCanonicalEntryPoint(noncanonical_entry))
        );
    }

    #[test]
    fn rejects_mapped_size_over_limit_including_bss() {
        let bss_heavy = Program {
            file_size: 512,
            memory_size: MAX_EXECUTABLE_MAPPED_BYTES + PAGE_SIZE,
            ..load()
        };
        assert_eq!(
            validate_bytes("bss", &build(BASE, ELF_TYPE_EXECUTABLE, &[bss_heavy])),
            Err(Error::MappedMemoryTooLarge(
                MAX_EXECUTABLE_MAPPED_BYTES + PAGE_SIZE
            ))
        );
    }

    #[test]
    fn accepts_mapped_size_at_limit() {
        let at_limit = Program {
            memory_size: MAX_EXECUTABLE_MAPPED_BYTES,
            ..load()
        };
        assert!(validate_bytes("limit", &build(BASE, ELF_TYPE_EXECUTABLE, &[at_limit])).is_ok());
    }

    #[test]
    fn rejects_nonoverlapping_bytes_that_share_a_mapped_page() {
        let first = Program {
            file_size: 0,
            memory_size: 0x100,
            alignment: 1,
            ..load()
        };
        let second = Program {
            flags: PROGRAM_FLAG_READ,
            file_size: 0,
            memory_size: 0x100,
            virtual_address: BASE + 0x200,
            alignment: 1,
            ..load()
        };
        assert_eq!(
            validate_bytes(
                "page-overlap",
                &build(BASE, ELF_TYPE_EXECUTABLE, &[first, second])
            ),
            Err(Error::OverlappingSegments {
                first: 0,
                second: 1
            })
        );
    }

    #[test]
    fn reports_page_overlap_in_program_header_order() {
        let first = Program {
            file_size: 0,
            memory_size: PAGE_SIZE,
            virtual_address: BASE + PAGE_SIZE,
            alignment: PAGE_SIZE,
            ..load()
        };
        let second = Program {
            file_size: 0,
            memory_size: PAGE_SIZE,
            virtual_address: BASE,
            alignment: PAGE_SIZE,
            ..load()
        };
        let third = Program {
            file_size: 0,
            memory_size: PAGE_SIZE,
            virtual_address: BASE + PAGE_SIZE,
            alignment: PAGE_SIZE,
            ..load()
        };
        assert_eq!(
            validate_bytes(
                "ordered",
                &build(BASE, ELF_TYPE_EXECUTABLE, &[first, second, third])
            ),
            Err(Error::OverlappingSegments {
                first: 0,
                second: 2
            })
        );
    }

    #[test]
    fn rejects_more_than_32_nonempty_load_segments() {
        let mut programs = Vec::new();
        for index in 0..=MAX_LOAD_SEGMENTS {
            programs.push(Program {
                flags: if index == 0 {
                    PROGRAM_FLAG_READ | PROGRAM_FLAG_EXECUTE
                } else {
                    PROGRAM_FLAG_READ
                },
                file_size: 0,
                memory_size: 1,
                virtual_address: BASE + index as u64 * PAGE_SIZE,
                alignment: PAGE_SIZE,
                ..load()
            });
        }
        assert_eq!(
            validate_bytes("many-loads", &build(BASE, ELF_TYPE_EXECUTABLE, &programs)),
            Err(Error::TooManyLoadSegments)
        );
    }

    #[test]
    fn empty_load_segments_do_not_count_toward_limit() {
        let mut programs = Vec::new();
        for _ in 0..MAX_LOAD_SEGMENTS {
            programs.push(Program {
                file_size: 0,
                memory_size: 0,
                ..load()
            });
        }
        programs.push(load());
        assert!(
            validate_bytes("empty-loads", &build(BASE, ELF_TYPE_EXECUTABLE, &programs)).is_ok()
        );
    }

    #[test]
    fn rejects_entry_outside_executable_segment() {
        assert_eq!(
            validate_bytes(
                "entry",
                &build(BASE + PAGE_SIZE, ELF_TYPE_EXECUTABLE, &[load()])
            ),
            Err(Error::EntryPointNotExecutable(BASE + PAGE_SIZE))
        );
    }

    #[test]
    fn every_truncation_and_generated_input_is_panic_free() {
        let valid = valid_bytes();
        for length in 0..valid.len() {
            let _ = validate_bytes("truncated", &valid[..length]);
        }

        let mut state = 0x4d59_5df4_d0f3_3173_u64;
        for length in 0..2048 {
            let mut bytes = vec![0_u8; length];
            for byte in &mut bytes {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = state as u8;
            }
            let _ = validate_bytes("generated", &bytes);
        }
    }

    #[test]
    fn errors_have_specific_descriptions_and_display_messages() {
        let file_error = Error::FileTooLarge(1_048_577);
        assert_eq!(
            file_error.description(),
            "ELF file exceeds the configured size bound"
        );
        assert!(alloc::format!("{file_error}").contains("1048576"));

        let mapped_error = Error::MappedMemoryTooLarge(16_781_312);
        assert_eq!(
            mapped_error.description(),
            "ELF mapped-page footprint exceeds the configured bound"
        );
        assert!(alloc::format!("{mapped_error}").contains("16777216"));
    }
}
