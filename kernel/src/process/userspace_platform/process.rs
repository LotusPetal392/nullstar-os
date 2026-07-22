// Parent-process discovery, process-group control, and direct-child signal delivery.

fn platform_getppid(process_id: u64) -> u64 {
    PROCESS_MANAGER
        .lock()
        .processes
        .iter()
        .find(|process| process.process_id == process_id)
        .and_then(|process| process.parent_process_id)
        .unwrap_or(KERNEL_REAPER_PROCESS_ID)
}

fn platform_get_process_group(process_id: u64, target_process_id: u64) -> u64 {
    let target_process_id = if target_process_id == 0 {
        process_id
    } else {
        target_process_id
    };
    let manager = PROCESS_MANAGER.lock();
    let Some(target) = manager
        .processes
        .iter()
        .find(|process| process.process_id == target_process_id && process.is_live())
    else {
        return error_return(ERR_NO_PROCESS);
    };
    if target_process_id != process_id && target.parent_process_id != Some(process_id) {
        return error_return(abi::errno::PERMISSION);
    }
    target.process_group_id
}

fn platform_set_process_group(
    process_id: u64,
    target_process_id: u64,
    process_group_id: u64,
) -> u64 {
    let target_process_id = if target_process_id == 0 {
        process_id
    } else {
        target_process_id
    };
    let process_group_id = if process_group_id == 0 {
        target_process_id
    } else {
        process_group_id
    };
    let target_is_foreground = terminal::is_foreground(target_process_id);

    let mut manager = PROCESS_MANAGER.lock();
    let Some(caller_index) = manager
        .processes
        .iter()
        .position(|process| process.process_id == process_id && process.is_live())
    else {
        return error_return(ERR_NO_PROCESS);
    };
    let Some(target_index) = manager
        .processes
        .iter()
        .position(|process| process.process_id == target_process_id && process.is_live())
    else {
        return error_return(ERR_NO_PROCESS);
    };

    let caller_group_id = manager.processes[caller_index].process_group_id;
    let caller_path = manager.processes[caller_index].path.clone();
    let target_parent = manager.processes[target_index].parent_process_id;
    if target_process_id != process_id && target_parent != Some(process_id) {
        return error_return(abi::errno::PERMISSION);
    }

    let current_group_id = manager.processes[target_index].process_group_id;
    if current_group_id == process_group_id {
        return process_group_id;
    }
    if target_is_foreground || manager.processes[target_index].terminal_parent.is_some() {
        return error_return(ERR_IO);
    }

    if process_group_id != target_process_id {
        let joinable = manager.processes.iter().any(|process| {
            process.process_id != target_process_id
                && process.parent_process_id == target_parent
                && process.process_group_id == process_group_id
                && process.is_live()
        });
        if !joinable {
            return error_return(ERR_NO_PROCESS);
        }
    }

    let generic_launch = target_process_id != process_id
        && target_parent == Some(process_id)
        && current_group_id == caller_group_id
        && manager.processes[target_index].path == caller_path
        && manager.processes[target_index].exec_count == 0;

    manager.processes[target_index].process_group_id = process_group_id;
    if generic_launch {
        let parent = &mut manager.processes[caller_index];
        parent.child_spawn_count = parent.child_spawn_count.saturating_add(1);
        manager.child_spawns = manager.child_spawns.saturating_add(1);
    }
    drop(manager);

    crate::serial_println!(
        "userspace process group changed: caller={}, process={}, group={}, generic_launch={}",
        process_id,
        target_process_id,
        process_group_id,
        generic_launch
    );
    process_group_id
}

fn platform_foreground_process_group(process_id: u64, process_group_id: u64) -> u64 {
    if process_group_id == 0 {
        return error_return(ERR_INVALID_ARGUMENT);
    }

    let (members, leader_parent, caller_owns_group, caller_is_leader) = {
        let manager = PROCESS_MANAGER.lock();
        let direct_members = manager
            .processes
            .iter()
            .filter(|process| {
                process.parent_process_id == Some(process_id)
                    && process.process_group_id == process_group_id
                    && process.is_live()
            })
            .map(|process| process.process_id)
            .collect::<Vec<_>>();
        if !direct_members.is_empty() {
            (direct_members, None, true, false)
        } else {
            let leader = manager.processes.iter().find(|process| {
                process.process_id == process_id
                    && process.process_group_id == process_group_id
                    && process.process_id == process_group_id
                    && process.is_live()
            });
            let Some(leader) = leader else {
                return error_return(ERR_NO_CHILD);
            };
            let parent = leader.parent_process_id;
            let members = manager
                .processes
                .iter()
                .filter(|process| {
                    process.parent_process_id == parent
                        && process.process_group_id == process_group_id
                        && process.is_live()
                })
                .map(|process| process.process_id)
                .collect::<Vec<_>>();
            (members, parent, false, true)
        }
    };

    let foreground = terminal::foreground_process();
    if foreground.is_some_and(|foreground| members.contains(&foreground)) {
        return members.len() as u64;
    }

    if caller_owns_group {
        return match foreground_process_group(process_id, process_group_id) {
            Ok(count) => count as u64,
            Err(error) => error_return(error),
        };
    }

    if caller_is_leader {
        let Some(parent_process_id) = leader_parent else {
            return error_return(ERR_IO);
        };
        if !terminal::transfer(parent_process_id, process_id) {
            return error_return(ERR_IO);
        }
        let updated = {
            let mut manager = PROCESS_MANAGER.lock();
            if let Some(process) = manager.process_mut(process_id) {
                process.terminal_parent = Some(parent_process_id);
                true
            } else {
                false
            }
        };
        if !updated {
            let _ = terminal::transfer(process_id, parent_process_id);
            return error_return(ERR_NO_PROCESS);
        }
        return members.len() as u64;
    }

    error_return(ERR_IO)
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
