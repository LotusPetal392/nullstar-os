//! Stable service-route catalog exposed through a desktop application's restricted namespace.

use nswp_logging::{LOGGING_PRODUCER_ROLE, LOGGING_SERVICE_ID};
use service_route::{RoleId, RouteKey, ServiceId};

pub const BASELINE_APPLICATION_ROUTE_COUNT: usize = 6;

pub const DISPLAY_SERVICE_ID: ServiceId = service_id([
    0xd8, 0x01, 0x3c, 0xcf, 0x3f, 0x73, 0x48, 0xe5, 0xa7, 0x7d, 0xb7, 0xc2, 0xfd, 0xa4, 0x5f, 0x91,
]);
pub const APPLICATION_LIFECYCLE_SERVICE_ID: ServiceId = service_id([
    0xf0, 0xf2, 0xf0, 0xc7, 0x79, 0x9b, 0x4c, 0x81, 0xa5, 0x71, 0xa2, 0x3f, 0x11, 0xe2, 0xde, 0xea,
]);
pub const SETTINGS_SERVICE_ID: ServiceId = service_id([
    0xdc, 0x60, 0xe2, 0xc7, 0xf5, 0x2c, 0x43, 0xd7, 0xac, 0xd1, 0xa0, 0x32, 0x41, 0xad, 0x49, 0xb5,
]);
pub const AUDIO_PLAYBACK_SERVICE_ID: ServiceId = service_id([
    0x8e, 0xee, 0xe3, 0x4d, 0xac, 0x55, 0x48, 0x6a, 0xbc, 0xf0, 0x37, 0xd6, 0xdd, 0xc9, 0x3c, 0x92,
]);
pub const PORTAL_SERVICE_ID: ServiceId = service_id([
    0x08, 0x6e, 0xad, 0x96, 0xe1, 0xc1, 0x43, 0xc9, 0xb8, 0x46, 0x16, 0xcd, 0x4f, 0x5d, 0xf1, 0xc5,
]);

pub const CLIENT_ROLE: RoleId = role_id(1);

pub const DISPLAY_CLIENT_ROUTE: RouteKey = RouteKey::new(DISPLAY_SERVICE_ID, CLIENT_ROLE);
pub const APPLICATION_LIFECYCLE_CLIENT_ROUTE: RouteKey =
    RouteKey::new(APPLICATION_LIFECYCLE_SERVICE_ID, CLIENT_ROLE);
pub const SETTINGS_CLIENT_ROUTE: RouteKey = RouteKey::new(SETTINGS_SERVICE_ID, CLIENT_ROLE);
pub const LOGGING_PRODUCER_ROUTE: RouteKey =
    RouteKey::new(LOGGING_SERVICE_ID, LOGGING_PRODUCER_ROLE);
pub const AUDIO_PLAYBACK_CLIENT_ROUTE: RouteKey =
    RouteKey::new(AUDIO_PLAYBACK_SERVICE_ID, CLIENT_ROLE);
pub const PORTAL_CLIENT_ROUTE: RouteKey = RouteKey::new(PORTAL_SERVICE_ID, CLIENT_ROLE);

/// Default desktop-root policy. Availability remains independent: a route in this list may still
/// resolve to `Unavailable` until its provider generation is published.
pub const BASELINE_DESKTOP_ROUTES: [RouteKey; BASELINE_APPLICATION_ROUTE_COUNT] = [
    DISPLAY_CLIENT_ROUTE,
    APPLICATION_LIFECYCLE_CLIENT_ROUTE,
    SETTINGS_CLIENT_ROUTE,
    LOGGING_PRODUCER_ROUTE,
    AUDIO_PLAYBACK_CLIENT_ROUTE,
    PORTAL_CLIENT_ROUTE,
];

const fn service_id(bytes: [u8; 16]) -> ServiceId {
    match ServiceId::from_bytes(bytes) {
        Ok(identifier) => identifier,
        Err(_) => panic!("application service ID must be a canonical UUIDv4"),
    }
}

const fn role_id(value: u32) -> RoleId {
    match RoleId::new(value) {
        Some(identifier) => identifier,
        None => panic!("application service role must be nonzero"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_desktop_routes_are_unique_and_include_logging_production() {
        for (index, route) in BASELINE_DESKTOP_ROUTES.iter().enumerate() {
            assert!(!BASELINE_DESKTOP_ROUTES[..index].contains(route));
            assert_eq!(route.role().get(), 1);
        }
        assert!(BASELINE_DESKTOP_ROUTES.contains(&LOGGING_PRODUCER_ROUTE));
    }

    #[test]
    fn application_service_ids_are_distinct() {
        let identifiers = [
            DISPLAY_SERVICE_ID,
            APPLICATION_LIFECYCLE_SERVICE_ID,
            SETTINGS_SERVICE_ID,
            LOGGING_SERVICE_ID,
            AUDIO_PLAYBACK_SERVICE_ID,
            PORTAL_SERVICE_ID,
        ];
        for (index, identifier) in identifiers.iter().enumerate() {
            assert!(!identifiers[..index].contains(identifier));
        }
    }
}
