//! Allocation-free data sections for the native process-start contract.
//!
//! Capability attachments remain in the `NSPC` envelope owned by
//! [`crate::runtime_context`]. This module carries the descriptive portions of
//! `ProcessStart` as ordered `NSPD` frames so arguments and environment data can
//! exceed one bounded IPC message without becoming authority.

use core::array;

use crate::{
    abi::limits,
    handle::{Endpoint, OwnedHandle},
    ipc::{self, ObjectKind, Rights},
};

const FRAME_MAGIC: [u8; 4] = *b"NSPD";
const FRAME_HEADER_BYTES: usize = 24;
const FRAME_REQUIRED: u16 = 1 << 0;
const FRAME_FINAL: u16 = 1 << 1;
const FRAME_FLAGS: u16 = FRAME_REQUIRED | FRAME_FINAL;
const END_MAGIC: [u8; 4] = *b"NSPX";
const END_BYTES: usize = 16;
const ARGUMENTS_HEADER_BYTES: usize = 4;
const ENVIRONMENT_HEADER_BYTES: usize = 4;
const IDENTITY_BYTES: usize = 72;
const LAUNCH_BYTES: usize = 40;

/// First supported version of the process-start data-frame format.
pub const STARTUP_DATA_VERSION: u16 = 1;

/// Well-known capability-table slot used by managed-launch bootstrap.
///
/// The slot is stable; the opaque handle installed there carries a generation
/// and must be resolved with `ipc::handle_at_slot` or `OwnedHandle::from_slot`.
pub const PROCESS_START_BOOTSTRAP_SLOT: u64 = 1;

/// Maximum payload carried by one process-start data frame.
pub const STARTUP_DATA_FRAME_PAYLOAD_BYTES: usize =
    limits::MAX_IPC_MESSAGE_BYTES - FRAME_HEADER_BYTES;

const _: () = assert!(FRAME_HEADER_BYTES < limits::MAX_IPC_MESSAGE_BYTES);

/// Stable identity for one descriptive process-start section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StartupSectionId(u16);

impl StartupSectionId {
    pub const IDENTITY: Self = Self(1);
    pub const ARGUMENTS: Self = Self(2);
    pub const ENVIRONMENT: Self = Self(3);
    pub const LAUNCH: Self = Self(4);
    /// Stable application principal and installation provenance.
    pub const APPLICATION_IDENTITY: Self = Self(5);

    pub const fn new(value: u16) -> Option<Self> {
        if value == 0 { None } else { Some(Self(value)) }
    }

    pub const fn value(self) -> u16 {
        self.0
    }
}

/// One decoded `NSPD` frame borrowed from a received IPC message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupDataFrame<'a> {
    pub section: StartupSectionId,
    pub required: bool,
    pub total_bytes: usize,
    pub offset: usize,
    pub payload: &'a [u8],
    pub final_frame: bool,
}

impl StartupDataFrame<'_> {
    pub fn decode(bytes: &[u8]) -> Result<StartupDataFrame<'_>, StartupDataError> {
        if bytes.len() < FRAME_HEADER_BYTES || bytes[..4] != FRAME_MAGIC {
            return Err(StartupDataError::MalformedFrame);
        }
        let version = get_u16(bytes, 4);
        if version != STARTUP_DATA_VERSION {
            return Err(StartupDataError::UnsupportedVersion(version));
        }
        let section =
            StartupSectionId::new(get_u16(bytes, 6)).ok_or(StartupDataError::MalformedFrame)?;
        let flags = get_u16(bytes, 8);
        let header_bytes = get_u16(bytes, 10) as usize;
        let total_bytes = get_u32(bytes, 12) as usize;
        let offset = get_u32(bytes, 16) as usize;
        let payload_bytes = get_u16(bytes, 20) as usize;
        if flags & !FRAME_FLAGS != 0
            || header_bytes != FRAME_HEADER_BYTES
            || get_u16(bytes, 22) != 0
            || payload_bytes == 0
            || payload_bytes > STARTUP_DATA_FRAME_PAYLOAD_BYTES
            || bytes.len() != FRAME_HEADER_BYTES + payload_bytes
        {
            return Err(StartupDataError::MalformedFrame);
        }
        let end = offset
            .checked_add(payload_bytes)
            .ok_or(StartupDataError::MalformedFrame)?;
        let final_frame = flags & FRAME_FINAL != 0;
        if total_bytes == 0
            || total_bytes > limits::MAX_ARGUMENT_BYTES
            || end > total_bytes
            || final_frame != (end == total_bytes)
        {
            return Err(StartupDataError::MalformedFrame);
        }
        Ok(StartupDataFrame {
            section,
            required: flags & FRAME_REQUIRED != 0,
            total_bytes,
            offset,
            payload: &bytes[FRAME_HEADER_BYTES..],
            final_frame,
        })
    }
}

