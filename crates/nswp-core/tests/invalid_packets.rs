use nswp_core::{
    BoundProtocol, ConnectionLimits, ConnectionState, DecodeError, Direction, Header, HeaderFlags,
    NSWP_HEADER_BYTES, OutstandingTransaction, PROTOCOL_ERROR_BODY_BYTES, PacketKind,
    ProtocolErrorCode, ProtocolErrorRecord, ProtocolId, TransactionState, TransportStatus,
    ValidationContext, offset,
};

const TRACE: [u8; 16] = [1; 16];

fn protocol_id() -> ProtocolId {
    ProtocolId::parse("00112233-4455-4677-8899-aabbccddeeff").unwrap()
}

fn bound() -> ConnectionState<'static> {
    ConnectionState::Bound(
        BoundProtocol::new(
            protocol_id(),
            1,
            2,
            ConnectionLimits::ENDPOINT_PROTOTYPE,
            7,
            &[],
        )
        .unwrap(),
    )
}

fn request() -> Header {
    Header {
        kind: PacketKind::Request,
        flags: HeaderFlags::NONE,
        protocol_major: 1,
        protocol_minor: 2,
        ordinal: 9,
        body_bytes: 8,
        handle_count: 0,
        transport_status: TransportStatus::Ok,
        transaction_id: 3,
        deadline_ns: 100,
        trace_id: TRACE,
    }
}

fn encoded_request() -> [u8; NSWP_HEADER_BYTES] {
    let mut bytes = [0; NSWP_HEADER_BYTES];
    request().encode(&mut bytes).unwrap();
    bytes
}

#[test]
fn fixed_header_fields_are_rejected_independently() {
    let mut bytes = encoded_request();
    bytes[0] ^= 1;
    assert_eq!(Header::decode(&bytes), Err(DecodeError::InvalidMagic));

    let mut bytes = encoded_request();
    bytes[offset::HEADER_BYTES..offset::HEADER_BYTES + 2].copy_from_slice(&63_u16.to_le_bytes());
    assert_eq!(Header::decode(&bytes), Err(DecodeError::InvalidHeaderSize));
    bytes[offset::HEADER_BYTES..offset::HEADER_BYTES + 2].copy_from_slice(&65_u16.to_le_bytes());
    assert_eq!(Header::decode(&bytes), Err(DecodeError::InvalidHeaderSize));

    let mut bytes = encoded_request();
    bytes[offset::WIRE_MAJOR] = 2;
    assert_eq!(
        Header::decode(&bytes),
        Err(DecodeError::UnsupportedWireVersion)
    );

    let mut bytes = encoded_request();
    bytes[offset::KIND] = 0xfe;
    assert_eq!(Header::decode(&bytes), Err(DecodeError::UnknownPacketKind));

    let mut bytes = encoded_request();
    bytes[offset::FLAGS] = 0x80;
    assert_eq!(Header::decode(&bytes), Err(DecodeError::UnknownFlags));

    let mut bytes = encoded_request();
    bytes[offset::RESERVED0] = 1;
    assert_eq!(Header::decode(&bytes), Err(DecodeError::ReservedValueUsed));
}

#[test]
fn intrinsic_relationship_matrix_is_enforced() {
    let mut bytes = encoded_request();
    bytes[offset::BODY_BYTES..offset::BODY_BYTES + 4].copy_from_slice(&7_u32.to_le_bytes());
    assert_eq!(Header::decode(&bytes), Err(DecodeError::BodyNotAligned));

    let mut bytes = encoded_request();
    bytes[offset::TRANSACTION_ID..offset::TRANSACTION_ID + 8].fill(0);
    assert_eq!(
        Header::decode(&bytes),
        Err(DecodeError::InvalidPacketRelationship)
    );

    let mut event = request();
    event.kind = PacketKind::Event;
    event.transaction_id = 1;
    event.deadline_ns = u64::MAX;
    assert_eq!(
        event.encode(&mut [0; NSWP_HEADER_BYTES]),
        Err(nswp_core::EncodeError::InvalidHeader(
            DecodeError::InvalidPacketRelationship
        ))
    );

    let mut failure = request();
    failure.kind = PacketKind::Response;
    failure.transport_status = TransportStatus::Unavailable;
    assert_eq!(
        failure.encode(&mut [0; NSWP_HEADER_BYTES]),
        Err(nswp_core::EncodeError::InvalidHeader(
            DecodeError::InvalidPacketRelationship
        ))
    );

    let mut cancel = request();
    cancel.kind = PacketKind::Cancel;
    assert_eq!(
        cancel.encode(&mut [0; NSWP_HEADER_BYTES]),
        Err(nswp_core::EncodeError::InvalidHeader(
            DecodeError::InvalidPacketRelationship
        ))
    );

    let mut sampled = request();
    sampled.flags = HeaderFlags::TRACE_SAMPLED;
    sampled.trace_id = [0; 16];
    assert_eq!(
        sampled.encode(&mut [0; NSWP_HEADER_BYTES]),
        Err(nswp_core::EncodeError::InvalidHeader(
            DecodeError::InvalidPacketRelationship
        ))
    );
}

