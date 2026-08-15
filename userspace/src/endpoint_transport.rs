//! Allocation-free NSWP transport over two one-way endpoint mailboxes.

use nswp_runtime::{TryRecvError, TrySendError, TryTransport};

use crate::{
    handle::{Endpoint, OwnedHandle},
    ipc::{self, CapabilityInfo, ObjectKind, Rights},
};

pub struct EndpointTransport {
    send_handle: Option<OwnedHandle<Endpoint>>,
    receive_handle: Option<OwnedHandle<Endpoint>>,
    peer_process_id: Option<u64>,
}

impl EndpointTransport {
    pub fn new(
        send_handle: OwnedHandle<Endpoint>,
        receive_handle: OwnedHandle<Endpoint>,
    ) -> ipc::Result<Self> {
        let send_info = send_handle.info()?;
        let receive_info = receive_handle.info()?;
        if !valid_endpoint_pair(send_info, receive_info) {
            return Err(ipc::Error::INVALID_ARGUMENT);
        }
        Ok(Self {
            send_handle: Some(send_handle),
            receive_handle: Some(receive_handle),
            peer_process_id: None,
        })
    }

    pub const fn peer_process_id(&self) -> Option<u64> {
        self.peer_process_id
    }

    fn close_local(&mut self) {
        self.send_handle.take();
        self.receive_handle.take();
    }

    fn fail_send(&mut self) -> Result<(), TrySendError> {
        self.close_local();
        // The current mailbox ABI has no peer-close signal; this is the runtime's fatal error.
        Err(TrySendError::PeerClosed)
    }

    fn fail_receive(&mut self) -> Result<usize, TryRecvError> {
        self.close_local();
        Err(TryRecvError::PeerClosed)
    }
}

impl TryTransport for EndpointTransport {
    fn try_send(&mut self, packet: &[u8]) -> Result<(), TrySendError> {
        let Some(send_handle) = self.send_handle.as_ref() else {
            return self.fail_send();
        };
        if packet.len() > crate::abi::limits::MAX_IPC_MESSAGE_BYTES {
            return self.fail_send();
        }
        match send_handle.send(packet) {
            Ok(()) => Ok(()),
            Err(error) if error == ipc::Error::TRY_AGAIN => Err(TrySendError::Full),
            Err(_) => self.fail_send(),
        }
    }

    fn try_recv(&mut self, output: &mut [u8]) -> Result<usize, TryRecvError> {
        let Some(receive_handle) = self.receive_handle.as_ref() else {
            return self.fail_receive();
        };
        if output.len() > crate::abi::limits::MAX_IPC_MESSAGE_BYTES {
            return self.fail_receive();
        }
        let message = match receive_handle.try_receive(output) {
            Ok(message) => message,
            Err(error) if error == ipc::Error::TRY_AGAIN => return Err(TryRecvError::Empty),
            Err(error) if error == ipc::Error::RANGE => {
                return Err(TryRecvError::MessageTooLarge {
                    bytes: output.len().saturating_add(1),
                });
            }
            Err(_) => return self.fail_receive(),
        };

        if message.capability.is_some() {
            return self.fail_receive();
        }
        if !accept_sender(&mut self.peer_process_id, message.sender_process_id) {
            return self.fail_receive();
        }
        Ok(message.bytes)
    }

    fn close(&mut self) {
        self.close_local();
    }
}

impl Drop for EndpointTransport {
    fn drop(&mut self) {
        // Taking the owned fields also makes an explicit transport close idempotent.
        self.close_local();
    }
}

fn valid_endpoint_pair(send: CapabilityInfo, receive: CapabilityInfo) -> bool {
    send.kind == ObjectKind::Endpoint
        && send.rights == Rights::SEND
        && receive.kind == ObjectKind::Endpoint
        && receive.rights == Rights::RECEIVE
        && send.object_id != receive.object_id
}

fn accept_sender(peer_process_id: &mut Option<u64>, sender_process_id: u64) -> bool {
    if sender_process_id == 0 {
        return false;
    }
    match *peer_process_id {
        None => {
            *peer_process_id = Some(sender_process_id);
            true
        }
        Some(process_id) => process_id == sender_process_id,
    }
}

#[cfg(test)]
mod tests {
    use super::{accept_sender, valid_endpoint_pair};
    use crate::ipc::{CapabilityInfo, ObjectKind, Rights};

    fn endpoint(object_id: u64, rights: Rights) -> CapabilityInfo {
        CapabilityInfo {
            object_id,
            kind: ObjectKind::Endpoint,
            rights,
            size: 0,
        }
    }

    #[test]
    fn endpoint_pair_requires_exact_rights_and_distinct_mailboxes() {
        let send = endpoint(1, Rights::SEND);
        let receive = endpoint(2, Rights::RECEIVE);
        assert!(valid_endpoint_pair(send, receive));
        assert!(!valid_endpoint_pair(
            endpoint(1, Rights::SEND | Rights::TRANSFER),
            receive,
        ));
        assert!(!valid_endpoint_pair(
            send,
            endpoint(2, Rights::RECEIVE | Rights::TRANSFER),
        ));
        assert!(!valid_endpoint_pair(send, endpoint(1, Rights::RECEIVE)));
        assert!(!valid_endpoint_pair(
            CapabilityInfo {
                kind: ObjectKind::Notification,
                ..send
            },
            receive,
        ));
    }

    #[test]
    fn sender_identity_is_nonzero_and_pinned_by_the_first_packet() {
        let mut peer = None;
        assert!(!accept_sender(&mut peer, 0));
        assert_eq!(peer, None);
        assert!(accept_sender(&mut peer, 42));
        assert!(accept_sender(&mut peer, 42));
        assert!(!accept_sender(&mut peer, 43));
        assert_eq!(peer, Some(42));
    }
}
