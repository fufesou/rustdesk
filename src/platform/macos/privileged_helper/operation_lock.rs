use super::{clear_extended_acl, ROOT_UID, WHEEL_GID};
use hbb_common::{bail, ResultType};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

pub(crate) const SERVICE_MAINTENANCE_LOCK_PATH: &str =
    "/var/run/com.carriez.service-maintenance.lock";
const LOCK_MODE: u32 = 0o600;

#[derive(Clone, Copy)]
pub(crate) enum ServiceMaintenanceLockMode {
    Wait,
    FailIfLocked,
}

pub(crate) struct ServiceMaintenanceLock {
    _file: std::fs::File,
}

pub(crate) fn acquire_mac_service_maintenance_lock(
    mode: ServiceMaintenanceLockMode,
) -> ResultType<ServiceMaintenanceLock> {
    if unsafe { hbb_common::libc::geteuid() } != ROOT_UID {
        bail!("macOS service maintenance lock requires root");
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(LOCK_MODE)
        .custom_flags(hbb_common::libc::O_NOFOLLOW | hbb_common::libc::O_CLOEXEC)
        .open(SERVICE_MAINTENANCE_LOCK_PATH)?;
    let initial = file.metadata()?;
    if !initial.file_type().is_file() || initial.uid() != ROOT_UID {
        bail!("macOS service maintenance lock is not a root-owned regular file");
    }
    if unsafe { hbb_common::libc::fchown(file.as_raw_fd(), ROOT_UID, WHEEL_GID) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    clear_extended_acl(&file)?;
    file.set_permissions(std::fs::Permissions::from_mode(LOCK_MODE))?;
    validate_lock_identity(&file)?;

    let mut operation = hbb_common::libc::LOCK_EX;
    if matches!(mode, ServiceMaintenanceLockMode::FailIfLocked) {
        operation |= hbb_common::libc::LOCK_NB;
    }
    if unsafe { hbb_common::libc::flock(file.as_raw_fd(), operation) } != 0 {
        let error = std::io::Error::last_os_error();
        if matches!(mode, ServiceMaintenanceLockMode::FailIfLocked)
            && error.kind() == std::io::ErrorKind::WouldBlock
        {
            bail!("another macOS service maintenance transaction is already running");
        }
        return Err(error.into());
    }
    Ok(ServiceMaintenanceLock { _file: file })
}

fn validate_lock_identity(file: &std::fs::File) -> ResultType<()> {
    let opened = file.metadata()?;
    let path = std::fs::symlink_metadata(SERVICE_MAINTENANCE_LOCK_PATH)?;
    let opened_mode = opened.mode() & 0o7777;
    if !path.file_type().is_file()
        || opened.dev() != path.dev()
        || opened.ino() != path.ino()
        || opened.uid() != ROOT_UID
        || opened.gid() != WHEEL_GID
        || opened_mode != LOCK_MODE
    {
        bail!("macOS service maintenance lock identity or permissions changed");
    }
    Ok(())
}
