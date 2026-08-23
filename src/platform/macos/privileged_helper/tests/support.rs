use super::super::migration::{
    write_phase, LaunchdControl, MigrationPaths, MigrationPhase, MigrationSource,
};
use super::super::{ExpectedOwner, HelperPaths};
use hbb_common::{bail, ResultType};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) struct TestTree {
    pub(super) root: PathBuf,
    pub(super) paths: HelperPaths,
    pub(super) owner: ExpectedOwner,
}

impl TestTree {
    pub(super) fn new() -> Self {
        let requested_root = std::env::temp_dir().join(format!(
            "rustdesk-privileged-helper-test-{}-{}",
            std::process::id(),
            hbb_common::rand::random::<u64>()
        ));
        std::fs::create_dir_all(&requested_root).unwrap();
        let root = std::fs::canonicalize(requested_root).unwrap();
        let privileged_tools = root.join("PrivilegedHelperTools");
        let paths = HelperPaths::for_privileged_tools_dir(&privileged_tools, "RustDesk").unwrap();
        std::fs::create_dir_all(&paths.macos).unwrap();
        std::fs::create_dir_all(&paths.resources).unwrap();
        std::fs::write(&paths.service, b"service").unwrap();
        std::fs::set_permissions(&paths.service, std::fs::Permissions::from_mode(0o755)).unwrap();
        for directory in [
            &paths.privileged_tools,
            &paths.bundle,
            &paths.contents,
            &paths.macos,
            &paths.resources,
        ] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let metadata = std::fs::metadata(&paths.bundle).unwrap();
        Self {
            root,
            paths,
            owner: ExpectedOwner {
                uid: metadata.uid(),
                gid: metadata.gid(),
            },
        }
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(in crate::platform::macos::privileged_helper) struct MigrationHarness {
    pub(super) root: PathBuf,
    pub(in crate::platform::macos::privileged_helper) paths: MigrationPaths,
    pub(in crate::platform::macos::privileged_helper) owner: ExpectedOwner,
    pub(super) source_service: PathBuf,
    pub(super) source_custom: PathBuf,
    pub(in crate::platform::macos::privileged_helper) old_daemon_plist: String,
    pub(in crate::platform::macos::privileged_helper) new_daemon_plist: String,
}

impl MigrationHarness {
    pub(in crate::platform::macos::privileged_helper) fn new() -> Self {
        let requested_root = std::env::temp_dir().join(format!(
            "rustdesk-helper-migration-test-{}-{}",
            std::process::id(),
            hbb_common::rand::random::<u64>()
        ));
        std::fs::create_dir_all(&requested_root).unwrap();
        let root = std::fs::canonicalize(requested_root).unwrap();
        let privileged_tools = root.join("PrivilegedHelperTools");
        let launch_daemons = root.join("LaunchDaemons");
        std::fs::create_dir(&privileged_tools).unwrap();
        std::fs::create_dir(&launch_daemons).unwrap();
        for directory in [&privileged_tools, &launch_daemons] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let metadata = std::fs::metadata(&privileged_tools).unwrap();
        let owner = ExpectedOwner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        };
        let paths =
            MigrationPaths::for_roots(&privileged_tools, &launch_daemons, "RustDesk").unwrap();
        let old_daemon_plist = daemon_plist(Path::new(
            "/Applications/RustDesk.app/Contents/MacOS/service",
        ));
        let new_daemon_plist = daemon_plist(&paths.helper.service);
        std::fs::write(&paths.daemon_plist, &old_daemon_plist).unwrap();
        std::fs::set_permissions(&paths.daemon_plist, std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let source_directory = root.join("source");
        std::fs::create_dir(&source_directory).unwrap();
        let source_service = source_directory.join("service");
        let source_custom = source_directory.join("custom.txt");
        std::fs::write(&source_service, b"new service").unwrap();
        std::fs::write(&source_custom, b"custom config").unwrap();
        Self {
            root,
            paths,
            owner,
            source_service,
            source_custom,
            old_daemon_plist,
            new_daemon_plist,
        }
    }

    pub(in crate::platform::macos::privileged_helper) fn source(&self) -> MigrationSource<'_> {
        MigrationSource {
            service: &self.source_service,
            custom: Some(&self.source_custom),
            daemon_plist_body: &self.new_daemon_plist,
        }
    }

    pub(in crate::platform::macos::privileged_helper) fn install_helper(&self) {
        for directory in [
            &self.paths.helper.bundle,
            &self.paths.helper.contents,
            &self.paths.helper.macos,
            &self.paths.helper.resources,
        ] {
            std::fs::create_dir(directory).unwrap();
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::copy(&self.source_service, &self.paths.helper.service).unwrap();
        std::fs::copy(&self.source_custom, &self.paths.helper.custom).unwrap();
        std::fs::set_permissions(
            &self.paths.helper.service,
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::set_permissions(
            &self.paths.helper.custom,
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }

    pub(in crate::platform::macos::privileged_helper) fn write_state(
        &self,
        migration_phase: MigrationPhase,
    ) {
        std::fs::create_dir(&self.paths.state_directory).unwrap();
        std::fs::set_permissions(
            &self.paths.state_directory,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        for path in [&self.paths.plist_backup, &self.paths.plist_expected] {
            std::fs::write(path, b"plist").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        write_phase(&self.paths, migration_phase, self.owner).unwrap();
    }
}

fn daemon_plist(program: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\n\
         <key>ProgramArguments</key><array><string>{}</string></array>\n\
         </dict></plist>\n",
        program.display()
    )
}

impl Drop for MigrationHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
pub(super) struct FakeLaunchd {
    pub(super) reload_calls: usize,
    pub(super) fail_first_reload: bool,
    pub(super) ready: bool,
}

impl LaunchdControl for FakeLaunchd {
    fn reload(&mut self, _label: &str, _plist: &Path) -> ResultType<()> {
        self.reload_calls += 1;
        if self.fail_first_reload && self.reload_calls == 1 {
            bail!("injected bootstrap failure");
        }
        Ok(())
    }

    fn is_expected_ready(
        &mut self,
        _label: &str,
        _socket: &Path,
        _expected_executable: &Path,
    ) -> ResultType<bool> {
        Ok(self.ready)
    }
}
