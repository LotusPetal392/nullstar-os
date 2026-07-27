#[cfg(not(target_os = "linux"))]
compile_error!("nullfs-fuse is supported only on Linux");

use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    process::ExitCode,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fuser::{
    BsdFileFlags, Config, Errno, FileAttr, FileHandle, FileType, Filesystem as FuseFilesystem,
    FopenFlags, Generation, INodeNo, LockOwner, MountOption, OpenAccMode, OpenFlags, RenameFlags,
    ReplyAttr, ReplyCreate, ReplyData, ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen,
    ReplyStatfs, ReplyWrite, Request, TimeOrNow, WriteFlags,
};
use nullfs_blockdev::{BlockDeviceError, FileBlockDevice};
use nullfs_core::{Error, Filesystem, NodeAttributes, NodeId, OpenHandle};
use nullfs_format::{BLOCK_SIZE, NodeKind, Timestamp};

const TTL: Duration = Duration::from_secs(1);
const USAGE: &str = "usage: nullfs-fuse [--read-write] IMAGE MOUNTPOINT";

#[derive(Debug, Clone, Copy)]
struct AdapterHandle {
    core: OpenHandle,
    access: OpenAccMode,
}

#[derive(Default)]
struct HandleTable {
    next_id: u64,
    entries: HashMap<u64, AdapterHandle>,
}

impl HandleTable {
    fn insert(&mut self, handle: AdapterHandle) -> Result<FileHandle, Errno> {
        self.next_id = self.next_id.checked_add(1).ok_or(Errno::EOVERFLOW)?;
        self.entries.insert(self.next_id, handle);
        Ok(FileHandle(self.next_id))
    }

    fn get(&self, handle: FileHandle) -> Result<AdapterHandle, Errno> {
        self.entries.get(&handle.0).copied().ok_or(Errno::EBADF)
    }

    fn remove(&mut self, handle: FileHandle) -> Result<AdapterHandle, Errno> {
        self.entries.remove(&handle.0).ok_or(Errno::EBADF)
    }
}

struct SharedState {
    filesystem: Option<Filesystem<FileBlockDevice>>,
    handles: HandleTable,
    unmount_error: Option<String>,
}

struct NullFsFuse {
    state: Arc<Mutex<SharedState>>,
    writable: bool,
}

impl NullFsFuse {
    fn new(
        filesystem: Filesystem<FileBlockDevice>,
        writable: bool,
    ) -> (Self, Arc<Mutex<SharedState>>) {
        let state = Arc::new(Mutex::new(SharedState {
            filesystem: Some(filesystem),
            handles: HandleTable::default(),
            unmount_error: None,
        }));
        (
            Self {
                state: Arc::clone(&state),
                writable,
            },
            state,
        )
    }

    fn state(&self) -> Result<MutexGuard<'_, SharedState>, Errno> {
        self.state.lock().map_err(|_| Errno::EIO)
    }

    fn attributes(&self, ino: INodeNo) -> Result<NodeAttributes, Errno> {
        self.state()?
            .filesystem
            .as_mut()
            .ok_or(Errno::EIO)?
            .attributes(NodeId(ino.0))
            .map_err(errno)
    }

    fn require_writable(&self) -> Result<(), Errno> {
        self.writable.then_some(()).ok_or(Errno::EROFS)
    }

    fn sync_handle(&self, fh: FileHandle) -> Result<(), Errno> {
        let mut state = self.state()?;
        let handle = state.handles.get(fh)?;
        let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
        filesystem.validate_handle(handle.core).map_err(errno)?;
        if self.writable {
            filesystem.sync().map_err(errno)?;
        }
        Ok(())
    }

    fn close_handle(&self, fh: FileHandle) -> Result<(), Errno> {
        let mut state = self.state()?;
        let handle = state.handles.get(fh)?;
        state
            .filesystem
            .as_mut()
            .ok_or(Errno::EIO)?
            .close_node(handle.core)
            .map_err(errno)?;
        state.handles.remove(fh)?;
        Ok(())
    }
}

