use nswp_core::{
    FeatureRecord, Header, HeaderFlags, NSWP_HEADER_BYTES, PacketKind, TransportStatus,
};
use nswp_runtime::{
    CancelDisposition, Client, ClientEvent, CloseReason, ConnectionPhase, MAX_PACKET_BYTES,
    RuntimeError, Server, ServerEvent, TryTransport,
};
use nswp_testkit::{
    ECHO_HI_7_BODY, ECHO_PING_ORDINAL, EchoService, SimEndpoint, channel_pair, echo_protocol,
    encode_echo,
};

type Endpoint = SimEndpoint<16>;
type EchoClient = Client<'static, Endpoint>;
type EchoServer = Server<'static, Endpoint>;

fn connected(generation: u64) -> (EchoClient, EchoServer, Endpoint, Endpoint) {
    let (client_endpoint, server_endpoint) = channel_pair::<16>();
    let client_control = client_endpoint.clone();
    let server_control = server_endpoint.clone();
    let mut client = Client::new(client_endpoint, echo_protocol());
    let mut server = Server::new(server_endpoint, echo_protocol(), generation).unwrap();

    assert_eq!(client.phase(), ConnectionPhase::New);
    client.try_negotiate().unwrap();
    assert_eq!(client.phase(), ConnectionPhase::Negotiating);
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Bound(bound)) if bound.service_generation() == generation
    ));
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Bound(bound)) if bound.service_generation() == generation
    ));
    assert_eq!(client.phase(), ConnectionPhase::Bound);
    assert_eq!(server.phase(), ConnectionPhase::Bound);
    (client, server, client_control, server_control)
}

fn call<T: TryTransport>(client: &mut Client<'static, T>, sequence: u64, deadline_ns: u64) -> u64 {
    let body = encode_echo("hi", sequence).unwrap();
    client
        .try_call(ECHO_PING_ORDINAL, body.as_slice(), 0, deadline_ns, [0; 16])
        .unwrap()
}

fn request_packet(bound: &nswp_runtime::BoundState, transaction_id: u64, ordinal: u32) -> Vec<u8> {
    let body = encode_echo("hi", transaction_id).unwrap();
    let header = Header {
        kind: PacketKind::Request,
        flags: HeaderFlags::NONE,
        protocol_major: bound.major(),
        protocol_minor: bound.minor(),
        ordinal,
        body_bytes: body.len() as u32,
        handle_count: 0,
        transport_status: TransportStatus::Ok,
        transaction_id,
        deadline_ns: u64::MAX,
        trace_id: [0; 16],
    };
    let mut packet = vec![0; NSWP_HEADER_BYTES + body.len()];
    let mut encoded_header = [0; NSWP_HEADER_BYTES];
    header.encode(&mut encoded_header).unwrap();
    packet[..NSWP_HEADER_BYTES].copy_from_slice(&encoded_header);
    packet[NSWP_HEADER_BYTES..].copy_from_slice(body.as_slice());
    packet
}

#[test]
fn server_binds_only_after_negotiation_response_is_queued() {
    let (client_endpoint, server_endpoint) = channel_pair::<1>();
    let client_control = client_endpoint.clone();
    let mut client = Client::new(client_endpoint, echo_protocol());
    let mut server = Server::new(server_endpoint, echo_protocol(), 40).unwrap();

    client.try_negotiate().unwrap();
    assert!(client_control.inject_incoming(vec![0]));
    assert_eq!(server.poll(0).unwrap(), None);
    assert_eq!(server.phase(), ConnectionPhase::Negotiating);
    assert!(server.bound().is_none());

    assert_eq!(client_control.discard_incoming(), Some(vec![0]));
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Bound(bound)) if bound.service_generation() == 40
    ));
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Bound(_))
    ));
}