#[test]
fn transport_and_bound_connection_limits_are_enforced() {
    let header = request();
    let state = bound();
    let context = |transport_bytes, attached_handles| ValidationContext {
        direction: Direction::ClientToServer,
        connection: &state,
        transport_bytes,
        attached_handles,
        transaction: TransactionState::Available {
            outstanding_count: 0,
        },
    };
    assert_eq!(
        header.validate_context(&context(NSWP_HEADER_BYTES + 16, 0)),
        Err(DecodeError::LengthMismatch)
    );
    assert_eq!(
        header.validate_context(&context(NSWP_HEADER_BYTES + 8, 1)),
        Err(DecodeError::HandleCountMismatch)
    );

    let mut oversized = header;
    oversized.body_bytes = 200;
    assert_eq!(
        oversized.validate_context(&ValidationContext {
            direction: Direction::ClientToServer,
            connection: &state,
            transport_bytes: NSWP_HEADER_BYTES + 200,
            attached_handles: 0,
            transaction: TransactionState::Available {
                outstanding_count: 0,
            },
        }),
        Err(DecodeError::BodyLimitExceeded)
    );
}

#[test]
fn direction_version_and_transaction_state_are_enforced() {
    let header = request();
    let state = bound();
    assert_eq!(
        header.validate(Direction::ServerToClient, &state),
        Err(DecodeError::InvalidDirection)
    );

    let mut wrong_version = header;
    wrong_version.protocol_minor = 3;
    assert_eq!(
        wrong_version.validate(Direction::ClientToServer, &state),
        Err(DecodeError::InvalidPacketRelationship)
    );

    let outstanding = OutstandingTransaction {
        transaction_id: header.transaction_id,
        ordinal: header.ordinal,
        deadline_ns: header.deadline_ns,
        trace_id: header.trace_id,
    };
    assert_eq!(
        header.validate_context(&ValidationContext {
            direction: Direction::ClientToServer,
            connection: &state,
            transport_bytes: NSWP_HEADER_BYTES + header.body_bytes as usize,
            attached_handles: 0,
            transaction: TransactionState::Outstanding(outstanding),
        }),
        Err(DecodeError::TransactionReuse)
    );
    assert_eq!(
        header.validate_context(&ValidationContext {
            direction: Direction::ClientToServer,
            connection: &state,
            transport_bytes: NSWP_HEADER_BYTES + header.body_bytes as usize,
            attached_handles: 0,
            transaction: TransactionState::Available {
                outstanding_count: 8,
            },
        }),
        Err(DecodeError::TransactionReuse)
    );
    assert_eq!(
        header.validate_context(&ValidationContext {
            direction: Direction::ClientToServer,
            connection: &state,
            transport_bytes: NSWP_HEADER_BYTES + header.body_bytes as usize,
            attached_handles: 0,
            transaction: TransactionState::Unknown,
        }),
        Err(DecodeError::TransactionMismatch)
    );

    let mut response = header;
    response.kind = PacketKind::Response;
    response.ordinal += 1;
    assert_eq!(
        response.validate_context(&ValidationContext {
            direction: Direction::ServerToClient,
            connection: &state,
            transport_bytes: NSWP_HEADER_BYTES + response.body_bytes as usize,
            attached_handles: 0,
            transaction: TransactionState::Outstanding(outstanding),
        }),
        Err(DecodeError::TransactionMismatch)
    );
    response.ordinal = outstanding.ordinal;
    assert!(
        response
            .validate_context(&ValidationContext {
                direction: Direction::ServerToClient,
                connection: &state,
                transport_bytes: NSWP_HEADER_BYTES + response.body_bytes as usize,
                attached_handles: 0,
                transaction: TransactionState::RecentlyCanceled(outstanding),
            })
            .is_ok()
    );

    let mut cancel = header;
    cancel.kind = PacketKind::Cancel;
    cancel.body_bytes = 0;
    assert!(
        cancel
            .validate_context(&ValidationContext {
                direction: Direction::ClientToServer,
                connection: &state,
                transport_bytes: NSWP_HEADER_BYTES,
                attached_handles: 0,
                transaction: TransactionState::Unknown,
            })
            .is_ok()
    );

    assert_eq!(
        response.validate_context(&ValidationContext {
            direction: Direction::ServerToClient,
            connection: &state,
            transport_bytes: NSWP_HEADER_BYTES + response.body_bytes as usize,
            attached_handles: 0,
            transaction: TransactionState::Unknown,
        }),
        Err(DecodeError::TransactionMismatch)
    );
}

