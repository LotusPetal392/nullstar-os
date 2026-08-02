use nswp_core::{
    ConnectionState, Direction, Header, HeaderFlags, NegotiationRequest, NegotiationResponse,
    NegotiationStatus, OutstandingTransaction, PacketKind, ProtocolErrorCode, ProtocolErrorRecord,
    TransactionState, TransportStatus, ValidationContext,
};

use crate::{
    BodyBuf, BoundState, CloseReason, ConnectionPhase, PacketBuf, ProtocolDescriptor, RuntimeError,
    TryRecvError, TrySendError, TryTransport,
    types::validate_deadline,
    wire::{body_from_packet, encode_packet, protocol_error_packet},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelDisposition {
    Queued,
    PendingBackpressure,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClientEvent {
    Bound(BoundState),
    Response {
        transaction_id: u64,
        status: TransportStatus,
        body: BodyBuf,
    },
    LateResponseDrained {
        transaction_id: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RecentCanceled {
    transaction: OutstandingTransaction,
    pending_cancel: Option<PacketBuf>,
}

pub struct Client<'a, T: TryTransport> {
    transport: T,
    protocol: ProtocolDescriptor<'a>,
    phase: ConnectionPhase,
    bound: Option<BoundState>,
    close_reason: Option<CloseReason>,
    negotiation_request: Option<PacketBuf>,
    outstanding: [Option<OutstandingTransaction>; 8],
    recent: [Option<RecentCanceled>; 8],
    next_transaction_id: u64,
}

impl<'a, T: TryTransport> Client<'a, T> {
    pub fn new(transport: T, protocol: ProtocolDescriptor<'a>) -> Self {
        Self {
            transport,
            protocol,
            phase: ConnectionPhase::New,
            bound: None,
            close_reason: None,
            negotiation_request: None,
            outstanding: [None; 8],
            recent: [None; 8],
            next_transaction_id: 1,
        }
    }

    pub const fn phase(&self) -> ConnectionPhase {
        self.phase
    }

    pub const fn bound(&self) -> Option<&BoundState> {
        self.bound.as_ref()
    }

    pub const fn close_reason(&self) -> Option<CloseReason> {
        self.close_reason
    }

    pub fn outstanding_count(&self) -> usize {
        self.outstanding.iter().flatten().count()
    }

    pub fn recently_canceled_count(&self) -> usize {
        self.recent.iter().flatten().count()
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn try_negotiate(&mut self) -> Result<(), RuntimeError> {
        self.require_phase(ConnectionPhase::New)?;
        if self.protocol.requested_features.len() > crate::MAX_NEGOTIATED_FEATURES {
            return Err(RuntimeError::TooManyFeatures);
        }
        let request = NegotiationRequest {
            protocol_id: self.protocol.protocol_id,
            protocol_major: self.protocol.major,
            min_minor: self.protocol.min_minor,
            max_minor: self.protocol.max_minor,
            max_body_bytes: self.protocol.limits.max_body_bytes,
            max_handles: 0,
            max_outstanding: self.protocol.limits.max_outstanding,
        };
        let mut body = [0; crate::MAX_BODY_BYTES];
        let body_bytes = request.encode(self.protocol.requested_features, &mut body)?;
        let packet = encode_packet(
            Header {
                kind: PacketKind::NegotiateRequest,
                flags: HeaderFlags::NONE,
                protocol_major: 0,
                protocol_minor: 0,
                ordinal: 0,
                body_bytes: body_bytes as u32,
                handle_count: 0,
                transport_status: TransportStatus::Ok,
                transaction_id: 0,
                deadline_ns: u64::MAX,
                trace_id: [0; 16],
            },
            &body[..body_bytes],
        )?;
        match self.transport.try_send(packet.as_slice()) {
            Ok(()) => {
                self.negotiation_request = Some(packet);
                self.phase = ConnectionPhase::Negotiating;
                Ok(())
            }
            Err(TrySendError::Full) => Err(RuntimeError::WouldBlock),
            Err(TrySendError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                Err(RuntimeError::PeerClosed)
            }
        }
    }

    pub fn try_call(
        &mut self,
        ordinal: u32,
        body: &[u8],
        now_ns: u64,
        deadline_ns: u64,
        trace_id: [u8; 16],
    ) -> Result<u64, RuntimeError> {
        self.require_phase(ConnectionPhase::Bound)?;
        if !self.flush_pending_cancel()? {
            return Err(RuntimeError::WouldBlock);
        }
        let bound = self.bound.ok_or(RuntimeError::InvalidState)?;
        let method = self
            .protocol
            .method(ordinal)
            .ok_or(RuntimeError::UnknownMethod)?;
        validate_deadline(method.deadline, now_ns, deadline_ns)?;
        (method.validate_request)(body, &bound.view()?)?;
        if self.outstanding_count() >= usize::from(bound.limits().max_outstanding)
            || self.outstanding.iter().all(Option::is_some)
        {
            return Err(RuntimeError::OutstandingLimit);
        }
        if self.next_transaction_id == 0 {
            self.close(CloseReason::TransactionIdExhausted);
            return Err(RuntimeError::Closed(CloseReason::TransactionIdExhausted));
        }
        let transaction = OutstandingTransaction {
            transaction_id: self.next_transaction_id,
            ordinal,
            deadline_ns,
            trace_id,
        };
        let packet = encode_packet(
            Header {
                kind: PacketKind::Request,
                flags: if trace_id == [0; 16] {
                    HeaderFlags::NONE
                } else {
                    HeaderFlags::TRACE_SAMPLED
                },
                protocol_major: bound.major(),
                protocol_minor: bound.minor(),
                ordinal,
                body_bytes: body.len() as u32,
                handle_count: 0,
                transport_status: TransportStatus::Ok,
                transaction_id: transaction.transaction_id,
                deadline_ns,
                trace_id,
            },
            body,
        )?;
        match self.transport.try_send(packet.as_slice()) {
            Ok(()) => {
                let slot = self
                    .outstanding
                    .iter_mut()
                    .find(|entry| entry.is_none())
                    .ok_or(RuntimeError::OutstandingLimit)?;
                *slot = Some(transaction);
                self.next_transaction_id = self.next_transaction_id.wrapping_add(1);
                Ok(transaction.transaction_id)
            }
            Err(TrySendError::Full) => Err(RuntimeError::WouldBlock),
            Err(TrySendError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                Err(RuntimeError::PeerClosed)
            }
        }
    }

    pub fn try_cancel(&mut self, transaction_id: u64) -> Result<CancelDisposition, RuntimeError> {
        self.require_phase(ConnectionPhase::Bound)?;
        let can_send = self.flush_pending_cancel()?;
        let recent_slot = self
            .recent
            .iter()
            .position(Option::is_none)
            .ok_or_else(|| {
                self.close(CloseReason::RecentlyCanceledExhausted);
                RuntimeError::RecentlyCanceledExhausted
            })?;
        let outstanding_slot = self
            .outstanding
            .iter()
            .position(|entry| {
                entry.is_some_and(|transaction| transaction.transaction_id == transaction_id)
            })
            .ok_or(RuntimeError::UnknownTransaction)?;
        let transaction = self.outstanding[outstanding_slot].ok_or(RuntimeError::InvalidState)?;
        let bound = self.bound.ok_or(RuntimeError::InvalidState)?;
        let packet = encode_packet(
            Header {
                kind: PacketKind::Cancel,
                flags: if transaction.trace_id == [0; 16] {
                    HeaderFlags::NONE
                } else {
                    HeaderFlags::TRACE_SAMPLED
                },
                protocol_major: bound.major(),
                protocol_minor: bound.minor(),
                ordinal: transaction.ordinal,
                body_bytes: 0,
                handle_count: 0,
                transport_status: TransportStatus::Ok,
                transaction_id,
                deadline_ns: transaction.deadline_ns,
                trace_id: transaction.trace_id,
            },
            &[],
        )?;
        let disposition = if can_send {
            match self.transport.try_send(packet.as_slice()) {
                Ok(()) => CancelDisposition::Queued,
                Err(TrySendError::Full) => CancelDisposition::PendingBackpressure,
                Err(TrySendError::PeerClosed) => {
                    self.close(CloseReason::PeerClosed);
                    return Err(RuntimeError::PeerClosed);
                }
            }
        } else {
            CancelDisposition::PendingBackpressure
        };
        self.outstanding[outstanding_slot] = None;
        self.recent[recent_slot] = Some(RecentCanceled {
            transaction,
            pending_cancel: (disposition == CancelDisposition::PendingBackpressure)
                .then_some(packet),
        });
        Ok(disposition)
    }

    pub fn tick(&mut self, now_ns: u64) -> Result<usize, RuntimeError> {
        self.require_phase(ConnectionPhase::Bound)?;
        let mut expired = [0; 8];
        let mut count = 0;
        for transaction in self.outstanding.iter().flatten() {
            if transaction.deadline_ns != u64::MAX && now_ns >= transaction.deadline_ns {
                expired[count] = transaction.transaction_id;
                count += 1;
            }
        }
        let mut canceled = 0;
        for transaction_id in expired[..count].iter().copied() {
            match self.try_cancel(transaction_id) {
                Ok(_) => canceled += 1,
                Err(RuntimeError::RecentlyCanceledExhausted) => {
                    return Err(RuntimeError::RecentlyCanceledExhausted);
                }
                Err(error) => return Err(error),
            }
        }
        Ok(canceled)
    }

    pub fn poll(&mut self) -> Result<Option<ClientEvent>, RuntimeError> {
        if self.phase == ConnectionPhase::Closed {
            return Err(RuntimeError::Closed(
                self.close_reason.unwrap_or(CloseReason::LocalClosed),
            ));
        }
        self.flush_pending_cancel()?;
        let mut packet = PacketBuf::new();
        let bytes = match self.transport.try_recv(packet.as_mut_capacity()) {
            Ok(bytes) => bytes,
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                return Err(RuntimeError::PeerClosed);
            }
            Err(TryRecvError::MessageTooLarge { .. }) => {
                return self.protocol_failure(None, ProtocolErrorCode::LimitExceeded);
            }
        };
        packet.set_len(bytes)?;
        let header = match Header::decode_prefix(packet.as_slice()) {
            Ok(header) => header,
            Err(_) => return self.protocol_failure(None, ProtocolErrorCode::InvalidHeader),
        };
        let result = match self.phase {
            ConnectionPhase::Negotiating => self.receive_negotiation(packet, header),
            ConnectionPhase::Bound => self.receive_bound(packet, header),
            ConnectionPhase::New | ConnectionPhase::Closed => {
                self.protocol_failure(Some(&header), ProtocolErrorCode::UnexpectedPacketKind)
            }
        };
        match result {
            Err(RuntimeError::Decode(_)) | Err(RuntimeError::Negotiation(_)) => {
                self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidHeader)
            }
            Err(RuntimeError::Body(_)) => {
                self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidBody)
            }
            Err(RuntimeError::TooManyFeatures) => {
                self.protocol_failure(Some(&header), ProtocolErrorCode::LimitExceeded)
            }
            result => result,
        }
    }

    pub fn service_replaced(&mut self) {
        self.close(CloseReason::ServiceGenerationReplaced);
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    fn receive_negotiation(
        &mut self,
        packet: PacketBuf,
        header: Header,
    ) -> Result<Option<ClientEvent>, RuntimeError> {
        if header
            .validate_context(&ValidationContext {
                direction: Direction::ServerToClient,
                connection: &ConnectionState::Negotiating,
                transport_bytes: packet.len(),
                attached_handles: 0,
                transaction: TransactionState::NotApplicable,
            })
            .is_err()
        {
            return self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidHeader);
        }
        let body = body_from_packet(&packet, &header)?;
        if header.kind == PacketKind::ProtocolError {
            let encoded: &[u8; nswp_core::PROTOCOL_ERROR_BODY_BYTES] =
                body.as_slice()
                    .try_into()
                    .map_err(|_| RuntimeError::InvalidState)?;
            ProtocolErrorRecord::decode(&header, encoded)?;
            self.close(CloseReason::ProtocolError);
            return Err(RuntimeError::Closed(CloseReason::ProtocolError));
        }
        let response = NegotiationResponse::decode(body.as_slice())?;
        let request_packet = self.negotiation_request.ok_or(RuntimeError::InvalidState)?;
        let request_header = Header::decode_prefix(request_packet.as_slice())?;
        let request_body = body_from_packet(&request_packet, &request_header)?;
        let request = NegotiationRequest::decode(request_body.as_slice())?;
        response.validate_against(
            &request,
            self.protocol.available_features,
            self.protocol.feature_set_fits,
        )?;
        if response.root().status != NegotiationStatus::Ok {
            let reason = CloseReason::NegotiationRejected(response.root().status);
            self.close(reason);
            return Err(RuntimeError::Closed(reason));
        }
        let root = response.root();
        let bound = BoundState::from_parts(
            root.protocol_id,
            root.protocol_major,
            root.selected_minor,
            nswp_core::ConnectionLimits {
                max_body_bytes: root.max_body_bytes,
                max_handles: root.max_handles,
                max_outstanding: root.max_outstanding,
            },
            root.service_generation,
            response.features().iter().map(|feature| feature.id),
        )?;
        self.bound = Some(bound);
        self.phase = ConnectionPhase::Bound;
        self.negotiation_request = None;
        Ok(Some(ClientEvent::Bound(bound)))
    }

    fn receive_bound(
        &mut self,
        packet: PacketBuf,
        header: Header,
    ) -> Result<Option<ClientEvent>, RuntimeError> {
        let bound = self.bound.ok_or(RuntimeError::InvalidState)?;
        match header.kind {
            PacketKind::Response => {
                if let Some(index) = self.outstanding.iter().position(|entry| {
                    entry.is_some_and(|transaction| {
                        transaction.transaction_id == header.transaction_id
                    })
                }) {
                    let transaction = self.outstanding[index].ok_or(RuntimeError::InvalidState)?;
                    self.validate_response(&packet, &header, transaction, false)?;
                    let body = body_from_packet(&packet, &header)?;
                    if header.transport_status == TransportStatus::Ok {
                        let method = self
                            .protocol
                            .method(header.ordinal)
                            .ok_or(RuntimeError::UnknownMethod)?;
                        (method.validate_response)(body.as_slice(), &bound.view()?)?;
                    }
                    self.outstanding[index] = None;
                    Ok(Some(ClientEvent::Response {
                        transaction_id: header.transaction_id,
                        status: header.transport_status,
                        body,
                    }))
                } else if let Some(index) = self.recent.iter().position(|entry| {
                    entry.is_some_and(|recent| {
                        recent.transaction.transaction_id == header.transaction_id
                    })
                }) {
                    let recent = self.recent[index].ok_or(RuntimeError::InvalidState)?;
                    self.validate_response(&packet, &header, recent.transaction, true)?;
                    let body = body_from_packet(&packet, &header)?;
                    if header.transport_status == TransportStatus::Ok {
                        let method = self
                            .protocol
                            .method(header.ordinal)
                            .ok_or(RuntimeError::UnknownMethod)?;
                        (method.validate_response)(body.as_slice(), &bound.view()?)?;
                    }
                    self.recent[index] = None;
                    Ok(Some(ClientEvent::LateResponseDrained {
                        transaction_id: header.transaction_id,
                    }))
                } else {
                    self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidTransaction)
                }
            }
            PacketKind::ProtocolError => {
                if header
                    .validate_context(&ValidationContext {
                        direction: Direction::ServerToClient,
                        connection: &ConnectionState::Bound(bound.view()?),
                        transport_bytes: packet.len(),
                        attached_handles: 0,
                        transaction: TransactionState::NotApplicable,
                    })
                    .is_err()
                {
                    return self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidHeader);
                }
                let body = body_from_packet(&packet, &header)?;
                let encoded: &[u8; nswp_core::PROTOCOL_ERROR_BODY_BYTES] = body
                    .as_slice()
                    .try_into()
                    .map_err(|_| RuntimeError::InvalidState)?;
                ProtocolErrorRecord::decode(&header, encoded)?;
                self.close(CloseReason::ProtocolError);
                Err(RuntimeError::Closed(CloseReason::ProtocolError))
            }
            _ => self.protocol_failure(Some(&header), ProtocolErrorCode::UnexpectedPacketKind),
        }
    }

    fn validate_response(
        &self,
        packet: &PacketBuf,
        header: &Header,
        transaction: OutstandingTransaction,
        recent: bool,
    ) -> Result<(), RuntimeError> {
        let bound = self.bound.ok_or(RuntimeError::InvalidState)?;
        header.validate_context(&ValidationContext {
            direction: Direction::ServerToClient,
            connection: &ConnectionState::Bound(bound.view()?),
            transport_bytes: packet.len(),
            attached_handles: 0,
            transaction: if recent {
                TransactionState::RecentlyCanceled(transaction)
            } else {
                TransactionState::Outstanding(transaction)
            },
        })?;
        Ok(())
    }

    fn flush_pending_cancel(&mut self) -> Result<bool, RuntimeError> {
        for recent in self.recent.iter_mut().flatten() {
            let Some(packet) = recent.pending_cancel else {
                continue;
            };
            match self.transport.try_send(packet.as_slice()) {
                Ok(()) => recent.pending_cancel = None,
                Err(TrySendError::Full) => return Ok(false),
                Err(TrySendError::PeerClosed) => {
                    self.close(CloseReason::PeerClosed);
                    return Err(RuntimeError::PeerClosed);
                }
            }
        }
        Ok(true)
    }

    fn protocol_failure<R>(
        &mut self,
        related: Option<&Header>,
        code: ProtocolErrorCode,
    ) -> Result<R, RuntimeError> {
        if let Ok(packet) = protocol_error_packet(self.bound.as_ref(), related, code) {
            let _ = self.transport.try_send(packet.as_slice());
        }
        self.close(CloseReason::ProtocolError);
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    }

    fn require_phase(&self, expected: ConnectionPhase) -> Result<(), RuntimeError> {
        if self.phase == expected {
            Ok(())
        } else if self.phase == ConnectionPhase::Closed {
            Err(RuntimeError::Closed(
                self.close_reason.unwrap_or(CloseReason::LocalClosed),
            ))
        } else {
            Err(RuntimeError::InvalidState)
        }
    }

    fn close(&mut self, reason: CloseReason) {
        if self.phase != ConnectionPhase::Closed {
            self.phase = ConnectionPhase::Closed;
            self.close_reason = Some(reason);
            self.transport.close();
        }
    }
}
