#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    Permission,
    InvalidGeneration,
    BadHandle,
    WrongObjectKind,
    MissingSendRight,
}

pub fn validate_registration(
    process_id: u64,
    init_process_id: u64,
    generation: u64,
    capability: Option<(u64, u64)>,
    endpoint_kind: u64,
    send_right: u64,
) -> Result<u32, RegistrationError> {
    if process_id != init_process_id {
        return Err(RegistrationError::Permission);
    }
    let generation = u32::try_from(generation).map_err(|_| RegistrationError::InvalidGeneration)?;
    if generation == 0 {
        return Err(RegistrationError::InvalidGeneration);
    }
    let Some((kind, rights)) = capability else {
        return Err(RegistrationError::BadHandle);
    };
    if kind != endpoint_kind {
        return Err(RegistrationError::WrongObjectKind);
    }
    if rights & send_right != send_right {
        return Err(RegistrationError::MissingSendRight);
    }
    Ok(generation)
}

#[derive(Clone, Copy)]
#[cfg_attr(not(test), allow(dead_code))]
pub struct ReplyEnvelope {
    pub byte_length: usize,
    pub expected_byte_length: usize,
    pub has_capability: bool,
    pub version: u16,
    pub expected_version: u16,
    pub operation: u16,
    pub expected_operation: u16,
    pub generation: u32,
    pub expected_generation: u32,
    pub data_length: usize,
    pub maximum_data_length: usize,
    pub reserved: u16,
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn valid_reply_envelope(envelope: ReplyEnvelope) -> bool {
    envelope.byte_length == envelope.expected_byte_length
        && !envelope.has_capability
        && envelope.version == envelope.expected_version
        && envelope.operation == envelope.expected_operation
        && envelope.generation == envelope.expected_generation
        && envelope.data_length <= envelope.maximum_data_length
        && envelope.reserved == 0
}

#[cfg(test)]
mod tests {
    use super::{RegistrationError, ReplyEnvelope, valid_reply_envelope, validate_registration};

    const INIT: u64 = 1;
    const ENDPOINT: u64 = 1;
    const SEND: u64 = 1 << 1;

    #[test]
    fn registration_requires_init_and_a_bounded_nonzero_generation() {
        let capability = Some((ENDPOINT, SEND));
        assert_eq!(
            validate_registration(2, INIT, 1, capability, ENDPOINT, SEND),
            Err(RegistrationError::Permission)
        );
        assert_eq!(
            validate_registration(INIT, INIT, 0, capability, ENDPOINT, SEND),
            Err(RegistrationError::InvalidGeneration)
        );
        assert_eq!(
            validate_registration(
                INIT,
                INIT,
                u64::from(u32::MAX) + 1,
                capability,
                ENDPOINT,
                SEND
            ),
            Err(RegistrationError::InvalidGeneration)
        );
    }

    #[test]
    fn registration_requires_an_endpoint_with_send_authority() {
        assert_eq!(
            validate_registration(INIT, INIT, 7, None, ENDPOINT, SEND),
            Err(RegistrationError::BadHandle)
        );
        assert_eq!(
            validate_registration(INIT, INIT, 7, Some((2, SEND)), ENDPOINT, SEND),
            Err(RegistrationError::WrongObjectKind)
        );
        assert_eq!(
            validate_registration(INIT, INIT, 7, Some((ENDPOINT, 0)), ENDPOINT, SEND),
            Err(RegistrationError::MissingSendRight)
        );
        assert_eq!(
            validate_registration(INIT, INIT, 7, Some((ENDPOINT, SEND)), ENDPOINT, SEND),
            Ok(7)
        );
    }

    fn valid_envelope() -> ReplyEnvelope {
        ReplyEnvelope {
            byte_length: 152,
            expected_byte_length: 152,
            has_capability: false,
            version: 2,
            expected_version: 2,
            operation: 3,
            expected_operation: 3,
            generation: 9,
            expected_generation: 9,
            data_length: 16,
            maximum_data_length: 128,
            reserved: 0,
        }
    }

    #[test]
    fn reply_envelope_rejects_untrusted_shape_and_identity_fields() {
        assert!(valid_reply_envelope(valid_envelope()));

        let mutations: [fn(&mut ReplyEnvelope); 7] = [
            |reply| reply.byte_length -= 1,
            |reply| reply.has_capability = true,
            |reply| reply.version += 1,
            |reply| reply.operation += 1,
            |reply| reply.generation += 1,
            |reply| reply.data_length = reply.maximum_data_length + 1,
            |reply| reply.reserved = 1,
        ];
        for mutate in mutations {
            let mut reply = valid_envelope();
            mutate(&mut reply);
            assert!(!valid_reply_envelope(reply));
        }
    }
}