/// Writes one canonical process-start data frame.
pub fn encode_startup_data_frame(
    section: StartupSectionId,
    required: bool,
    total_bytes: usize,
    offset: usize,
    payload: &[u8],
    output: &mut [u8],
) -> Result<usize, StartupDataError> {
    let payload_bytes = payload.len();
    let end = offset
        .checked_add(payload_bytes)
        .ok_or(StartupDataError::FrameBounds)?;
    if total_bytes == 0
        || total_bytes > limits::MAX_ARGUMENT_BYTES
        || payload_bytes == 0
        || payload_bytes > STARTUP_DATA_FRAME_PAYLOAD_BYTES
        || end > total_bytes
        || total_bytes > u32::MAX as usize
        || offset > u32::MAX as usize
    {
        return Err(StartupDataError::FrameBounds);
    }
    let length = FRAME_HEADER_BYTES + payload_bytes;
    if output.len() < length {
        return Err(StartupDataError::FrameBounds);
    }
    output[..length].fill(0);
    output[..4].copy_from_slice(&FRAME_MAGIC);
    put_u16(output, 4, STARTUP_DATA_VERSION);
    put_u16(output, 6, section.value());
    let mut flags = if required { FRAME_REQUIRED } else { 0 };
    if end == total_bytes {
        flags |= FRAME_FINAL;
    }
    put_u16(output, 8, flags);
    put_u16(output, 10, FRAME_HEADER_BYTES as u16);
    put_u32(output, 12, total_bytes as u32);
    put_u32(output, 16, offset as u32);
    put_u16(output, 20, payload_bytes as u16);
    output[FRAME_HEADER_BYTES..length].copy_from_slice(payload);
    Ok(length)
}

/// Allocation-free encoder that fragments one complete section into IPC frames.
#[derive(Debug)]
pub struct StartupSectionFrames<'a> {
    section: StartupSectionId,
    required: bool,
    payload: &'a [u8],
    offset: usize,
}

/// One complete section supplied to the process-start transport sender.
#[derive(Debug, Clone, Copy)]
pub struct StartupSectionPayload<'a> {
    pub id: StartupSectionId,
    pub required: bool,
    pub bytes: &'a [u8],
}

/// Sends ordered process-start sections followed by a canonical end record.
///
/// Callers must keep the process behind its launch barrier when the complete
/// sequence fits the endpoint queue. Larger future sequences require concurrent
/// receiver progress rather than an unbounded sender retry loop.
pub fn send_process_start_data(
    endpoint: u64,
    sections: &[StartupSectionPayload<'_>],
) -> Result<(), StartupTransportError> {
    if endpoint == 0 || sections.is_empty() || sections.len() > u16::MAX as usize {
        return Err(StartupTransportError::Data(
            StartupDataError::IncompleteSection,
        ));
    }
    for (index, section) in sections.iter().enumerate() {
        if section.bytes.is_empty()
            || sections[..index]
                .last()
                .is_some_and(|previous| section.id <= previous.id)
        {
            return Err(StartupTransportError::Data(StartupDataError::SectionOrder(
                section.id,
            )));
        }
    }

    let mut output = [0; limits::MAX_IPC_MESSAGE_BYTES];
    for section in sections {
        let mut frames = StartupSectionFrames::new(section.id, section.required, section.bytes)
            .map_err(StartupTransportError::Data)?;
        while let Some(length) = frames
            .next_frame(&mut output)
            .map_err(StartupTransportError::Data)?
        {
            ipc::send(endpoint, &output[..length], None).map_err(StartupTransportError::Ipc)?;
        }
    }
    let end = encode_startup_data_end(sections.len())?;
    ipc::send(endpoint, &end, None).map_err(StartupTransportError::Ipc)
}

/// Receives a complete process-start data stream from one trusted launcher.
pub fn receive_process_start_data<const BYTES: usize, const SECTIONS: usize>(
    endpoint: &OwnedHandle<Endpoint>,
    expected_sender: u64,
    supported: &[StartupSectionId],
    required: &[StartupSectionId],
) -> Result<ProcessStartData<BYTES, SECTIONS>, StartupTransportError> {
    if expected_sender == 0
        || !endpoint
            .info()
            .is_ok_and(|info| info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE)
    {
        return Err(StartupTransportError::InvalidEndpoint);
    }
    let mut data = ProcessStartData::new();
    let mut input = [0; limits::MAX_IPC_MESSAGE_BYTES];
    loop {
        let message = loop {
            match endpoint.try_receive(&mut input) {
                Ok(message) => break message,
                Err(error) if error == ipc::Error::TRY_AGAIN => {
                    crate::syscall::yield_now().map_err(|_| StartupTransportError::Ipc(error))?;
                }
                Err(error) => return Err(StartupTransportError::Ipc(error)),
            }
        };
        if message.sender_process_id != expected_sender {
            return Err(StartupTransportError::WrongSender);
        }
        if message.capability.is_some() {
            return Err(StartupTransportError::UnexpectedCapability);
        }
        let bytes = &input[..message.bytes];
        if bytes.starts_with(&END_MAGIC) {
            data.validate_end(bytes)?;
            data.validate_required(required)?;
            return Ok(data);
        }
        data.push_frame(bytes, supported)?;
    }
}

fn encode_startup_data_end(section_count: usize) -> Result<[u8; END_BYTES], StartupDataError> {
    if section_count == 0 || section_count > u16::MAX as usize {
        return Err(StartupDataError::IncompleteSection);
    }
    let mut output = [0; END_BYTES];
    output[..4].copy_from_slice(&END_MAGIC);
    put_u16(&mut output, 4, STARTUP_DATA_VERSION);
    put_u16(&mut output, 6, section_count as u16);
    Ok(output)
}

fn decode_startup_data_end(bytes: &[u8]) -> Result<usize, StartupDataError> {
    if bytes.len() != END_BYTES || bytes[..4] != END_MAGIC {
        return Err(StartupDataError::MalformedFrame);
    }
    let version = get_u16(bytes, 4);
    if version != STARTUP_DATA_VERSION {
        return Err(StartupDataError::UnsupportedVersion(version));
    }
    let section_count = get_u16(bytes, 6) as usize;
    if section_count == 0 || bytes[8..].iter().any(|byte| *byte != 0) {
        return Err(StartupDataError::MalformedFrame);
    }
    Ok(section_count)
}

impl<'a> StartupSectionFrames<'a> {
    pub fn new(
        section: StartupSectionId,
        required: bool,
        payload: &'a [u8],
    ) -> Result<Self, StartupDataError> {
        if payload.is_empty() || payload.len() > limits::MAX_ARGUMENT_BYTES {
            return Err(StartupDataError::SectionBounds(section));
        }
        Ok(Self {
            section,
            required,
            payload,
            offset: 0,
        })
    }

    /// Writes the next frame, returning `None` after the section is complete.
    pub fn next_frame(&mut self, output: &mut [u8]) -> Result<Option<usize>, StartupDataError> {
        if self.offset == self.payload.len() {
            return Ok(None);
        }
        let end = self
            .offset
            .saturating_add(STARTUP_DATA_FRAME_PAYLOAD_BYTES)
            .min(self.payload.len());
        let length = encode_startup_data_frame(
            self.section,
            self.required,
            self.payload.len(),
            self.offset,
            &self.payload[self.offset..end],
            output,
        )?;
        self.offset = end;
        Ok(Some(length))
    }
}

/// A retained, complete section in a decoded process-start record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupSection {
    pub id: StartupSectionId,
    pub required: bool,
    offset: usize,
    length: usize,
}

