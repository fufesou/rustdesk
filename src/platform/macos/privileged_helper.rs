use hbb_common::{bail, ResultType};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

mod files;
mod ipc_mode;
mod migration;
mod operation_lock;
mod plist_write;
mod runtime;
pub(super) use ipc_mode::complete_migration_readiness;
pub(crate) use ipc_mode::{legacy_rollback_ipc_enabled, protected_service_ipc_enabled};
use ipc_mode::{record_service_ipc_mode, ServiceIpcMode};
pub(crate) use operation_lock::{
    acquire_mac_service_maintenance_lock, ServiceMaintenanceLock, ServiceMaintenanceLockMode,
    SERVICE_MAINTENANCE_LOCK_PATH,
};
pub(super) use plist_write::with_prepared_helper_for_plist_write;
pub(super) use runtime::complete_helper_migration;
pub(super) use runtime::{
    prepare_service_start, wait_for_migration_completion, ServiceStartAction,
};

pub(crate) const PRIVILEGED_HELPER_TOOLS_DIR: &str = "/Library/PrivilegedHelperTools";
const LAUNCH_DAEMONS_DIR: &str = "/Library/LaunchDaemons";
const ROOT_UID: u32 = 0;
const WHEEL_GID: u32 = 0;
const ROOT_OWNER: ExpectedOwner = ExpectedOwner {
    uid: ROOT_UID,
    gid: WHEEL_GID,
};
const NON_ROOT_WRITE_BITS: u32 = 0o022;
const PERMISSION_BITS: u32 = 0o7777;
const STICKY_BIT: u32 = 0o1000;
const SERVICE_MODE: u32 = 0o755;
const CUSTOM_MODE: u32 = 0o600;
const ACL_TYPE_EXTENDED: u32 = 0x0000_0100;

type Acl = *mut hbb_common::libc::c_void;
type AclEntry = *mut hbb_common::libc::c_void;

extern "C" {
    fn acl_free(acl: *mut hbb_common::libc::c_void) -> hbb_common::libc::c_int;
    fn acl_get_entry(
        acl: Acl,
        entry_id: hbb_common::libc::c_int,
        entry: *mut AclEntry,
    ) -> hbb_common::libc::c_int;
    fn acl_get_fd_np(fd: hbb_common::libc::c_int, acl_type: u32) -> Acl;
    fn acl_init(count: hbb_common::libc::c_int) -> Acl;
    fn acl_set_fd_np(
        fd: hbb_common::libc::c_int,
        acl: Acl,
        acl_type: u32,
    ) -> hbb_common::libc::c_int;
    fn acl_valid(acl: Acl) -> hbb_common::libc::c_int;
}

struct AclGuard(Acl);

