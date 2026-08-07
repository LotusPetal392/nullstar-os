pub mod protocol {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../shared/vfs_protocol.rs"
    ));
}

#[cfg(test)]
mod tests {
    use core::mem::{offset_of, size_of};

    use super::protocol;

    #[test]
    fn routing_v2_wire_shapes_and_binding_offsets_are_stable() {
        assert_eq!(protocol::VERSION, 2);
        assert_eq!(size_of::<protocol::Request>(), 208);
        assert_eq!(size_of::<protocol::Reply>(), 224);
        assert_eq!(offset_of!(protocol::Reply, route_id), 12);
        assert_eq!(offset_of!(protocol::Reply, backend), 16);
        assert_eq!(offset_of!(protocol::Reply, prefix_length), 18);
        assert_eq!(offset_of!(protocol::Reply, backing_prefix_length), 20);
        assert_eq!(offset_of!(protocol::Reply, flags), 22);
        assert_eq!(offset_of!(protocol::Reply, reserved), 24);
        assert_eq!(offset_of!(protocol::Reply, backing_prefix), 32);
    }

    #[test]
    fn empty_reply_has_no_implicit_binding() {
        let reply = protocol::Reply::EMPTY;
        assert_eq!(reply.flags, 0);
        assert_eq!(reply.backing_prefix_length, 0);
        assert_eq!(reply.backing_prefix, [0; protocol::MAX_PATH_BYTES]);
        assert_eq!(reply.binding_prefix(), Ok(None));
        assert_eq!(protocol::reply_flags::ALL, protocol::reply_flags::BINDING);
        assert!(protocol::status::known(protocol::status::OK));
        assert!(protocol::status::known(protocol::status::INVALID));
        assert!(protocol::status::known(protocol::status::NOT_FOUND));
        assert!(!protocol::status::known(99));
    }

    #[test]
    fn binding_prefix_rejects_malformed_lengths_flags_paths_and_padding() {
        const APPLICATIONS: &[u8] = b"/Applications";
        let mut reply = protocol::Reply::EMPTY;
        reply.flags = protocol::reply_flags::BINDING;
        reply.backing_prefix_length = APPLICATIONS.len() as u16;
        reply.backing_prefix[..APPLICATIONS.len()].copy_from_slice(APPLICATIONS);
        assert_eq!(reply.binding_prefix(), Ok(Some("/Applications")));

        let mut malformed = reply;
        malformed.backing_prefix_length = u16::MAX;
        assert_eq!(
            malformed.binding_prefix(),
            Err(protocol::BindingError::Length)
        );

        malformed = reply;
        malformed.flags |= 1 << 15;
        assert_eq!(
            malformed.binding_prefix(),
            Err(protocol::BindingError::Flags)
        );

        malformed = reply;
        malformed.backing_prefix[0] = b'A';
        assert_eq!(
            malformed.binding_prefix(),
            Err(protocol::BindingError::Path)
        );

        malformed = reply;
        malformed.backing_prefix[APPLICATIONS.len()] = 1;
        assert_eq!(
            malformed.binding_prefix(),
            Err(protocol::BindingError::Padding)
        );

        malformed = protocol::Reply::EMPTY;
        malformed.backing_prefix[0] = 1;
        assert_eq!(
            malformed.binding_prefix(),
            Err(protocol::BindingError::Padding)
        );
    }

    #[test]
    fn route_prefixes_require_exact_path_component_boundaries() {
        assert!(protocol::path_has_prefix("/Applications", "/Applications"));
        assert!(protocol::path_has_prefix(
            "/Applications/Example.app",
            "/Applications"
        ));
        assert!(!protocol::path_has_prefix(
            "/ApplicationsX",
            "/Applications"
        ));
        assert!(!protocol::path_has_prefix("/Application", "/Applications"));
    }
}