#[derive(Debug, Clone, Copy)]
struct PartialSection {
    id: StartupSectionId,
    required: bool,
    total_bytes: usize,
    start: usize,
    received: usize,
    retained: bool,
}

/// Fixed-capacity assembler for ordered, possibly fragmented startup sections.
#[derive(Debug)]
pub struct ProcessStartData<const BYTES: usize, const SECTIONS: usize> {
    bytes: [u8; BYTES],
    sections: [Option<StartupSection>; SECTIONS],
    used: usize,
    section_count: usize,
    completed_section_count: usize,
    partial: Option<PartialSection>,
    last_section: Option<StartupSectionId>,
}

impl<const BYTES: usize, const SECTIONS: usize> ProcessStartData<BYTES, SECTIONS> {
    pub fn new() -> Self {
        Self {
            bytes: [0; BYTES],
            sections: array::from_fn(|_| None),
            used: 0,
            section_count: 0,
            completed_section_count: 0,
            partial: None,
            last_section: None,
        }
    }

    /// Accepts the next frame in canonical section and fragment order.
    ///
    /// Unsupported optional sections are validated and discarded. Unsupported
    /// required sections fail immediately.
    pub fn push_frame(
        &mut self,
        bytes: &[u8],
        supported: &[StartupSectionId],
    ) -> Result<(), StartupDataError> {
        let frame = StartupDataFrame::decode(bytes)?;
        let supported = supported.contains(&frame.section);
        if !supported && frame.required {
            return Err(StartupDataError::UnknownRequiredSection(frame.section));
        }

        let mut partial = match self.partial {
            Some(partial) => {
                if partial.id != frame.section
                    || partial.required != frame.required
                    || partial.total_bytes != frame.total_bytes
                    || partial.received != frame.offset
                {
                    return Err(StartupDataError::FragmentOrder(frame.section));
                }
                partial
            }
            None => {
                if frame.offset != 0 || self.last_section.is_some_and(|last| frame.section <= last)
                {
                    return Err(StartupDataError::SectionOrder(frame.section));
                }
                if supported
                    && (self.section_count == SECTIONS
                        || self
                            .used
                            .checked_add(frame.total_bytes)
                            .is_none_or(|end| end > BYTES))
                {
                    return Err(StartupDataError::StorageBounds(frame.section));
                }
                PartialSection {
                    id: frame.section,
                    required: frame.required,
                    total_bytes: frame.total_bytes,
                    start: self.used,
                    received: 0,
                    retained: supported,
                }
            }
        };

        if partial.retained {
            let start = partial.start + partial.received;
            let end = start + frame.payload.len();
            self.bytes[start..end].copy_from_slice(frame.payload);
        }
        partial.received += frame.payload.len();
        if frame.final_frame {
            if partial.received != partial.total_bytes {
                return Err(StartupDataError::FragmentOrder(frame.section));
            }
            if partial.retained {
                self.sections[self.section_count] = Some(StartupSection {
                    id: partial.id,
                    required: partial.required,
                    offset: partial.start,
                    length: partial.total_bytes,
                });
                self.section_count += 1;
                self.used += partial.total_bytes;
            }
            self.last_section = Some(partial.id);
            self.completed_section_count += 1;
            self.partial = None;
        } else {
            self.partial = Some(partial);
        }
        Ok(())
    }

