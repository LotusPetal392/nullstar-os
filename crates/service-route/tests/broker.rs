use service_route::{
    Authorizer, ConnectError, IssueError, ProviderGeneration, RoleId, RouteBroker, RouteIssuer,
    RouteKey, ServiceId,
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

struct Policy {
    calls: usize,
    result: Result<(), &'static str>,
}

impl Authorizer<u32> for Policy {
    type Error = &'static str;

    fn authorize(&mut self, caller: &u32, requested: RouteKey) -> Result<(), Self::Error> {
        self.calls += 1;
        assert_eq!(*caller, 7);
        assert_eq!(requested, key(1));
        self.result
    }
}

struct Issuer {
    calls: usize,
    result: Result<u32, IssueError<&'static str>>,
}

impl RouteIssuer<u32, u32> for Issuer {
    type Connection = u32;
    type Error = &'static str;

    fn issue(
        &mut self,
        authority: &mut u32,
        caller: &u32,
        requested: RouteKey,
        provider_generation: ProviderGeneration,
    ) -> Result<Self::Connection, IssueError<Self::Error>> {
        self.calls += 1;
        assert_eq!(*authority, 99);
        assert_eq!(*caller, 7);
        assert_eq!(requested, key(1));
        assert_eq!(provider_generation, generation(3));
        self.result
    }
}

#[test]
fn authorization_precedes_and_conceals_availability() {
    let mut broker = RouteBroker::<u32, 1>::new();
    let mut denied = Policy {
        calls: 0,
        result: Err("denied"),
    };
    let mut issuer = Issuer {
        calls: 0,
        result: Ok(10),
    };
    assert_eq!(
        broker.connect(&7, key(1), &mut denied, &mut issuer),
        Err(ConnectError::Unauthorized("denied"))
    );
    assert_eq!(denied.calls, 1);
    assert_eq!(issuer.calls, 0);

    let mut allowed = Policy {
        calls: 0,
        result: Ok(()),
    };
    assert_eq!(
        broker.connect(&7, key(1), &mut allowed, &mut issuer),
        Err(ConnectError::Unavailable)
    );
    assert_eq!(allowed.calls, 1);
    assert_eq!(issuer.calls, 0);
}

#[test]
fn successful_connections_are_generation_bound() {
    let mut broker = RouteBroker::<u32, 1>::default();
    broker.publish(key(1), generation(3), 99).unwrap();
    let mut policy = Policy {
        calls: 0,
        result: Ok(()),
    };
    let mut issuer = Issuer {
        calls: 0,
        result: Ok(123),
    };
    let issued = broker
        .connect(&7, key(1), &mut policy, &mut issuer)
        .unwrap();
    assert_eq!(issued.generation, generation(3));
    assert_eq!(issued.connection, 123);
    assert_eq!(policy.calls, 1);
    assert_eq!(issuer.calls, 1);
}

#[test]
fn issuer_capacity_and_provider_errors_remain_distinct() {
    let mut broker = RouteBroker::<u32, 1>::new();
    broker.publish(key(1), generation(3), 99).unwrap();
    let mut policy = Policy {
        calls: 0,
        result: Ok(()),
    };
    let mut capacity = Issuer {
        calls: 0,
        result: Err(IssueError::Capacity),
    };
    assert_eq!(
        broker.connect(&7, key(1), &mut policy, &mut capacity),
        Err(ConnectError::IssuerCapacity)
    );

    let mut provider = Issuer {
        calls: 0,
        result: Err(IssueError::Provider("closed")),
    };
    assert_eq!(
        broker.connect(&7, key(1), &mut policy, &mut provider),
        Err(ConnectError::Issuer("closed"))
    );
    assert_eq!(broker.table().get(key(1)).unwrap().authority, &99);
}