#[test]
fn client_rejects_feature_sets_larger_than_the_runtime_profile() {
    let features: [FeatureRecord; 17] =
        core::array::from_fn(|index| FeatureRecord::optional(index as u32 + 1));
    let mut protocol = echo_protocol();
    protocol.requested_features = &features;
    let (client_endpoint, _server_endpoint) = channel_pair::<1>();
    let mut client = Client::new(client_endpoint, protocol);
    assert_eq!(client.try_negotiate(), Err(RuntimeError::TooManyFeatures));
    assert_eq!(client.phase(), ConnectionPhase::New);
}

#[test]
fn negotiation_binds_one_immutable_service_generation() {
    let (mut client, _server, _, _) = connected(41);
    let bound = *client.bound().unwrap();
    assert_eq!(bound.service_generation(), 41);
    assert_eq!(bound.limits().max_body_bytes, 192);
    assert_eq!(bound.limits().max_handles, 0);
    assert_eq!(bound.limits().max_outstanding, 8);

    client.service_replaced();
    assert_eq!(client.phase(), ConnectionPhase::Closed);
    assert_eq!(
        client.close_reason(),
        Some(CloseReason::ServiceGenerationReplaced)
    );
    assert_eq!(bound.service_generation(), 41);
}

#[test]
fn queue_full_is_atomic_for_negotiation_and_calls() {
    let (client_endpoint, server_endpoint) = channel_pair::<1>();
    assert!(server_endpoint.inject_incoming(vec![1]));
    let mut client = Client::new(client_endpoint, echo_protocol());
    assert_eq!(client.try_negotiate(), Err(RuntimeError::WouldBlock));
    assert_eq!(client.phase(), ConnectionPhase::New);

    let (mut client, _server, _, server_control) = connected(2);
    for _ in 0..16 {
        assert!(server_control.inject_incoming(vec![0]));
    }
    let body = encode_echo("hi", 1).unwrap();
    assert_eq!(
        client.try_call(ECHO_PING_ORDINAL, body.as_slice(), 0, u64::MAX, [0; 16]),
        Err(RuntimeError::WouldBlock)
    );
    assert_eq!(client.outstanding_count(), 0);
}

#[test]
fn client_enforces_the_eight_call_limit() {
    let (mut client, _server, _, _) = connected(3);
    for sequence in 0..8 {
        assert_eq!(call(&mut client, sequence, u64::MAX), sequence + 1);
    }
    let body = encode_echo("hi", 9).unwrap();
    assert_eq!(
        client.try_call(ECHO_PING_ORDINAL, body.as_slice(), 0, u64::MAX, [0; 16]),
        Err(RuntimeError::OutstandingLimit)
    );
    assert_eq!(client.outstanding_count(), 8);
}

#[test]
fn full_server_validates_requests_and_retains_overload_response() {
    let (mut client, mut server, client_control, server_control) = connected(18);
    for sequence in 0..8 {
        call(&mut client, sequence, u64::MAX);
        assert!(matches!(
            server.poll(0).unwrap(),
            Some(ServerEvent::Request { .. })
        ));
    }
    let bound = *server.bound().unwrap();
    for _ in 0..16 {
        assert!(client_control.inject_incoming(vec![0]));
    }
    assert!(server_control.inject_incoming(request_packet(&bound, 999, ECHO_PING_ORDINAL,)));
    assert_eq!(server.poll(0).unwrap(), None);
    assert_eq!(server.phase(), ConnectionPhase::Bound);
    assert_eq!(client_control.queued_incoming(), 16);

    for _ in 0..16 {
        assert_eq!(client_control.discard_incoming(), Some(vec![0]));
    }
    assert_eq!(server.poll(0).unwrap(), None);
    let response = client_control.incoming_packet(0).unwrap();
    let header = Header::decode_prefix(&response).unwrap();
    assert_eq!(header.transaction_id, 999);
    assert_eq!(header.transport_status, TransportStatus::ResourceExhausted);

    let (mut client, mut server, _, server_control) = connected(19);
    for sequence in 0..8 {
        call(&mut client, sequence, u64::MAX);
        assert!(matches!(
            server.poll(0).unwrap(),
            Some(ServerEvent::Request { .. })
        ));
    }
    let bound = *server.bound().unwrap();
    assert!(server_control.inject_incoming(request_packet(&bound, 999, 99)));
    assert_eq!(
        server.poll(0),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );
}

