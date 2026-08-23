use super::super::tests::support::MigrationHarness;
use super::*;

const PROTECTED_HELPER: &str =
    "/Library/PrivilegedHelperTools/com.carriez.RustDesk_service.bundle/Contents/MacOS/service";
const LEGACY_HELPER: &str = "/Applications/RustDesk.app/Contents/MacOS/service";
const WRONG_HELPER: &str =
    "/Library/PrivilegedHelperTools/com.carriez.Other_service.bundle/Contents/MacOS/service";

#[derive(Default)]
struct NeverLaunchd {
    calls: usize,
}

impl super::super::migration::LaunchdControl for NeverLaunchd {
    fn reload(&mut self, _label: &str, _plist: &Path) -> ResultType<()> {
        self.calls += 1;
        bail!("unexpected reload")
    }

    fn is_expected_ready(
        &mut self,
        _label: &str,
        _socket: &Path,
        _expected_executable: &Path,
    ) -> ResultType<bool> {
        self.calls += 1;
        bail!("unexpected readiness check")
    }
}

#[test]
fn migration_waiter_accepts_state_cleanup_between_checks() {
    let harness = MigrationHarness::new();
    let state_error = Err(hbb_common::anyhow::anyhow!("migration state was removed"));

    assert!(migration_state_step::<()>(&harness.paths, state_error)
        .unwrap()
        .is_none());
}

#[test]
fn finalizer_identity_requires_root_and_exact_protected_helper() {
    let helper = Path::new(PROTECTED_HELPER);
    let legacy = Path::new(LEGACY_HELPER);

    assert!(validate_finalizer_identity(helper, helper, 0).is_ok());
    assert!(validate_finalizer_identity(helper, helper, 501).is_err());
    assert!(validate_finalizer_identity(legacy, helper, 0).is_err());
    assert!(validate_finalizer_identity(Path::new(WRONG_HELPER), helper, 0).is_err());
}

#[test]
fn service_identity_accepts_only_root_legacy_or_protected_paths() {
    let protected = Path::new(PROTECTED_HELPER);
    let legacy = Path::new(LEGACY_HELPER);
    for (current, uid, expected) in [
        (protected, 0, Some(ServiceIdentity::Protected)),
        (legacy, 0, Some(ServiceIdentity::Legacy)),
        (protected, 501, None),
        (Path::new("/tmp/service"), 0, None),
    ] {
        let actual = classify_service_identity(
            ServiceExecutables {
                current,
                protected,
                legacy,
            },
            uid,
        );
        assert_eq!(actual.ok(), expected);
    }
}

#[test]
fn finalizer_lock_serializes_migration_completion() {
    let harness = MigrationHarness::new();
    harness.write_state(MigrationPhase::Preparing);

    let first = FinalizerLock::acquire(&harness.paths.state_directory, harness.owner).unwrap();
    let second_directory = harness.paths.state_directory.clone();
    let owner = harness.owner;
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let result = FinalizerLock::acquire(&second_directory, owner);
        sender.send(result.is_ok()).unwrap();
    });
    assert!(receiver
        .recv_timeout(std::time::Duration::from_millis(50))
        .is_err());
    drop(first);
    assert!(receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap());
    waiter.join().unwrap();
}

#[test]
fn absent_migration_state_requires_an_installed_helper() {
    let harness = MigrationHarness::new();

    assert!(migration_is_complete(&harness.paths, harness.owner).is_err());
    harness.install_helper();
    assert!(migration_is_complete(&harness.paths, harness.owner).unwrap());
}

#[test]
fn rolled_back_migration_state_is_an_explicit_error() {
    let harness = MigrationHarness::new();
    harness.write_state(MigrationPhase::RolledBack);

    assert!(migration_is_complete(&harness.paths, harness.owner).is_err());
}

#[test]
fn migration_ipc_modes_gate_business_and_rollback_readiness() {
    use super::super::ServiceIpcMode;

    assert_eq!(
        legacy_pending_action(MigrationPhase::RollingBack),
        Some(ServiceStartAction::StartForRollbackReadiness)
    );
    assert_eq!(legacy_pending_action(MigrationPhase::Prepared), None);
    let readiness = ServiceIpcMode::MigrationReadiness;
    assert!(!readiness.protected_ipc_enabled());
    assert_eq!(
        readiness.after_migration_completion(),
        Some(ServiceIpcMode::ProtectedOnly)
    );
    assert!(ServiceIpcMode::ProtectedOnly.protected_ipc_enabled());
    assert_eq!(
        ServiceIpcMode::LegacyRollback.after_migration_completion(),
        None
    );
}

#[test]
fn finalizer_child_uses_a_separate_process_group() {
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg("10");
    detach_finalizer_process_group(&mut command);
    let mut child = command.spawn().unwrap();
    let child_pid = child.id() as i32;

    let child_group = unsafe { hbb_common::libc::getpgid(child_pid) };
    assert_eq!(child_group, child_pid);
    assert_ne!(child_group, unsafe { hbb_common::libc::getpgrp() });

    child.kill().unwrap();
    child.wait().unwrap();
}

#[test]
fn duplicate_finalizer_accepts_an_already_cleaned_state() {
    let harness = MigrationHarness::new();
    let mut launchd = NeverLaunchd::default();

    complete_existing_migration(&harness.paths, harness.owner, &mut launchd).unwrap();

    assert_eq!(launchd.calls, 0);
}
