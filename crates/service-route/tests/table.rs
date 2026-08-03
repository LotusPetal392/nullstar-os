use service_route::{
    ProviderGeneration, PublishError, RoleId, RouteKey, RouteTable, ServiceId, WithdrawError,
};

fn key(role: u32) -> RouteKey {
    RouteKey::new(
        ServiceId::from_bytes([
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x46, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ])
        .unwrap(),
        RoleId::new(role).unwrap(),
    )
}

fn generation(value: u64) -> ProviderGeneration {
    ProviderGeneration::new(value).unwrap()
}

#[test]
fn publication_is_strictly_newer_and_returns_every_unconsumed_authority() {
    let mut table = RouteTable::<u32, 1>::new();
    assert_eq!(table.publish(key(1), generation(2), 20), Ok(None));
    assert_eq!(table.get(key(1)).unwrap().authority, &20);

    assert_eq!(
        table.publish(key(1), generation(2), 21),
        Err(PublishError::GenerationNotNewer {
            authority: 21,
            current_generation: generation(2),
        })
    );
    let stale = table.publish(key(1), generation(1), 10).unwrap_err();
    assert_eq!(stale.authority(), &10);
    assert_eq!(stale.into_authority(), 10);

    assert_eq!(table.publish(key(1), generation(3), 30), Ok(Some(20)));
    assert_eq!(table.get(key(1)).unwrap().authority, &30);
    assert_eq!(
        table.publish(key(2), generation(1), 40),
        Err(PublishError::Capacity { authority: 40 })
    );
}

#[test]
fn withdrawal_requires_the_exact_active_generation_and_leaves_a_tombstone() {
    let mut table = RouteTable::<u32, 2>::new();
    table.publish(key(1), generation(5), 50).unwrap();
    assert_eq!(
        table.withdraw(key(1), generation(4)),
        Err(WithdrawError::GenerationMismatch {
            current_generation: generation(5),
        })
    );
    assert_eq!(table.withdraw(key(1), generation(5)), Ok(50));
    assert_eq!(table.get(key(1)), None);
    assert_eq!(table.generation(key(1)), Some(generation(5)));
    assert_eq!(table.len(), 1);
    assert_eq!(table.active_len(), 0);
    assert_eq!(
        table.withdraw(key(1), generation(5)),
        Err(WithdrawError::NotPublished)
    );
    assert_eq!(
        table.publish(key(1), generation(5), 51),
        Err(PublishError::GenerationNotNewer {
            authority: 51,
            current_generation: generation(5),
        })
    );
    assert_eq!(table.publish(key(1), generation(6), 60), Ok(None));
}

#[test]
fn tombstones_consume_distinct_key_capacity_and_unknown_withdrawals_are_rejected() {
    let mut table = RouteTable::<u32, 1>::default();
    assert_eq!(table.capacity(), 1);
    assert!(table.is_empty());
    table.publish(key(1), generation(1), 10).unwrap();
    table.withdraw(key(1), generation(1)).unwrap();
    assert_eq!(
        table.publish(key(2), generation(1), 20),
        Err(PublishError::Capacity { authority: 20 })
    );
    assert_eq!(
        table.withdraw(key(2), generation(1)),
        Err(WithdrawError::UnknownRoute)
    );
}

#[test]
fn zero_capacity_tables_return_authority_without_tracking_a_key() {
    let mut table = RouteTable::<u32, 0>::new();
    assert_eq!(
        table.publish(key(1), generation(1), 10),
        Err(PublishError::Capacity { authority: 10 })
    );
    assert_eq!(table.len(), 0);
    assert_eq!(table.active_len(), 0);
}