#[test]
fn responses_can_complete_out_of_order() {
    let (mut client, mut server, _, _) = connected(4);
    let first_id = call(&mut client, 1, u64::MAX);
    let second_id = call(&mut client, 2, u64::MAX);
    let first = match server.poll(0).unwrap().unwrap() {
        ServerEvent::Request { token, body } => (token, body),
        event => panic!("unexpected event: {event:?}"),
    };
    let second = match server.poll(0).unwrap().unwrap() {
        ServerEvent::Request { token, body } => (token, body),
        event => panic!("unexpected event: {event:?}"),
    };

    server
        .respond(second.0, TransportStatus::Ok, second.1.as_slice())
        .unwrap();
    server
        .respond(first.0, TransportStatus::Ok, first.1.as_slice())
        .unwrap();

    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Response { transaction_id, .. }) if transaction_id == second_id
    ));
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Response { transaction_id, .. }) if transaction_id == first_id
    ));
}

#[test]
fn canceled_late_response_is_validated_and_drained() {
    let (mut client, mut server, _, _) = connected(5);
    let transaction_id = call(&mut client, 1, u64::MAX);
    let (token, body) = match server.poll(0).unwrap().unwrap() {
        ServerEvent::Request { token, body } => (token, body),
        event => panic!("unexpected event: {event:?}"),
    };
    assert_eq!(
        client.try_cancel(transaction_id).unwrap(),
        CancelDisposition::Queued
    );
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Canceled { token: canceled, .. })
            if canceled.transaction_id() == transaction_id
    ));
    server
        .respond(token, TransportStatus::Ok, body.as_slice())
        .unwrap();
    assert_eq!(
        client.poll().unwrap(),
        Some(ClientEvent::LateResponseDrained { transaction_id })
    );
    assert_eq!(client.recently_canceled_count(), 0);
}

#[test]
fn cancel_is_retained_while_the_queue_is_full() {
    let (client_endpoint, server_endpoint) = channel_pair::<1>();
    let mut client = Client::new(client_endpoint, echo_protocol());
    let mut server = Server::new(server_endpoint, echo_protocol(), 6).unwrap();
    client.try_negotiate().unwrap();
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Bound(_))
    ));
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Bound(_))
    ));

    let first_id = call(&mut client, 1, u64::MAX);
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Request { .. })
    ));
    call(&mut client, 2, u64::MAX);
    assert_eq!(
        client.try_cancel(first_id).unwrap(),
        CancelDisposition::PendingBackpressure
    );
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Request { .. })
    ));
    let third = encode_echo("hi", 3).unwrap();
    assert_eq!(
        client.try_call(ECHO_PING_ORDINAL, third.as_slice(), 0, u64::MAX, [0; 16],),
        Err(RuntimeError::WouldBlock)
    );
    assert_eq!(client.outstanding_count(), 1);
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Canceled { token, .. }) if token.transaction_id() == first_id
    ));
}

#[test]
fn explicit_close_reason_wins_over_pending_cancel_flush() {
    let (client_endpoint, server_endpoint) = channel_pair::<1>();
    let mut client = Client::new(client_endpoint, echo_protocol());
    let mut server = Server::new(server_endpoint, echo_protocol(), 20).unwrap();
    client.try_negotiate().unwrap();
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Bound(_))
    ));
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Bound(_))
    ));

    let first_id = call(&mut client, 1, u64::MAX);
    assert!(matches!(
        server.poll(0).unwrap(),
        Some(ServerEvent::Request { .. })
    ));
    call(&mut client, 2, u64::MAX);
    assert_eq!(
        client.try_cancel(first_id).unwrap(),
        CancelDisposition::PendingBackpressure
    );
    client.service_replaced();
    assert_eq!(
        client.poll(),
        Err(RuntimeError::Closed(CloseReason::ServiceGenerationReplaced))
    );
}

