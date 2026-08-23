use super::{
    begin_rollback, finish_commit, finish_rolled_back_migration, rollback_expected_executable,
    write_phase, LaunchdControl, MigrationPaths, MigrationPhase,
};
use crate::platform::macos::privileged_helper::ExpectedOwner;
use hbb_common::{bail, ResultType};

pub(super) struct MigrationFailure {
    owner: ExpectedOwner,
    reason: String,
}

impl MigrationFailure {
    pub(super) fn new(owner: ExpectedOwner, reason: impl Into<String>) -> Self {
        Self {
            owner,
            reason: reason.into(),
        }
    }
}

pub(super) fn fail_and_rollback(
    paths: &MigrationPaths,
    launchd: &mut dyn LaunchdControl,
    failure: MigrationFailure,
) -> ResultType<()> {
    match rollback(paths, failure.owner, launchd) {
        Ok(()) => bail!(
            "Protected helper activation failed; old daemon restored: {}",
            failure.reason
        ),
        Err(rollback_error) => {
            let phase_result = write_phase(paths, MigrationPhase::RollbackFailed, failure.owner);
            bail!(
                "CRITICAL: protected helper activation failed ({}); rollback failed ({rollback_error}); state persistence: {phase_result:?}",
                failure.reason
            )
        }
    }
}

pub(super) fn verify_activation(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
    launchd: &mut dyn LaunchdControl,
) -> ResultType<()> {
    match launchd.is_expected_ready(
        &paths.daemon_label,
        &paths.daemon_socket,
        &paths.helper.service,
    ) {
        Ok(true) => finish_commit(paths, owner),
        Ok(false) => fail_and_rollback(
            paths,
            launchd,
            MigrationFailure::new(owner, "protected helper did not become ready"),
        ),
        Err(error) => fail_and_rollback(
            paths,
            launchd,
            MigrationFailure::new(owner, error.to_string()),
        ),
    }
}

pub(super) fn rollback(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
    launchd: &mut dyn LaunchdControl,
) -> ResultType<()> {
    begin_rollback(paths, owner)?;
    launchd.reload(&paths.daemon_label, &paths.daemon_plist)?;
    let expected_executable = rollback_expected_executable(paths)?;
    if !launchd.is_expected_ready(
        &paths.daemon_label,
        &paths.daemon_socket,
        &expected_executable,
    )? {
        bail!("Restored daemon did not become ready");
    }
    write_phase(paths, MigrationPhase::RolledBack, owner)?;
    finish_rolled_back_migration(paths, owner)
}