    pub const fn is_complete(&self) -> bool {
        self.partial.is_none()
    }

    pub const fn len(&self) -> usize {
        self.section_count
    }

    pub const fn is_empty(&self) -> bool {
        self.section_count == 0
    }

    pub fn section(&self, id: StartupSectionId) -> Option<&[u8]> {
        let section = self
            .sections
            .iter()
            .flatten()
            .find(|section| section.id == id)?;
        Some(&self.bytes[section.offset..section.offset + section.length])
    }

    pub fn sections(&self) -> impl ExactSizeIterator<Item = StartupSection> + '_ {
        self.sections[..self.section_count]
            .iter()
            .map(|section| section.expect("retained process-start section remains present"))
    }

    pub fn validate_required(&self, required: &[StartupSectionId]) -> Result<(), StartupDataError> {
        if !self.is_complete() {
            return Err(StartupDataError::IncompleteSection);
        }
        for id in required {
            if self.section(*id).is_none() {
                return Err(StartupDataError::MissingRequiredSection(*id));
            }
        }
        Ok(())
    }

    pub fn validate_end(&self, bytes: &[u8]) -> Result<(), StartupDataError> {
        if !self.is_complete() {
            return Err(StartupDataError::IncompleteSection);
        }
        if decode_startup_data_end(bytes)? != self.completed_section_count {
            return Err(StartupDataError::MalformedFrame);
        }
        Ok(())
    }
}

impl<const BYTES: usize, const SECTIONS: usize> Default for ProcessStartData<BYTES, SECTIONS> {
    fn default() -> Self {
        Self::new()
    }
}

/// Descriptive identities supplied by the process manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupIdentity {
    pub process: u64,
    pub package: u64,
    pub package_generation: u64,
    pub executable: u64,
    pub application: u64,
    pub service: u64,
    pub component: u64,
    pub user: u64,
    pub session: u64,
}

impl StartupIdentity {
    pub const ENCODED_BYTES: usize = IDENTITY_BYTES;

    pub fn encode(self) -> [u8; IDENTITY_BYTES] {
        let mut output = [0; IDENTITY_BYTES];
        for (index, value) in [
            self.process,
            self.package,
            self.package_generation,
            self.executable,
            self.application,
            self.service,
            self.component,
            self.user,
            self.session,
        ]
        .into_iter()
        .enumerate()
        {
            put_u64(&mut output, index * 8, value);
        }
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StartupDataError> {
        if bytes.len() != IDENTITY_BYTES {
            return Err(StartupDataError::InvalidSection(StartupSectionId::IDENTITY));
        }
        Ok(Self {
            process: get_u64(bytes, 0),
            package: get_u64(bytes, 8),
            package_generation: get_u64(bytes, 16),
            executable: get_u64(bytes, 24),
            application: get_u64(bytes, 32),
            service: get_u64(bytes, 40),
            component: get_u64(bytes, 48),
            user: get_u64(bytes, 56),
            session: get_u64(bytes, 64),
        })
    }
}

/// Why a process instance was launched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum StartupLaunchReason {
    Boot = 1,
    Activation = 2,
    Restart = 3,
    User = 4,
    Recovery = 5,
}

impl StartupLaunchReason {
    fn from_raw(value: u16) -> Option<Self> {
        match value {
            1 => Some(Self::Boot),
            2 => Some(Self::Activation),
            3 => Some(Self::Restart),
            4 => Some(Self::User),
            5 => Some(Self::Recovery),
            _ => None,
        }
    }
}

/// Descriptive launch metadata; none of these fields grants authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupLaunch {
    pub launch: u64,
    pub manager_generation: u64,
    pub namespace_profile: u64,
    pub monotonic_start_ns: u64,
    pub attempt: u32,
    pub reason: StartupLaunchReason,
    pub flags: u16,
}

impl StartupLaunch {
    pub const ENCODED_BYTES: usize = LAUNCH_BYTES;