#[test]
fn expired_request_never_reaches_echo_service() {
    let (mut client, mut server, _, _) = connected(7);
    let mut service = EchoService::new();
    let transaction_id = call(&mut client, 1, 100);
    assert_eq!(server.poll(100).unwrap(), None);
    assert_eq!(service.dispatch_count(), 0);
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Response {
            transaction_id: id,
            status: TransportStatus::TimedOut,
            body,
        }) if id == transaction_id && body.is_empty()
    ));

    let body = encode_echo("hi", 2).unwrap();
    let view = service
        .dispatch(body.as_slice(), &server.bound().unwrap().view().unwrap())
        .unwrap();
    assert_eq!(view.sequence, 2);
    assert_eq!(service.dispatch_count(), 1);
}

#[test]
fn executing_request_observes_deadline_cancellation() {
    let (mut client, mut server, _, _) = connected(17);
    let transaction_id = call(&mut client, 1, 100);
    let token = match server.poll(0).unwrap().unwrap() {
        ServerEvent::Request { token, .. } => token,
        event => panic!("unexpected event: {event:?}"),
    };
    assert!(matches!(
        server.poll(100).unwrap(),
        Some(ServerEvent::Canceled {
            token: canceled,
            reason: nswp_runtime::CancellationReason::Deadline,
        }) if canceled.transaction_id() == transaction_id
    ));
    server
        .respond(token, TransportStatus::TimedOut, &[])
        .unwrap();
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Response {
            transaction_id: id,
            status: TransportStatus::TimedOut,
            ..
        }) if id == transaction_id
    ));
}

#[test]
fn malformed_body_closes_before_dispatch() {
    let (mut client, mut server, _, server_control) = connected(8);
    let service = EchoService::new();
    server_control.set_hold_incoming(true);
    call(&mut client, 1, u64::MAX);
    assert_eq!(server_control.held_incoming(), 1);
    assert!(server_control.corrupt_held(0, NSWP_HEADER_BYTES + 80, 0xff));
    server_control.set_hold_incoming(false);
    assert!(server_control.release_held(0));
    assert_eq!(
        server.poll(0),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );
    assert_eq!(service.dispatch_count(), 0);
}

#[test]
fn unknown_and_duplicate_responses_close_the_client() {
    let (mut client, mut server, client_control, _) = connected(9);
    let transaction_id = call(&mut client, 1, u64::MAX);
    let (token, body) = match server.poll(0).unwrap().unwrap() {
        ServerEvent::Request { token, body } => (token, body),
        event => panic!("unexpected event: {event:?}"),
    };
    server
        .respond(token, TransportStatus::Ok, body.as_slice())
        .unwrap();
    let duplicate = client_control.incoming_packet(0).unwrap();
    assert!(matches!(
        client.poll().unwrap(),
        Some(ClientEvent::Response { transaction_id: id, .. }) if id == transaction_id
    ));
    assert!(client_control.inject_incoming(duplicate));
    assert_eq!(
        client.poll(),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );

    let (mut client, _server, client_control, _) = connected(10);
    let bound = *client.bound().unwrap();
    let header = Header {
        kind: PacketKind::Response,
        flags: HeaderFlags::NONE,
        protocol_major: bound.major(),
        protocol_minor: bound.minor(),
        ordinal: ECHO_PING_ORDINAL,
        body_bytes: 0,
        handle_count: 0,
        transport_status: TransportStatus::TimedOut,
        transaction_id: 999,
        deadline_ns: u64::MAX,
        trace_id: [0; 16],
    };
    let mut packet = [0; NSWP_HEADER_BYTES];
    header.encode(&mut packet).unwrap();
    assert!(client_control.inject_incoming(packet.to_vec()));
    assert_eq!(
        client.poll(),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );
}

