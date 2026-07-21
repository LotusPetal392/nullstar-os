// Parent-process discovery and direct-child signal delivery.

fn platform_getppid(process_id: u64) -> u64 {
    PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .and_then(|process| process.parent_process_id)
        .unwrap_or(KERNEL_REAPER_PROCESS_ID)
}

fn platform_kill(process_id: u64, target_process_id: u64, signal: u64) -> u64 {
    if !signal_is_supported(signal) {
        return error_return(ERR_INVALID_ARGUMENT);
    }
    let (authorized, process_group_id) = {
        let manager = PROCESS_MANAGER.lock();
        let Some(target) = manager
            .processes
            .iter()
            .find(|process| process.process_id == target_process_id && process.is_live())
        else {
            return error_return(ERR_NO_PROCESS);
        };
        (
            target.parent_process_id == Some(process_id),
            target.process_group_id,
        )
    };
    if !authorized {
        return error_return(abi::errno::PERMISSION);
    }

    let delivery = deliver_signal_to_process(target_process_id, signal);
    if !delivery.accepted {
        return error_return(ERR_NO_PROCESS);
    }
    {
        let mut manager = PROCESS_MANAGER.lock();
        if let Some(sender) = manager.process_mut(process_id) {
            sender.signal_sent_count = sender.signal_sent_count.saturating_add(1);
        }
        manager.signals_sent = manager.signals_sent.saturating_add(1);
    }
    if delivery.stopped {
        restore_group_terminal(process_group_id);
    }
    0
}