    pub fn encode(self) -> [u8; LAUNCH_BYTES] {
        let mut output = [0; LAUNCH_BYTES];
        put_u64(&mut output, 0, self.launch);
        put_u64(&mut output, 8, self.manager_generation);
        put_u64(&mut output, 16, self.namespace_profile);
        put_u64(&mut output, 24, self.monotonic_start_ns);
        put_u32(&mut output, 32, self.attempt);
        put_u16(&mut output, 36, self.reason as u16);
        put_u16(&mut output, 38, self.flags);
        output
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, StartupDataError> {
        if bytes.len() != LAUNCH_BYTES {
            return Err(StartupDataError::InvalidSection(StartupSectionId::LAUNCH));
        }
        let reason = StartupLaunchReason::from_raw(get_u16(bytes, 36))
            .ok_or(StartupDataError::InvalidSection(StartupSectionId::LAUNCH))?;
        Ok(Self {
            launch: get_u64(bytes, 0),
            manager_generation: get_u64(bytes, 8),
            namespace_profile: get_u64(bytes, 16),
            monotonic_start_ns: get_u64(bytes, 24),
            attempt: get_u32(bytes, 32),
            reason,
            flags: get_u16(bytes, 38),
        })
    }
}

/// Writes a bounded argument-vector section.
pub fn encode_startup_arguments(
    arguments: &[&[u8]],
    output: &mut [u8],
) -> Result<usize, StartupDataError> {
    if arguments.len() > limits::MAX_ARGUMENTS || output.len() < ARGUMENTS_HEADER_BYTES {
        return Err(StartupDataError::InvalidSection(
            StartupSectionId::ARGUMENTS,
        ));
    }
    output.fill(0);
    put_u16(output, 0, arguments.len() as u16);
    let mut cursor = ARGUMENTS_HEADER_BYTES;
    let mut argument_bytes = 0usize;
    for argument in arguments {
        argument_bytes = argument_bytes
            .checked_add(argument.len().saturating_add(1))
            .ok_or(StartupDataError::InvalidSection(
                StartupSectionId::ARGUMENTS,
            ))?;
        let end = cursor
            .checked_add(2)
            .and_then(|offset| offset.checked_add(argument.len()))
            .ok_or(StartupDataError::InvalidSection(
                StartupSectionId::ARGUMENTS,
            ))?;
        if argument.len() > u16::MAX as usize
            || argument.contains(&0)
            || argument_bytes > limits::MAX_ARGUMENT_BYTES
            || end > output.len()
        {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ARGUMENTS,
            ));
        }
        put_u16(output, cursor, argument.len() as u16);
        output[cursor + 2..end].copy_from_slice(argument);
        cursor = end;
    }
    Ok(cursor)
}

/// Validated, borrowed argument-vector section.
#[derive(Debug, Clone, Copy)]
pub struct StartupArguments<'a> {
    bytes: &'a [u8],
    count: usize,
}

impl<'a> StartupArguments<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, StartupDataError> {
        if bytes.len() < ARGUMENTS_HEADER_BYTES || get_u16(bytes, 2) != 0 {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ARGUMENTS,
            ));
        }
        let count = get_u16(bytes, 0) as usize;
        if count > limits::MAX_ARGUMENTS {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ARGUMENTS,
            ));
        }
        let mut cursor = ARGUMENTS_HEADER_BYTES;
        let mut argument_bytes = 0usize;
        for _ in 0..count {
            if cursor + 2 > bytes.len() {
                return Err(StartupDataError::InvalidSection(
                    StartupSectionId::ARGUMENTS,
                ));
            }
            let length = get_u16(bytes, cursor) as usize;
            cursor += 2;
            let end = cursor
                .checked_add(length)
                .ok_or(StartupDataError::InvalidSection(
                    StartupSectionId::ARGUMENTS,
                ))?;
            argument_bytes = argument_bytes.checked_add(length.saturating_add(1)).ok_or(
                StartupDataError::InvalidSection(StartupSectionId::ARGUMENTS),
            )?;
            if end > bytes.len()
                || bytes[cursor..end].contains(&0)
                || argument_bytes > limits::MAX_ARGUMENT_BYTES
            {
                return Err(StartupDataError::InvalidSection(
                    StartupSectionId::ARGUMENTS,
                ));
            }
            cursor = end;
        }
        if cursor != bytes.len() {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ARGUMENTS,
            ));
        }
        Ok(Self { bytes, count })
    }

    pub const fn len(self) -> usize {
        self.count
    }

    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn get(self, index: usize) -> Option<&'a [u8]> {
        if index >= self.count {
            return None;
        }
        let mut cursor = ARGUMENTS_HEADER_BYTES;
        for current in 0..self.count {
            let length = get_u16(self.bytes, cursor) as usize;
            cursor += 2;
            let end = cursor + length;
            if current == index {
                return Some(&self.bytes[cursor..end]);
            }
            cursor = end;
        }
        None
    }

    pub fn iter(self) -> impl ExactSizeIterator<Item = &'a [u8]> {
        (0..self.count).map(move |index| {
            self.get(index)
                .expect("validated startup argument remains present")
        })
    }
}

