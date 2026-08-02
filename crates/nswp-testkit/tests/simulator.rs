use nswp_runtime::{TryRecvError, TrySendError, TryTransport};
use nswp_testkit::{ManualClock, channel_pair};

#[test]
fn send_is_atomic_and_receive_preserves_messages() {
    let (mut left, mut right) = channel_pair::<1>();
    left.try_send(b"first").unwrap();
    assert_eq!(left.try_send(b"second"), Err(TrySendError::Full));

    let mut output = [0; 8];
    assert_eq!(right.try_recv(&mut output), Ok(5));
    assert_eq!(&output[..5], b"first");
    assert_eq!(right.try_recv(&mut output), Err(TryRecvError::Empty));
}

#[test]
fn held_messages_can_be_released_out_of_order() {
    let (mut left, mut right) = channel_pair::<4>();
    right.set_hold_incoming(true);
    left.try_send(b"one").unwrap();
    left.try_send(b"two").unwrap();
    assert_eq!(right.held_incoming(), 2);
    assert!(right.release_held(1));
    assert!(right.release_held(0));

    let mut output = [0; 8];
    assert_eq!(right.try_recv(&mut output), Ok(3));
    assert_eq!(&output[..3], b"two");
    assert_eq!(right.try_recv(&mut output), Ok(3));
    assert_eq!(&output[..3], b"one");
}

#[test]
fn oversize_does_not_consume_message_and_close_is_observable() {
    let (mut left, mut right) = channel_pair::<2>();
    left.try_send(&[1; 9]).unwrap();
    assert_eq!(
        right.try_recv(&mut [0; 8]),
        Err(TryRecvError::MessageTooLarge { bytes: 9 })
    );
    assert_eq!(right.queued_incoming(), 1);
    left.close();
    let mut output = [0; 9];
    assert_eq!(right.try_recv(&mut output), Ok(9));
    assert_eq!(right.try_recv(&mut output), Err(TryRecvError::PeerClosed));
}

#[test]
fn dropping_the_final_endpoint_handle_closes_the_peer() {
    let (left, mut right) = channel_pair::<1>();
    drop(left);
    assert_eq!(right.try_recv(&mut [0; 1]), Err(TryRecvError::PeerClosed));
    assert_eq!(right.try_send(&[1]), Err(TrySendError::PeerClosed));
}

#[test]
fn manual_clock_is_explicit_and_saturating() {
    let mut clock = ManualClock::new(10);
    clock.advance(5);
    assert_eq!(clock.now_ns(), 15);
    clock.set(u64::MAX - 1);
    clock.advance(10);
    assert_eq!(clock.now_ns(), u64::MAX);
}
