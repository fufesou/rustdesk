use super::migration::{
    begin_rollback, complete_migration_with, prepare_migration, write_phase, MigrationPhase,
};
use super::*;
use std::os::unix::fs::{symlink, PermissionsExt};

mod recovery;
pub(super) mod support;
use support::*;

fn add_write_acl(path: &Path) {
    let status = std::process::Command::new("/bin/chmod")
        .args(["+a", "everyone allow write", path.to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn helper_validation_rejects_and_clears_extended_acl() {
    let tree = TestTree::new();
    add_write_acl(&tree.paths.bundle);
    assert!(validate_helper_tree(&tree.paths, tree.owner).is_err());
    let bundle = std::fs::File::open(&tree.paths.bundle).unwrap();
    clear_extended_acl(&bundle).unwrap();
    assert!(validate_helper_tree(&tree.paths, tree.owner).is_ok());

    add_write_acl(&tree.paths.privileged_tools);
    assert!(validate_privileged_tools_directory(&tree.paths.privileged_tools, tree.owner).is_err());
}

#[test]
fn helper_paths_use_protected_bundle() {
    let paths = HelperPaths::for_app_name("RustDesk").unwrap();
    assert_eq!(
        paths.service,
        Path::new("/Library/PrivilegedHelperTools/com.carriez.RustDesk_service.bundle/Contents/MacOS/service")
    );
    assert_eq!(
        paths.custom,
        Path::new("/Library/PrivilegedHelperTools/com.carriez.RustDesk_service.bundle/Contents/Resources/custom.txt")
    );
    let tree = TestTree::new();
    assert!(validate_helper_tree(&tree.paths, tree.owner).is_ok());
}

#[test]
fn helper_validation_rejects_symlink_and_non_regular_service() {
    let tree = TestTree::new();
    std::fs::remove_file(&tree.paths.service).unwrap();
    let target = tree.root.join("external-service");
    std::fs::write(&target, b"service").unwrap();
    symlink(&target, &tree.paths.service).unwrap();
    assert!(validate_helper_tree(&tree.paths, tree.owner).is_err());

    std::fs::remove_file(&tree.paths.service).unwrap();
    std::fs::create_dir(&tree.paths.service).unwrap();
    assert!(validate_helper_tree(&tree.paths, tree.owner).is_err());
}

#[test]
fn helper_validation_rejects_unsafe_mode_and_owner() {
    let tree = TestTree::new();
    std::fs::set_permissions(&tree.paths.service, std::fs::Permissions::from_mode(0o775)).unwrap();
    assert!(validate_helper_tree(&tree.paths, tree.owner).is_err());

    std::fs::set_permissions(&tree.paths.service, std::fs::Permissions::from_mode(0o755)).unwrap();
    let wrong_owner = ExpectedOwner {
        uid: tree.owner.uid.wrapping_add(1),
        gid: tree.owner.gid,
    };
    assert!(validate_helper_tree(&tree.paths, wrong_owner).is_err());
}

#[test]
fn privileged_tools_rejects_non_root_owner_when_read_only() {
    let tree = TestTree::new();
    let expected_root = ExpectedOwner {
        uid: tree.owner.uid.wrapping_add(1),
        gid: tree.owner.gid,
    };
    std::fs::set_permissions(
        &tree.paths.privileged_tools,
        std::fs::Permissions::from_mode(0o555),
    )
    .unwrap();
    assert!(
        validate_privileged_tools_directory(&tree.paths.privileged_tools, expected_root).is_err()
    );
}

#[test]
fn migration_commits_after_normal_or_interrupted_bootstrap() {
    for (interrupted, include_custom) in [(false, true), (true, false)] {
        let harness = MigrationHarness::new();
        let mut source = harness.source();
        source.custom = include_custom.then_some(harness.source_custom.as_path());
        prepare_migration(&harness.paths, &source, harness.owner).unwrap();
        if interrupted {
            write_phase(&harness.paths, MigrationPhase::Bootstrapping, harness.owner).unwrap();
        }
        let mut launchd = FakeLaunchd {
            ready: true,
            ..Default::default()
        };

        complete_migration_with(&harness.paths, harness.owner, &mut launchd).unwrap();
        assert_eq!(
            std::fs::read(&harness.paths.helper.service).unwrap(),
            b"new service"
        );
        assert_eq!(harness.paths.helper.custom.exists(), include_custom);
        if include_custom {
            assert_eq!(
                std::fs::read(&harness.paths.helper.custom).unwrap(),
                b"custom config"
            );
            let mode = std::fs::metadata(&harness.paths.helper.custom)
                .unwrap()
                .mode()
                & 0o7777;
            assert_eq!(mode, 0o600);
        }
        assert!(!harness.paths.state_directory.exists());
        assert_eq!(launchd.reload_calls, 1);
    }
}

#[test]
fn migration_bootstrap_failure_restores_old_plist() {
    let harness = MigrationHarness::new();
    prepare_migration(&harness.paths, &harness.source(), harness.owner).unwrap();
    let mut launchd = FakeLaunchd {
        fail_first_reload: true,
        ready: true,
        ..Default::default()
    };

    assert!(complete_migration_with(&harness.paths, harness.owner, &mut launchd).is_err());

    assert_eq!(
        std::fs::read(&harness.paths.daemon_plist).unwrap(),
        harness.old_daemon_plist.as_bytes()
    );
    assert!(!harness.paths.helper.bundle.exists());
    assert!(!harness.paths.state_directory.exists());
    assert_eq!(launchd.reload_calls, 2);
}

#[test]
fn migration_staging_rejects_symlink_service() {
    let harness = MigrationHarness::new();
    std::fs::remove_file(&harness.source_service).unwrap();
    let external = harness.root.join("external-service");
    std::fs::write(&external, b"replaced").unwrap();
    symlink(&external, &harness.source_service).unwrap();

    assert!(prepare_migration(&harness.paths, &harness.source(), harness.owner).is_err());
    assert!(!harness.paths.helper.bundle.exists());
}