impl Drop for AclGuard {
    fn drop(&mut self) {
        unsafe {
            acl_free(self.0);
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HelperPaths {
    pub(crate) privileged_tools: PathBuf,
    pub(crate) bundle: PathBuf,
    pub(crate) contents: PathBuf,
    pub(crate) macos: PathBuf,
    pub(crate) resources: PathBuf,
    pub(crate) service: PathBuf,
    pub(crate) custom: PathBuf,
}

impl HelperPaths {
    pub(crate) fn for_app_name(app_name: &str) -> ResultType<Self> {
        Self::for_privileged_tools_dir(Path::new(PRIVILEGED_HELPER_TOOLS_DIR), app_name)
    }

    fn for_privileged_tools_dir(privileged_tools: &Path, app_name: &str) -> ResultType<Self> {
        super::super::validate_install_app_name(app_name)?;
        let bundle = privileged_tools.join(format!("com.carriez.{app_name}_service.bundle"));
        let contents = bundle.join("Contents");
        let macos = contents.join("MacOS");
        let resources = contents.join("Resources");
        Ok(Self {
            privileged_tools: privileged_tools.to_owned(),
            service: macos.join("service"),
            custom: resources.join("custom.txt"),
            bundle,
            contents,
            macos,
            resources,
        })
    }
}

pub(crate) fn expected_gui_executable(app_name: &str) -> ResultType<PathBuf> {
    super::super::validate_install_app_name(app_name)?;
    Ok(Path::new("/Applications")
        .join(format!("{app_name}.app"))
        .join("Contents/MacOS")
        .join(app_name))
}

pub(crate) fn harden_root_private_directory(path: &Path) -> ResultType<()> {
    files::harden_owned_directory(
        path,
        ExpectedOwner {
            uid: ROOT_UID,
            gid: WHEEL_GID,
        },
        0o700,
    )
}

#[derive(Clone, Copy)]
struct ExpectedOwner {
    uid: u32,
    gid: u32,
}

struct HelperSource<'a> {
    service: &'a Path,
    custom: Option<&'a Path>,
}

pub(crate) fn validate_installed_helper(app_name: &str) -> ResultType<HelperPaths> {
    let paths = HelperPaths::for_app_name(app_name)?;
    validate_helper_tree(
        &paths,
        ExpectedOwner {
            uid: ROOT_UID,
            gid: WHEEL_GID,
        },
    )?;
    Ok(paths)
}

fn validate_helper_tree(paths: &HelperPaths, owner: ExpectedOwner) -> ResultType<()> {
    validate_ancestor_directories(&paths.service)?;
    validate_privileged_tools_directory(&paths.privileged_tools, owner)?;
    for directory in [
        &paths.bundle,
        &paths.contents,
        &paths.macos,
        &paths.resources,
    ] {
        validate_owned_directory(directory, owner)?;
    }
    validate_owned_file(&paths.service, owner, SERVICE_MODE)?;
    match std::fs::symlink_metadata(&paths.custom) {
        Ok(_) => validate_owned_file(&paths.custom, owner, CUSTOM_MODE)?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    Ok(())
}

fn validate_ancestor_directories(path: &Path) -> ResultType<()> {
    let Some(parent) = path.parent() else {
        bail!("Helper path has no parent: {}", path.display());
    };
    for ancestor in parent.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            bail!(
                "Helper ancestor is not a real directory: {}",
                ancestor.display()
            );
        }
    }
    Ok(())
}

fn validate_privileged_tools_directory(path: &Path, owner: ExpectedOwner) -> ResultType<()> {
    let metadata = directory_metadata(path)?;
    validate_no_extended_acl(path)?;
    let mode = metadata.mode() & PERMISSION_BITS;
    if metadata.uid() != owner.uid {
        bail!("Helper directory is not root-owned: {}", path.display());
    }
    if mode & NON_ROOT_WRITE_BITS != 0 && mode & STICKY_BIT == 0 {
        bail!(
            "Non-root-writable helper directory lacks sticky bit: {}",
            path.display()
        );
    }
    Ok(())
}

fn validate_owned_directory(path: &Path, owner: ExpectedOwner) -> ResultType<()> {
    let metadata = directory_metadata(path)?;
    validate_no_extended_acl(path)?;
    validate_owner(path, &metadata, owner)?;
    if metadata.mode() & NON_ROOT_WRITE_BITS != 0 {
        bail!(
            "Helper directory is writable by non-root: {}",
            path.display()
        );
    }
    Ok(())
}

fn directory_metadata(path: &Path) -> ResultType<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        bail!("Expected a real directory: {}", path.display());
    }
    Ok(metadata)
}

fn validate_owned_file(path: &Path, owner: ExpectedOwner, mode: u32) -> ResultType<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("Expected a regular file: {}", path.display());
    }
    validate_no_extended_acl(path)?;
    validate_owner(path, &metadata, owner)?;
    let actual_mode = metadata.mode() & PERMISSION_BITS;
    if actual_mode != mode {
        bail!(
            "Unexpected permissions on {}: {:o}, expected {:o}",
            path.display(),
            actual_mode,
            mode
        );
    }
    Ok(())
}

fn validate_no_extended_acl(path: &Path) -> ResultType<()> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(hbb_common::libc::O_NOFOLLOW)
        .open(path)?;
    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(hbb_common::libc::ENOENT) {
            return Ok(());
        }
        return Err(error.into());
    }
    let acl = AclGuard(acl);
    if unsafe { acl_valid(acl.0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut entry = std::ptr::null_mut();
    if unsafe { acl_get_entry(acl.0, 0, &mut entry) } == 0 {
        bail!(
            "Extended ACL is not allowed on helper path: {}",
            path.display()
        );
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(hbb_common::libc::EINVAL) {
        return Ok(());
    }
    Err(error.into())
}

fn clear_extended_acl(file: &std::fs::File) -> ResultType<()> {
    let acl = unsafe { acl_init(0) };
    if acl.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let acl = AclGuard(acl);
    if unsafe { acl_set_fd_np(file.as_raw_fd(), acl.0, ACL_TYPE_EXTENDED) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

fn validate_owner(
    path: &Path,
    metadata: &std::fs::Metadata,
    owner: ExpectedOwner,
) -> ResultType<()> {
    if metadata.uid() != owner.uid || metadata.gid() != owner.gid {
        bail!("Unexpected owner for helper path: {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
