#![no_std]
#![no_main]

use core::cell::Cell;

use userspace::{
    abi::{capability, file, limits, signal},
    args::Args,
    async_ipc::{
        CancellationSource, DrainReport, MAX_TASK_TRACE_EVENTS, PeriodicTimer, Reactor, RunError,
        RunScope, Shutdown, TaskAttribution, TaskExecutor, TaskGroup, TaskOutcome, TaskRole,
        TaskTraceEvent, TaskTraceKind,
    },
    blocking_ipc,
    handle::{
        Endpoint, Event, EventPort, MoveHandle, Notification, OwnedHandle, SharedMemory, Timer,
        WaitSet,
    },
    heap::BumpHeap,
    ipc::{self, Deadline, ObjectKind, Rights, Signals, Transfer, WaitItem},
    platform::{self, DirectoryEntry},
    runtime_context::{
        self, CapabilityRole, Client, ContextCapability, ContextError, ProcessContext,
        ProtocolDescriptor, ServiceBinding, ServiceProcess, ServiceProtocol,
    },
    syscall::{self, OpenFlags, STDERR, STDIN, STDOUT, SignalAction, SignalMask, SpawnFlags},
};

userspace::entry!(rust_main);
userspace::panic_handler!();

const SUCCESS: &[u8] = b"userspace Rust runtime probe passed\n";
const DIRECTORY_PAGE: usize = 8;
const JOB_WAIT_YIELDS: usize = 4096;

const PROBE_CLIENT_RIGHTS: Rights =
    match Rights::from_bits(Rights::SEND.bits() | Rights::WAIT.bits() | Rights::TRANSFER.bits()) {
        Some(rights) => rights,
        None => panic!("runtime probe client rights must be valid"),
    };

struct RuntimeProbeProtocol;

impl ServiceProtocol for RuntimeProbeProtocol {
    const DESCRIPTOR: ProtocolDescriptor = ProtocolDescriptor {
        name: "test.runtime-context",
        major: 1,
        minor: 0,
        max_message_bytes: 32,
        max_handles: 1,
    };
    const CLIENT_RIGHTS: Rights = PROBE_CLIENT_RIGHTS;
    const SERVER_RIGHTS: Rights = Rights::RECEIVE;
}

extern "C" fn rust_main(initial_stack: *const usize) -> ! {
    let arguments = unsafe { Args::from_stack(initial_stack) };
    let Some(argument) = arguments.get(1) else {
        syscall::exit(64);
    };
    if arguments.len() != 2 || argument.is_empty() {
        syscall::exit(64);
    }

    let mut heap = BumpHeap::<4096>::new();
    let block_length = {
        let Some(block) = heap.allocate(257, 16) else {
            syscall::exit(1);
        };
        if !(block.as_ptr() as usize).is_multiple_of(16) {
            syscall::exit(1);
        }
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = (index & 0xff) as u8;
        }
        if block[0] != 0 || block[256] != 0 {
            syscall::exit(1);
        }
        block.len()
    };

    let copy_matches = {
        let Some(copy) = heap.copy_bytes(argument, 8) else {
            syscall::exit(1);
        };
        copy == argument
    };
    if !copy_matches || heap.used() <= block_length {
        syscall::exit(1);
    }

    heap.reset();
    if heap.used() != 0 || heap.remaining() != heap.capacity() {
        syscall::exit(1);
    }
    let process_id = match syscall::getpid() {
        Ok(process_id) => process_id,
        Err(_) => syscall::exit(1),
    };
    if !platform_probe(argument, process_id) {
        syscall::exit(1);
    }
    if syscall::write_all(STDOUT, SUCCESS).is_err() {
        syscall::exit(1);
    }
    syscall::exit(0)
}

