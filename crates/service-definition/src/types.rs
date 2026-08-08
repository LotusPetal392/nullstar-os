use service_route::ServiceId;

pub const SERVICE_DEFINITION_HEADER: &str = "NullStar Service Definition 1";
pub const MAX_DEFINITION_BYTES: usize = 4096;
pub const MAX_NAME_BYTES: usize = 63;
pub const MAX_DESCRIPTION_BYTES: usize = 256;
pub const MAX_EXECUTABLE_BYTES: usize = 192;
pub const MAX_ARGUMENTS: usize = 16;
pub const MAX_ARGUMENT_BYTES: usize = 256;
pub const MAX_READY_MESSAGE_BYTES: usize = 128;
pub const MAX_RESTART_LIMIT: u32 = 16;
pub const MAX_RESTART_BACKOFF_YIELDS: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    Immediate,
    Notify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestartPolicy {
    Never,
    OnFailure,
    Always,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServiceDefinition<'a> {
    service_id: ServiceId,
    name: &'a str,
    description: &'a str,
    executable: &'a str,
    arguments: [&'a str; MAX_ARGUMENTS],
    argument_count: usize,
    readiness: Readiness,
    ready_message: Option<&'a str>,
    restart: RestartPolicy,
    restart_limit: u32,
    restart_backoff_yields: u32,
}

impl<'a> ServiceDefinition<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) const fn new(
        service_id: ServiceId,
        name: &'a str,
        description: &'a str,
        executable: &'a str,
        arguments: [&'a str; MAX_ARGUMENTS],
        argument_count: usize,
        readiness: Readiness,
        ready_message: Option<&'a str>,
        restart: RestartPolicy,
        restart_limit: u32,
        restart_backoff_yields: u32,
    ) -> Self {
        Self {
            service_id,
            name,
            description,
            executable,
            arguments,
            argument_count,
            readiness,
            ready_message,
            restart,
            restart_limit,
            restart_backoff_yields,
        }
    }

    pub const fn service_id(&self) -> ServiceId {
        self.service_id
    }

    pub const fn name(&self) -> &'a str {
        self.name
    }

    pub const fn description(&self) -> &'a str {
        self.description
    }

    pub const fn executable(&self) -> &'a str {
        self.executable
    }

    pub fn arguments(&self) -> impl ExactSizeIterator<Item = &'a str> + '_ {
        self.arguments[..self.argument_count].iter().copied()
    }

    pub const fn readiness(&self) -> Readiness {
        self.readiness
    }

    pub const fn ready_message(&self) -> Option<&'a str> {
        self.ready_message
    }

    pub const fn restart_policy(&self) -> RestartPolicy {
        self.restart
    }

    pub const fn restart_limit(&self) -> u32 {
        self.restart_limit
    }

    pub const fn restart_backoff_yields(&self) -> u32 {
        self.restart_backoff_yields
    }
}