/// Writes a bounded compatibility-environment section.
pub fn encode_startup_environment(
    environment: &[(&[u8], &[u8])],
    output: &mut [u8],
) -> Result<usize, StartupDataError> {
    if environment.len() > limits::MAX_ENVIRONMENT_VARIABLES
        || output.len() < ENVIRONMENT_HEADER_BYTES
    {
        return Err(StartupDataError::InvalidSection(
            StartupSectionId::ENVIRONMENT,
        ));
    }
    output.fill(0);
    put_u16(output, 0, environment.len() as u16);
    let mut cursor = ENVIRONMENT_HEADER_BYTES;
    let mut environment_bytes = 0usize;
    for (index, (name, value)) in environment.iter().enumerate() {
        if !valid_environment_name(name)
            || value.contains(&0)
            || environment[..index]
                .iter()
                .any(|(existing, _)| *existing == *name)
        {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT,
            ));
        }
        environment_bytes = environment_bytes
            .checked_add(name.len().saturating_add(value.len()).saturating_add(2))
            .ok_or(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT,
            ))?;
        let end = cursor
            .checked_add(4)
            .and_then(|offset| offset.checked_add(name.len()))
            .and_then(|offset| offset.checked_add(value.len()))
            .ok_or(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT,
            ))?;
        if value.len() > u16::MAX as usize
            || environment_bytes > limits::MAX_ENVIRONMENT_BYTES
            || end > output.len()
        {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT,
            ));
        }
        put_u16(output, cursor, name.len() as u16);
        put_u16(output, cursor + 2, value.len() as u16);
        cursor += 4;
        output[cursor..cursor + name.len()].copy_from_slice(name);
        cursor += name.len();
        output[cursor..cursor + value.len()].copy_from_slice(value);
        cursor += value.len();
    }
    Ok(cursor)
}

/// Validated, borrowed compatibility-environment section.
#[derive(Debug, Clone, Copy)]
pub struct StartupEnvironment<'a> {
    bytes: &'a [u8],
    count: usize,
}

impl<'a> StartupEnvironment<'a> {
    pub fn decode(bytes: &'a [u8]) -> Result<Self, StartupDataError> {
        if bytes.len() < ENVIRONMENT_HEADER_BYTES || get_u16(bytes, 2) != 0 {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT,
            ));
        }
        let count = get_u16(bytes, 0) as usize;
        if count > limits::MAX_ENVIRONMENT_VARIABLES {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT,
            ));
        }
        let mut cursor = ENVIRONMENT_HEADER_BYTES;
        let mut environment_bytes = 0usize;
        for index in 0..count {
            let Some((name, value, next)) = environment_entry(bytes, cursor) else {
                return Err(StartupDataError::InvalidSection(
                    StartupSectionId::ENVIRONMENT,
                ));
            };
            if !valid_environment_name(name) || value.contains(&0) {
                return Err(StartupDataError::InvalidSection(
                    StartupSectionId::ENVIRONMENT,
                ));
            }
            let mut previous_cursor = ENVIRONMENT_HEADER_BYTES;
            for _ in 0..index {
                let Some((previous, _, following)) = environment_entry(bytes, previous_cursor)
                else {
                    return Err(StartupDataError::InvalidSection(
                        StartupSectionId::ENVIRONMENT,
                    ));
                };
                if previous == name {
                    return Err(StartupDataError::InvalidSection(
                        StartupSectionId::ENVIRONMENT,
                    ));
                }
                previous_cursor = following;
            }
            environment_bytes = environment_bytes
                .checked_add(name.len().saturating_add(value.len()).saturating_add(2))
                .ok_or(StartupDataError::InvalidSection(
                    StartupSectionId::ENVIRONMENT,
                ))?;
            if environment_bytes > limits::MAX_ENVIRONMENT_BYTES {
                return Err(StartupDataError::InvalidSection(
                    StartupSectionId::ENVIRONMENT,
                ));
            }
            cursor = next;
        }
        if cursor != bytes.len() {
            return Err(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT,
            ));
        }
        Ok(Self { bytes, count })
    }

    pub const fn len(self) -> usize {
        self.count
    }

    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn get(self, index: usize) -> Option<(&'a [u8], &'a [u8])> {
        if index >= self.count {
            return None;
        }
        let mut cursor = ENVIRONMENT_HEADER_BYTES;
        for current in 0..self.count {
            let (name, value, next) = environment_entry(self.bytes, cursor)?;
            if current == index {
                return Some((name, value));
            }
            cursor = next;
        }
        None
    }

    pub fn find(self, name: &[u8]) -> Option<&'a [u8]> {
        (0..self.count).find_map(|index| {
            let (candidate, value) = self.get(index)?;
            (candidate == name).then_some(value)
        })
    }
}

/// Typed view of all currently standardized descriptive startup sections.
#[derive(Debug, Clone, Copy)]
pub struct ValidatedProcessStart<'a> {
    pub identity: StartupIdentity,
    pub arguments: StartupArguments<'a>,
    pub environment: StartupEnvironment<'a>,
    pub launch: StartupLaunch,
}

