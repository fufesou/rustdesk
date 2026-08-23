use super::files::{legacy_custom_path, read_regular_file};
use super::migration::{
    finish_rolled_back_migration, phase, prepare_migration, validate_state, write_phase,
    MigrationPaths, MigrationPhase, MigrationSource,
};
use super::{
    validate_helper_tree, validate_installed_helper, validate_owned_file, ExpectedOwner,
    HelperPaths, LAUNCH_DAEMONS_DIR, ROOT_OWNER, ROOT_UID,
};
use hbb_common::{bail, ResultType};
use std::path::{Path, PathBuf};

mod finalizer;
mod launchd;
mod lock;
pub(crate) use finalizer::complete_helper_migration;
use finalizer::spawn_finalizer;
#[cfg(test)]
use finalizer::{
    complete_existing_migration, detach_finalizer_process_group, validate_finalizer_identity,
};
#[cfg(test)]
use lock::FinalizerLock;

const FINALIZER_LOCK_NAME: &str = "finalizer.lock";
const MIGRATION_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServiceStartAction {
    Start,
    StartForMigrationReadiness,
    StartForRollbackReadiness,
    ExitAfterMigrationLaunch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ServiceIdentity {
    Protected,
    Legacy,
}

struct ServiceExecutables<'a> {
    current: &'a Path,
    protected: &'a Path,
    legacy: &'a Path,
}

fn classify_service_identity(
    executables: ServiceExecutables<'_>,
    effective_uid: u32,
) -> ResultType<ServiceIdentity> {
    if effective_uid != ROOT_UID {
        bail!("macOS service requires root");
    }
    if executables.current == executables.protected {
        return Ok(ServiceIdentity::Protected);
    }
    if executables.current == executables.legacy {
        return Ok(ServiceIdentity::Legacy);
    }
    bail!(
        "Refusing macOS service executable path: {}",
        executables.current.display()
    )
}

pub(crate) fn prepare_service_start() -> ResultType<ServiceStartAction> {
    let app_name = crate::get_app_name();
    let helper = HelperPaths::for_app_name(&app_name)?;
    let legacy = legacy_service_path(&app_name)?;
    let current = std::fs::canonicalize(std::env::current_exe()?)?;
    let owner = ROOT_OWNER;
    let effective_uid = unsafe { hbb_common::libc::geteuid() };
    let executables = ServiceExecutables {
        current: &current,
        protected: &helper.service,
        legacy: &legacy,
    };
    let action = match classify_service_identity(executables, effective_uid)? {
        ServiceIdentity::Protected => prepare_protected_start(&helper, &app_name, owner),
        ServiceIdentity::Legacy => prepare_legacy_start(&helper, &app_name, owner),
    }?;
    // The legacy IPC exception is process-local and can only be selected after
    // validating a root-owned rollback state in prepare_legacy_start().
    let ipc_mode = match action {
        ServiceStartAction::StartForMigrationReadiness => super::ServiceIpcMode::MigrationReadiness,
        ServiceStartAction::StartForRollbackReadiness => super::ServiceIpcMode::LegacyRollback,
        ServiceStartAction::Start | ServiceStartAction::ExitAfterMigrationLaunch => {
            super::ServiceIpcMode::ProtectedOnly
        }
    };
    super::record_service_ipc_mode(ipc_mode)?;
    Ok(action)
}