fn platform_probe(argument: &[u8], process_id: u64) -> bool {
    let Ok(info) = platform::system_info() else {
        return false;
    };
    if info.abi_major != userspace::abi::ABI_VERSION_MAJOR
        || info.abi_minor != userspace::abi::ABI_VERSION_MINOR
        || info.capabilities & capability::PLATFORM_V1 != capability::PLATFORM_V1
        || info.capabilities & capability::PROTECTION_V1 != capability::PROTECTION_V1
        || info.capabilities & capability::PROCESS_GROUP_CONTROL == 0
        || info.page_size != 4096
        || info.maximum_open_files < 3
    {
        return false;
    }
    if !capability_probe(process_id)
        || !owned_handle_probe()
        || !channel_pair_probe(process_id)
        || !endpoint_multi_handle_probe(process_id)
        || !wait_set_probe()
        || !event_port_probe()
        || !timer_probe()
        || !event_probe()
        || !runtime_context_probe()
        || !async_ipc_probe(process_id)
        || !job_probe()
    {
        return false;
    }
    let Ok(process_group) = platform::get_process_group(0) else {
        return false;
    };
    if process_group == 0
        || platform::set_process_group(0, process_group).ok() != Some(process_group)
    {
        return false;
    }

    let Ok(hello_stat) = platform::stat(b"/hello.txt") else {
        return false;
    };
    if !hello_stat.is_file() || hello_stat.size < 2 {
        return false;
    }

    if !root_directory_has_expected_entries() {
        return false;
    }

    let mut cwd = [0_u8; 64];
    let Ok(initial_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if initial_directory != b"/" || platform::chdir(b"/tmp").is_err() {
        return false;
    }
    let Ok(tmp_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if tmp_directory != b"/tmp" {
        return false;
    }
    let Ok(relative_stat) = platform::stat(b".") else {
        return false;
    };
    if !relative_stat.is_directory() {
        return false;
    }
    let mut directory_page = [DirectoryEntry::EMPTY; 1];
    if platform::read_directory(b".", 0, &mut directory_page).is_err() {
        return false;
    }
    if supplementary_probes_enabled(argument) && (!relative_open_probe() || !relative_spawn_probe())
    {
        return false;
    }
    if syscall::environment_set(b"PWD", b"/").is_ok() {
        return false;
    }
    if platform::chdir(b"..").is_err() {
        return false;
    }
    let Ok(parent_directory) = platform::getcwd(&mut cwd) else {
        return false;
    };
    if parent_directory != b"/" {
        return false;
    }

    if !descriptor_probe()
        || (supplementary_probes_enabled(argument) && !ordinary_descriptor_probe())
        || platform::getppid().is_err()
        || platform::kill(u64::MAX, signal::TERMINATE).err() != Some(platform::Errno::NO_PROCESS)
        || SignalMask::from_bits(signal::bit(signal::KILL)) != Some(SignalMask::EMPTY)
        || syscall::signal_action(signal::KILL, Some(&SignalAction::IGNORE), None).err()
            != Some(syscall::Errno::INVALID_ARGUMENT)
    {
        return false;
    }
    true
}

fn runtime_context_probe() -> bool {
    let Ok(endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(mut restricted) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    if restricted.replace_rights(Rights::SEND).is_err() {
        return false;
    }
    let capabilities = [
        Some(ContextCapability::new(
            CapabilityRole::SERVICE_NAMESPACE,
            endpoint,
        )),
        Some(ContextCapability::new(
            CapabilityRole::CONFIGURATION,
            restricted,
        )),
    ];
    let Ok(mut context) = ProcessContext::<ServiceProcess, 2>::new(capabilities) else {
        return false;
    };
    if context.runtime_role() != "service"
        || !context.contains(CapabilityRole::SERVICE_NAMESPACE)
        || context.len() != 2
        || runtime_context::validate_protocol::<RuntimeProbeProtocol>().is_err()
        || runtime_context::validate_message_shape::<RuntimeProbeProtocol>(32, 1).is_err()
        || runtime_context::validate_message_shape::<RuntimeProbeProtocol>(33, 1).is_ok()
    {
        return false;
    }
    if ServiceBinding::<RuntimeProbeProtocol, Client>::from_context(
        &mut context,
        CapabilityRole::CONFIGURATION,
    )
    .err()
        != Some(ContextError::InsufficientRights(
            CapabilityRole::CONFIGURATION,
        ))
        || !context.contains(CapabilityRole::CONFIGURATION)
    {
        return false;
    }
    let Ok(binding) = ServiceBinding::<RuntimeProbeProtocol, Client>::from_context(
        &mut context,
        CapabilityRole::SERVICE_NAMESPACE,
    ) else {
        return false;
    };
    binding.descriptor() == RuntimeProbeProtocol::DESCRIPTOR
        && binding
            .endpoint()
            .info()
            .is_ok_and(|info| info.rights == PROBE_CLIENT_RIGHTS)
        && context.len() == 1
        && context.contains(CapabilityRole::CONFIGURATION)
}

fn wait_set_probe() -> bool {
    const NOTIFICATION_KEY: u64 = 41;

    let Ok(notification) = OwnedHandle::<Notification>::create() else {
        return false;
    };
    let Ok(wait_set) = OwnedHandle::<WaitSet>::create() else {
        return false;
    };
    if !wait_set.info().is_ok_and(|info| {
        info.kind == ObjectKind::WaitSet && info.rights == Rights::WAIT_SET && info.size == 0
    }) || wait_set.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::INVALID_ARGUMENT)
        || wait_set
            .add(
                notification.borrow(),
                Signals::SIGNALED,
                userspace::abi::wait_set::MAX_KEY + 1,
            )
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || wait_set
            .add(notification.borrow(), Signals::READABLE, NOTIFICATION_KEY)
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || wait_set
            .add(notification.borrow(), Signals::SIGNALED, NOTIFICATION_KEY)
            .is_err()
        || wait_set
            .add(notification.borrow(), Signals::SIGNALED, NOTIFICATION_KEY)
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || wait_set.info().ok().map(|info| info.size) != Some(1)
        || wait_set.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || notification.signal(1).ok() != Some(1)
    {
        return false;
    }

    let first = wait_set.wait_next(Deadline::IMMEDIATE).ok();
    let repeated = wait_set.wait_next(Deadline::IMMEDIATE).ok();
    if first != repeated
        || first
            != Some(ipc::WaitSetEvent {
                key: NOTIFICATION_KEY,
                signals: Signals::SIGNALED,
            })
        || notification.try_wait().ok() != Some(0)
        || wait_set.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || wait_set.remove(NOTIFICATION_KEY).is_err()
        || wait_set.remove(NOTIFICATION_KEY).err() != Some(ipc::Error::NO_ENTRY)
        || wait_set.info().ok().map(|info| info.size) != Some(0)
    {
        return false;
    }

    wait_set_waiter_probe()
}

fn wait_set_waiter_probe() -> bool {
    const CHILD_WAIT_SET_HANDLE: ipc::CapabilityHandle = 1;
    const NOTIFICATION_KEY: u64 = 73;

    let Ok(notification) = OwnedHandle::<Notification>::create() else {
        return false;
    };
    let Ok(wait_set) = OwnedHandle::<WaitSet>::create() else {
        return false;
    };
    if wait_set
        .add(notification.borrow(), Signals::SIGNALED, NOTIFICATION_KEY)
        .is_err()
    {
        return false;
    }
    let Ok(barrier) = syscall::pipe_pair() else {
        return false;
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return false;
    };
    if child == 0 {
        let _ = syscall::close(barrier.reader);
        if !ipc::wait_for_handle(CHILD_WAIT_SET_HANDLE)
            .is_ok_and(|info| info.kind == ObjectKind::WaitSet && info.rights == Rights::WAIT)
            || syscall::write_all(barrier.writer, &[1]).is_err()
            || syscall::close(barrier.writer).is_err()
        {
            syscall::exit(76);
        }
        let woke = ipc::wait_set_wait(CHILD_WAIT_SET_HANDLE, Deadline::INFINITE).ok()
            == Some(ipc::WaitSetEvent {
                key: NOTIFICATION_KEY,
                signals: Signals::SIGNALED,
            });
        let closed = ipc::close(CHILD_WAIT_SET_HANDLE).is_ok();
        syscall::exit(if woke && closed { 0 } else { 77 });
    }

    let setup = syscall::close(barrier.writer).is_ok()
        && ipc::grant_child(
            child,
            wait_set.as_raw(),
            Rights::WAIT,
            CHILD_WAIT_SET_HANDLE,
        )
        .ok()
            == Some(CHILD_WAIT_SET_HANDLE);
    let mut ready = [0_u8; 1];
    let synchronized = setup
        && syscall::read(barrier.reader, &mut ready).ok() == Some(1)
        && ready[0] == 1
        && syscall::close(barrier.reader).is_ok();
    if synchronized {
        for _ in 0..4 {
            let _ = syscall::yield_now();
        }
    }
    let signaled = synchronized && notification.signal(1).ok() == Some(1);
    let mut child_succeeded = false;
    if signaled {
        for _ in 0..JOB_WAIT_YIELDS {
            match syscall::try_wait_child(child) {
                Ok(status) => {
                    child_succeeded = status.success();
                    break;
                }
                Err(error) if error == syscall::Errno::TRY_AGAIN => {
                    let _ = syscall::yield_now();
                }
                Err(_) => break,
            }
        }
    }
    if !child_succeeded {
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
    }
    let _ = syscall::close(barrier.reader);
    let _ = syscall::close(barrier.writer);
    signaled && child_succeeded
}

fn event_port_probe() -> bool {
    const NOTIFICATION_KEY: u64 = 101;

    let Ok(notification) = OwnedHandle::<Notification>::create() else {
        return false;
    };
    let Ok(event_port) = OwnedHandle::<EventPort>::create() else {
        return false;
    };
    if !event_port.info().is_ok_and(|info| {
        info.kind == ObjectKind::EventPort && info.rights == Rights::EVENT_PORT && info.size == 0
    }) || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || event_port
            .add(
                notification.borrow(),
                Signals::SIGNALED,
                userspace::abi::event_port::MAX_KEY + 1,
            )
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || event_port
            .add(notification.borrow(), Signals::READABLE, NOTIFICATION_KEY)
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || event_port
            .add(notification.borrow(), Signals::SIGNALED, NOTIFICATION_KEY)
            .is_err()
        || event_port
            .add(notification.borrow(), Signals::SIGNALED, NOTIFICATION_KEY)
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || event_port.info().ok().map(|info| info.size) != Some(0)
        || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || notification.signal(1).ok() != Some(1)
        || event_port.wait_next(Deadline::IMMEDIATE).ok()
            != Some(ipc::EventPortEvent {
                key: NOTIFICATION_KEY,
                signals: Signals::SIGNALED,
            })
        || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || notification.signal(1).ok() != Some(2)
        || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || notification.try_wait().ok() != Some(1)
        || notification.try_wait().ok() != Some(0)
        || notification.signal(1).ok() != Some(1)
        || event_port.wait_next(Deadline::IMMEDIATE).ok()
            != Some(ipc::EventPortEvent {
                key: NOTIFICATION_KEY,
                signals: Signals::SIGNALED,
            })
        || notification.try_wait().ok() != Some(0)
        || notification.signal(1).ok() != Some(1)
        || event_port.remove(NOTIFICATION_KEY).is_err()
        || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || event_port.remove(NOTIFICATION_KEY).err() != Some(ipc::Error::NO_ENTRY)
        || event_port.info().ok().map(|info| info.size) != Some(0)
    {
        return false;
    }

    event_port_waiter_probe()
}

fn timer_probe() -> bool {
    const TIMER_KEY: u64 = 151;
    const FIRE_DELAY_NS: u64 = 20_000_000;
    const WAIT_MARGIN_NS: u64 = 500_000_000;

    let Ok(timer) = OwnedHandle::<Timer>::create() else {
        return false;
    };
    let Ok(wait_only) = timer.borrow().duplicate(Rights::WAIT) else {
        return false;
    };
    let Ok(event_port) = OwnedHandle::<EventPort>::create() else {
        return false;
    };
    if !timer.info().is_ok_and(|info| {
        info.kind == ObjectKind::Timer && info.rights == Rights::TIMER && info.size == 0
    }) || timer.signal_state().ok() != Some(Signals::EMPTY)
        || timer
            .borrow()
            .wait(Signals::TIMER_FIRED, Deadline::IMMEDIATE)
            .err()
            != Some(ipc::Error::TIMED_OUT)
        || timer.arm(Deadline::INFINITE).err() != Some(ipc::Error::INVALID_ARGUMENT)
        || wait_only.arm(Deadline::IMMEDIATE).err() != Some(ipc::Error::PERMISSION)
        || wait_only.cancel().err() != Some(ipc::Error::PERMISSION)
        || event_port
            .add(timer.borrow(), Signals::READABLE, TIMER_KEY)
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || event_port
            .add(timer.borrow(), Signals::TIMER_FIRED, TIMER_KEY)
            .is_err()
    {
        return false;
    }

    let Ok(start) = platform::monotonic_time_ns() else {
        return false;
    };
    let fire_at = start.saturating_add(FIRE_DELAY_NS);
    if timer.arm(Deadline::from_monotonic_ns(fire_at)).is_err()
        || timer.info().ok().map(|info| info.size) != Some(1)
        || event_port
            .wait_next(Deadline::from_monotonic_ns(
                fire_at.saturating_add(WAIT_MARGIN_NS),
            ))
            .ok()
            != Some(ipc::EventPortEvent {
                key: TIMER_KEY,
                signals: Signals::TIMER_FIRED,
            })
        || timer.info().ok().map(|info| info.size) != Some(0)
        || timer.signal_state().ok() != Some(Signals::TIMER_FIRED)
        || wait_only
            .borrow()
            .wait(Signals::TIMER_FIRED, Deadline::IMMEDIATE)
            .ok()
            != Some(Signals::TIMER_FIRED)
        || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || timer.arm(Deadline::IMMEDIATE).is_err()
        || event_port.wait_next(Deadline::IMMEDIATE).ok()
            != Some(ipc::EventPortEvent {
                key: TIMER_KEY,
                signals: Signals::TIMER_FIRED,
            })
    {
        return false;
    }

    let Ok(cancel_start) = platform::monotonic_time_ns() else {
        return false;
    };
    let canceled_fire_at = cancel_start.saturating_add(FIRE_DELAY_NS);
    if timer
        .arm(Deadline::from_monotonic_ns(canceled_fire_at))
        .is_err()
        || timer.signal_state().ok() != Some(Signals::EMPTY)
        || timer.cancel().is_err()
        || timer.info().ok().map(|info| info.size) != Some(0)
        || event_port
            .wait_next(Deadline::from_monotonic_ns(
                canceled_fire_at.saturating_add(FIRE_DELAY_NS),
            ))
            .err()
            != Some(ipc::Error::TIMED_OUT)
        || timer.signal_state().ok() != Some(Signals::EMPTY)
        || timer.arm(Deadline::IMMEDIATE).is_err()
        || event_port.wait_next(Deadline::IMMEDIATE).ok()
            != Some(ipc::EventPortEvent {
                key: TIMER_KEY,
                signals: Signals::TIMER_FIRED,
            })
        || timer.cancel().is_err()
        || timer.signal_state().ok() != Some(Signals::EMPTY)
    {
        return false;
    }

    true
}

fn event_probe() -> bool {
    const EVENT_PORT_KEY: u64 = 181;
    const WAIT_SET_KEY: u64 = 182;

    let Ok(event) = OwnedHandle::<Event>::create() else {
        return false;
    };
    let Ok(wait_only) = event.borrow().duplicate(Rights::WAIT) else {
        return false;
    };
    let Ok(signal_only) = event.borrow().duplicate(Rights::SIGNAL) else {
        return false;
    };
    let Ok(wait_set) = OwnedHandle::<WaitSet>::create() else {
        return false;
    };
    let Ok(event_port) = OwnedHandle::<EventPort>::create() else {
        return false;
    };

    if !event.info().is_ok_and(|info| {
        info.kind == ObjectKind::Event && info.rights == Rights::EVENT && info.size == 0
    }) || event.signal_state().ok() != Some(Signals::EMPTY)
        || event
            .borrow()
            .wait(Signals::SIGNALED, Deadline::IMMEDIATE)
            .err()
            != Some(ipc::Error::TIMED_OUT)
        || wait_only.set().err() != Some(ipc::Error::PERMISSION)
        || wait_only.reset().err() != Some(ipc::Error::PERMISSION)
        || signal_only.signal_state().err() != Some(ipc::Error::PERMISSION)
        || event_port
            .add(event.borrow(), Signals::READABLE, EVENT_PORT_KEY)
            .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || event_port
            .add(event.borrow(), Signals::SIGNALED, EVENT_PORT_KEY)
            .is_err()
        || wait_set
            .add(event.borrow(), Signals::SIGNALED, WAIT_SET_KEY)
            .is_err()
        || signal_only.set().is_err()
        || event.info().ok().map(|info| info.size) != Some(1)
        || wait_only.signal_state().ok() != Some(Signals::SIGNALED)
        || wait_only
            .borrow()
            .wait(Signals::SIGNALED, Deadline::IMMEDIATE)
            .ok()
            != Some(Signals::SIGNALED)
        || wait_set.wait_next(Deadline::IMMEDIATE).ok()
            != Some(ipc::WaitSetEvent {
                key: WAIT_SET_KEY,
                signals: Signals::SIGNALED,
            })
        || event_port.wait_next(Deadline::IMMEDIATE).ok()
            != Some(ipc::EventPortEvent {
                key: EVENT_PORT_KEY,
                signals: Signals::SIGNALED,
            })
        || signal_only.set().is_err()
        || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || signal_only.reset().is_err()
        || event.info().ok().map(|info| info.size) != Some(0)
        || event.signal_state().ok() != Some(Signals::EMPTY)
        || wait_set.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || event_port.wait_next(Deadline::IMMEDIATE).err() != Some(ipc::Error::TIMED_OUT)
        || signal_only.reset().is_err()
        || event.set().is_err()
        || event_port.wait_next(Deadline::IMMEDIATE).ok()
            != Some(ipc::EventPortEvent {
                key: EVENT_PORT_KEY,
                signals: Signals::SIGNALED,
            })
        || event.reset().is_err()
        || event.signal_state().ok() != Some(Signals::EMPTY)
    {
        return false;
    }

    true
}

fn event_port_waiter_probe() -> bool {
    const CHILD_EVENT_PORT_HANDLE: ipc::CapabilityHandle = 1;
    const NOTIFICATION_KEY: u64 = 131;

    let Ok(notification) = OwnedHandle::<Notification>::create() else {
        return false;
    };
    let Ok(event_port) = OwnedHandle::<EventPort>::create() else {
        return false;
    };
    if event_port
        .add(notification.borrow(), Signals::SIGNALED, NOTIFICATION_KEY)
        .is_err()
    {
        return false;
    }
    let Ok(barrier) = syscall::pipe_pair() else {
        return false;
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return false;
    };
    if child == 0 {
        let _ = syscall::close(barrier.reader);
        if !ipc::wait_for_handle(CHILD_EVENT_PORT_HANDLE)
            .is_ok_and(|info| info.kind == ObjectKind::EventPort && info.rights == Rights::WAIT)
            || syscall::write_all(barrier.writer, &[1]).is_err()
            || syscall::close(barrier.writer).is_err()
        {
            syscall::exit(78);
        }
        let woke = ipc::event_port_wait(CHILD_EVENT_PORT_HANDLE, Deadline::INFINITE).ok()
            == Some(ipc::EventPortEvent {
                key: NOTIFICATION_KEY,
                signals: Signals::SIGNALED,
            });
        let closed = ipc::close(CHILD_EVENT_PORT_HANDLE).is_ok();
        syscall::exit(if woke && closed { 0 } else { 79 });
    }

    let setup = syscall::close(barrier.writer).is_ok()
        && ipc::grant_child(
            child,
            event_port.as_raw(),
            Rights::WAIT,
            CHILD_EVENT_PORT_HANDLE,
        )
        .ok()
            == Some(CHILD_EVENT_PORT_HANDLE);
    let mut ready = [0_u8; 1];
    let synchronized = setup
        && syscall::read(barrier.reader, &mut ready).ok() == Some(1)
        && ready[0] == 1
        && syscall::close(barrier.reader).is_ok();
    if synchronized {
        for _ in 0..4 {
            let _ = syscall::yield_now();
        }
    }
    let signaled = synchronized && notification.signal(1).ok() == Some(1);
    let mut child_succeeded = false;
    if signaled {
        for _ in 0..JOB_WAIT_YIELDS {
            match syscall::try_wait_child(child) {
                Ok(status) => {
                    child_succeeded = status.success();
                    break;
                }
                Err(error) if error == syscall::Errno::TRY_AGAIN => {
                    let _ = syscall::yield_now();
                }
                Err(_) => break,
            }
        }
    }
    if !child_succeeded {
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
    }
    let _ = syscall::close(barrier.reader);
    let _ = syscall::close(barrier.writer);
    signaled && child_succeeded
}

fn supplementary_probes_enabled(argument: &[u8]) -> bool {
    argument != b"runtime-smoke" && argument != b"manual-argv"
}

fn owned_handle_probe() -> bool {
    const OWNED_MESSAGE: &[u8] = b"owned-handle";

    let Ok(endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    if endpoint.info().ok().map(|info| info.kind) != Some(ObjectKind::Endpoint)
        || endpoint
            .borrow()
            .wait(Signals::WRITABLE, Deadline::IMMEDIATE)
            .ok()
            != Some(Signals::WRITABLE)
    {
        return false;
    }

    let Ok(notification) = OwnedHandle::<Notification>::create() else {
        return false;
    };
    let notification_raw = notification.as_raw();
    if ipc::send(
        endpoint.as_raw(),
        OWNED_MESSAGE,
        Some(Transfer {
            handle: notification_raw,
            rights: Rights::WAIT,
        }),
    )
    .is_err()
    {
        return false;
    }
    let mut output = [0_u8; OWNED_MESSAGE.len()];
    let Ok(message) = endpoint.try_receive(&mut output) else {
        return false;
    };
    let Some(received) = message.capability else {
        return false;
    };
    if message.bytes != OWNED_MESSAGE.len()
        || output != OWNED_MESSAGE
        || received.rights != Rights::WAIT
    {
        return false;
    }
    let received_raw = received.handle.as_raw();
    let Ok(received) = received.handle.try_cast::<Notification>() else {
        return false;
    };
    drop(received);
    if ipc::info(received_raw).err() != Some(ipc::Error::BAD_FILE_DESCRIPTOR)
        || notification.close().is_err()
        || ipc::info(notification_raw).err() != Some(ipc::Error::BAD_FILE_DESCRIPTOR)
    {
        return false;
    }

    let untyped = endpoint.erase();
    let untyped = match untyped.try_cast::<Notification>() {
        Err((ipc::Error::INVALID_ARGUMENT, untyped)) => untyped,
        Ok(notification) => {
            drop(notification);
            return false;
        }
        Err((_, untyped)) => {
            drop(untyped);
            return false;
        }
    };
    let Ok(mut endpoint) = untyped.try_cast::<Endpoint>() else {
        return false;
    };

    let Ok(duplicate) = endpoint.borrow().duplicate(Rights::SEND) else {
        return false;
    };
    let duplicate_raw = duplicate.as_raw();
    drop(duplicate);
    if ipc::info(duplicate_raw).err() != Some(ipc::Error::BAD_FILE_DESCRIPTOR)
        || endpoint.replace_rights(Rights::SEND).is_err()
        || endpoint.info().ok().map(|info| info.rights) != Some(Rights::SEND)
    {
        return false;
    }
    let endpoint_raw = endpoint.into_raw();
    if ipc::info(endpoint_raw).is_err() || ipc::close(endpoint_raw).is_err() {
        return false;
    }
    true
}

fn channel_pair_probe(current_process: u64) -> bool {
    const FORWARD: &[u8] = b"channel-forward";
    const REPLY: &[u8] = b"channel-reply";
    const QUEUED: &[u8] = b"queued-after-close";

    let Ok((first, second)) = OwnedHandle::<Endpoint>::create_pair() else {
        return false;
    };
    let Ok(first_info) = first.info() else {
        return false;
    };
    let Ok(second_info) = second.info() else {
        return false;
    };
    if first_info.object_id == second_info.object_id
        || first_info.kind != ObjectKind::Endpoint
        || second_info.kind != ObjectKind::Endpoint
        || first_info.rights != Rights::ENDPOINT
        || second_info.rights != Rights::ENDPOINT
        || first.signal_state().ok() != Some(Signals::WRITABLE)
        || second.signal_state().ok() != Some(Signals::WRITABLE)
    {
        return false;
    }

    let mut buffer = [0_u8; QUEUED.len()];
    if first.send(FORWARD).is_err()
        || first.signal_state().ok() != Some(Signals::WRITABLE)
        || second.signal_state().ok() != Some(Signals::READABLE | Signals::WRITABLE)
    {
        return false;
    }
    let Ok(forward) = second.try_receive(&mut buffer) else {
        return false;
    };
    if forward.sender_process_id != current_process
        || forward.bytes != FORWARD.len()
        || forward.capability.is_some()
        || &buffer[..FORWARD.len()] != FORWARD
        || second.send(REPLY).is_err()
    {
        return false;
    }
    let Ok(reply) = first.try_receive(&mut buffer) else {
        return false;
    };
    if reply.sender_process_id != current_process
        || reply.bytes != REPLY.len()
        || reply.capability.is_some()
        || &buffer[..REPLY.len()] != REPLY
        || first.send(QUEUED).is_err()
    {
        return false;
    }

    let first_raw = first.as_raw();
    drop(first);
    if ipc::info(first_raw).err() != Some(ipc::Error::BAD_FILE_DESCRIPTOR)
        || second.signal_state().ok() != Some(Signals::READABLE | Signals::PEER_CLOSED)
        || second.wait(Signals::PEER_CLOSED, Deadline::IMMEDIATE).ok() != Some(Signals::PEER_CLOSED)
        || ipc::wait_many(
            &[WaitItem::new(second.as_raw(), Signals::PEER_CLOSED)],
            Deadline::IMMEDIATE,
        )
        .ok()
            != Some(0)
    {
        return false;
    }
    let Ok(queued) = second.try_receive(&mut buffer) else {
        return false;
    };
    if queued.sender_process_id != current_process
        || queued.bytes != QUEUED.len()
        || queued.capability.is_some()
        || buffer != *QUEUED
        || second.signal_state().ok() != Some(Signals::PEER_CLOSED)
        || second.send(b"closed").err() != Some(ipc::Error::BROKEN_PIPE)
        || second.try_receive(&mut buffer).err() != Some(ipc::Error::BROKEN_PIPE)
    {
        return false;
    }
    drop(second);

    let Ok((first, second)) = OwnedHandle::<Endpoint>::create_pair() else {
        return false;
    };
    let Ok(duplicate) = first.borrow().duplicate(Rights::ENDPOINT) else {
        return false;
    };
    drop(first);
    if second.signal_state().ok() != Some(Signals::WRITABLE) {
        return false;
    }
    drop(duplicate);
    if second.signal_state().ok() != Some(Signals::PEER_CLOSED) {
        return false;
    }
    drop(second);

    channel_pair_process_exit_probe()
}

fn channel_pair_process_exit_probe() -> bool {
    const CHILD_ENDPOINT_HANDLE: ipc::CapabilityHandle = 1;

    let Ok((first, second)) = OwnedHandle::<Endpoint>::create_pair() else {
        return false;
    };
    let Ok(barrier) = syscall::pipe_pair() else {
        return false;
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return false;
    };
    if child == 0 {
        let _ = syscall::close(barrier.reader);
        let ready = ipc::wait_for_handle(CHILD_ENDPOINT_HANDLE)
            .is_ok_and(|info| info.kind == ObjectKind::Endpoint)
            && syscall::write_all(barrier.writer, &[1]).is_ok()
            && syscall::close(barrier.writer).is_ok();
        syscall::exit(if ready { 0 } else { 79 });
    }

    let setup = syscall::close(barrier.writer).is_ok()
        && ipc::grant_child(
            child,
            second.as_raw(),
            Rights::ENDPOINT,
            CHILD_ENDPOINT_HANDLE,
        )
        .ok()
            == Some(CHILD_ENDPOINT_HANDLE);
    let mut ready = [0_u8; 1];
    let synchronized = setup
        && syscall::read(barrier.reader, &mut ready).ok() == Some(1)
        && ready[0] == 1
        && syscall::close(barrier.reader).is_ok();
    drop(second);
    let child_succeeded =
        synchronized && syscall::wait_child(child).is_ok_and(|status| status.success());
    let peer_closed = child_succeeded
        && first.signal_state().ok() == Some(Signals::PEER_CLOSED)
        && first.wait(Signals::PEER_CLOSED, Deadline::IMMEDIATE).ok() == Some(Signals::PEER_CLOSED);
    if !child_succeeded {
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
    }
    let _ = syscall::close(barrier.reader);
    let _ = syscall::close(barrier.writer);
    peer_closed
}

fn async_ipc_probe(current_process: u64) -> bool {
    const CHILD_ENDPOINT_HANDLE: ipc::CapabilityHandle = 1;
    const CHILD_MESSAGE: &[u8] = b"async-wakeup";
    const LOCAL_MESSAGE: &[u8] = b"async-send";
    const MOVE_MESSAGE: &[u8] = b"async-move";

    let Ok(endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(barrier) = syscall::pipe_pair() else {
        return false;
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return false;
    };
    if child == 0 {
        let _ = syscall::close(barrier.reader);
        let ready = ipc::wait_for_handle(CHILD_ENDPOINT_HANDLE)
            .is_ok_and(|info| info.kind == ObjectKind::Endpoint && info.rights == Rights::SEND)
            && syscall::write_all(barrier.writer, &[1]).is_ok()
            && syscall::close(barrier.writer).is_ok();
        if ready {
            for _ in 0..4 {
                let _ = syscall::yield_now();
            }
        }
        let sent = ready
            && ipc::send(CHILD_ENDPOINT_HANDLE, CHILD_MESSAGE, None).is_ok()
            && ipc::close(CHILD_ENDPOINT_HANDLE).is_ok();
        syscall::exit(if sent { 0 } else { 80 });
    }

    let setup = syscall::close(barrier.writer).is_ok()
        && ipc::grant_child(
            child,
            endpoint.as_raw(),
            Rights::SEND,
            CHILD_ENDPOINT_HANDLE,
        )
        .ok()
            == Some(CHILD_ENDPOINT_HANDLE);
    let mut ready = [0_u8; 1];
    let synchronized = setup
        && syscall::read(barrier.reader, &mut ready).ok() == Some(1)
        && ready[0] == 1
        && syscall::close(barrier.reader).is_ok();

    let reactor = Reactor::<2>::new();
    let mut buffer = [0_u8; 16];
    let child_message = if synchronized {
        let mut receive = reactor.receive(endpoint.borrow(), &mut buffer);
        match reactor.run(&mut receive) {
            Ok(Ok(message)) => Some(message),
            _ => None,
        }
    } else {
        None
    };
    let child_received = child_message.is_some_and(|message| {
        message.sender_process_id == child
            && message.bytes == CHILD_MESSAGE.len()
            && message.capability.is_none()
            && buffer[..CHILD_MESSAGE.len()] == *CHILD_MESSAGE
    });

    let local_sent = if child_received {
        let mut send = reactor.send(endpoint.borrow(), LOCAL_MESSAGE);
        matches!(reactor.run(&mut send), Ok(Ok(())))
    } else {
        false
    };
    let local_received = if local_sent {
        let mut receive = reactor.receive(endpoint.borrow(), &mut buffer);
        match reactor.run(&mut receive) {
            Ok(Ok(message)) => {
                message.sender_process_id == current_process
                    && message.bytes == LOCAL_MESSAGE.len()
                    && message.capability.is_none()
                    && buffer[..LOCAL_MESSAGE.len()] == *LOCAL_MESSAGE
            }
            _ => false,
        }
    } else {
        false
    };

    let moved = match local_received
        .then(OwnedHandle::<Notification>::create)
        .transpose()
    {
        Ok(Some(notification)) => {
            let mut send =
                reactor.send_move(endpoint.borrow(), MOVE_MESSAGE, notification, Rights::WAIT);
            matches!(reactor.run(&mut send), Ok(Ok(())))
        }
        _ => false,
    };
    let move_received = if moved {
        let mut receive = reactor.receive(endpoint.borrow(), &mut buffer);
        match reactor.run(&mut receive) {
            Ok(Ok(message)) => match message.capability {
                Some(capability) if capability.rights == Rights::WAIT => {
                    let typed = capability.handle.try_cast::<Notification>();
                    message.sender_process_id == current_process
                        && message.bytes == MOVE_MESSAGE.len()
                        && buffer[..MOVE_MESSAGE.len()] == *MOVE_MESSAGE
                        && typed.is_ok()
                }
                _ => false,
            },
            _ => false,
        }
    } else {
        false
    };

    let mut child_succeeded = false;
    if synchronized {
        for _ in 0..JOB_WAIT_YIELDS {
            match syscall::try_wait_child(child) {
                Ok(status) => {
                    child_succeeded = status.success();
                    break;
                }
                Err(error) if error == syscall::Errno::TRY_AGAIN => {
                    let _ = syscall::yield_now();
                }
                Err(_) => break,
            }
        }
    }
    if !child_succeeded {
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
    }
    let _ = syscall::close(barrier.reader);
    let _ = syscall::close(barrier.writer);
    child_received
        && local_received
        && move_received
        && child_succeeded
        && async_control_probe(&reactor)
}

fn async_control_probe(reactor: &Reactor<2>) -> bool {
    const PERIOD_NS: u64 = 20_000_000;
    const WAIT_MARGIN_NS: u64 = 500_000_000;

    let Ok((source, token)) = CancellationSource::new() else {
        return false;
    };
    let Ok(cloned_token) = token.try_clone() else {
        return false;
    };
    let Ok(endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(start) = platform::monotonic_time_ns() else {
        return false;
    };
    let deadline_ns = start.saturating_add(PERIOD_NS);
    let scope = RunScope::with_cancellation(Deadline::from_monotonic_ns(deadline_ns), &token);
    let mut buffer = [0_u8; 1];
    let timed_out = {
        let mut receive = reactor.receive(endpoint.borrow(), &mut buffer);
        matches!(
            reactor.run_scoped(&mut receive, scope),
            Err(RunError::Wait(error)) if error == ipc::Error::TIMED_OUT
        )
    };
    if !timed_out
        || !platform::monotonic_time_ns().is_ok_and(|now| now >= deadline_ns)
        || token.is_cancelled().ok() != Some(false)
        || source.cancel().is_err()
        || token.is_cancelled().ok() != Some(true)
        || cloned_token.is_cancelled().ok() != Some(true)
    {
        return false;
    }

    let mut canceled_receive = reactor.receive(endpoint.borrow(), &mut buffer);
    if !matches!(
        reactor.run_scoped(
            &mut canceled_receive,
            RunScope::with_cancellation(Deadline::INFINITE, &token),
        ),
        Err(RunError::Cancelled)
    ) {
        return false;
    }
    let mut cancelled = reactor.cancelled(&cloned_token);
    if !matches!(reactor.run(&mut cancelled), Ok(Ok(()))) {
        return false;
    }

    let Ok(mut periodic) = PeriodicTimer::start_after(PERIOD_NS) else {
        return false;
    };
    let first_deadline = periodic.next_deadline();
    let mut first_tick = reactor.next_tick(&mut periodic);
    let first = match reactor.run_until(
        &mut first_tick,
        Deadline::from_monotonic_ns(
            first_deadline
                .as_monotonic_ns()
                .saturating_add(WAIT_MARGIN_NS),
        ),
    ) {
        Ok(Ok(tick)) => tick,
        _ => return false,
    };
    if first.scheduled != first_deadline
        || first.observed_ns < first.scheduled.as_monotonic_ns()
        || first.expirations == 0
    {
        return false;
    }

    let coalesce_at = periodic
        .next_deadline()
        .as_monotonic_ns()
        .saturating_add(PERIOD_NS.saturating_mul(2));
    loop {
        match platform::monotonic_time_ns() {
            Ok(now) if now >= coalesce_at => break,
            Ok(_) => {
                if syscall::yield_now().is_err() {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    let scheduled = periodic.next_deadline();
    let mut coalesced_tick = reactor.next_tick(&mut periodic);
    let coalesced = match reactor.run(&mut coalesced_tick) {
        Ok(Ok(tick)) => tick,
        _ => return false,
    };
    coalesced.scheduled == scheduled
        && coalesced.observed_ns >= coalesce_at
        && coalesced.expirations >= 3
        && periodic.next_deadline().as_monotonic_ns() > coalesced.observed_ns
        && periodic.cancel().is_ok()
        && task_executor_probe()
}

fn task_executor_probe() -> bool {
    const PRODUCER_PERIOD_NS: u64 = 10_000_000;
    const CANCELLER_PERIOD_NS: u64 = 20_000_000;
    const TASK_TIMEOUT_NS: u64 = 50_000_000;

    let reactor = Reactor::<8>::new();
    let Ok(root_group) = TaskGroup::root(TaskRole::Service, Deadline::INFINITE) else {
        return false;
    };
    let Ok(request_parent) = root_group.child(TaskRole::Activation, Deadline::INFINITE) else {
        return false;
    };
    let Ok(cancelled_group) = request_parent.child(TaskRole::Request, Deadline::INFINITE) else {
        return false;
    };
    let Ok(now) = platform::monotonic_time_ns() else {
        return false;
    };
    let timeout_deadline = Deadline::from_monotonic_ns(now.saturating_add(TASK_TIMEOUT_NS));
    let Ok(timeout_group) = root_group.child(TaskRole::Background, timeout_deadline) else {
        return false;
    };
    let Ok(endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(cancelled_endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(timeout_endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(mut producer_timer) = PeriodicTimer::start_after(PRODUCER_PERIOD_NS) else {
        return false;
    };
    let Ok(mut canceller_timer) = PeriodicTimer::start_after(CANCELLER_PERIOD_NS) else {
        return false;
    };

    let receiver_completed = Cell::new(false);
    let producer_completed = Cell::new(false);
    let canceller_completed = Cell::new(false);
    let mut receiver = core::pin::pin!(async {
        let mut bytes = [0_u8; 1];
        let message = reactor.receive(endpoint.borrow(), &mut bytes).await?;
        if message.bytes != 1 || bytes[0] != 0x5a {
            return Err(ipc::Error::IO);
        }
        receiver_completed.set(true);
        Ok(())
    });
    let mut producer = core::pin::pin!(async {
        let tick = reactor.next_tick(&mut producer_timer).await?;
        if tick.expirations == 0 {
            return Err(ipc::Error::IO);
        }
        reactor.send(endpoint.borrow(), &[0x5a]).await?;
        producer_completed.set(true);
        Ok(())
    });
    let mut cancelled = core::pin::pin!(async {
        let mut bytes = [0_u8; 1];
        let _ = reactor
            .receive(cancelled_endpoint.borrow(), &mut bytes)
            .await?;
        Err(ipc::Error::IO)
    });
    let mut canceller = core::pin::pin!(async {
        let tick = reactor.next_tick(&mut canceller_timer).await?;
        if tick.expirations == 0 {
            return Err(ipc::Error::IO);
        }
        request_parent.cancel()?;
        canceller_completed.set(true);
        Ok(())
    });
    let mut timed_out = core::pin::pin!(async {
        let mut bytes = [0_u8; 1];
        let _ = reactor
            .receive(timeout_endpoint.borrow(), &mut bytes)
            .await?;
        Err(ipc::Error::IO)
    });

    let Ok(mut executor) = TaskExecutor::<5, 8>::new(&reactor) else {
        return false;
    };
    let Ok(receiver_id) = executor.spawn_pinned(receiver.as_mut(), &root_group, Deadline::INFINITE)
    else {
        return false;
    };
    let Ok(producer_id) = executor.spawn_pinned(producer.as_mut(), &root_group, Deadline::INFINITE)
    else {
        return false;
    };
    let Ok(cancelled_id) =
        executor.spawn_pinned(cancelled.as_mut(), &cancelled_group, Deadline::INFINITE)
    else {
        return false;
    };
    let Ok(canceller_id) =
        executor.spawn_pinned(canceller.as_mut(), &root_group, Deadline::INFINITE)
    else {
        return false;
    };
    let Ok(timed_out_id) =
        executor.spawn_pinned(timed_out.as_mut(), &timeout_group, Deadline::INFINITE)
    else {
        return false;
    };

    if executor.run().is_err() {
        return false;
    }
    let mut trace = [TaskTraceEvent {
        sequence: 0,
        task: receiver_id,
        attribution: root_group.attribution(),
        kind: TaskTraceKind::Spawned,
    }; MAX_TASK_TRACE_EVENTS];
    let trace_read = executor.read_trace(0, &mut trace);
    let trace = &trace[..trace_read.events];
    let trace_is_conformant = trace_read.missed == 0
        && trace
            .windows(2)
            .all(|events| events[0].sequence < events[1].sequence)
        && trace
            .iter()
            .any(|event| event.task == cancelled_id && event.kind == TaskTraceKind::Cancelled)
        && trace
            .iter()
            .any(|event| event.task == timed_out_id && event.kind == TaskTraceKind::TimedOut)
        && trace.iter().any(|event| {
            event.task == receiver_id && event.kind == TaskTraceKind::Completed(Ok(()))
        });

    trace_is_conformant
        && executor.outcome(receiver_id) == Some(TaskOutcome::Completed(Ok(())))
        && executor.outcome(producer_id) == Some(TaskOutcome::Completed(Ok(())))
        && executor.outcome(cancelled_id) == Some(TaskOutcome::Cancelled)
        && executor.outcome(canceller_id) == Some(TaskOutcome::Completed(Ok(())))
        && executor.outcome(timed_out_id) == Some(TaskOutcome::TimedOut)
        && executor.attribution(cancelled_id)
            == Some(TaskAttribution {
                role: TaskRole::Request,
                group_depth: 3,
            })
        && executor.attribution(timed_out_id)
            == Some(TaskAttribution {
                role: TaskRole::Background,
                group_depth: 2,
            })
        && receiver_completed.get()
        && producer_completed.get()
        && canceller_completed.get()
        && cancelled_group.is_cancelled().ok() == Some(true)
        && cancelled_group.is_locally_cancelled().ok() == Some(false)
        && request_parent.is_locally_cancelled().ok() == Some(true)
        && root_group.is_cancelled().ok() == Some(false)
        && timeout_group.is_cancelled().ok() == Some(false)
        && producer_timer.cancel().is_ok()
        && canceller_timer.cancel().is_ok()
        && task_shutdown_probe()
}

fn task_shutdown_probe() -> bool {
    const DRAIN_NS: u64 = 30_000_000;

    let reactor = Reactor::<3>::new();
    let Ok(group) = TaskGroup::root(TaskRole::Service, Deadline::INFINITE) else {
        return false;
    };
    let Ok(background_group) = group.child(TaskRole::Background, Deadline::INFINITE) else {
        return false;
    };
    let Ok(shutdown) = Shutdown::new() else {
        return false;
    };
    let Ok(stubborn_endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let cooperative_completed = Cell::new(false);
    let mut cooperative = core::pin::pin!(async {
        reactor.cancelled(shutdown.token()).await?;
        cooperative_completed.set(true);
        Ok(())
    });
    let mut stubborn = core::pin::pin!(async {
        let mut bytes = [0_u8; 1];
        let _ = reactor
            .receive(stubborn_endpoint.borrow(), &mut bytes)
            .await?;
        Err(ipc::Error::IO)
    });
    let Ok(mut executor) = TaskExecutor::<2, 3>::new(&reactor) else {
        return false;
    };
    let Ok(cooperative_id) =
        executor.spawn_pinned(cooperative.as_mut(), &group, Deadline::INFINITE)
    else {
        return false;
    };
    let Ok(stubborn_id) =
        executor.spawn_pinned(stubborn.as_mut(), &background_group, Deadline::INFINITE)
    else {
        return false;
    };
    let Ok(now) = platform::monotonic_time_ns() else {
        return false;
    };
    let drain_deadline = Deadline::from_monotonic_ns(now.saturating_add(DRAIN_NS));
    let Ok(report) = executor.shutdown(&shutdown, drain_deadline) else {
        return false;
    };

    shutdown.is_requested().ok() == Some(true)
        && cooperative_completed.get()
        && executor.outcome(cooperative_id) == Some(TaskOutcome::Completed(Ok(())))
        && executor.outcome(stubborn_id) == Some(TaskOutcome::ShutdownTimedOut)
        && executor.attribution(stubborn_id)
            == Some(TaskAttribution {
                role: TaskRole::Background,
                group_depth: 2,
            })
        && report
            == DrainReport {
                completed: 1,
                cancelled: 0,
                timed_out: 0,
                shutdown_timed_out: 1,
            }
        && report.total() == 2
}

fn capability_probe(current_process: u64) -> bool {
    const MESSAGE: &[u8] = b"phase-one-ipc";
    const SHARED_BYTES: &[u8] = b"shared capability memory";
    const SHARED_OFFSET: usize = 7;

    let Ok(endpoint) = ipc::endpoint_create() else {
        return false;
    };
    let Ok(endpoint_info) = ipc::info(endpoint) else {
        return false;
    };
    if endpoint_info.kind != ObjectKind::Endpoint
        || !endpoint_info
            .rights
            .contains(Rights::SEND | Rights::RECEIVE)
        || endpoint_info.size != 0
        || ipc::signal_state(endpoint).ok() != Some(Signals::WRITABLE)
        || ipc::wait_one(endpoint, Signals::WRITABLE, Deadline::IMMEDIATE).ok()
            != Some(Signals::WRITABLE)
    {
        return false;
    }

    let Ok(replacement_source) = ipc::duplicate(endpoint, Rights::ENDPOINT) else {
        return false;
    };
    if ipc::replace(replacement_source, Rights::EMPTY).err() != Some(ipc::Error::PERMISSION) {
        return false;
    }
    let Ok(send_only) = ipc::replace(replacement_source, Rights::SEND) else {
        return false;
    };
    let Ok(send_only_info) = ipc::info(send_only) else {
        return false;
    };
    let mut denied_buffer = [0_u8; 1];
    if send_only_info.object_id != endpoint_info.object_id
        || send_only_info.kind != ObjectKind::Endpoint
        || send_only_info.rights != Rights::SEND
        || ipc::signal_state(send_only).err() != Some(ipc::Error::PERMISSION)
        || ipc::wait_one(send_only, Signals::WRITABLE, Deadline::IMMEDIATE).err()
            != Some(ipc::Error::PERMISSION)
        || ipc::try_receive(send_only, &mut denied_buffer).err() != Some(ipc::Error::PERMISSION)
    {
        return false;
    }

    let Ok(notification) = ipc::notification_create() else {
        return false;
    };
    let Ok(shared_memory) = ipc::shared_memory_create(64) else {
        return false;
    };
    let Ok(read_only_memory) = ipc::duplicate(shared_memory, Rights::READ) else {
        return false;
    };
    if ipc::shared_memory_write(read_only_memory, 0, b"denied").err()
        != Some(ipc::Error::PERMISSION)
    {
        return false;
    }
    if ipc::shared_memory_write(shared_memory, SHARED_OFFSET, SHARED_BYTES).ok()
        != Some(SHARED_BYTES.len())
    {
        return false;
    }
    let mut shared_readback = [0_u8; SHARED_BYTES.len()];
    if ipc::shared_memory_read(read_only_memory, SHARED_OFFSET, &mut shared_readback).ok()
        != Some(SHARED_BYTES.len())
        || shared_readback.as_slice() != SHARED_BYTES
    {
        return false;
    }

    if ipc::send(
        endpoint,
        MESSAGE,
        Some(Transfer {
            handle: notification,
            rights: Rights::WAIT,
        }),
    )
    .is_err()
        || ipc::signal_state(endpoint).ok() != Some(Signals::READABLE | Signals::WRITABLE)
    {
        return false;
    }

    let mut message_buffer = [0_u8; 32];
    let Ok(message) = ipc::try_receive(endpoint, &mut message_buffer) else {
        return false;
    };
    let Some(received_capability) = message.capability else {
        return false;
    };
    if message.sender_process_id != current_process
        || message.bytes != MESSAGE.len()
        || &message_buffer[..message.bytes] != MESSAGE
        || received_capability.rights != Rights::WAIT
        || ipc::signal_state(endpoint).ok() != Some(Signals::WRITABLE)
    {
        return false;
    }
    let Ok(received_info) = ipc::info(received_capability.handle) else {
        return false;
    };
    if received_info.kind != ObjectKind::Notification
        || received_info.rights != Rights::WAIT
        || ipc::signal_state(received_capability.handle).ok() != Some(Signals::EMPTY)
        || ipc::wait_many(
            &[
                WaitItem::new(received_capability.handle, Signals::SIGNALED),
                WaitItem::new(endpoint, Signals::WRITABLE),
            ],
            Deadline::IMMEDIATE,
        )
        .ok()
            != Some(1)
        || ipc::wait_many(
            &[
                WaitItem::new(endpoint, Signals::WRITABLE),
                WaitItem::new(send_only, Signals::WRITABLE),
            ],
            Deadline::IMMEDIATE,
        )
        .err()
            != Some(ipc::Error::PERMISSION)
        || ipc::wait_many(&[], Deadline::IMMEDIATE).err() != Some(ipc::Error::INVALID_ARGUMENT)
        || ipc::wait_many(
            &[WaitItem::new(endpoint, Signals::WRITABLE); limits::MAX_OBJECT_WAIT_ITEMS + 1],
            Deadline::IMMEDIATE,
        )
        .err()
            != Some(ipc::Error::ARGUMENT_TOO_LARGE)
        || ipc::wait_one(
            received_capability.handle,
            Signals::SIGNALED,
            Deadline::IMMEDIATE,
        )
        .err()
            != Some(ipc::Error::TIMED_OUT)
        || ipc::wait_one(
            received_capability.handle,
            Signals::READABLE,
            Deadline::IMMEDIATE,
        )
        .err()
            != Some(ipc::Error::INVALID_ARGUMENT)
        || ipc::notification_signal(received_capability.handle, 1).err()
            != Some(ipc::Error::PERMISSION)
    {
        return false;
    }

    if ipc::notification_signal(notification, 2).ok() != Some(2)
        || ipc::signal_state(received_capability.handle).ok() != Some(Signals::SIGNALED)
        || ipc::wait_one(
            received_capability.handle,
            Signals::SIGNALED,
            Deadline::IMMEDIATE,
        )
        .ok()
            != Some(Signals::SIGNALED)
        || ipc::notification_try_wait(received_capability.handle).ok() != Some(1)
        || ipc::notification_try_wait(received_capability.handle).ok() != Some(0)
        || ipc::notification_try_wait(received_capability.handle).err()
            != Some(ipc::Error::TRY_AGAIN)
        || ipc::signal_state(received_capability.handle).ok() != Some(Signals::EMPTY)
    {
        return false;
    }

    let Ok(before_timeout) = platform::monotonic_time_ns() else {
        return false;
    };
    let timeout = before_timeout.saturating_add(20_000_000);
    if ipc::wait_one(
        received_capability.handle,
        Signals::SIGNALED,
        Deadline::from_monotonic_ns(timeout),
    )
    .err()
        != Some(ipc::Error::TIMED_OUT)
        || !platform::monotonic_time_ns().is_ok_and(|now| now >= timeout)
    {
        return false;
    }

    ipc::close(received_capability.handle).is_ok()
        && ipc::close(read_only_memory).is_ok()
        && ipc::close(shared_memory).is_ok()
        && ipc::close(notification).is_ok()
        && ipc::close(send_only).is_ok()
        && ipc::close(endpoint).is_ok()
        && endpoint_move_transfer_probe(current_process)
        && notification_waiter_probe()
}

fn notification_waiter_probe() -> bool {
    const FIRST_NOTIFICATION_HANDLE: ipc::CapabilityHandle = 1;
    const SECOND_NOTIFICATION_HANDLE: ipc::CapabilityHandle = 2;

    let Ok(first_notification) = ipc::notification_create() else {
        return false;
    };
    let Ok(second_notification) = ipc::notification_create() else {
        let _ = ipc::close(first_notification);
        return false;
    };
    let Ok(barrier) = syscall::pipe_pair() else {
        let _ = ipc::close(first_notification);
        let _ = ipc::close(second_notification);
        return false;
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        let _ = ipc::close(first_notification);
        let _ = ipc::close(second_notification);
        return false;
    };
    if child == 0 {
        let _ = syscall::close(barrier.reader);
        if !ipc::wait_for_handle(FIRST_NOTIFICATION_HANDLE)
            .is_ok_and(|info| info.kind == ObjectKind::Notification && info.rights == Rights::WAIT)
            || !ipc::wait_for_handle(SECOND_NOTIFICATION_HANDLE).is_ok_and(|info| {
                info.kind == ObjectKind::Notification && info.rights == Rights::WAIT
            })
            || syscall::write_all(barrier.writer, &[1]).is_err()
            || syscall::close(barrier.writer).is_err()
        {
            syscall::exit(74);
        }
        let woke = ipc::wait_many(
            &[
                WaitItem::new(FIRST_NOTIFICATION_HANDLE, Signals::SIGNALED),
                WaitItem::new(SECOND_NOTIFICATION_HANDLE, Signals::SIGNALED),
            ],
            Deadline::INFINITE,
        )
        .ok()
            == Some(1);
        let consumed = ipc::notification_try_wait(SECOND_NOTIFICATION_HANDLE).ok() == Some(0)
            && ipc::notification_try_wait(FIRST_NOTIFICATION_HANDLE).err()
                == Some(ipc::Error::TRY_AGAIN);
        let closed = ipc::close(FIRST_NOTIFICATION_HANDLE).is_ok()
            && ipc::close(SECOND_NOTIFICATION_HANDLE).is_ok();
        syscall::exit(if woke && consumed && closed { 0 } else { 75 });
    }

    let setup = syscall::close(barrier.writer).is_ok()
        && ipc::grant_child(
            child,
            first_notification,
            Rights::WAIT,
            FIRST_NOTIFICATION_HANDLE,
        )
        .ok()
            == Some(FIRST_NOTIFICATION_HANDLE)
        && ipc::grant_child(
            child,
            second_notification,
            Rights::WAIT,
            SECOND_NOTIFICATION_HANDLE,
        )
        .ok()
            == Some(SECOND_NOTIFICATION_HANDLE);
    let mut ready = [0_u8; 1];
    let synchronized = setup
        && syscall::read(barrier.reader, &mut ready).ok() == Some(1)
        && ready[0] == 1
        && syscall::close(barrier.reader).is_ok();
    if synchronized {
        for _ in 0..4 {
            let _ = syscall::yield_now();
        }
    }
    let signaled = synchronized && ipc::notification_signal(second_notification, 1).ok() == Some(1);
    let mut child_succeeded = false;
    if signaled {
        for _ in 0..JOB_WAIT_YIELDS {
            match syscall::try_wait_child(child) {
                Ok(status) => {
                    child_succeeded = status.success();
                    break;
                }
                Err(error) if error == syscall::Errno::TRY_AGAIN => {
                    let _ = syscall::yield_now();
                }
                Err(_) => break,
            }
        }
    }
    if !child_succeeded {
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
    }
    let _ = syscall::close(barrier.reader);
    let _ = syscall::close(barrier.writer);
    ipc::close(first_notification).is_ok()
        && ipc::close(second_notification).is_ok()
        && signaled
        && child_succeeded
}

fn endpoint_move_transfer_probe(current_process: u64) -> bool {
    const MOVED_MESSAGE: &[u8] = b"move-capability";

    let Ok(endpoint) = OwnedHandle::<Endpoint>::create() else {
        return false;
    };
    let Ok(notification) = OwnedHandle::<Notification>::create() else {
        return false;
    };
    let Ok(source_info) = notification.info() else {
        return false;
    };
    for _ in 0..limits::MAX_ENDPOINT_MESSAGES {
        if endpoint.send(&[]).is_err() {
            return false;
        }
    }
    let notification = match endpoint.send_move(MOVED_MESSAGE, notification, Rights::NOTIFICATION) {
        Err(error) if error.error() == ipc::Error::TRY_AGAIN => error.into_handle(),
        Err(_) | Ok(()) => return false,
    };
    if notification.info().ok() != Some(source_info) {
        return false;
    }

    let mut buffer = [0_u8; MOVED_MESSAGE.len()];
    for _ in 0..limits::MAX_ENDPOINT_MESSAGES {
        let Ok(message) = endpoint.try_receive(&mut buffer) else {
            return false;
        };
        if message.bytes != 0 || message.capability.is_some() {
            return false;
        }
    }
    let notification_raw = notification.as_raw();
    if endpoint
        .send_move(MOVED_MESSAGE, notification, Rights::NOTIFICATION)
        .is_err()
        || ipc::info(notification_raw).err() != Some(ipc::Error::BAD_FILE_DESCRIPTOR)
    {
        return false;
    }
    let Ok(message) = endpoint.try_receive(&mut buffer) else {
        return false;
    };
    let Some(received) = message.capability else {
        return false;
    };
    let received_rights = received.rights;
    let Ok(received) = received.handle.try_cast::<Notification>() else {
        return false;
    };
    let moved = message.sender_process_id == current_process
        && message.bytes == MOVED_MESSAGE.len()
        && buffer.as_slice() == MOVED_MESSAGE
        && received_rights == Rights::NOTIFICATION
        && received.info().is_ok_and(|info| {
            info.object_id == source_info.object_id
                && info.kind == ObjectKind::Notification
                && info.rights == Rights::NOTIFICATION
        })
        && received.signal(1).ok() == Some(1)
        && received.try_wait().ok() == Some(0);

    drop(received);
    drop(endpoint);
    moved && endpoint_move_waiter_probe(current_process)
}

fn endpoint_multi_handle_probe(current_process: u64) -> bool {
    const MESSAGE: &[u8] = b"multi-handle";

    let Ok((sender, receiver)) = OwnedHandle::<Endpoint>::create_pair() else {
        return false;
    };
    let Ok(notification) = OwnedHandle::<Notification>::create() else {
        return false;
    };
    let Ok(memory) = OwnedHandle::<SharedMemory>::create(16) else {
        return false;
    };
    if notification.signal(1).ok() != Some(1) || memory.write(0, b"bulk").ok() != Some(4) {
        return false;
    }
    let Ok(notification_info) = notification.info() else {
        return false;
    };
    let Ok(memory_info) = memory.info() else {
        return false;
    };

    for _ in 0..limits::MAX_ENDPOINT_MESSAGES {
        if sender.send(&[]).is_err() {
            return false;
        }
    }
    let failed = sender
        .send_move_many(
            MESSAGE,
            [
                MoveHandle::new(notification, Rights::WAIT),
                MoveHandle::new(memory, Rights::READ),
            ],
        )
        .unwrap_err();
    if failed.error() != ipc::Error::TRY_AGAIN {
        return false;
    }
    let handles = failed.into_handles();
    if handles[0].handle().info().ok() != Some(notification_info)
        || handles[1].handle().info().ok() != Some(memory_info)
    {
        return false;
    }
    let mut empty = [];
    for _ in 0..limits::MAX_ENDPOINT_MESSAGES {
        if receiver.try_receive(&mut empty).is_err() {
            return false;
        }
    }

    let source_handles = [handles[0].handle().as_raw(), handles[1].handle().as_raw()];
    if sender.send_move_many(MESSAGE, handles).is_err()
        || source_handles
            .iter()
            .any(|handle| ipc::info(*handle).err() != Some(ipc::Error::BAD_FILE_DESCRIPTOR))
    {
        return false;
    }
    let mut bytes = [0_u8; MESSAGE.len()];
    if receiver.try_receive(&mut bytes).err() != Some(ipc::Error::RANGE) {
        return false;
    }
    let mut too_small = [None; 1];
    let insufficient =
        ipc::try_receive_many(receiver.as_raw(), &mut bytes, &mut too_small).unwrap_err();
    if insufficient.error() != ipc::Error::RANGE
        || insufficient.required_bytes() != MESSAGE.len()
        || insufficient.required_capabilities() != 2
        || too_small != [None]
        || receiver.signal_state().ok() != Some(Signals::READABLE | Signals::WRITABLE)
    {
        return false;
    }

    let Ok(message) = receiver.try_receive_many::<2>(&mut bytes) else {
        return false;
    };
    let [Some(notification), Some(memory)] = message.capabilities else {
        return false;
    };
    let notification_rights = notification.rights;
    let memory_rights = memory.rights;
    let Ok(notification) = notification.handle.try_cast::<Notification>() else {
        return false;
    };
    let Ok(memory) = memory.handle.try_cast::<SharedMemory>() else {
        return false;
    };
    let mut bulk = [0_u8; 4];
    let delivered = message.sender_process_id == current_process
        && message.bytes == MESSAGE.len()
        && message.capability_count == 2
        && bytes == MESSAGE
        && notification_rights == Rights::WAIT
        && memory_rights == Rights::READ
        && notification.info().is_ok_and(|info| {
            info.object_id == notification_info.object_id && info.rights == Rights::WAIT
        })
        && memory.info().is_ok_and(|info| {
            info.object_id == memory_info.object_id && info.rights == Rights::READ
        })
        && notification.try_wait().ok() == Some(0)
        && memory.read(0, &mut bulk).ok() == Some(4)
        && bulk == *b"bulk";

    drop(notification);
    drop(memory);
    drop(receiver);
    drop(sender);
    delivered && endpoint_multi_handle_duplicate_probe()
}

fn endpoint_multi_handle_duplicate_probe() -> bool {
    let Ok((sender, receiver)) = ipc::endpoint_create_pair() else {
        return false;
    };
    let Ok(notification) = ipc::notification_create() else {
        let _ = ipc::close(sender);
        let _ = ipc::close(receiver);
        return false;
    };
    let transfers = [
        Transfer {
            handle: notification,
            rights: Rights::WAIT,
        },
        Transfer {
            handle: notification,
            rights: Rights::WAIT,
        },
    ];
    let rejected = ipc::send_move_many(sender, b"duplicate", &transfers).err()
        == Some(ipc::Error::INVALID_ARGUMENT)
        && ipc::info(notification).is_ok()
        && ipc::signal_state(receiver).ok() == Some(Signals::WRITABLE);
    let closed = ipc::close(notification).is_ok()
        && ipc::close(receiver).is_ok()
        && ipc::close(sender).is_ok();
    rejected && closed
}

fn endpoint_move_waiter_probe(current_process: u64) -> bool {
    const ENDPOINT_HANDLE: ipc::CapabilityHandle = 1;
    const MESSAGE: &[u8] = b"move-wakes-waiter";

    let Ok(endpoint) = ipc::endpoint_create() else {
        return false;
    };
    let Ok(notification) = ipc::notification_create() else {
        let _ = ipc::close(endpoint);
        return false;
    };
    let Ok(barrier) = syscall::pipe_pair() else {
        let _ = ipc::close(notification);
        let _ = ipc::close(endpoint);
        return false;
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        let _ = ipc::close(notification);
        let _ = ipc::close(endpoint);
        return false;
    };
    if child == 0 {
        let _ = syscall::close(barrier.reader);
        if !ipc::wait_for_handle(ENDPOINT_HANDLE)
            .is_ok_and(|info| info.kind == ObjectKind::Endpoint && info.rights == Rights::RECEIVE)
            || syscall::write_all(barrier.writer, &[1]).is_err()
            || syscall::close(barrier.writer).is_err()
        {
            syscall::exit(70);
        }
        let mut bytes = [0_u8; MESSAGE.len()];
        let Ok(message) = blocking_ipc::receive(ENDPOINT_HANDLE, &mut bytes) else {
            syscall::exit(71);
        };
        let Some(received) = message.capability else {
            syscall::exit(72);
        };
        let valid = message.sender_process_id == current_process
            && message.bytes == MESSAGE.len()
            && bytes.as_slice() == MESSAGE
            && received.rights == Rights::WAIT
            && ipc::info(received.handle).is_ok_and(|info| {
                info.kind == ObjectKind::Notification && info.rights == Rights::WAIT
            });
        let closed = ipc::close(received.handle).is_ok() && ipc::close(ENDPOINT_HANDLE).is_ok();
        syscall::exit(if valid && closed { 0 } else { 73 });
    }

    let setup = syscall::close(barrier.writer).is_ok()
        && ipc::grant_child(child, endpoint, Rights::RECEIVE, ENDPOINT_HANDLE).ok()
            == Some(ENDPOINT_HANDLE);
    let mut ready = [0_u8; 1];
    let synchronized = setup
        && syscall::read(barrier.reader, &mut ready).ok() == Some(1)
        && ready[0] == 1
        && syscall::close(barrier.reader).is_ok();
    if synchronized {
        for _ in 0..4 {
            let _ = syscall::yield_now();
        }
    }
    let sent = synchronized
        && ipc::send_move(
            endpoint,
            MESSAGE,
            Transfer {
                handle: notification,
                rights: Rights::WAIT,
            },
        )
        .is_ok()
        && ipc::info(notification).err() == Some(ipc::Error::BAD_FILE_DESCRIPTOR);

    let mut child_succeeded = false;
    if sent {
        for _ in 0..JOB_WAIT_YIELDS {
            match syscall::try_wait_child(child) {
                Ok(status) => {
                    child_succeeded = status.success();
                    break;
                }
                Err(error) if error == syscall::Errno::TRY_AGAIN => {
                    let _ = syscall::yield_now();
                }
                Err(_) => break,
            }
        }
    }
    if !child_succeeded {
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
    }
    if !sent {
        let _ = ipc::close(notification);
    }
    let _ = syscall::close(barrier.reader);
    let _ = syscall::close(barrier.writer);
    ipc::close(endpoint).is_ok() && sent && child_succeeded
}

fn job_probe() -> bool {
    let Ok(job) = ipc::job_create() else {
        return false;
    };
    let Ok(wait_only) = ipc::duplicate(job, Rights::WAIT) else {
        let _ = ipc::close(job);
        return false;
    };
    let Ok(info) = ipc::info(job) else {
        let _ = ipc::close(wait_only);
        let _ = ipc::close(job);
        return false;
    };
    if info.kind != ObjectKind::Job
        || info.rights != Rights::JOB
        || info.size != 0
        || ipc::signal_state(wait_only).ok() != Some(Signals::TERMINATED)
        || ipc::wait_one(wait_only, Signals::TERMINATED, Deadline::IMMEDIATE).ok()
            != Some(Signals::TERMINATED)
        || ipc::job_get_process_limit(job).ok() != Some(limits::MAX_JOB_PROCESSES)
        || ipc::job_get_process_limit(wait_only).ok() != Some(limits::MAX_JOB_PROCESSES)
        || ipc::job_try_wait(wait_only).err() != Some(ipc::Error::NO_CHILD)
    {
        let _ = ipc::close(wait_only);
        let _ = ipc::close(job);
        return false;
    }

    let Ok(barrier) = syscall::pipe_pair() else {
        return close_job_handles(job, wait_only, false);
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return close_job_handles(job, wait_only, false);
    };
    if child == 0 {
        let _ = syscall::close(barrier.writer);
        let mut byte = [0_u8; 1];
        if syscall::read(barrier.reader, &mut byte).ok() != Some(0)
            || syscall::close(barrier.reader).is_err()
        {
            syscall::exit(120);
        }
        match syscall::fork() {
            Ok(0) => syscall::exit(42),
            Ok(descendant) => {
                if syscall::wait_child(descendant)
                    .ok()
                    .map(|status| status.raw())
                    != Some(42)
                {
                    syscall::exit(121);
                }
            }
            Err(_) => syscall::exit(122),
        }
        syscall::exit(23);
    }

    let reader_closed = syscall::close(barrier.reader).is_ok();
    let attenuated_denied = ipc::job_assign(wait_only, child).err() == Some(ipc::Error::PERMISSION);
    let assigned = ipc::job_assign(job, child).ok() == Some(child);
    let member_visible = ipc::info(job).is_ok_and(|info| info.size == 1);
    let barrier_released = syscall::close(barrier.writer).is_ok();
    let active_signal_state = ipc::signal_state(wait_only).ok() == Some(Signals::EMPTY)
        && ipc::wait_one(wait_only, Signals::TERMINATED, Deadline::IMMEDIATE).err()
            == Some(ipc::Error::TIMED_OUT);
    let setup_ok = reader_closed
        && attenuated_denied
        && assigned
        && member_visible
        && active_signal_state
        && barrier_released;
    if !setup_ok {
        let _ = ipc::job_terminate(job);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_job_handles(job, wait_only, false);
    }

    let Some(first) = bounded_job_wait(wait_only) else {
        let _ = ipc::job_terminate(job);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_job_handles(job, wait_only, false);
    };
    let Some(second) = bounded_job_wait(wait_only) else {
        let _ = ipc::job_terminate(job);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_job_handles(job, wait_only, false);
    };
    let descendant_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id != child && exit.status.raw() == 42);
    let child_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id == child && exit.status.raw() == 23);
    if !descendant_observed
        || !child_observed
        || syscall::wait_child(child).ok().map(|status| status.raw()) != Some(23)
        || ipc::job_try_wait(wait_only).err() != Some(ipc::Error::NO_CHILD)
        || !ipc::info(job).is_ok_and(|info| info.size == 0)
        || ipc::signal_state(wait_only).ok() != Some(Signals::TERMINATED)
        || ipc::wait_one(wait_only, Signals::TERMINATED, Deadline::IMMEDIATE).ok()
            != Some(Signals::TERMINATED)
    {
        return close_job_handles(job, wait_only, false);
    }

    let Ok(termination_barrier) = syscall::pipe_pair() else {
        return close_job_handles(job, wait_only, false);
    };
    let Ok(terminated_child) = syscall::fork() else {
        let _ = syscall::close(termination_barrier.reader);
        let _ = syscall::close(termination_barrier.writer);
        return close_job_handles(job, wait_only, false);
    };
    if terminated_child == 0 {
        let _ = syscall::close(termination_barrier.writer);
        let mut byte = [0_u8; 1];
        let _ = syscall::read(termination_barrier.reader, &mut byte);
        syscall::exit(123);
    }
    let termination_reader_closed = syscall::close(termination_barrier.reader).is_ok();
    let termination_assigned =
        ipc::job_assign(job, terminated_child).ok() == Some(terminated_child);
    let attenuated_termination_denied =
        ipc::job_terminate(wait_only).err() == Some(ipc::Error::PERMISSION);
    let termination_count = ipc::job_terminate(job).ok();
    if termination_count != Some(1) {
        let _ = platform::kill(terminated_child, signal::KILL);
    }
    let terminated_exit = termination_assigned
        .then(|| bounded_job_wait(wait_only))
        .flatten();
    let waited_status = syscall::wait_child(terminated_child).ok();
    let _ = syscall::close(termination_barrier.writer);
    let terminated = termination_reader_closed
        && termination_assigned
        && attenuated_termination_denied
        && termination_count == Some(1)
        && terminated_exit.is_some_and(|exit| {
            exit.process_id == terminated_child && exit.status.signal() == Some(signal::KILL)
        })
        && waited_status.is_some_and(|status| status.signal() == Some(signal::KILL));

    let hierarchy_verified = terminated && hierarchical_job_probe(job, wait_only);
    close_job_handles(job, wait_only, hierarchy_verified)
}

fn hierarchical_job_probe(
    parent: ipc::CapabilityHandle,
    parent_wait: ipc::CapabilityHandle,
) -> bool {
    let Ok(manage_only) = ipc::duplicate(parent, Rights::MANAGE) else {
        return false;
    };
    let query_rights_verified = ipc::job_get_process_limit(manage_only).err()
        == Some(ipc::Error::PERMISSION)
        && ipc::close(manage_only).is_ok();
    if ipc::job_retire(parent).err() != Some(ipc::Error::INVALID_ARGUMENT)
        || ipc::job_retire(parent_wait).err() != Some(ipc::Error::PERMISSION)
        || ipc::job_create_child(parent_wait).err() != Some(ipc::Error::PERMISSION)
        || !query_rights_verified
    {
        return false;
    }
    let Ok(child_job) = ipc::job_create_child(parent) else {
        return false;
    };
    let Ok(grandchild_job) = ipc::job_create_child(child_job) else {
        let _ = ipc::close(child_job);
        return false;
    };
    if ipc::job_retire(child_job).err() != Some(ipc::Error::TRY_AGAIN) {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }
    let hierarchy_shape = ipc::info(child_job).is_ok_and(|child| {
        ipc::info(grandchild_job).is_ok_and(|grandchild| {
            child.kind == ObjectKind::Job
                && child.rights == Rights::JOB
                && child.size == 0
                && grandchild.kind == ObjectKind::Job
                && grandchild.rights == Rights::JOB
                && grandchild.size == 0
                && child.object_id != grandchild.object_id
        })
    });
    if !hierarchy_shape || ipc::job_try_wait(parent_wait).err() != Some(ipc::Error::NO_CHILD) {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }

    let Ok(barrier) = syscall::pipe_pair() else {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    if child == 0 {
        let _ = syscall::close(barrier.writer);
        let mut byte = [0_u8; 1];
        if syscall::read(barrier.reader, &mut byte).ok() != Some(0)
            || syscall::close(barrier.reader).is_err()
        {
            syscall::exit(124);
        }
        match syscall::fork() {
            Ok(0) => syscall::exit(44),
            Ok(descendant) => {
                if syscall::wait_child(descendant)
                    .ok()
                    .map(|status| status.raw())
                    != Some(44)
                {
                    syscall::exit(125);
                }
            }
            Err(_) => syscall::exit(126),
        }
        syscall::exit(24);
    }

    let reader_closed = syscall::close(barrier.reader).is_ok();
    let assigned = ipc::job_assign(grandchild_job, child).ok() == Some(child);
    let subtree_visible = [parent, child_job, grandchild_job]
        .iter()
        .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 1));
    let barrier_released = syscall::close(barrier.writer).is_ok();
    if !reader_closed || !assigned || !subtree_visible || !barrier_released {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }

    let Some(first) = bounded_job_wait(parent_wait) else {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let Some(second) = bounded_job_wait(parent_wait) else {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let descendant_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id != child && exit.status.raw() == 44);
    let child_observed = [first, second]
        .iter()
        .any(|exit| exit.process_id == child && exit.status.raw() == 24);
    if !descendant_observed
        || !child_observed
        || syscall::wait_child(child).ok().map(|status| status.raw()) != Some(24)
        || ipc::job_try_wait(grandchild_job).err() != Some(ipc::Error::NO_CHILD)
        || ![parent, child_job, grandchild_job]
            .iter()
            .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 0))
    {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    }

    let Ok(termination_barrier) = syscall::pipe_pair() else {
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    let Ok(terminated_child) = syscall::fork() else {
        let _ = syscall::close(termination_barrier.reader);
        let _ = syscall::close(termination_barrier.writer);
        return close_hierarchy_handles(child_job, grandchild_job, false);
    };
    if terminated_child == 0 {
        let _ = syscall::close(termination_barrier.writer);
        let mut byte = [0_u8; 1];
        let _ = syscall::read(termination_barrier.reader, &mut byte);
        syscall::exit(127);
    }
    let termination_reader_closed = syscall::close(termination_barrier.reader).is_ok();
    let termination_assigned =
        ipc::job_assign(child_job, terminated_child).ok() == Some(terminated_child);
    let termination_count = ipc::job_terminate(parent).ok();
    if termination_count != Some(1) {
        let _ = platform::kill(terminated_child, signal::KILL);
    }
    let terminated_exit = termination_assigned
        .then(|| bounded_job_wait(parent_wait))
        .flatten();
    let waited_status = syscall::wait_child(terminated_child).ok();
    let _ = syscall::close(termination_barrier.writer);
    let terminated = termination_reader_closed
        && termination_assigned
        && termination_count == Some(1)
        && terminated_exit.is_some_and(|exit| {
            exit.process_id == terminated_child && exit.status.signal() == Some(signal::KILL)
        })
        && waited_status.is_some_and(|status| status.signal() == Some(signal::KILL))
        && ipc::job_try_wait(parent_wait).err() == Some(ipc::Error::NO_CHILD);

    let process_limit_verified = terminated && job_process_limit_probe(parent, parent_wait);
    let retired = retire_and_close_hierarchy(child_job, grandchild_job, process_limit_verified);
    retired && job_reclamation_probe(parent)
}

fn job_process_limit_probe(
    parent: ipc::CapabilityHandle,
    parent_wait: ipc::CapabilityHandle,
) -> bool {
    if ipc::job_set_process_limit(parent_wait, 1).err() != Some(ipc::Error::PERMISSION) {
        return false;
    }
    let Ok(limited_job) = ipc::job_create_child(parent) else {
        return false;
    };
    if ipc::job_set_process_limit(limited_job, 1).ok() != Some(1) {
        let _ = ipc::close(limited_job);
        return false;
    }
    let Ok(leaf_job) = ipc::job_create_child(limited_job) else {
        let _ = ipc::close(limited_job);
        return false;
    };
    if ipc::job_get_process_limit(limited_job).ok() != Some(1)
        || ipc::job_get_process_limit(leaf_job).ok() != Some(1)
        || ipc::job_retire(limited_job).err() != Some(ipc::Error::TRY_AGAIN)
        || ipc::job_set_process_limit(leaf_job, 2).err() != Some(ipc::Error::PERMISSION)
    {
        return close_hierarchy_handles(limited_job, leaf_job, false);
    }

    let Ok(barrier) = syscall::pipe_pair() else {
        return close_hierarchy_handles(limited_job, leaf_job, false);
    };
    let Ok(child) = syscall::fork() else {
        let _ = syscall::close(barrier.reader);
        let _ = syscall::close(barrier.writer);
        return close_hierarchy_handles(limited_job, leaf_job, false);
    };
    if child == 0 {
        let _ = syscall::close(barrier.writer);
        let mut byte = [0_u8; 1];
        if syscall::read(barrier.reader, &mut byte).ok() != Some(0)
            || syscall::close(barrier.reader).is_err()
        {
            syscall::exit(128);
        }
        match syscall::fork() {
            Err(error) if error == syscall::Errno::NO_SPACE => syscall::exit(46),
            Ok(0) => syscall::exit(129),
            Ok(descendant) => {
                let _ = platform::kill(descendant, signal::KILL);
                let _ = syscall::wait_child(descendant);
                syscall::exit(130);
            }
            Err(_) => syscall::exit(131),
        }
    }

    let reader_closed = syscall::close(barrier.reader).is_ok();
    let assigned = ipc::job_assign(leaf_job, child).ok() == Some(child);
    let tightened_below_usage = ipc::job_set_process_limit(limited_job, 0).ok() == Some(0);
    let relaxation_denied =
        ipc::job_set_process_limit(limited_job, 1).err() == Some(ipc::Error::PERMISSION);
    let subtree_visible = [parent, limited_job, leaf_job]
        .iter()
        .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 1));
    let barrier_released = syscall::close(barrier.writer).is_ok();
    if !reader_closed
        || !assigned
        || !tightened_below_usage
        || !relaxation_denied
        || !subtree_visible
        || !barrier_released
    {
        let _ = ipc::job_terminate(parent);
        let _ = platform::kill(child, signal::KILL);
        let _ = syscall::wait_child(child);
        return close_hierarchy_handles(limited_job, leaf_job, false);
    }

    let exit = bounded_job_wait(parent_wait);
    let waited_status = syscall::wait_child(child).ok();
    let denied = exit.is_some_and(|exit| exit.process_id == child && exit.status.raw() == 46)
        && waited_status.is_some_and(|status| status.raw() == 46)
        && ipc::job_try_wait(parent_wait).err() == Some(ipc::Error::NO_CHILD)
        && [parent, limited_job, leaf_job]
            .iter()
            .all(|job| ipc::info(*job).is_ok_and(|info| info.size == 0));

    let limits_visible = ipc::job_get_process_limit(limited_job).ok() == Some(0)
        && ipc::job_get_process_limit(leaf_job).ok() == Some(1);

    retire_and_close_hierarchy(limited_job, leaf_job, denied && limits_visible)
}

fn retire_and_close_hierarchy(
    parent: ipc::CapabilityHandle,
    child: ipc::CapabilityHandle,
    result: bool,
) -> bool {
    if !result || ipc::job_retire(child).is_err() {
        return close_hierarchy_handles(parent, child, false);
    }
    let child_is_inert = ipc::job_retire(child).err() == Some(ipc::Error::PERMISSION)
        && ipc::job_create_child(child).err() == Some(ipc::Error::PERMISSION)
        && ipc::job_set_process_limit(child, 0).err() == Some(ipc::Error::PERMISSION)
        && ipc::job_get_process_limit(child).is_ok()
        && ipc::job_try_wait(child).err() == Some(ipc::Error::NO_CHILD);
    let parent_retired = child_is_inert && ipc::job_retire(parent).is_ok();
    close_hierarchy_handles(parent, child, parent_retired)
}

fn job_reclamation_probe(parent: ipc::CapabilityHandle) -> bool {
    for _ in 0..=limits::MAX_JOB_OBJECTS {
        let Ok(child) = ipc::job_create_child(parent) else {
            return false;
        };
        if ipc::job_retire(child).is_err() || ipc::close(child).is_err() {
            return false;
        }
    }
    ipc::info(parent).is_ok_and(|info| info.size == 0)
}

fn close_hierarchy_handles(
    child: ipc::CapabilityHandle,
    grandchild: ipc::CapabilityHandle,
    result: bool,
) -> bool {
    let grandchild_closed = ipc::close(grandchild).is_ok();
    let child_closed = ipc::close(child).is_ok();
    grandchild_closed && child_closed && result
}

fn bounded_job_wait(handle: ipc::CapabilityHandle) -> Option<ipc::JobExit> {
    for _ in 0..JOB_WAIT_YIELDS {
        if ipc::wait_one(handle, Signals::READABLE, Deadline::INFINITE).is_err() {
            return None;
        }
        match ipc::job_try_wait(handle) {
            Ok(exit) => return Some(exit),
            Err(error) if error == ipc::Error::TRY_AGAIN => {
                if syscall::yield_now().is_err() {
                    return None;
                }
            }
            Err(_) => return None,
        }
    }
    None
}

fn close_job_handles(
    job: ipc::CapabilityHandle,
    wait_only: ipc::CapabilityHandle,
    result: bool,
) -> bool {
    let wait_closed = ipc::close(wait_only).is_ok();
    let job_closed = ipc::close(job).is_ok();
    wait_closed && job_closed && result
}

fn relative_open_probe() -> bool {
    const CONTENTS: &[u8] = b"cwd-aware open\n";
    let flags = OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE;
    let Ok(descriptor) = syscall::open(b"cwd-open.txt", flags) else {
        return false;
    };
    let written = syscall::write_all(descriptor, CONTENTS).is_ok();
    let closed = syscall::close(descriptor).is_ok();
    written
        && closed
        && platform::stat(b"/tmp/cwd-open.txt")
            .is_ok_and(|stat| stat.is_file() && stat.size == CONTENTS.len() as u64)
}

fn relative_spawn_probe() -> bool {
    let Ok(process_id) = syscall::spawn_command(
        b"../pwd",
        SpawnFlags::NEW_PROCESS_GROUP,
        None,
        None,
        None,
        None,
    ) else {
        return false;
    };
    syscall::wait_child(process_id).is_ok_and(|status| status.success())
}

fn root_directory_has_expected_entries() -> bool {
    let mut offset = 0usize;
    let mut found_hello = false;
    let mut found_tmp = false;
    loop {
        let mut entries = [DirectoryEntry::EMPTY; DIRECTORY_PAGE];
        let Ok(count) = platform::read_directory(b"/", offset, &mut entries) else {
            return false;
        };
        for entry in &entries[..count] {
            found_hello |= entry.is_file() && ascii_eq_ignore_case(entry.name(), b"hello.txt");
            found_tmp |=
                entry.kind == file::KIND_DIRECTORY && ascii_eq_ignore_case(entry.name(), b"tmp");
        }
        offset = offset.saturating_add(count);
        if count < entries.len() {
            break;
        }
        if offset > 128 {
            return false;
        }
    }
    found_hello && found_tmp
}

fn descriptor_probe() -> bool {
    let Ok(stdout_stat) = platform::fstat(STDOUT) else {
        return false;
    };
    if !matches!(stdout_stat.kind, file::KIND_TERMINAL | file::KIND_FILE) {
        return false;
    }

    match platform::dup(STDOUT) {
        Ok(duplicate) if stdout_stat.kind == file::KIND_FILE => {
            let duplicate_matches = platform::fstat(duplicate)
                .is_ok_and(|stat| stat.kind == stdout_stat.kind && stat.flags == stdout_stat.flags);
            if syscall::close(duplicate).is_err() || !duplicate_matches {
                return false;
            }
        }
        Err(error)
            if stdout_stat.kind == file::KIND_TERMINAL
                && error == platform::Errno::NOT_IMPLEMENTED => {}
        _ => return false,
    }

    if platform::dup2(STDOUT, STDOUT).ok() != Some(STDOUT)
        || platform::dup2(STDOUT, STDERR).ok() != Some(STDERR)
        || platform::dup2(STDOUT, STDIN).err() != Some(platform::Errno::BAD_FILE_DESCRIPTOR)
    {
        return false;
    }
    platform::fstat(STDERR)
        .is_ok_and(|stat| stat.kind == stdout_stat.kind && stat.flags == stdout_stat.flags)
}

fn ordinary_descriptor_probe() -> bool {
    let Ok(descriptor) = syscall::open(b"/hello.txt", OpenFlags::READ) else {
        return false;
    };
    let Ok(stat) = platform::fstat(descriptor) else {
        let _ = syscall::close(descriptor);
        return false;
    };
    if !stat.is_file() {
        let _ = syscall::close(descriptor);
        return false;
    }

    let Ok(duplicate) = platform::dup(descriptor) else {
        let _ = syscall::close(descriptor);
        return false;
    };
    let Ok(reference) = syscall::open(b"/hello.txt", OpenFlags::READ) else {
        let _ = syscall::close(duplicate);
        let _ = syscall::close(descriptor);
        return false;
    };

    let mut first = [0_u8; 1];
    let mut second = [0_u8; 1];
    let mut expected = [0_u8; 2];
    let shared_offset = syscall::read(descriptor, &mut first).ok() == Some(1)
        && syscall::read(duplicate, &mut second).ok() == Some(1)
        && syscall::read(reference, &mut expected).ok() == Some(2)
        && first[0] == expected[0]
        && second[0] == expected[1];

    const DUP2_TARGET: u64 = 15;
    let dup2_ok = platform::dup2(descriptor, DUP2_TARGET).ok() == Some(DUP2_TARGET)
        && platform::fstat(DUP2_TARGET).is_ok();

    let _ = syscall::close(DUP2_TARGET);
    let _ = syscall::close(reference);
    let _ = syscall::close(duplicate);
    let _ = syscall::close(descriptor);
    shared_offset && dup2_ok
}

fn ascii_eq_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}
