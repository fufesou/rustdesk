use super::*;
use crate::platform::macos::privileged_helper::migration::{phase, MigrationPhase};
use crate::platform::macos::privileged_helper::tests::support::MigrationHarness;
use hbb_common::bail;
use std::os::unix::fs::PermissionsExt;

fn legacy_migration(harness: &MigrationHarness) -> LegacyPlistMigration<'_> {
    LegacyPlistMigration {
        paths: &harness.paths,
        source: harness.source(),
        owner: harness.owner,
    }
}

#[test]
fn legacy_plist_write_leaves_a_resumable_prepared_migration() {
    let harness = MigrationHarness::new();

    with_legacy_helper_for_plist_write(legacy_migration(&harness), || {
        assert_eq!(
            std::fs::read(&harness.paths.helper.service).unwrap(),
            b"new service"
        );
        Ok(())
    })
    .unwrap();

    assert_eq!(
        phase(&harness.paths, harness.owner).unwrap(),
        MigrationPhase::Prepared
    );
    assert_eq!(
        std::fs::read(&harness.paths.daemon_plist).unwrap(),
        harness.new_daemon_plist.as_bytes()
    );
}

#[test]
fn plist_write_or_validation_failure_rolls_back_the_migration() {
    for corrupt_helper in [false, true] {
        let harness = MigrationHarness::new();
        let result = with_legacy_helper_for_plist_write(legacy_migration(&harness), || {
            if !corrupt_helper {
                bail!("injected plist write failure");
            }
            std::fs::set_permissions(
                &harness.paths.helper.service,
                std::fs::Permissions::from_mode(0o777),
            )?;
            Ok(())
        });

        assert!(result.is_err());
        assert_eq!(
            std::fs::read(&harness.paths.daemon_plist).unwrap(),
            harness.old_daemon_plist.as_bytes()
        );
        assert!(!harness.paths.helper.bundle.exists());
        assert!(!harness.paths.state_directory.exists());
    }
}

#[test]
fn plist_writer_rejects_non_root_uid() {
    assert!(validate_plist_writer_uid(ROOT_UID.saturating_add(1)).is_err());
}
