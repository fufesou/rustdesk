use super::*;

struct PlistPairHarness {
    directory: std::path::PathBuf,
    daemon: std::path::PathBuf,
    agent: std::path::PathBuf,
}

impl PlistPairHarness {
    fn new(test_name: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "rustdesk-{test_name}-{}-{}",
            std::process::id(),
            hbb_common::rand::random::<u64>()
        ));
        std::fs::create_dir(&directory).unwrap();
        let daemon = directory.join("daemon.plist");
        let agent = directory.join("agent.plist");
        std::fs::write(&daemon, b"old daemon").unwrap();
        std::fs::write(&agent, b"old agent").unwrap();
        Self {
            directory,
            daemon,
            agent,
        }
    }

    fn definitions(&self) -> [PlistDefinition<'_>; 2] {
        [
            PlistDefinition {
                path: &self.daemon,
                body: b"new daemon",
            },
            PlistDefinition {
                path: &self.agent,
                body: b"new agent",
            },
        ]
    }

    fn assert_unchanged(&self) {
        assert_eq!(std::fs::read(&self.daemon).unwrap(), b"old daemon");
        assert_eq!(std::fs::read(&self.agent).unwrap(), b"old agent");
    }
}

impl Drop for PlistPairHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn second_plist_failure_restores_the_first_plist() {
    let harness = PlistPairHarness::new("plist-pair-test");
    let blocked_agent_stage = std::path::PathBuf::from(format!(
        "{}.tmp.{}",
        harness.agent.display(),
        std::process::id()
    ));
    std::fs::write(&blocked_agent_stage, b"block stage creation").unwrap();

    let result = write_plist_pair_atomically(harness.definitions());

    assert!(result.is_err());
    harness.assert_unchanged();
}

#[test]
fn agent_plist_symlink_is_rejected_before_daemon_write() {
    let harness = PlistPairHarness::new("plist-link-test");
    let target = harness.directory.join("target.plist");
    std::fs::write(&target, b"unrelated").unwrap();
    std::fs::remove_file(&harness.agent).unwrap();
    std::os::unix::fs::symlink(&target, &harness.agent).unwrap();

    let result = write_plist_pair_atomically(harness.definitions());

    assert!(result.is_err());
    assert_eq!(std::fs::read(&harness.daemon).unwrap(), b"old daemon");
    assert_eq!(std::fs::read(target).unwrap(), b"unrelated");
}

#[test]
fn post_rename_error_restores_both_plists() {
    let harness = PlistPairHarness::new("plist-post-rename-test");
    let mut writes = 0;

    let result = write_plist_pair_with(harness.definitions(), |path, body| {
        writes += 1;
        std::fs::write(path, body)?;
        if writes == 2 {
            bail!("injected post-rename failure");
        }
        Ok(())
    });

    assert!(result.is_err());
    harness.assert_unchanged();
}