impl FuseFilesystem for NullFsFuse {
    fn destroy(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let result = state
            .filesystem
            .as_mut()
            .ok_or_else(|| String::from("filesystem unavailable during destroy"))
            .and_then(|filesystem| filesystem.try_unmount().map_err(|error| error.to_string()));
        if let Err(error) = result {
            state.unmount_error = Some(error);
        }
    }

    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let result = self.state().and_then(|mut state| {
            let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
            let node = filesystem
                .lookup(NodeId(parent.0), name.as_bytes())
                .map_err(errno)?;
            filesystem.attributes(node).map_err(errno)
        });
        match result.and_then(file_attr) {
            Ok((attributes, generation)) => reply.entry(&TTL, &attributes, Generation(generation)),
            Err(error) => reply.error(error),
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match self.attributes(ino).and_then(file_attr) {
            Ok((attributes, _)) => reply.attr(&TTL, &attributes),
            Err(error) => reply.error(error),
        }
    }

    fn setattr(
        &self,
        _req: &Request,
        ino: INodeNo,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        ctime: Option<SystemTime>,
        _fh: Option<FileHandle>,
        crtime: Option<SystemTime>,
        chgtime: Option<SystemTime>,
        bkuptime: Option<SystemTime>,
        flags: Option<BsdFileFlags>,
        reply: ReplyAttr,
    ) {
        if let Err(error) = self.require_writable() {
            reply.error(error);
            return;
        }
        if mode.is_some()
            || uid.is_some()
            || gid.is_some()
            || atime.is_some()
            || mtime.is_some()
            || ctime.is_some()
            || crtime.is_some()
            || chgtime.is_some()
            || bkuptime.is_some()
            || flags.is_some()
        {
            reply.error(Errno::ENOTSUP);
            return;
        }
        let result = self.state().and_then(|mut state| {
            let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
            if let Some(size) = size {
                filesystem.truncate(NodeId(ino.0), size).map_err(errno)?;
            }
            filesystem.attributes(NodeId(ino.0)).map_err(errno)
        });
        match result.and_then(file_attr) {
            Ok((attributes, _)) => reply.attr(&TTL, &attributes),
            Err(error) => reply.error(error),
        }
    }

    fn mkdir(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        reply: ReplyEntry,
    ) {
        let result = self.require_writable().and_then(|()| {
            self.state().and_then(|mut state| {
                let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
                let node = filesystem
                    .mkdir(
                        NodeId(parent.0),
                        name.as_bytes(),
                        permission_mode(mode, umask),
                    )
                    .map_err(errno)?;
                filesystem.attributes(node).map_err(errno)
            })
        });
        match result.and_then(file_attr) {
            Ok((attributes, generation)) => reply.entry(&TTL, &attributes, Generation(generation)),
            Err(error) => reply.error(error),
        }
    }

    fn unlink(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = self.require_writable().and_then(|()| {
            self.state()?
                .filesystem
                .as_mut()
                .ok_or(Errno::EIO)?
                .unlink(NodeId(parent.0), name.as_bytes())
                .map_err(errno)
        });
        finish_empty(result, reply);
    }

    fn rmdir(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEmpty) {
        let result = self.require_writable().and_then(|()| {
            self.state()?
                .filesystem
                .as_mut()
                .ok_or(Errno::EIO)?
                .rmdir(NodeId(parent.0), name.as_bytes())
                .map_err(errno)
        });
        finish_empty(result, reply);
    }

    fn rename(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        newparent: INodeNo,
        newname: &OsStr,
        flags: RenameFlags,
        reply: ReplyEmpty,
    ) {
        if !flags.is_empty() {
            reply.error(Errno::EINVAL);
            return;
        }
        let result = self.require_writable().and_then(|()| {
            self.state()?
                .filesystem
                .as_mut()
                .ok_or(Errno::EIO)?
                .rename(
                    NodeId(parent.0),
                    name.as_bytes(),
                    NodeId(newparent.0),
                    newname.as_bytes(),
                )
                .map_err(errno)
        });
        finish_empty(result, reply);
    }