#[test]
fn peer_closure_and_service_replacement_never_replay_calls() {
    let (mut client, mut server, _, _) = connected(11);
    call(&mut client, 1, u64::MAX);
    server.transport_mut().close();
    assert_eq!(client.poll(), Err(RuntimeError::PeerClosed));
    assert_eq!(client.close_reason(), Some(CloseReason::PeerClosed));
    assert_eq!(client.outstanding_count(), 1);

    let (mut client, _server, _, server_control) = connected(12);
    call(&mut client, 1, u64::MAX);
    assert_eq!(server_control.queued_incoming(), 1);
    client.service_replaced();
    assert_eq!(client.outstanding_count(), 1);
    assert_eq!(server_control.queued_incoming(), 1);

    let (new_client_endpoint, new_server_endpoint) = channel_pair::<16>();
    let mut new_client = Client::new(new_client_endpoint, echo_protocol());
    let mut new_server = Server::new(new_server_endpoint, echo_protocol(), 13).unwrap();
    new_client.try_negotiate().unwrap();
    assert!(matches!(
        new_server.poll(0).unwrap(),
        Some(ServerEvent::Bound(_))
    ));
    assert!(matches!(
        new_client.poll().unwrap(),
        Some(ClientEvent::Bound(bound)) if bound.service_generation() == 13
    ));
    assert_eq!(new_client.outstanding_count(), 0);
}

#[test]
fn recently_canceled_table_never_silently_evicts() {
    let (mut client, mut server, _, _) = connected(14);
    for sequence in 0..8 {
        let id = call(&mut client, sequence, u64::MAX);
        assert!(matches!(
            server.poll(0).unwrap(),
            Some(ServerEvent::Request { .. })
        ));
        client.try_cancel(id).unwrap();
        assert!(matches!(
            server.poll(0).unwrap(),
            Some(ServerEvent::Canceled { .. })
        ));
    }
    let id = call(&mut client, 9, u64::MAX);
    assert_eq!(
        client.try_cancel(id),
        Err(RuntimeError::RecentlyCanceledExhausted)
    );
    assert_eq!(
        client.close_reason(),
        Some(CloseReason::RecentlyCanceledExhausted)
    );
}

#[test]
fn packet_and_echo_profiles_have_exact_boundaries() {
    assert_eq!(MAX_PACKET_BYTES, 256);
    assert!(nswp_runtime::PacketBuf::from_slice(&[0; 256]).is_ok());
    assert_eq!(
        nswp_runtime::PacketBuf::from_slice(&[0; 257]),
        Err(RuntimeError::PacketTooLarge { bytes: 257 })
    );
    assert!(nswp_runtime::BodyBuf::from_slice(&[0; 192]).is_ok());
    assert_eq!(
        nswp_runtime::BodyBuf::from_slice(&[0; 193]),
        Err(RuntimeError::BodyTooLarge { bytes: 193 })
    );
    assert_eq!(encode_echo("hi", 7).unwrap().as_slice(), ECHO_HI_7_BODY);
    assert_eq!(encode_echo(&"x".repeat(32), 7).unwrap().len(), 120);
}

#[test]
fn oversized_message_and_corrupt_header_close_protocol() {
    let (mut client, _server, client_control, _) = connected(15);
    assert!(client_control.inject_incoming(vec![0; 257]));
    assert_eq!(
        client.poll(),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );

    let (mut client, _server, client_control, _) = connected(16);
    assert!(client_control.inject_incoming(vec![0; NSWP_HEADER_BYTES]));
    assert_eq!(
        client.poll(),
        Err(RuntimeError::Closed(CloseReason::ProtocolError))
    );
}
