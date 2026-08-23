use super::files::legacy_custom_path;
use super::migration::{
    begin_rollback, finish_rolled_back_migration, prepare_migration, write_phase, MigrationPaths,
    MigrationPhase, MigrationSource,
};
use super::runtime::{embedded_daemon_plist, legacy_service_path};
use super::{
    validate_helper_tree, ExpectedOwner, HelperPaths, LAUNCH_DAEMONS_DIR, ROOT_OWNER, ROOT_UID,
};
use hbb_common::{anyhow::anyhow, bail, ResultType};
use std::path::Path;

pub(in crate::platform::macos) fn with_prepared_helper_for_plist_write<T>(
    app_name: &str,
    operation: impl FnOnce() -> ResultType<T>,
) -> ResultType<T> {
    validate_plist_writer_uid(unsafe { hbb_common::libc::geteuid() })?;
    let target = HelperPaths::for_app_name(app_name)?;
    let current = std::fs::canonicalize(std::env::current_exe()?)?;
    if current == target.service {
        validate_helper_tree(&target, ROOT_OWNER)?;
        return operation();
    }
    validate_legacy_writer(&current, app_name)?;
    let _maintenance_lock = super::acquire_mac_service_maintenance_lock(
        super::ServiceMaintenanceLockMode::FailIfLocked,
    )?;
    let custom = legacy_custom_path(&current)?;
    let daemon_plist_body = embedded_daemon_plist(app_name)?;
    let paths = MigrationPaths::for_roots(
        &target.privileged_tools,
        Path::new(LAUNCH_DAEMONS_DIR),
        app_name,
    )?;
    with_legacy_helper_for_plist_write(
        LegacyPlistMigration {
            paths: &paths,
            source: MigrationSource {
                service: &current,
                custom: custom.as_deref(),
                daemon_plist_body: &daemon_plist_body,
            },
            owner: ROOT_OWNER,
        },
        operation,
    )
}

struct LegacyPlistMigration<'a> {
    paths: &'a MigrationPaths,
    source: MigrationSource<'a>,
    owner: ExpectedOwner,
}

fn with_legacy_helper_for_plist_write<T>(
    migration: LegacyPlistMigration<'_>,
    operation: impl FnOnce() -> ResultType<T>,
) -> ResultType<T> {
    prepare_migration(migration.paths, &migration.source, migration.owner)?;
    let operation_result = operation().and_then(|value| {
        validate_helper_tree(&migration.paths.helper, migration.owner)?;
        Ok(value)
    });
    match operation_result {
        Ok(value) => Ok(value),
        Err(operation_error) => match rollback_legacy_migration(migration.paths, migration.owner) {
            Ok(()) => Err(operation_error),
            Err(rollback_error) => Err(anyhow!(
                "plist write failed ({operation_error}); migration rollback failed ({rollback_error})"
            )),
        },
    }
}

fn rollback_legacy_migration(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    match std::fs::symlink_metadata(&paths.state_directory) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    begin_rollback(paths, owner)?;
    write_phase(paths, MigrationPhase::RolledBack, owner)?;
    finish_rolled_back_migration(paths, owner)
}

fn validate_legacy_writer(current: &Path, app_name: &str) -> ResultType<()> {
    let legacy = legacy_service_path(app_name)?;
    if current != legacy {
        bail!(
            "Refusing plist writer executable path: {}",
            current.display()
        );
    }
    Ok(())
}

fn validate_plist_writer_uid(effective_uid: u32) -> ResultType<()> {
    if effective_uid != ROOT_UID {
        bail!("Privileged plist writer requires root");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
