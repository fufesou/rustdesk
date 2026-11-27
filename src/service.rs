use librustdesk::*;

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    crate::common::load_custom_client();
    if args.len() > 1 && args[1] == "--install-daemon-config" {
        if let Err(err) = install_daemon_config(&args[2..]) {
            eprintln!("Failed to install daemon configuration: {err}");
            std::process::exit(1);
        }
        return;
    }
    if args.len() > 1 && args[1] == "--write-plists" {
        if let Err(e) = librustdesk::platform::write_plists() {
            eprintln!("Failed to write plists: {}", e);
            std::process::exit(1);
        }
        std::process::exit(0);
    }
    hbb_common::init_log(false, "service");
    crate::start_os_service();
}

#[cfg(target_os = "macos")]
fn install_daemon_config(args: &[String]) -> hbb_common::ResultType<()> {
    let [user, path] = args else {
        hbb_common::bail!("Expected the login user and configuration path");
    };
    crate::common::init_sodium();
    hbb_common::config::unix_unlock_pin::install_daemon_config(|| {
        match std::fs::metadata(path) {
            Ok(file) if file.is_file() => (),
            Ok(_) => return Ok(String::new()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
            Err(err) => return Err(err.into()),
        }
        read_user_settings(
            std::process::Command::new("/usr/bin/sudo")
                .args(["-n", "-u", user, "--", "/bin/cat", path]),
        )
    })
}

#[cfg(any(target_os = "macos", all(test, unix)))]
fn read_user_settings(reader: &mut std::process::Command) -> hbb_common::ResultType<String> {
    let output = reader.output()?;
    hbb_common::anyhow::ensure!(
        output.status.success(),
        "Could not read user settings ({})",
        output.status
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn incomplete_user_settings_are_discarded() {
        let mut reader = std::process::Command::new("/bin/sh");
        reader.args(["-c", "printf 'serial = 9\\n'; exit 7"]);
        assert!(read_user_settings(&mut reader).is_err());
    }
}
