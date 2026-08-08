pub const BOOT_MODE_PATH: &str = "/BOOTMODE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Normal,
    SmokeTest,
    NullfsRestartTest,
    NullfsUnavailableTest,
    LoggingLifecycleTest,
}

impl BootMode {
    pub const fn parse(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"normal" | b"normal\n" => Some(Self::Normal),
            b"smoke-test" | b"smoke-test\n" => Some(Self::SmokeTest),
            b"nullfs-restart-test" | b"nullfs-restart-test\n" => Some(Self::NullfsRestartTest),
            b"nullfs-unavailable-test" | b"nullfs-unavailable-test\n" => {
                Some(Self::NullfsUnavailableTest)
            }
            b"logging-lifecycle-test" | b"logging-lifecycle-test\n" => {
                Some(Self::LoggingLifecycleTest)
            }
            _ => None,
        }
    }

    pub const fn is_smoke_test(self) -> bool {
        matches!(self, Self::SmokeTest)
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::SmokeTest => "smoke-test",
            Self::NullfsRestartTest => "nullfs-restart-test",
            Self::NullfsUnavailableTest => "nullfs-unavailable-test",
            Self::LoggingLifecycleTest => "logging-lifecycle-test",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BootMode;

    #[test]
    fn parses_supported_image_modes() {
        assert_eq!(BootMode::parse(b"normal\n"), Some(BootMode::Normal));
        assert_eq!(BootMode::parse(b"smoke-test\n"), Some(BootMode::SmokeTest));
        assert_eq!(
            BootMode::parse(b"nullfs-restart-test\n"),
            Some(BootMode::NullfsRestartTest)
        );
        assert_eq!(
            BootMode::parse(b"nullfs-unavailable-test\n"),
            Some(BootMode::NullfsUnavailableTest)
        );
        assert_eq!(
            BootMode::parse(b"logging-lifecycle-test\n"),
            Some(BootMode::LoggingLifecycleTest)
        );
    }

    #[test]
    fn rejects_missing_or_ambiguous_modes() {
        assert_eq!(BootMode::parse(b""), None);
        assert_eq!(BootMode::parse(b"smoke"), None);
        assert_eq!(BootMode::parse(b"normal extra"), None);
    }
}
