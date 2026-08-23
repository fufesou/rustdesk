use super::launchd::SystemLaunchd;
use super::lock::FinalizerLock;
use super::{migration_state_exists, migration_state_step, LAUNCH_DAEMONS_DIR, ROOT_OWNER};
use crate::platform::macos::privileged_helper::migration::{
    complete_migration_with, validate_state, LaunchdControl, MigrationPaths,
};
use crate::platform::macos::privileged_helper::{ExpectedOwner, HelperPaths, ROOT_UID};
use hbb_common::{bail, log, ResultType};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

pub(super) fn spawn_finalizer(helper: &Path) -> ResultType<()> {
    let mut command = Command::new(helper);
    command.arg("--complete-helper-migration");
    detach_finalizer_process_group(&mut command);
    let mut child = command.spawn()?;
    std::thread::Builder::new()
        .name("helper-migration-finalizer".to_owned())
        .spawn(move || match child.wait() {
            Ok(status) if status.success() => {}
            Ok(status) => log::error!("Helper migration finalizer exited with {status}"),
            Err(err) => log::error!("Failed to wait for helper migration finalizer: {err}"),
        })?;
    Ok(())
}

pub(super) fn detach_finalizer_process_group(command: &mut Command) {
    command.process_group(0);
}

pub(super) fn validate_finalizer_identity(
    current_executable: &Path,
    expected_helper: &Path,
    effective_uid: u32,
) -> ResultType<()> {
    if effective_uid != ROOT_UID {
        bail!("Helper migration finalizer requires root");
    }
    if current_executable != expected_helper {
        bail!(
            "Helper migration finalizer must run from protected path: {}",
            expected_helper.display()
        );
    }
    Ok(())
}

pub(crate) fn complete_helper_migration() -> ResultType<()> {
    let app_name = crate::get_app_name();
    let helper = HelperPaths::for_app_name(&app_name)?;
    let current_executable = std::env::current_exe()?;
    let effective_uid = unsafe { hbb_common::libc::geteuid() };
    validate_finalizer_identity(&current_executable, &helper.service, effective_uid)?;
    let paths = MigrationPaths::for_roots(
        &helper.privileged_tools,
        Path::new(LAUNCH_DAEMONS_DIR),
        &app_name,
    )?;
    let _maintenance_lock = super::super::acquire_mac_service_maintenance_lock(
        super::super::ServiceMaintenanceLockMode::Wait,
    )?;
    let mut launchd = SystemLaunchd;
    complete_existing_migration(&paths, ROOT_OWNER, &mut launchd)
}

pub(super) fn complete_existing_migration(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
    launchd: &mut dyn LaunchdControl,
) -> ResultType<()> {
    if !migration_state_exists(paths)? {
        return Ok(());
    }
    let Some(()) = migration_state_step(paths, validate_state(paths, owner))? else {
        return Ok(());
    };
    let Some(_finalizer_lock) =
        migration_state_step(paths, FinalizerLock::acquire(&paths.state_directory, owner))?
    else {
        return Ok(());
    };
    if !migration_state_exists(paths)? {
        return Ok(());
    }
    complete_migration_with(paths, owner, launchd)
}