    fn open(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if !self.writable && flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
        let result = self.state().and_then(|mut state| {
            let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
            let handle = filesystem.open_node(NodeId(ino.0)).map_err(errno)?;
            if handle.kind != NodeKind::Regular {
                filesystem.close_node(handle).map_err(errno)?;
                return Err(if handle.kind == NodeKind::Directory {
                    Errno::EISDIR
                } else {
                    Errno::ENOTSUP
                });
            }
            state.handles.insert(AdapterHandle {
                core: handle,
                access: flags.acc_mode(),
            })
        });
        match result {
            Ok(handle) => reply.opened(handle, FopenFlags::empty()),
            Err(error) => reply.error(error),
        }
    }

    fn create(
        &self,
        _req: &Request,
        parent: INodeNo,
        name: &OsStr,
        mode: u32,
        umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = self.require_writable().and_then(|()| {
            self.state().and_then(|mut state| {
                let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
                let node = filesystem
                    .create(
                        NodeId(parent.0),
                        name.as_bytes(),
                        permission_mode(mode, umask),
                    )
                    .map_err(errno)?;
                let attributes = filesystem.attributes(node).map_err(errno)?;
                let core = filesystem.open_node(node).map_err(errno)?;
                let handle = state.handles.insert(AdapterHandle {
                    core,
                    access: OpenFlags(flags).acc_mode(),
                })?;
                Ok((attributes, handle))
            })
        });
        match result.and_then(|(attributes, handle)| {
            file_attr(attributes).map(|attributes| (attributes, handle))
        }) {
            Ok(((attributes, generation), handle)) => reply.created(
                &TTL,
                &attributes,
                Generation(generation),
                handle,
                FopenFlags::empty(),
            ),
            Err(error) => reply.error(error),
        }
    }

    fn read(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let mut data = vec![0; size as usize];
        let result = self.state().and_then(|mut state| {
            let handle = state.handles.get(fh)?;
            if handle.access == OpenAccMode::O_WRONLY {
                return Err(Errno::EBADF);
            }
            state
                .filesystem
                .as_mut()
                .ok_or(Errno::EIO)?
                .read_handle(handle.core, offset, &mut data)
                .map_err(errno)
        });
        match result {
            Ok(count) => reply.data(&data[..count]),
            Err(error) => reply.error(error),
        }
    }

    fn write(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        data: &[u8],
        _write_flags: WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyWrite,
    ) {
        if let Err(error) = self.require_writable() {
            reply.error(error);
            return;
        }
        let result = self.state().and_then(|mut state| {
            let handle = state.handles.get(fh)?;
            if handle.access == OpenAccMode::O_RDONLY {
                return Err(Errno::EBADF);
            }
            state
                .filesystem
                .as_mut()
                .ok_or(Errno::EIO)?
                .write_handle(handle.core, offset, data)
                .map_err(errno)
        });
        match result {
            Ok(count) => match u32::try_from(count) {
                Ok(count) => reply.written(count),
                Err(_) => reply.error(Errno::EOVERFLOW),
            },
            Err(error) => reply.error(error),
        }
    }

    fn flush(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _lock_owner: LockOwner,
        reply: ReplyEmpty,
    ) {
        finish_empty(self.sync_handle(fh), reply);
    }

    fn fsync(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        finish_empty(self.sync_handle(fh), reply);
    }

    fn release(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        finish_empty(self.close_handle(fh), reply);
    }

