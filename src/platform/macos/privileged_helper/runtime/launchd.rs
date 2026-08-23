use super::super::migration::LaunchdControl;
use super::super::ROOT_UID;
use hbb_common::{bail, log, ResultType};
use std::ffi::OsStr;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::process::{Command, ExitStatus};

const READINESS_ATTEMPTS: usize = 30;
const READINESS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const STOP_ATTEMPTS: usize = 30;

pub(super) struct SystemLaunchd;

impl LaunchdControl for SystemLaunchd {
    fn reload(&mut self, label: &str, plist: &Path) -> ResultType<()> {
        let target = format!("system/{label}");
        let stopping_pid = launchd_pid(&target)?;
        let bootout = launchctl_status(&[OsStr::new("bootout"), OsStr::new(&target)])?;
        if !bootout.success() {
            let unload =
                launchctl_status(&[OsStr::new("unload"), OsStr::new("-w"), plist.as_os_str()])?;
            if !unload.success() {
                log::warn!(
                    "launchctl could not stop existing helper job: bootout={}, unload={}",
                    bootout,
                    unload
                );
            }
        }
        if !wait_for_job_stop(&target, stopping_pid)? {
            bail!("launchctl did not stop the previous helper job");
        }
        let bootstrap = launchctl_status(&[
            OsStr::new("bootstrap"),
            OsStr::new("system"),
            plist.as_os_str(),
        ])?;
        if bootstrap.success() {
            return Ok(());
        }
        let load = launchctl_status(&[OsStr::new("load"), OsStr::new("-w"), plist.as_os_str()])?;
        if !load.success() {
            bail!("launchctl reload failed: bootstrap={bootstrap}, load={load}");
        }
        Ok(())
    }

    fn is_expected_ready(
        &mut self,
        label: &str,
        socket: &Path,
        expected_executable: &Path,
    ) -> ResultType<bool> {
        let expected_executable = std::fs::canonicalize(expected_executable)?;
        wait_for_ready(label, socket, &expected_executable)
    }
}

fn wait_for_ready(label: &str, socket: &Path, expected_executable: &Path) -> ResultType<bool> {
    for _ in 0..READINESS_ATTEMPTS {
        if let Some(pid) = launchd_ready_pid(label, socket, expected_executable)? {
            std::thread::sleep(READINESS_INTERVAL);
            if launchd_ready_pid(label, socket, expected_executable)? == Some(pid) {
                return Ok(true);
            }
        } else {
            std::thread::sleep(READINESS_INTERVAL);
        }
    }
    Ok(false)
}

fn launchctl_status(args: &[&OsStr]) -> ResultType<ExitStatus> {
    Ok(Command::new("/bin/launchctl").args(args).status()?)
}

fn launchd_output_is_running(output: &[u8]) -> bool {
    String::from_utf8_lossy(output)
        .lines()
        .any(|line| line.trim() == "state = running")
}

fn launchd_output_ready_pid(output: &[u8]) -> Option<u32> {
    if !launchd_output_is_running(output) {
        return None;
    }
    String::from_utf8_lossy(output).lines().find_map(|line| {
        line.trim()
            .strip_prefix("pid = ")
            .and_then(|pid| pid.parse().ok())
    })
}

fn launchd_ready_pid(
    label: &str,
    socket: &Path,
    expected_executable: &Path,
) -> ResultType<Option<u32>> {
    let target = format!("system/{label}");
    let output = Command::new("/bin/launchctl")
        .args(["print", &target])
        .output()?;
    if !output.status.success() || !path_is_socket(socket, ROOT_UID) {
        return Ok(None);
    }
    let Some(pid) = launchd_output_ready_pid(&output.stdout) else {
        return Ok(None);
    };
    let Ok(pid_for_signal) = i32::try_from(pid) else {
        return Ok(None);
    };
    if unsafe { hbb_common::libc::kill(pid_for_signal, 0) } != 0 {
        return Ok(None);
    }
    if process_executable_path(pid)?.as_deref() != Some(expected_executable) {
        return Ok(None);
    }
    Ok(Some(pid))
}

fn launchd_pid(target: &str) -> ResultType<Option<u32>> {
    let output = Command::new("/bin/launchctl")
        .args(["print", target])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = ")?.parse().ok()))
}

fn wait_for_job_stop(target: &str, stopping_pid: Option<u32>) -> ResultType<bool> {
    for _ in 0..STOP_ATTEMPTS {
        let captured_pid_gone = match stopping_pid {
            Some(pid) => i32::try_from(pid)
                .map(|pid| unsafe { hbb_common::libc::kill(pid, 0) } != 0)
                .unwrap_or(true),
            None => true,
        };
        if captured_pid_gone && launchd_pid(target)?.is_none() {
            return Ok(true);
        }
        std::thread::sleep(READINESS_INTERVAL);
    }
    Ok(false)
}

fn process_executable_path(pid: u32) -> ResultType<Option<std::path::PathBuf>> {
    use hbb_common::libc;
    use std::os::unix::ffi::OsStringExt;

    let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let length = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr().cast(),
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return Ok(None);
    }
    buffer.truncate(length as usize);
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(buffer));
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn path_is_socket(path: &Path, expected_uid: u32) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    metadata.file_type().is_socket()
        && metadata.uid() == expected_uid
        && std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_readiness_requires_a_live_socket_with_the_expected_owner() {
        let path = std::env::temp_dir().join(format!(
            "rustdesk-readiness-socket-{}-{}",
            std::process::id(),
            hbb_common::rand::random::<u64>()
        ));
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let current_uid = unsafe { hbb_common::libc::geteuid() as u32 };
        let other_uid = if current_uid == 0 { 1 } else { 0 };

        assert!(path_is_socket(&path, current_uid));
        assert!(!path_is_socket(&path, other_uid));
        let _ = listener.accept().unwrap();

        drop(listener);
        std::thread::sleep(READINESS_INTERVAL);
        assert!(!path_is_socket(&path, current_uid));
        std::fs::remove_file(path).unwrap();
    }
}