#[test]
fn protocol_error_record_is_fixed_and_bound_to_header_relationships() {
    let header = Header {
        kind: PacketKind::ProtocolError,
        flags: HeaderFlags::NONE,
        protocol_major: 1,
        protocol_minor: 2,
        ordinal: 9,
        body_bytes: PROTOCOL_ERROR_BODY_BYTES as u32,
        handle_count: 0,
        transport_status: TransportStatus::Ok,
        transaction_id: 3,
        deadline_ns: 100,
        trace_id: TRACE,
    };
    let record = ProtocolErrorRecord {
        code: ProtocolErrorCode::InvalidTransaction,
        detail: 7,
        related_transaction_id: 3,
        related_ordinal: 9,
    };
    let mut body = [0; PROTOCOL_ERROR_BODY_BYTES];
    record.encode(&header, &mut body).unwrap();
    assert_eq!(ProtocolErrorRecord::decode(&header, &body), Ok(record));
    assert!(
        header
            .validate_context(&ValidationContext {
                direction: Direction::ServerToClient,
                connection: &bound(),
                transport_bytes: NSWP_HEADER_BYTES + PROTOCOL_ERROR_BODY_BYTES,
                attached_handles: 0,
                transaction: TransactionState::NotApplicable,
            })
            .is_ok()
    );

    body[20] = 1;
    assert_eq!(
        ProtocolErrorRecord::decode(&header, &body),
        Err(DecodeError::ReservedValueUsed)
    );
    let mut wrong_header = header;
    wrong_header.transaction_id += 1;
    assert_eq!(
        record.encode(&wrong_header, &mut body),
        Err(nswp_core::EncodeError::InvalidHeader(
            DecodeError::InvalidProtocolError
        ))
    );

    let mut missing_body = header;
    missing_body.body_bytes = 0;
    assert_eq!(
        missing_body.encode(&mut [0; NSWP_HEADER_BYTES]),
        Err(nswp_core::EncodeError::InvalidHeader(
            DecodeError::InvalidPacketRelationship
        ))
    );
}

#[test]
fn negotiation_packets_require_prebound_state_and_no_handles() {
    let header = Header {
        kind: PacketKind::NegotiateRequest,
        flags: HeaderFlags::NONE,
        protocol_major: 0,
        protocol_minor: 0,
        ordinal: 0,
        body_bytes: 48,
        handle_count: 0,
        transport_status: TransportStatus::Ok,
        transaction_id: 0,
        deadline_ns: u64::MAX,
        trace_id: [0; 16],
    };
    assert!(
        header
            .validate(Direction::ClientToServer, &ConnectionState::New)
            .is_ok()
    );

    let mut bytes = [0; NSWP_HEADER_BYTES];
    header.encode(&mut bytes).unwrap();
    bytes[offset::HANDLE_COUNT..offset::HANDLE_COUNT + 2].copy_from_slice(&1_u16.to_le_bytes());
    assert_eq!(
        Header::decode(&bytes),
        Err(DecodeError::InvalidPacketRelationship)
    );
}