fn prepare_protected_start(
    helper: &HelperPaths,
    app_name: &str,
    owner: ExpectedOwner,
) -> ResultType<ServiceStartAction> {
    validate_installed_helper(app_name)?;
    let paths = migration_paths(helper, app_name)?;
    let action = match std::fs::symlink_metadata(&paths.state_directory) {
        Ok(_) => {
            validate_state(&paths, owner)?;
            recover_protected_preparation(&paths, owner)?;
            phase(&paths, owner)?;
            spawn_finalizer(&helper.service)?;
            ServiceStartAction::StartForMigrationReadiness
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ServiceStartAction::Start,
        Err(err) => return Err(err.into()),
    };
    Ok(action)
}

pub(super) fn recover_protected_preparation(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
) -> ResultType<()> {
    if phase(paths, owner)? != MigrationPhase::Preparing {
        return Ok(());
    }
    validate_helper_tree(&paths.helper, owner)?;
    validate_owned_file(&paths.daemon_plist, owner, 0o644)?;
    if read_regular_file(&paths.daemon_plist)? != read_regular_file(&paths.plist_expected)? {
        bail!("Interrupted helper preparation has an unexpected daemon plist");
    }
    write_phase(paths, MigrationPhase::Prepared, owner)
}

pub(crate) fn wait_for_migration_completion() -> ResultType<()> {
    let app_name = crate::get_app_name();
    let helper = validate_installed_helper(&app_name)?;
    let paths = migration_paths(&helper, &app_name)?;
    loop {
        if migration_is_complete(&paths, ROOT_OWNER)? {
            return Ok(());
        }
        std::thread::sleep(MIGRATION_POLL_INTERVAL);
    }
}

fn migration_is_complete(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<bool> {
    match std::fs::symlink_metadata(&paths.state_directory) {
        Ok(_) => {
            let Some(()) = migration_state_step(paths, validate_state(paths, owner))? else {
                validate_helper_tree(&paths.helper, owner)?;
                return Ok(true);
            };
            let Some(pending_phase) = migration_state_step(paths, phase(paths, owner))? else {
                validate_helper_tree(&paths.helper, owner)?;
                return Ok(true);
            };
            match pending_phase {
                MigrationPhase::Committed => Ok(true),
                MigrationPhase::RollingBack
                | MigrationPhase::RolledBack
                | MigrationPhase::RollbackFailed => {
                    bail!("Privileged helper migration rolled back")
                }
                _ => Ok(false),
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            validate_helper_tree(&paths.helper, owner)?;
            Ok(true)
        }
        Err(err) => Err(err.into()),
    }
}

fn migration_state_step<T>(paths: &MigrationPaths, result: ResultType<T>) -> ResultType<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(_error) if !migration_state_exists(paths)? => Ok(None),
        Err(error) => Err(error),
    }
}

fn migration_state_exists(paths: &MigrationPaths) -> ResultType<bool> {
    match std::fs::symlink_metadata(&paths.state_directory) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn prepare_legacy_start(
    helper: &HelperPaths,
    app_name: &str,
    owner: ExpectedOwner,
) -> ResultType<ServiceStartAction> {
    let legacy = legacy_service_path(app_name)?;
    let paths = migration_paths(helper, app_name)?;
    if let Some((pending_phase, action)) = pending_legacy_action(&paths, owner)? {
        if pending_phase == MigrationPhase::RolledBack {
            finish_rolled_back_migration(&paths, owner)?;
        } else {
            validate_installed_helper(app_name)?;
            spawn_finalizer(&helper.service)?;
        }
        return Ok(action);
    }
    let _maintenance_lock = super::acquire_mac_service_maintenance_lock(
        super::ServiceMaintenanceLockMode::FailIfLocked,
    )?;
    let custom = legacy_custom_path(&legacy)?;
    let daemon_body = embedded_daemon_plist(app_name)?;
    prepare_migration(
        &paths,
        &MigrationSource {
            service: &legacy,
            custom: custom.as_deref(),
            daemon_plist_body: &daemon_body,
        },
        owner,
    )?;
    spawn_finalizer(&helper.service)?;
    Ok(ServiceStartAction::ExitAfterMigrationLaunch)
}

fn pending_legacy_action(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
) -> ResultType<Option<(MigrationPhase, ServiceStartAction)>> {
    match std::fs::symlink_metadata(&paths.state_directory) {
        Ok(_) => {
            validate_state(paths, owner)?;
            let pending_phase = phase(paths, owner)?;
            Ok(legacy_pending_action(pending_phase).map(|action| (pending_phase, action)))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn legacy_pending_action(phase: MigrationPhase) -> Option<ServiceStartAction> {
    match phase {
        MigrationPhase::RollingBack
        | MigrationPhase::RolledBack
        | MigrationPhase::RollbackFailed => Some(ServiceStartAction::StartForRollbackReadiness),
        _ => None,
    }
}

fn migration_paths(helper: &HelperPaths, app_name: &str) -> ResultType<MigrationPaths> {
    MigrationPaths::for_roots(
        &helper.privileged_tools,
        Path::new(LAUNCH_DAEMONS_DIR),
        app_name,
    )
}

pub(super) fn legacy_service_path(app_name: &str) -> ResultType<PathBuf> {
    super::super::super::validate_install_app_name(app_name)?;
    Ok(Path::new("/Applications")
        .join(format!("{app_name}.app"))
        .join("Contents/MacOS/service"))
}

pub(super) fn embedded_daemon_plist(app_name: &str) -> ResultType<String> {
    let Some(plist) = super::super::PRIVILEGES_SCRIPTS_DIR.get_file("daemon.plist") else {
        bail!("daemon.plist not found in embedded resources");
    };
    let Some(body) = plist.contents_utf8() else {
        bail!("Failed to read embedded daemon.plist");
    };
    super::super::render_installed_plist_body(body, app_name)
}

#[cfg(test)]
mod tests;
