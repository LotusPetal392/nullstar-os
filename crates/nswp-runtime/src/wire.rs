use nswp_core::{
    Header, HeaderFlags, NSWP_HEADER_BYTES, PacketKind, ProtocolErrorCode, ProtocolErrorRecord,
    TransportStatus,
};

use crate::{BodyBuf, BoundState, PacketBuf, RuntimeError};

pub(crate) fn encode_packet(header: Header, body: &[u8]) -> Result<PacketBuf, RuntimeError> {
    if body.len() != header.body_bytes as usize {
        return Err(RuntimeError::InvalidState);
    }
    let mut packet = PacketBuf::new();
    let mut encoded_header = [0; NSWP_HEADER_BYTES];
    header.encode(&mut encoded_header)?;
    packet.as_mut_capacity()[..NSWP_HEADER_BYTES].copy_from_slice(&encoded_header);
    packet.as_mut_capacity()[NSWP_HEADER_BYTES..NSWP_HEADER_BYTES + body.len()]
        .copy_from_slice(body);
    packet.set_len(NSWP_HEADER_BYTES + body.len())?;
    Ok(packet)
}

pub(crate) fn body_from_packet(
    packet: &PacketBuf,
    header: &Header,
) -> Result<BodyBuf, RuntimeError> {
    let end = NSWP_HEADER_BYTES
        .checked_add(header.body_bytes as usize)
        .ok_or(RuntimeError::InvalidState)?;
    let body = packet
        .as_slice()
        .get(NSWP_HEADER_BYTES..end)
        .ok_or(RuntimeError::InvalidState)?;
    BodyBuf::from_slice(body)
}

pub(crate) fn response_packet(
    bound: &BoundState,
    transaction: nswp_core::OutstandingTransaction,
    status: TransportStatus,
    body: &[u8],
) -> Result<PacketBuf, RuntimeError> {
    if status != TransportStatus::Ok && !body.is_empty() {
        return Err(RuntimeError::InvalidState);
    }
    encode_packet(
        Header {
            kind: PacketKind::Response,
            flags: if transaction.trace_id == [0; 16] {
                HeaderFlags::NONE
            } else {
                HeaderFlags::TRACE_SAMPLED
            },
            protocol_major: bound.major(),
            protocol_minor: bound.minor(),
            ordinal: transaction.ordinal,
            body_bytes: body.len() as u32,
            handle_count: 0,
            transport_status: status,
            transaction_id: transaction.transaction_id,
            deadline_ns: transaction.deadline_ns,
            trace_id: transaction.trace_id,
        },
        body,
    )
}

pub(crate) fn protocol_error_packet(
    bound: Option<&BoundState>,
    related: Option<&Header>,
    code: ProtocolErrorCode,
) -> Result<PacketBuf, RuntimeError> {
    let (major, minor) = bound.map_or((0, 0), |bound| (bound.major(), bound.minor()));
    let transaction_id = related.map_or(0, |header| header.transaction_id);
    let ordinal = related.map_or(0, |header| header.ordinal);
    let deadline_ns = related.map_or(u64::MAX, |header| header.deadline_ns);
    let trace_id = related.map_or([0; 16], |header| header.trace_id);
    let header = Header {
        kind: PacketKind::ProtocolError,
        flags: if trace_id == [0; 16] {
            HeaderFlags::NONE
        } else {
            HeaderFlags::TRACE_SAMPLED
        },
        protocol_major: major,
        protocol_minor: minor,
        ordinal,
        body_bytes: nswp_core::PROTOCOL_ERROR_BODY_BYTES as u32,
        handle_count: 0,
        transport_status: TransportStatus::Ok,
        transaction_id,
        deadline_ns,
        trace_id,
    };
    let mut body = [0; nswp_core::PROTOCOL_ERROR_BODY_BYTES];
    ProtocolErrorRecord {
        code,
        detail: 0,
        related_transaction_id: transaction_id,
        related_ordinal: ordinal,
    }
    .encode(&header, &mut body)?;
    encode_packet(header, &body)
}
