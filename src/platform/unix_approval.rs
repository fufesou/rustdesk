use hbb_common::{
    anyhow::{anyhow, ensure},
    ResultType,
};
use std::process::Command;

#[cfg(target_os = "macos")]
const ADMIN_SCRIPT: &str = r#"on run argv
    set cmd to quoted form of (item 1 of argv)
    repeat with arg in rest of argv
        set cmd to cmd & " " & quoted form of (contents of arg)
    end repeat
    do shell script cmd with administrator privileges
end run"#;

pub(crate) fn request(command: &str, target: u32, digest: &[u8; 32]) -> ResultType<()> {
    let exe = std::env::current_exe()?;
    let exe = exe
        .to_str()
        .ok_or_else(|| anyhow!("Invalid executable path"))?;
    let target = target.to_string();
    let digest = hex::encode(digest);
    #[cfg(target_os = "linux")]
    let status = Command::new(exe)
        .args(["-gtk-sudo", "credential", exe, command, &target, &digest])
        .status()?;
    #[cfg(target_os = "macos")]
    let status = Command::new("/usr/bin/osascript")
        .args(["-e", ADMIN_SCRIPT, exe, command, &target, &digest])
        .status()?;
    ensure!(status.success(), "Authentication Required");
    Ok(())
}

#[cfg(all(target_os = "macos", feature = "flutter"))]
pub(crate) fn approve_offline_security_settings() -> ResultType<bool> {
    ensure!(crate::platform::is_installed(), "Authentication Required");
    let probe = offline_pin_command()?;
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", ADMIN_SCRIPT, "/usr/bin/env", "-i"])
        .arg(probe.get_program())
        .args(probe.get_args())
        .current_dir("/")
        .output()?;
    ensure!(
        output.status.success(),
        "Offline PIN check failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(std::str::from_utf8(&output.stdout)?.trim().parse()?)
}

#[cfg(all(target_os = "macos", any(feature = "flutter", test)))]
const OFFLINE_PIN_CHECK: &str = include_str!("privileges_scripts/offline_pin.pl");

#[cfg(all(target_os = "macos", any(feature = "flutter", test)))]
fn offline_pin_command() -> ResultType<Command> {
    use hbb_common::config::unix_unlock_pin::RootPin;

    // Only system code and the embedded reader run under administrator approval.
    let mut probe = Command::new("/usr/bin/perl");
    probe.args([
        "-f",
        "-e",
        OFFLINE_PIN_CHECK,
        "-e",
        "print root_pin_clear(@ARGV) ? 'true' : 'false';",
        "--",
    ]);
    probe.args(RootPin::offline_config_names()?);
    Ok(probe)
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use hbb_common::config::{store_path, unix_unlock_pin::RootPin};
    use std::{fs, path::Path};

    fn set_fixture_acl(path: &Path, args: &[&str]) -> ResultType<()> {
        let status = Command::new("/bin/chmod").args(args).arg(path).status()?;
        ensure!(status.success(), "Could not set the fixture ACL");
        Ok(())
    }

    #[test]
    fn offline_pin_check_uses_only_a_system_executable() -> ResultType<()> {
        let mut probe = offline_pin_command()?;
        assert_eq!(probe.get_program(), "/usr/bin/perl");
        if unsafe { hbb_common::libc::geteuid() } != 0 {
            let output = probe.env_clear().current_dir("/").output()?;
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(String::from_utf8_lossy(&output.stderr)
                .contains("Administrator authorization required"));
        }
        Ok(())
    }

    #[test]
    fn offline_check_recognizes_only_a_confirmed_empty_profile() -> ResultType<()> {
        let home = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&home)?;
        let [profile, authority_name, legacy_name] = RootPin::offline_config_names()?;
        let dir = home.join("Library/Preferences").join(&profile);
        let check = || -> ResultType<bool> {
            let output = Command::new("/usr/bin/perl")
                .env_clear()
                .args([
                    "-f",
                    "-e",
                    OFFLINE_PIN_CHECK,
                    "-e",
                    r"my $home = shift; print offline_pin_clear($home, \@ARGV) ? 'true' : 'false';",
                    "--",
                ])
                .arg(&home)
                .args([&profile, &authority_name, &legacy_name])
                .output()?;
            ensure!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(std::str::from_utf8(&output.stdout)?.parse()?)
        };
        assert!(check()?);
        assert!(!dir.exists());
        fs::create_dir_all(&dir)?;
        let legacy = dir.join(&legacy_name);
        fs::write(&legacy, "unlock_pin = 'legacy'\n")?;
        assert!(!check()?);
        let file = dir.join(&authority_name);
        store_path(file.clone(), RootPin::default())?;
        set_fixture_acl(&file, &["+a", "everyone deny delete"])?;
        assert!(check()?);
        set_fixture_acl(&file, &["+a", "everyone allow write"])?;
        assert!(check().is_err());
        set_fixture_acl(&file, &["-N"])?;
        fs::write(&file, "unlock_pin = 'set'\n")?;
        assert!(!check()?);
        fs::write(&file, "unlock_pin = ''\n[")?;
        assert!(!check()?);
        assert_eq!(fs::read_to_string(legacy)?, "unlock_pin = 'legacy'\n");
        fs::remove_dir_all(home)?;
        Ok(())
    }
}
