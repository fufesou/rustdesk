use super::super::files::{read_regular_file, write_owned_atomically, OwnedFileOptions};
use super::super::{validate_owned_directory, validate_owned_file, ExpectedOwner, HelperPaths};
use hbb_common::{bail, ResultType};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::platform::macos::privileged_helper) enum MigrationPhase {
    Preparing,
    InstallingHelper,
    Prepared,
    Bootstrapping,
    RollingBack,
    RolledBack,
    RollbackFailed,
    Committed,
}

impl MigrationPhase {
    pub(in crate::platform::macos::privileged_helper) fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::InstallingHelper => "installing-helper",
            Self::Prepared => "prepared",
            Self::Bootstrapping => "bootstrapping",
            Self::RollingBack => "rolling-back",
            Self::RolledBack => "rolled-back",
            Self::RollbackFailed => "rollback-failed",
            Self::Committed => "committed",
        }
    }

    pub(in crate::platform::macos::privileged_helper) fn parse(value: &str) -> ResultType<Self> {
        match value {
            "preparing" => Ok(Self::Preparing),
            "installing-helper" => Ok(Self::InstallingHelper),
            "prepared" => Ok(Self::Prepared),
            "bootstrapping" => Ok(Self::Bootstrapping),
            "rolling-back" => Ok(Self::RollingBack),
            "rolled-back" => Ok(Self::RolledBack),
            "rollback-failed" => Ok(Self::RollbackFailed),
            "committed" => Ok(Self::Committed),
            _ => bail!("Unknown privileged helper migration phase: {value}"),
        }
    }
}

#[derive(Clone, Debug)]
pub(in crate::platform::macos::privileged_helper) struct MigrationPaths {
    pub(in crate::platform::macos::privileged_helper) helper: HelperPaths,
    pub(in crate::platform::macos::privileged_helper) staging_helper: HelperPaths,
    pub(in crate::platform::macos::privileged_helper) state_directory: PathBuf,
    pub(in crate::platform::macos::privileged_helper) phase_file: PathBuf,
    pub(in crate::platform::macos::privileged_helper) plist_backup: PathBuf,
    pub(in crate::platform::macos::privileged_helper) plist_expected: PathBuf,
    pub(in crate::platform::macos::privileged_helper) daemon_plist: PathBuf,
    pub(in crate::platform::macos::privileged_helper) daemon_label: String,
    pub(in crate::platform::macos::privileged_helper) daemon_socket: PathBuf,
}

impl MigrationPaths {
    pub(in crate::platform::macos::privileged_helper) fn for_roots(
        privileged_tools: &Path,
        launch_daemons: &Path,
        app_name: &str,
    ) -> ResultType<Self> {
        let helper = HelperPaths::for_privileged_tools_dir(privileged_tools, app_name)?;
        let state_directory =
            privileged_tools.join(format!(".com.carriez.{app_name}_service.migration"));
        let staging_root = state_directory.join("staging");
        Ok(Self {
            staging_helper: HelperPaths::for_privileged_tools_dir(&staging_root, app_name)?,
            phase_file: state_directory.join("phase"),
            plist_backup: state_directory.join("daemon.plist.backup"),
            plist_expected: state_directory.join("daemon.plist.expected"),
            daemon_plist: launch_daemons.join(format!("com.carriez.{app_name}_service.plist")),
            daemon_label: format!("com.carriez.{app_name}_service"),
            daemon_socket: PathBuf::from(format!("/tmp/{app_name}-service/ipc_service")),
            helper,
            state_directory,
        })
    }

    pub(super) fn expected_legacy_service(&self) -> ResultType<PathBuf> {
        let Some(app_name) = self
            .daemon_label
            .strip_prefix("com.carriez.")
            .and_then(|label| label.strip_suffix("_service"))
        else {
            bail!("Invalid privileged helper launchd label");
        };
        let gui = super::super::expected_gui_executable(app_name)?;
        let Some(executable_directory) = gui.parent() else {
            bail!("Expected GUI path has no executable directory");
        };
        Ok(executable_directory.join("service"))
    }
}

pub(in crate::platform::macos::privileged_helper) fn write_phase(
    paths: &MigrationPaths,
    phase: MigrationPhase,
    owner: ExpectedOwner,
) -> ResultType<()> {
    write_owned_atomically(
        &paths.phase_file,
        phase.as_str().as_bytes(),
        OwnedFileOptions { owner, mode: 0o600 },
    )
}

pub(in crate::platform::macos::privileged_helper) fn phase(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
) -> ResultType<MigrationPhase> {
    validate_owned_file(&paths.phase_file, owner, 0o600)?;
    let bytes = read_regular_file(&paths.phase_file)?;
    MigrationPhase::parse(std::str::from_utf8(&bytes)?)
}

pub(in crate::platform::macos::privileged_helper) fn validate_state(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
) -> ResultType<()> {
    validate_state_directory(paths, owner)?;
    validate_owned_file(&paths.plist_backup, owner, 0o600)?;
    validate_owned_file(&paths.plist_expected, owner, 0o600)
}

pub(in crate::platform::macos::privileged_helper) fn validate_state_directory(
    paths: &MigrationPaths,
    owner: ExpectedOwner,
) -> ResultType<()> {
    validate_owned_directory(&paths.state_directory, owner)?;
    let mode = std::fs::symlink_metadata(&paths.state_directory)?.mode() & 0o7777;
    if mode != 0o700 {
        bail!("Unexpected privileged helper migration directory mode: {mode:o}");
    }
    Ok(())
}