impl<'a> ValidatedProcessStart<'a> {
    pub fn from_data<const BYTES: usize, const SECTIONS: usize>(
        data: &'a ProcessStartData<BYTES, SECTIONS>,
    ) -> Result<Self, StartupDataError> {
        let required = [
            StartupSectionId::IDENTITY,
            StartupSectionId::ARGUMENTS,
            StartupSectionId::ENVIRONMENT,
            StartupSectionId::LAUNCH,
        ];
        data.validate_required(&required)?;
        Ok(Self {
            identity: StartupIdentity::decode(
                data.section(StartupSectionId::IDENTITY)
                    .expect("required identity section is present"),
            )?,
            arguments: StartupArguments::decode(
                data.section(StartupSectionId::ARGUMENTS)
                    .expect("required arguments section is present"),
            )?,
            environment: StartupEnvironment::decode(
                data.section(StartupSectionId::ENVIRONMENT)
                    .expect("required environment section is present"),
            )?,
            launch: StartupLaunch::decode(
                data.section(StartupSectionId::LAUNCH)
                    .expect("required launch section is present"),
            )?,
        })
    }
}

/// Why process-start data encoding, assembly, or typed validation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupDataError {
    MalformedFrame,
    UnsupportedVersion(u16),
    FrameBounds,
    SectionBounds(StartupSectionId),
    SectionOrder(StartupSectionId),
    FragmentOrder(StartupSectionId),
    StorageBounds(StartupSectionId),
    UnknownRequiredSection(StartupSectionId),
    MissingRequiredSection(StartupSectionId),
    InvalidSection(StartupSectionId),
    IncompleteSection,
}

/// Why process-start data transport failed before entry-point dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupTransportError {
    Data(StartupDataError),
    Ipc(ipc::Error),
    InvalidEndpoint,
    WrongSender,
    UnexpectedCapability,
}

impl From<StartupDataError> for StartupTransportError {
    fn from(error: StartupDataError) -> Self {
        Self::Data(error)
    }
}

fn environment_entry(bytes: &[u8], cursor: usize) -> Option<(&[u8], &[u8], usize)> {
    if cursor.checked_add(4)? > bytes.len() {
        return None;
    }
    let name_length = get_u16(bytes, cursor) as usize;
    let value_length = get_u16(bytes, cursor + 2) as usize;
    let name_start = cursor + 4;
    let value_start = name_start.checked_add(name_length)?;
    let end = value_start.checked_add(value_length)?;
    (end <= bytes.len()).then_some((
        &bytes[name_start..value_start],
        &bytes[value_start..end],
        end,
    ))
}

