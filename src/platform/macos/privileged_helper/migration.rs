use super::files::{
    create_owned_directory, create_staged_helper, helper_contents_match, read_regular_file,
    reject_existing_path, sync_parent, write_owned_atomically, OwnedFileOptions,
};
use super::{
    validate_ancestor_directories, validate_helper_tree, validate_owned_file,
    validate_privileged_tools_directory, ExpectedOwner, HelperSource,
};
use hbb_common::{bail, log, ResultType};
use std::path::{Path, PathBuf};

mod activation;
mod state;
use activation::{fail_and_rollback, rollback, verify_activation, MigrationFailure};
pub(super) use state::{
    phase, validate_state, validate_state_directory, write_phase, MigrationPaths, MigrationPhase,
};

pub(super) struct MigrationSource<'a> {
    pub(super) service: &'a Path,
    pub(super) custom: Option<&'a Path>,
    pub(super) daemon_plist_body: &'a str,
}

pub(super) trait LaunchdControl {
    fn reload(&mut self, label: &str, plist: &Path) -> ResultType<()>;
    fn is_expected_ready(
        &mut self,
        _label: &str,
        _socket: &Path,
        _expected_executable: &Path,
    ) -> ResultType<bool> {
        bail!("Launchd readiness requires an expected executable check")
    }
}

pub(super) fn prepare_migration(
    paths: &MigrationPaths,
    source: &MigrationSource<'_>,
    owner: ExpectedOwner,
) -> ResultType<()> {
    ensure_privileged_tools_directory(paths, owner)?;
    if paths.state_directory.exists() {
        return recover_interrupted_preparation(paths, owner);
    }
    create_owned_directory(&paths.state_directory, owner, 0o700)?;
    validate_owned_file(&paths.daemon_plist, owner, 0o644)?;
    let old_plist = read_regular_file(&paths.daemon_plist)?;
    write_owned_atomically(
        &paths.plist_backup,
        &old_plist,
        OwnedFileOptions { owner, mode: 0o600 },
    )?;
    write_owned_atomically(
        &paths.plist_expected,
        source.daemon_plist_body.as_bytes(),
        OwnedFileOptions { owner, mode: 0o600 },
    )?;
    write_phase(paths, MigrationPhase::Preparing, owner)?;
    create_staged_helper(
        &paths.staging_helper,
        HelperSource {
            service: source.service,
            custom: source.custom,
        },
        owner,
    )?;
    install_or_reuse_staged_helper(paths, owner)?;
    write_owned_atomically(
        &paths.daemon_plist,
        source.daemon_plist_body.as_bytes(),
        OwnedFileOptions { owner, mode: 0o644 },
    )?;
    write_phase(paths, MigrationPhase::Prepared, owner)
}

fn install_or_reuse_staged_helper(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    match std::fs::symlink_metadata(&paths.helper.bundle) {
        Ok(_) => {
            validate_helper_tree(&paths.helper, owner)?;
            if !helper_contents_match(&paths.staging_helper, &paths.helper)? {
                return reject_helper_mismatch(paths, owner);
            }
            std::fs::remove_dir_all(&paths.staging_helper.bundle)?;
            sync_parent(&paths.staging_helper.bundle)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_phase(paths, MigrationPhase::InstallingHelper, owner)?;
            std::fs::rename(&paths.staging_helper.bundle, &paths.helper.bundle)?;
            sync_parent(&paths.helper.bundle)?;
            validate_helper_tree(&paths.helper, owner)?;
            write_phase(paths, MigrationPhase::Preparing, owner)?;
        }
        Err(error) => return Err(error.into()),
    }
    validate_helper_tree(&paths.helper, owner)
}

fn reject_helper_mismatch<T>(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<T> {
    match cleanup_state(paths, owner) {
        Ok(()) => bail!("Existing protected helper does not match migration source"),
        Err(error) => bail!(
            "Existing protected helper does not match migration source; state cleanup failed: {error}"
        ),
    }
}

pub(super) fn complete_migration_with(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
    launchd: &mut dyn LaunchdControl,
) -> ResultType<()> {
    validate_state(paths, owner)?;
    let pending_phase = phase(paths, owner)?;
    if pending_phase != MigrationPhase::RolledBack {
        validate_helper_tree(&paths.helper, owner)?;
    }
    match pending_phase {
        MigrationPhase::Preparing | MigrationPhase::InstallingHelper => {
            bail!("Privileged helper migration is not ready for launchd reload")
        }
        MigrationPhase::Prepared | MigrationPhase::Bootstrapping => {
            if pending_phase == MigrationPhase::Prepared {
                write_phase(paths, MigrationPhase::Bootstrapping, owner)?;
            }
            if let Err(err) = launchd.reload(&paths.daemon_label, &paths.daemon_plist) {
                return fail_and_rollback(
                    paths,
                    launchd,
                    MigrationFailure::new(owner, err.to_string()),
                );
            }
        }
        MigrationPhase::RollingBack | MigrationPhase::RollbackFailed => {
            rollback(paths, owner, launchd)?;
            bail!("Recovered a previously failed privileged helper rollback")
        }
        MigrationPhase::RolledBack => {
            finish_rolled_back_migration(paths, owner)?;
            log::warn!("Finished interrupted privileged helper rollback");
            return Ok(());
        }
        MigrationPhase::Committed => return finish_commit(paths, owner),
    }
    verify_activation(paths, owner, launchd)
}

fn ensure_privileged_tools_directory(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
) -> ResultType<()> {
    match std::fs::symlink_metadata(&paths.helper.privileged_tools) {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            create_owned_directory(&paths.helper.privileged_tools, owner, 0o755)?;
        }
        Err(err) => return Err(err.into()),
    }
    validate_ancestor_directories(&paths.helper.privileged_tools.join("placeholder"))?;
    validate_privileged_tools_directory(&paths.helper.privileged_tools, owner)
}

