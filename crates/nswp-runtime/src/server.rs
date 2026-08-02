use nswp_core::{
    ConnectionState, Direction, FeatureRecord, Header, HeaderFlags, NegotiationRequest,
    NegotiationStatus, OutstandingTransaction, PacketKind, ProtocolErrorCode, ProtocolErrorRecord,
    TransactionState, TransportStatus, ValidationContext, negotiate,
};

use crate::{
    BodyBuf, BoundState, CloseReason, ConnectionPhase, MethodKind, PacketBuf, PeerContextId,
    ProtocolDescriptor, RuntimeError, TryRecvError, TrySendError, TryTransport,
    types::validate_deadline,
    wire::{body_from_packet, encode_packet, protocol_error_packet, response_packet},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancellationReason {
    Client,
    Deadline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestToken {
    transaction: OutstandingTransaction,
}

impl RequestToken {
    pub const fn transaction_id(self) -> u64 {
        self.transaction.transaction_id
    }

    pub const fn ordinal(self) -> u32 {
        self.transaction.ordinal
    }

    pub const fn deadline_ns(self) -> u64 {
        self.transaction.deadline_ns
    }

    pub const fn trace_id(self) -> [u8; 16] {
        self.transaction.trace_id
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerEvent {
    Bound(BoundState),
    Request {
        token: RequestToken,
        body: BodyBuf,
    },
    OneWay {
        peer_context: PeerContextId,
        ordinal: u32,
        trace_id: [u8; 16],
        body: BodyBuf,
    },
    Canceled {
        token: RequestToken,
        reason: CancellationReason,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ServerTransaction {
    transaction: OutstandingTransaction,
    pending_response: Option<PacketBuf>,
    cancellation_notified: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingNegotiation {
    packet: PacketBuf,
    bound: Option<BoundState>,
    rejection: Option<NegotiationStatus>,
}

pub struct Server<'a, T: TryTransport> {
    transport: T,
    protocol: ProtocolDescriptor<'a>,
    service_generation: u64,
    peer_context: PeerContextId,
    phase: ConnectionPhase,
    bound: Option<BoundState>,
    close_reason: Option<CloseReason>,
    pending_negotiation: Option<PendingNegotiation>,
    pending_control: Option<PacketBuf>,
    transactions: [Option<ServerTransaction>; 8],
}

impl<'a, T: TryTransport> Server<'a, T> {
    pub fn new(
        transport: T,
        protocol: ProtocolDescriptor<'a>,
        service_generation: u64,
    ) -> Result<Self, RuntimeError> {
        Self::new_with_peer_context(
            transport,
            protocol,
            service_generation,
            PeerContextId::UNSPECIFIED,
        )
    }

    pub fn new_with_peer_context(
        transport: T,
        protocol: ProtocolDescriptor<'a>,
        service_generation: u64,
        peer_context: PeerContextId,
    ) -> Result<Self, RuntimeError> {
        if service_generation == 0 {
            return Err(RuntimeError::InvalidState);
        }
        Ok(Self {
            transport,
            protocol,
            service_generation,
            peer_context,
            phase: ConnectionPhase::New,
            bound: None,
            close_reason: None,
            pending_negotiation: None,
            pending_control: None,
            transactions: [None; 8],
        })
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

    pub fn executing_count(&self) -> usize {
        self.transactions.iter().flatten().count()
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub fn poll(&mut self, now_ns: u64) -> Result<Option<ServerEvent>, RuntimeError> {
        if self.phase == ConnectionPhase::Closed {
            return Err(RuntimeError::Closed(
                self.close_reason.unwrap_or(CloseReason::LocalClosed),
            ));
        }
        if let Some(event) = self.flush_pending_negotiation()? {
            return Ok(Some(event));
        }
        if !self.flush_pending_control()? {
            return Ok(None);
        }
        self.flush_pending_responses()?;
        if let Some(index) = self.transactions.iter().position(|entry| {
            entry.is_some_and(|entry| {
                !entry.cancellation_notified
                    && entry.transaction.deadline_ns != u64::MAX
                    && now_ns >= entry.transaction.deadline_ns
            })
        }) {
            let entry = self.transactions[index]
                .as_mut()
                .ok_or(RuntimeError::InvalidState)?;
            entry.cancellation_notified = true;
            return Ok(Some(ServerEvent::Canceled {
                token: RequestToken {
                    transaction: entry.transaction,
                },
                reason: CancellationReason::Deadline,
            }));
        }

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
            ConnectionPhase::New => self.receive_negotiation(packet, header),
            ConnectionPhase::Bound => self.receive_bound(packet, header, now_ns),
            ConnectionPhase::Negotiating | ConnectionPhase::Closed => {
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

    pub fn respond(
        &mut self,
        token: RequestToken,
        status: TransportStatus,
        body: &[u8],
    ) -> Result<(), RuntimeError> {
        self.require_bound()?;
        let index = self
            .transactions
            .iter()
            .position(|entry| {
                entry.is_some_and(|entry| {
                    entry.transaction.transaction_id == token.transaction.transaction_id
                })
            })
            .ok_or(RuntimeError::UnknownTransaction)?;
        let entry = self.transactions[index].ok_or(RuntimeError::InvalidState)?;
        if entry.transaction != token.transaction || entry.pending_response.is_some() {
            return Err(RuntimeError::TransactionNotExecuting);
        }
        let bound = self.bound.ok_or(RuntimeError::InvalidState)?;
        if status == TransportStatus::Ok {
            let method = self
                .protocol
                .method(token.ordinal())
                .ok_or(RuntimeError::UnknownMethod)?;
            (method.validate_response)(body, &bound.view()?)?;
        } else if !body.is_empty() {
            return Err(RuntimeError::InvalidState);
        }
        let packet = response_packet(&bound, token.transaction, status, body)?;
        match self.transport.try_send(packet.as_slice()) {
            Ok(()) => self.transactions[index] = None,
            Err(TrySendError::Full) => {
                self.transactions[index]
                    .as_mut()
                    .ok_or(RuntimeError::InvalidState)?
                    .pending_response = Some(packet);
            }
            Err(TrySendError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                return Err(RuntimeError::PeerClosed);
            }
        }
        Ok(())
    }

    pub fn into_transport(self) -> T {
        self.transport
    }

    fn receive_negotiation(
        &mut self,
        packet: PacketBuf,
        header: Header,
    ) -> Result<Option<ServerEvent>, RuntimeError> {
        if header
            .validate_context(&ValidationContext {
                direction: Direction::ClientToServer,
                connection: &ConnectionState::New,
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
        let request = NegotiationRequest::decode(body.as_slice())?;
        if request.features().len() > crate::MAX_NEGOTIATED_FEATURES {
            return self.protocol_failure(Some(&header), ProtocolErrorCode::LimitExceeded);
        }
        let mut selected = [FeatureRecord::enabled(1); crate::MAX_NEGOTIATED_FEATURES];
        let outcome = negotiate(
            &request,
            &self.protocol.server_profile(self.service_generation),
            &mut selected,
        )?;
        let selected = &selected[..outcome.feature_count];
        let mut response_body = [0; crate::MAX_BODY_BYTES];
        let response_bytes = outcome.response.encode(selected, &mut response_body)?;
        let response_packet = encode_packet(
            Header {
                kind: PacketKind::NegotiateResponse,
                flags: HeaderFlags::NONE,
                protocol_major: 0,
                protocol_minor: 0,
                ordinal: 0,
                body_bytes: response_bytes as u32,
                handle_count: 0,
                transport_status: TransportStatus::Ok,
                transaction_id: 0,
                deadline_ns: u64::MAX,
                trace_id: [0; 16],
            },
            &response_body[..response_bytes],
        )?;
        let bound = if outcome.response.status == NegotiationStatus::Ok {
            Some(BoundState::from_parts(
                outcome.response.protocol_id,
                outcome.response.protocol_major,
                outcome.response.selected_minor,
                nswp_core::ConnectionLimits {
                    max_body_bytes: outcome.response.max_body_bytes,
                    max_handles: outcome.response.max_handles,
                    max_outstanding: outcome.response.max_outstanding,
                },
                outcome.response.service_generation,
                selected.iter().map(|feature| feature.id),
            )?)
        } else {
            None
        };
        match self.transport.try_send(response_packet.as_slice()) {
            Ok(()) => {
                if let Some(bound) = bound {
                    self.bound = Some(bound);
                    self.phase = ConnectionPhase::Bound;
                    Ok(Some(ServerEvent::Bound(bound)))
                } else {
                    let reason = CloseReason::NegotiationRejected(outcome.response.status);
                    self.close(reason);
                    Err(RuntimeError::Closed(reason))
                }
            }
            Err(TrySendError::Full) => {
                self.pending_negotiation = Some(PendingNegotiation {
                    packet: response_packet,
                    bound,
                    rejection: (outcome.response.status != NegotiationStatus::Ok)
                        .then_some(outcome.response.status),
                });
                self.phase = ConnectionPhase::Negotiating;
                Ok(None)
            }
            Err(TrySendError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                Err(RuntimeError::PeerClosed)
            }
        }
    }

    fn receive_bound(
        &mut self,
        packet: PacketBuf,
        header: Header,
        now_ns: u64,
    ) -> Result<Option<ServerEvent>, RuntimeError> {
        let bound = self.bound.ok_or(RuntimeError::InvalidState)?;
        match header.kind {
            PacketKind::Request => {
                if self
                    .transactions
                    .iter()
                    .flatten()
                    .any(|entry| entry.transaction.transaction_id == header.transaction_id)
                {
                    return self
                        .protocol_failure(Some(&header), ProtocolErrorCode::DuplicateTransaction);
                }
                let count = self.executing_count();
                let validation_count =
                    (count as u16).min(bound.limits().max_outstanding.saturating_sub(1));
                header.validate_context(&ValidationContext {
                    direction: Direction::ClientToServer,
                    connection: &ConnectionState::Bound(bound.view()?),
                    transport_bytes: packet.len(),
                    attached_handles: 0,
                    transaction: TransactionState::Available {
                        outstanding_count: validation_count,
                    },
                })?;
                let method = match self.protocol.method(header.ordinal) {
                    Some(method) => method,
                    None => {
                        return self
                            .protocol_failure(Some(&header), ProtocolErrorCode::UnknownOrdinal);
                    }
                };
                if method.kind != MethodKind::RequestResponse {
                    return self
                        .protocol_failure(Some(&header), ProtocolErrorCode::UnexpectedPacketKind);
                }
                let body = body_from_packet(&packet, &header)?;
                if (method.validate_request)(body.as_slice(), &bound.view()?).is_err() {
                    return self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidBody);
                }
                if validate_deadline(method.deadline, now_ns, header.deadline_ns).is_err() {
                    return self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidHeader);
                }
                let transaction = transaction_from_header(&header);
                if header.deadline_ns != u64::MAX && now_ns >= header.deadline_ns {
                    let response =
                        response_packet(&bound, transaction, TransportStatus::TimedOut, &[])?;
                    self.send_or_queue_control(response)?;
                    return Ok(None);
                }
                if count >= usize::from(bound.limits().max_outstanding)
                    || self.transactions.iter().all(Option::is_some)
                {
                    let response = response_packet(
                        &bound,
                        transaction,
                        TransportStatus::ResourceExhausted,
                        &[],
                    )?;
                    self.send_or_queue_control(response)?;
                    return Ok(None);
                }
                let slot = self
                    .transactions
                    .iter_mut()
                    .find(|entry| entry.is_none())
                    .ok_or(RuntimeError::OutstandingLimit)?;
                *slot = Some(ServerTransaction {
                    transaction,
                    pending_response: None,
                    cancellation_notified: false,
                });
                Ok(Some(ServerEvent::Request {
                    token: RequestToken { transaction },
                    body,
                }))
            }
            PacketKind::OneWay => {
                header.validate_context(&ValidationContext {
                    direction: Direction::ClientToServer,
                    connection: &ConnectionState::Bound(bound.view()?),
                    transport_bytes: packet.len(),
                    attached_handles: 0,
                    transaction: TransactionState::NotApplicable,
                })?;
                let method = match self.protocol.method(header.ordinal) {
                    Some(method) => method,
                    None => {
                        return self
                            .protocol_failure(Some(&header), ProtocolErrorCode::UnknownOrdinal);
                    }
                };
                if method.kind != MethodKind::OneWay {
                    return self
                        .protocol_failure(Some(&header), ProtocolErrorCode::UnexpectedPacketKind);
                }
                if validate_deadline(method.deadline, now_ns, header.deadline_ns).is_err() {
                    return self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidHeader);
                }
                let body = body_from_packet(&packet, &header)?;
                if (method.validate_request)(body.as_slice(), &bound.view()?).is_err() {
                    return self.protocol_failure(Some(&header), ProtocolErrorCode::InvalidBody);
                }
                if header.deadline_ns != u64::MAX && now_ns >= header.deadline_ns {
                    return Ok(None);
                }
                Ok(Some(ServerEvent::OneWay {
                    peer_context: self.peer_context,
                    ordinal: header.ordinal,
                    trace_id: header.trace_id,
                    body,
                }))
            }
            PacketKind::Cancel => {
                let index = self.transactions.iter().position(|entry| {
                    entry.is_some_and(|entry| {
                        entry.transaction.transaction_id == header.transaction_id
                    })
                });
                let state = match index {
                    Some(index) => TransactionState::Outstanding(
                        self.transactions[index]
                            .ok_or(RuntimeError::InvalidState)?
                            .transaction,
                    ),
                    None => TransactionState::Unknown,
                };
                header.validate_context(&ValidationContext {
                    direction: Direction::ClientToServer,
                    connection: &ConnectionState::Bound(bound.view()?),
                    transport_bytes: packet.len(),
                    attached_handles: 0,
                    transaction: state,
                })?;
                let Some(index) = index else {
                    return Ok(None);
                };
                let entry = self.transactions[index]
                    .as_mut()
                    .ok_or(RuntimeError::InvalidState)?;
                if entry.cancellation_notified {
                    return Ok(None);
                }
                entry.cancellation_notified = true;
                Ok(Some(ServerEvent::Canceled {
                    token: RequestToken {
                        transaction: entry.transaction,
                    },
                    reason: CancellationReason::Client,
                }))
            }
            PacketKind::ProtocolError => {
                header.validate_context(&ValidationContext {
                    direction: Direction::ClientToServer,
                    connection: &ConnectionState::Bound(bound.view()?),
                    transport_bytes: packet.len(),
                    attached_handles: 0,
                    transaction: TransactionState::NotApplicable,
                })?;
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

    fn flush_pending_negotiation(&mut self) -> Result<Option<ServerEvent>, RuntimeError> {
        let Some(pending) = self.pending_negotiation else {
            return Ok(None);
        };
        match self.transport.try_send(pending.packet.as_slice()) {
            Ok(()) => {
                self.pending_negotiation = None;
                if let Some(bound) = pending.bound {
                    self.bound = Some(bound);
                    self.phase = ConnectionPhase::Bound;
                    Ok(Some(ServerEvent::Bound(bound)))
                } else {
                    let reason = CloseReason::NegotiationRejected(
                        pending.rejection.ok_or(RuntimeError::InvalidState)?,
                    );
                    self.close(reason);
                    Err(RuntimeError::Closed(reason))
                }
            }
            Err(TrySendError::Full) => Ok(None),
            Err(TrySendError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                Err(RuntimeError::PeerClosed)
            }
        }
    }

    fn send_or_queue_control(&mut self, packet: PacketBuf) -> Result<(), RuntimeError> {
        if self.pending_control.is_some() {
            return Err(RuntimeError::InvalidState);
        }
        match self.transport.try_send(packet.as_slice()) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full) => {
                self.pending_control = Some(packet);
                Ok(())
            }
            Err(TrySendError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                Err(RuntimeError::PeerClosed)
            }
        }
    }

    fn flush_pending_control(&mut self) -> Result<bool, RuntimeError> {
        let Some(packet) = self.pending_control else {
            return Ok(true);
        };
        match self.transport.try_send(packet.as_slice()) {
            Ok(()) => {
                self.pending_control = None;
                Ok(true)
            }
            Err(TrySendError::Full) => Ok(false),
            Err(TrySendError::PeerClosed) => {
                self.close(CloseReason::PeerClosed);
                Err(RuntimeError::PeerClosed)
            }
        }
    }

    fn flush_pending_responses(&mut self) -> Result<(), RuntimeError> {
        for index in 0..self.transactions.len() {
            let Some(packet) = self.transactions[index].and_then(|entry| entry.pending_response)
            else {
                continue;
            };
            match self.transport.try_send(packet.as_slice()) {
                Ok(()) => self.transactions[index] = None,
                Err(TrySendError::Full) => break,
                Err(TrySendError::PeerClosed) => {
                    self.close(CloseReason::PeerClosed);
                    return Err(RuntimeError::PeerClosed);
                }
            }
        }
        Ok(())
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

    fn require_bound(&self) -> Result<(), RuntimeError> {
        if self.phase == ConnectionPhase::Bound {
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

fn transaction_from_header(header: &Header) -> OutstandingTransaction {
    OutstandingTransaction {
        transaction_id: header.transaction_id,
        ordinal: header.ordinal,
        deadline_ns: header.deadline_ns,
        trace_id: header.trace_id,
    }
}
