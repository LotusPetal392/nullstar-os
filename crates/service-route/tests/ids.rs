use service_route::{ProviderGeneration, RoleId, RouteKey, ServiceId, ServiceIdError};

const UUID: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff,
];

#[test]
fn service_ids_validate_uuid_v4_rfc_bytes() {
    let service = ServiceId::from_bytes(UUID).unwrap();
    assert_eq!(service.as_bytes(), &UUID);
    assert_eq!(service.into_bytes(), UUID);
    assert_eq!(service.to_string(), "00112233-4455-4677-8899-aabbccddeeff");

    assert_eq!(ServiceId::from_bytes([0; 16]), Err(ServiceIdError::Nil));
    let mut version = UUID;
    version[6] = 0x56;
    assert_eq!(
        ServiceId::from_bytes(version),
        Err(ServiceIdError::InvalidVersion)
    );
    let mut variant = UUID;
    variant[8] = 0x40;
    assert_eq!(
        ServiceId::from_bytes(variant),
        Err(ServiceIdError::InvalidVariant)
    );
}

#[test]
fn scalar_ids_are_nonzero_and_route_keys_preserve_both_parts() {
    assert_eq!(RoleId::new(0), None);
    assert_eq!(ProviderGeneration::new(0), None);
    let role = RoleId::new(u32::MAX).unwrap();
    let generation = ProviderGeneration::new(u64::MAX).unwrap();
    assert_eq!(role.get(), u32::MAX);
    assert_eq!(generation.get(), u64::MAX);

    let service = ServiceId::from_bytes(UUID).unwrap();
    let key = RouteKey::new(service, role);
    assert_eq!(key.service(), service);
    assert_eq!(key.role(), role);
}