fn recover_interrupted_preparation(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    validate_state_directory(paths, owner)?;
    let pending_phase = match std::fs::symlink_metadata(&paths.phase_file) {
        Ok(_) => Some(phase(paths, owner)?),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => return Err(err.into()),
    };
    match pending_phase {
        Some(MigrationPhase::Prepared | MigrationPhase::Bootstrapping) => {
            validate_state(paths, owner)?;
            validate_helper_tree(&paths.helper, owner)?;
            Ok(())
        }
        Some(MigrationPhase::Committed) => finish_commit(paths, owner),
        Some(MigrationPhase::InstallingHelper) => {
            rollback_interrupted_preparation(paths, owner, true)
        }
        Some(MigrationPhase::Preparing) | None => {
            rollback_interrupted_preparation(paths, owner, false)
        }
        Some(_) => bail!("Helper migration recovery requires the migration finalizer"),
    }
}

fn rollback_interrupted_preparation(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
    remove_installed_helper: bool,
) -> ResultType<()> {
    if paths.plist_backup.exists() {
        restore_plist(paths, owner)?;
    } else if remove_installed_helper && paths.helper.bundle.exists() {
        bail!("CRITICAL: helper exists without a migration plist backup");
    }
    if remove_installed_helper {
        remove_helper_if_present(paths, owner)?;
    }
    cleanup_state(paths, owner)?;
    bail!("Interrupted helper preparation was rolled back; retry migration")
}

pub(super) fn begin_rollback(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    write_phase(paths, MigrationPhase::RollingBack, owner)?;
    restore_plist(paths, owner)
}

pub(super) fn finish_rolled_back_migration(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
) -> ResultType<()> {
    validate_state(paths, owner)?;
    if phase(paths, owner)? != MigrationPhase::RolledBack {
        bail!("Privileged helper rollback has not reached a verified state");
    }
    if rollback_references_protected_helper(paths)? {
        validate_helper_tree(&paths.helper, owner)?;
    } else {
        remove_helper_if_present(paths, owner)?;
    }
    cleanup_state(paths, owner)
}

pub(super) fn rollback_expected_executable(paths: &MigrationPaths) -> ResultType<PathBuf> {
    if rollback_references_protected_helper(paths)? {
        return Ok(paths.helper.service.clone());
    }
    paths.expected_legacy_service()
}

fn rollback_references_protected_helper(paths: &MigrationPaths) -> ResultType<bool> {
    let output = std::process::Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg("Print :ProgramArguments:0")
        .arg(&paths.plist_backup)
        .output()?;
    if !output.status.success() {
        bail!(
            "Failed to read restored daemon program: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let program = std::str::from_utf8(&output.stdout)?.trim();
    Ok(Path::new(program) == paths.helper.service)
}

fn restore_plist(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    validate_owned_file(&paths.plist_backup, owner, 0o600)?;
    let old_plist = read_regular_file(&paths.plist_backup)?;
    write_owned_atomically(
        &paths.daemon_plist,
        &old_plist,
        OwnedFileOptions { owner, mode: 0o644 },
    )
}

fn finish_commit(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    write_phase(paths, MigrationPhase::Committed, owner)?;
    if let Err(err) = cleanup_state(paths, owner) {
        log::warn!("Protected helper committed, but migration cleanup failed: {err}");
    }
    Ok(())
}

fn remove_helper_if_present(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    match std::fs::symlink_metadata(&paths.helper.bundle) {
        Ok(_) => {
            if let Err(validation_error) = validate_helper_tree(&paths.helper, owner) {
                return quarantine_invalid_helper(paths, owner, validation_error);
            }
            std::fs::remove_dir_all(&paths.helper.bundle)?;
            sync_parent(&paths.helper.bundle)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn quarantine_invalid_helper(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
    validation_error: hbb_common::anyhow::Error,
) -> ResultType<()> {
    validate_state_directory(paths, owner)?;
    let quarantine = paths.state_directory.join("failed-helper.bundle");
    reject_existing_path(&quarantine)?;
    log::warn!("Quarantining invalid helper during migration rollback: {validation_error}");
    std::fs::rename(&paths.helper.bundle, &quarantine)?;
    sync_parent(&paths.helper.bundle)
}

fn cleanup_state(paths: &MigrationPaths, owner: ExpectedOwner) -> ResultType<()> {
    validate_state_directory(paths, owner)?;
    std::fs::remove_dir_all(&paths.state_directory)?;
    sync_parent(&paths.state_directory)
}