    fn opendir(&self, _req: &Request, ino: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        if !self.writable && flags.acc_mode() != OpenAccMode::O_RDONLY {
            reply.error(Errno::EROFS);
            return;
        }
        let result = self.state().and_then(|mut state| {
            let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
            let handle = filesystem.open_node(NodeId(ino.0)).map_err(errno)?;
            if handle.kind != NodeKind::Directory {
                filesystem.close_node(handle).map_err(errno)?;
                return Err(Errno::ENOTDIR);
            }
            state.handles.insert(AdapterHandle {
                core: handle,
                access: flags.acc_mode(),
            })
        });
        match result {
            Ok(handle) => reply.opened(handle, FopenFlags::empty()),
            Err(error) => reply.error(error),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let records = self.state().and_then(|mut state| {
            let handle = state.handles.get(fh)?;
            if handle.core.kind != NodeKind::Directory {
                return Err(Errno::ENOTDIR);
            }
            let filesystem = state.filesystem.as_mut().ok_or(Errno::EIO)?;
            let node = filesystem.validate_handle(handle.core).map_err(errno)?;
            filesystem
                .read_directory(node, offset, usize::MAX)
                .map_err(errno)
        });
        match records {
            Ok(records) => {
                for record in records {
                    let kind = match fuse_kind(record.kind) {
                        Ok(kind) => kind,
                        Err(error) => {
                            reply.error(error);
                            return;
                        }
                    };
                    if reply.add(
                        INodeNo(record.node.0),
                        record.next_cookie,
                        kind,
                        record.name,
                    ) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(error) => reply.error(error),
        }
    }

    fn fsyncdir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        finish_empty(self.sync_handle(fh), reply);
    }

    fn releasedir(
        &self,
        _req: &Request,
        _ino: INodeNo,
        fh: FileHandle,
        _flags: OpenFlags,
        reply: ReplyEmpty,
    ) {
        finish_empty(self.close_handle(fh), reply);
    }

    fn statfs(&self, _req: &Request, _ino: INodeNo, reply: ReplyStatfs) {
        let statistics = self.state().and_then(|state| {
            state
                .filesystem
                .as_ref()
                .ok_or(Errno::EIO)?
                .statistics()
                .map_err(errno)
        });
        match statistics {
            Ok(statistics) => reply.statfs(
                statistics.total_data_blocks,
                statistics.free_data_blocks,
                statistics.free_data_blocks,
                statistics.total_inodes,
                statistics.free_inodes,
                BLOCK_SIZE as u32,
                96,
                BLOCK_SIZE as u32,
            ),
            Err(error) => reply.error(error),
        }
    }
}

fn finish_empty(result: Result<(), Errno>, reply: ReplyEmpty) {
    match result {
        Ok(()) => reply.ok(),
        Err(error) => reply.error(error),
    }
}

fn permission_mode(mode: u32, umask: u32) -> u16 {
    ((mode & !umask) & 0o7777) as u16
}

fn file_attr(attributes: NodeAttributes) -> Result<(FileAttr, u64), Errno> {
    let generation = attributes.generation;
    Ok((
        FileAttr {
            ino: INodeNo(attributes.node.0),
            size: attributes.size,
            blocks: attributes
                .allocated_blocks
                .saturating_mul((BLOCK_SIZE / 512) as u64),
            atime: system_time(attributes.accessed),
            mtime: system_time(attributes.modified),
            ctime: system_time(attributes.changed),
            crtime: system_time(attributes.created),
            kind: fuse_kind(attributes.kind)?,
            perm: attributes.mode,
            nlink: attributes.link_count,
            uid: attributes.uid,
            gid: attributes.gid,
            rdev: 0,
            blksize: BLOCK_SIZE as u32,
            flags: 0,
        },
        generation,
    ))
}

fn fuse_kind(kind: NodeKind) -> Result<FileType, Errno> {
    match kind {
        NodeKind::Regular => Ok(FileType::RegularFile),
        NodeKind::Directory => Ok(FileType::Directory),
        NodeKind::Symlink => Err(Errno::ENOTSUP),
        NodeKind::Free => Err(Errno::ENOENT),
    }
}

fn system_time(timestamp: Timestamp) -> SystemTime {
    UNIX_EPOCH
        .checked_add(Duration::new(timestamp.seconds, timestamp.nanoseconds))
        .unwrap_or(UNIX_EPOCH)
}

fn errno(error: Error) -> Errno {
    match error {
        Error::NotFound | Error::InvalidNode => Errno::ENOENT,
        Error::InvalidName | Error::InvalidCookie => Errno::EINVAL,
        Error::NotDirectory => Errno::ENOTDIR,
        Error::IsDirectory => Errno::EISDIR,
        Error::UnsupportedNodeKind => Errno::ENOTSUP,
        Error::ReadOnly => Errno::EROFS,
        Error::AlreadyExists => Errno::EEXIST,
        Error::NoSpace | Error::ExtentLimit => Errno::ENOSPC,
        Error::DirectoryNotEmpty => Errno::ENOTEMPTY,
        Error::DirectoryCycle => Errno::EINVAL,
        Error::TransactionTooLarge => Errno::E2BIG,
        Error::InvalidHandle => Errno::EBADF,
        Error::Device(BlockDeviceError::ReadOnly) => Errno::EROFS,
        Error::Device(_)
        | Error::Format(_)
        | Error::Phase2Required
        | Error::Phase3Required
        | Error::RecoveryRequired
        | Error::RedundantSuperblocksDisagree
        | Error::CorruptJournal
        | Error::ProtectedBlock
        | Error::Poisoned
        | Error::TransactionInProgress
        | Error::CorruptVolume
        | Error::ArithmeticOverflow => Errno::EIO,
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("nullfs-fuse: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut writable = false;
    let mut paths = Vec::new();
    for argument in env::args_os().skip(1) {
        if argument == "--read-write" {
            if writable {
                return Err(String::from("--read-write specified more than once"));
            }
            writable = true;
        } else if argument.as_bytes().starts_with(b"-") {
            return Err(format!(
                "unknown option `{}`; {USAGE}",
                argument.to_string_lossy()
            ));
        } else {
            paths.push(PathBuf::from(argument));
        }
    }
    if paths.len() != 2 {
        return Err(String::from(USAGE));
    }
    let mountpoint = paths.pop().expect("length checked");
    let image = paths.pop().expect("length checked");

    let device =
        FileBlockDevice::open(image, BLOCK_SIZE, writable).map_err(|error| error.to_string())?;
    let filesystem = if writable {
        Filesystem::mount_read_write(device)
    } else {
        Filesystem::mount(device)
    }
    .map_err(|error| error.to_string())?;

    let mut config = Config::default();
    // The mount is private to its owner unless an allow option is added. NullFS
    // does not support chown yet, so kernel-side checks against on-disk uid 0
    // would prevent the mounting user from mutating a freshly formatted image.
    config.mount_options = vec![MountOption::FSName(String::from("nullfs"))];
    if !writable {
        config.mount_options.push(MountOption::RO);
    }
    let (adapter, state) = NullFsFuse::new(filesystem, writable);
    let mount_result = fuser::mount(adapter, mountpoint, &config)
        .map_err(|error| format!("mount failed: {error}"));
    let mut state = state
        .lock()
        .map_err(|_| String::from("filesystem state lock poisoned after unmount"))?;
    if state.unmount_error.is_none()
        && let Some(filesystem) = state.filesystem.as_mut()
        && let Err(error) = filesystem.try_unmount()
    {
        state.unmount_error = Some(error.to_string());
    }
    finish_mount(mount_result, state.unmount_error.take())
}

fn finish_mount(
    mount_result: Result<(), String>,
    unmount_error: Option<String>,
) -> Result<(), String> {
    mount_result?;
    match unmount_error {
        Some(error) => Err(format!("clean unmount failed: {error}")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn core_handle(id: u64, generation: u64) -> OpenHandle {
        OpenHandle {
            id,
            node: NodeId(7),
            generation,
            kind: NodeKind::Regular,
        }
    }

    #[test]
    fn adapter_handles_do_not_alias_removed_entries() {
        let mut table = HandleTable::default();
        let first = table
            .insert(AdapterHandle {
                core: core_handle(1, 10),
                access: OpenAccMode::O_RDONLY,
            })
            .unwrap();
        assert_eq!(table.remove(first).unwrap().core.generation, 10);
        assert_eq!(table.get(first).unwrap_err(), Errno::EBADF);

        let second = table
            .insert(AdapterHandle {
                core: core_handle(2, 11),
                access: OpenAccMode::O_RDWR,
            })
            .unwrap();
        assert_ne!(first, second);
        assert_eq!(table.get(second).unwrap().core.generation, 11);
        assert_eq!(table.get(first).unwrap_err(), Errno::EBADF);
    }

    #[test]
    fn clean_unmount_error_is_returned_after_successful_mount_loop() {
        let result = finish_mount(Ok(()), Some(String::from("device flush failed")));
        assert_eq!(
            result,
            Err(String::from("clean unmount failed: device flush failed"))
        );
    }

    #[test]
    fn mount_error_remains_primary() {
        let result = finish_mount(
            Err(String::from("mount failed")),
            Some(String::from("unmount failed")),
        );
        assert_eq!(result, Err(String::from("mount failed")));
    }
}