fn valid_environment_name(name: &[u8]) -> bool {
    if name.is_empty() || name.len() > limits::MAX_ENVIRONMENT_NAME_BYTES {
        return false;
    }
    if !matches!(name[0], b'A'..=b'Z' | b'a'..=b'z' | b'_') {
        return false;
    }
    name[1..]
        .iter()
        .all(|byte| matches!(*byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u16(input: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([input[offset], input[offset + 1]])
}

fn get_u32(input: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
    ])
}

fn get_u64(input: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        input[offset],
        input[offset + 1],
        input[offset + 2],
        input[offset + 3],
        input[offset + 4],
        input[offset + 5],
        input[offset + 6],
        input[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUPPORTED: [StartupSectionId; 4] = [
        StartupSectionId::IDENTITY,
        StartupSectionId::ARGUMENTS,
        StartupSectionId::ENVIRONMENT,
        StartupSectionId::LAUNCH,
    ];

    fn push_section<const BYTES: usize, const SECTIONS: usize>(
        data: &mut ProcessStartData<BYTES, SECTIONS>,
        id: StartupSectionId,
        payload: &[u8],
    ) {
        let mut frames = StartupSectionFrames::new(id, true, payload).unwrap();
        let mut frame = [0; limits::MAX_IPC_MESSAGE_BYTES];
        while let Some(length) = frames.next_frame(&mut frame).unwrap() {
            data.push_frame(&frame[..length], &SUPPORTED).unwrap();
        }
    }

    #[test]
    fn typed_process_start_round_trips_fragmented_sections() {
        let identity = StartupIdentity {
            process: 31,
            package: 41,
            package_generation: 2,
            executable: 51,
            application: 61,
            service: 0,
            component: 71,
            user: 1000,
            session: 81,
        };
        let launch = StartupLaunch {
            launch: 91,
            manager_generation: 4,
            namespace_profile: 101,
            monotonic_start_ns: 500,
            attempt: 2,
            reason: StartupLaunchReason::Activation,
            flags: 3,
        };
        let long_argument = [b'x'; STARTUP_DATA_FRAME_PAYLOAD_BYTES + 10];
        let mut argument_bytes = [0; 512];
        let argument_length =
            encode_startup_arguments(&[b"program", &long_argument], &mut argument_bytes).unwrap();
        let mut environment_bytes = [0; 128];
        let environment_length = encode_startup_environment(
            &[(b"LANG", b"en_US.UTF-8"), (b"MODE", b"test")],
            &mut environment_bytes,
        )
        .unwrap();

        let mut data = ProcessStartData::<1024, 4>::new();
        push_section(&mut data, StartupSectionId::IDENTITY, &identity.encode());
        push_section(
            &mut data,
            StartupSectionId::ARGUMENTS,
            &argument_bytes[..argument_length],
        );
        push_section(
            &mut data,
            StartupSectionId::ENVIRONMENT,
            &environment_bytes[..environment_length],
        );
        push_section(&mut data, StartupSectionId::LAUNCH, &launch.encode());

        let decoded = ValidatedProcessStart::from_data(&data).unwrap();
        assert_eq!(decoded.identity, identity);
        assert_eq!(decoded.arguments.len(), 2);
        assert_eq!(decoded.arguments.get(0), Some(&b"program"[..]));
        assert_eq!(decoded.arguments.get(1), Some(&long_argument[..]));
        assert_eq!(decoded.environment.find(b"LANG"), Some(&b"en_US.UTF-8"[..]));
        assert_eq!(decoded.environment.find(b"MISSING"), None);
        assert_eq!(decoded.launch, launch);
        assert_eq!(data.len(), 4);
        assert_eq!(
            data.validate_end(&encode_startup_data_end(4).unwrap()),
            Ok(())
        );
        assert_eq!(
            data.validate_end(&encode_startup_data_end(3).unwrap()),
            Err(StartupDataError::MalformedFrame)
        );
    }

    #[test]
    fn optional_unknown_sections_are_discarded_but_required_ones_fail() {
        let unknown = StartupSectionId::new(99).unwrap();
        let mut frame = [0; limits::MAX_IPC_MESSAGE_BYTES];
        let length = encode_startup_data_frame(unknown, false, 3, 0, b"new", &mut frame).unwrap();
        let mut data = ProcessStartData::<16, 1>::new();
        data.push_frame(&frame[..length], &SUPPORTED).unwrap();
        assert!(data.is_empty());
        assert!(data.is_complete());
        assert_eq!(
            data.validate_end(&encode_startup_data_end(1).unwrap()),
            Ok(())
        );

        let length = encode_startup_data_frame(unknown, true, 3, 0, b"new", &mut frame).unwrap();
        assert_eq!(
            data.push_frame(&frame[..length], &SUPPORTED),
            Err(StartupDataError::UnknownRequiredSection(unknown))
        );
    }

    #[test]
    fn assembler_rejects_fragment_gaps_and_section_reordering() {
        let mut frame = [0; limits::MAX_IPC_MESSAGE_BYTES];
        let first =
            encode_startup_data_frame(StartupSectionId::ARGUMENTS, true, 8, 0, b"abcd", &mut frame)
                .unwrap();
        let mut data = ProcessStartData::<32, 2>::new();
        data.push_frame(&frame[..first], &SUPPORTED).unwrap();

        let gap =
            encode_startup_data_frame(StartupSectionId::ARGUMENTS, true, 8, 5, b"xyz", &mut frame)
                .unwrap();
        assert_eq!(
            data.push_frame(&frame[..gap], &SUPPORTED),
            Err(StartupDataError::FragmentOrder(StartupSectionId::ARGUMENTS))
        );

        let final_frame =
            encode_startup_data_frame(StartupSectionId::ARGUMENTS, true, 8, 4, b"efgh", &mut frame)
                .unwrap();
        data.push_frame(&frame[..final_frame], &SUPPORTED).unwrap();
        let reordered =
            encode_startup_data_frame(StartupSectionId::IDENTITY, true, 1, 0, b"x", &mut frame)
                .unwrap();
        assert_eq!(
            data.push_frame(&frame[..reordered], &SUPPORTED),
            Err(StartupDataError::SectionOrder(StartupSectionId::IDENTITY))
        );
    }

    #[test]
    fn argument_and_environment_validation_is_canonical() {
        let mut output = [0; 128];
        assert_eq!(
            encode_startup_arguments(&[b"bad\0argument"], &mut output),
            Err(StartupDataError::InvalidSection(
                StartupSectionId::ARGUMENTS
            ))
        );
        assert_eq!(
            encode_startup_environment(&[(b"1INVALID", b"x")], &mut output),
            Err(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT
            ))
        );
        assert_eq!(
            encode_startup_environment(&[(b"MODE", b"a"), (b"MODE", b"b")], &mut output),
            Err(StartupDataError::InvalidSection(
                StartupSectionId::ENVIRONMENT
            ))
        );

        let length = encode_startup_environment(&[(b"EMPTY", b"")], &mut output).unwrap();
        let environment = StartupEnvironment::decode(&output[..length]).unwrap();
        assert_eq!(environment.find(b"EMPTY"), Some(&b""[..]));
    }

    #[test]
    fn required_sections_and_storage_bounds_fail_closed() {
        let mut data = ProcessStartData::<8, 1>::new();
        assert_eq!(
            data.validate_required(&[StartupSectionId::IDENTITY]),
            Err(StartupDataError::MissingRequiredSection(
                StartupSectionId::IDENTITY
            ))
        );

        let mut frame = [0; limits::MAX_IPC_MESSAGE_BYTES];
        let length = encode_startup_data_frame(
            StartupSectionId::IDENTITY,
            true,
            IDENTITY_BYTES,
            0,
            &[0; IDENTITY_BYTES],
            &mut frame,
        )
        .unwrap();
        assert_eq!(
            data.push_frame(&frame[..length], &SUPPORTED),
            Err(StartupDataError::StorageBounds(StartupSectionId::IDENTITY))
        );
    }
}
