use super::super::migration::{phase, LaunchdControl};
use super::super::runtime::recover_protected_preparation;
use super::*;
use hbb_common::{bail, ResultType};
use std::path::Path;

#[test]
fn interrupted_helper_install_restores_plist_and_removes_helper() {
    let harness = MigrationHarness::new();
    prepare_migration(&harness.paths, &harness.source(), harness.owner).unwrap();
    write_phase(
        &harness.paths,
        MigrationPhase::InstallingHelper,
        harness.owner,
    )
    .unwrap();

    assert!(prepare_migration(&harness.paths, &harness.source(), harness.owner).is_err());
    assert_eq!(
        std::fs::read(&harness.paths.daemon_plist).unwrap(),
        harness.old_daemon_plist.as_bytes()
    );
    assert!(!harness.paths.helper.bundle.exists());
    assert!(!harness.paths.state_directory.exists());
}

#[test]
fn rollback_intent_is_persisted_before_restoring_the_old_plist() {
    let harness = MigrationHarness::new();
    prepare_migration(&harness.paths, &harness.source(), harness.owner).unwrap();
    write_phase(&harness.paths, MigrationPhase::Bootstrapping, harness.owner).unwrap();
    std::fs::remove_file(&harness.paths.plist_backup).unwrap();

    assert!(begin_rollback(&harness.paths, harness.owner).is_err());
    assert_eq!(
        std::fs::read(&harness.paths.phase_file).unwrap(),
        MigrationPhase::RollingBack.as_str().as_bytes()
    );
}

#[derive(Default)]
struct CompatibilityRollbackLaunchd {
    reload_calls: usize,
    expected_executables: Vec<std::path::PathBuf>,
}

impl LaunchdControl for CompatibilityRollbackLaunchd {
    fn reload(&mut self, _label: &str, _plist: &Path) -> ResultType<()> {
        self.reload_calls += 1;
        if self.reload_calls == 1 {
            bail!("injected initial reload failure");
        }
        Ok(())
    }

    fn is_expected_ready(
        &mut self,
        _label: &str,
        _socket: &Path,
        expected_executable: &Path,
    ) -> ResultType<bool> {
        self.expected_executables
            .push(expected_executable.to_owned());
        Ok(true)
    }
}

fn preparing_harness() -> MigrationHarness {
    let harness = MigrationHarness::new();
    prepare_migration(&harness.paths, &harness.source(), harness.owner).unwrap();
    write_phase(&harness.paths, MigrationPhase::Preparing, harness.owner).unwrap();
    harness
}

fn assert_preparation_rejected(change: impl FnOnce(&MigrationHarness)) {
    let harness = preparing_harness();
    change(&harness);

    assert!(recover_protected_preparation(&harness.paths, harness.owner).is_err());
    assert_eq!(
        phase(&harness.paths, harness.owner).unwrap(),
        MigrationPhase::Preparing
    );
}

#[test]
fn interrupted_compatibility_helper_is_reused_when_bytes_match() {
    let harness = MigrationHarness::new();
    harness.install_helper();

    prepare_migration(&harness.paths, &harness.source(), harness.owner).unwrap();

    assert_eq!(
        phase(&harness.paths, harness.owner).unwrap(),
        MigrationPhase::Prepared
    );
}

#[test]
fn compatibility_rollback_uses_the_plist_program_not_exact_bytes() {
    for plist_suffix in ["", "<key>KeepAlive</key><true/>"] {
        let harness = MigrationHarness::new();
        harness.install_helper();
        let plist = harness
            .new_daemon_plist
            .replace("</dict>", &format!("{plist_suffix}</dict>"));
        std::fs::write(&harness.paths.daemon_plist, &plist).unwrap();
        prepare_migration(&harness.paths, &harness.source(), harness.owner).unwrap();
        let mut launchd = CompatibilityRollbackLaunchd::default();

        assert!(complete_migration_with(&harness.paths, harness.owner, &mut launchd).is_err());
        assert_eq!(
            launchd.expected_executables,
            vec![harness.paths.helper.service.clone()]
        );
        assert!(harness.paths.helper.bundle.exists());
        assert_eq!(
            std::fs::read(&harness.paths.daemon_plist).unwrap(),
            plist.as_bytes()
        );
        assert!(!harness.paths.state_directory.exists());
    }
}

#[test]
fn mismatched_compatibility_helper_is_rejected() {
    let harness = MigrationHarness::new();
    harness.install_helper();
    std::fs::write(&harness.paths.helper.service, b"unexpected service").unwrap();

    assert!(prepare_migration(&harness.paths, &harness.source(), harness.owner).is_err());
    assert!(!harness.paths.state_directory.exists());
    assert!(prepare_migration(&harness.paths, &harness.source(), harness.owner).is_err());
    assert_eq!(
        std::fs::read(&harness.paths.helper.service).unwrap(),
        b"unexpected service"
    );
}

#[test]
fn rolled_back_finalizer_accepts_an_already_removed_helper() {
    let harness = MigrationHarness::new();
    prepare_migration(&harness.paths, &harness.source(), harness.owner).unwrap();
    std::fs::write(&harness.paths.daemon_plist, &harness.old_daemon_plist).unwrap();
    write_phase(&harness.paths, MigrationPhase::RolledBack, harness.owner).unwrap();
    std::fs::remove_dir_all(&harness.paths.helper.bundle).unwrap();
    let mut launchd = FakeLaunchd::default();

    complete_migration_with(&harness.paths, harness.owner, &mut launchd).unwrap();

    assert_eq!(launchd.reload_calls, 0);
    assert!(!harness.paths.state_directory.exists());
}

#[test]
fn protected_start_promotes_interrupted_post_plist_preparation() {
    let harness = preparing_harness();

    recover_protected_preparation(&harness.paths, harness.owner).unwrap();

    assert_eq!(
        phase(&harness.paths, harness.owner).unwrap(),
        MigrationPhase::Prepared
    );
}

#[test]
fn protected_start_rejects_preparation_with_unexpected_plist() {
    assert_preparation_rejected(|harness| {
        std::fs::write(&harness.paths.daemon_plist, b"unexpected plist").unwrap();
    });
}

#[test]
fn protected_start_rejects_preparation_with_unsafe_plist_mode() {
    assert_preparation_rejected(|harness| {
        std::fs::set_permissions(
            &harness.paths.daemon_plist,
            std::fs::Permissions::from_mode(0o666),
        )
        .unwrap();
    });
}
