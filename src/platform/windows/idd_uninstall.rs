pub(crate) fn uninstall(reboot: &mut bool) -> hbb_common::ResultType<()> {
    let (_, _, _, installed_exe) = super::get_install_info();
    virtual_display::uninstall_test_driver_for(std::path::Path::new(&installed_exe), reboot)
}

pub(super) fn before_service_removal(
    service: &str,
    stop_processes: &str,
) -> hbb_common::ResultType<String> {
    use windows::Win32::Foundation::{ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_NOT_ACTIVE};

    let current = std::env::current_exe()?;
    let current = super::installer_shell::path_for_cmd_environment(&current)?;
    // NET waits for shutdown. On failure SC distinguishes an inactive or missing service
    // from an error that must abort the uninstall.
    Ok(format!(
        r#"set "RUSTDESK_IDD_RESTART_SERVICE="
net.exe stop "{service}" <nul
if not errorlevel 1 goto rustdesk_idd_stopped_service
sc.exe stop "{service}"
if "%errorlevel%"=="{missing}" goto rustdesk_idd_remove
if "%errorlevel%"=="{inactive}" goto rustdesk_idd_remove
if errorlevel 1 exit /b %errorlevel%
:rustdesk_idd_stopped_service
set "RUSTDESK_IDD_RESTART_SERVICE=1"
:rustdesk_idd_remove
{stop_processes}
start "" /b /wait "{current}" --uninstall-rustdesk-idd
if errorlevel 1 (
    if defined RUSTDESK_IDD_RESTART_SERVICE net.exe start "{service}"
    exit /b 1
)"#,
        missing = ERROR_SERVICE_DOES_NOT_EXIST.0,
        inactive = ERROR_SERVICE_NOT_ACTIVE.0,
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn idd_uninstall_can_abort_before_removing_the_native_installation() {
        let script = super::super::get_before_uninstall(true, true).unwrap();
        let (stopping, cleanup) = script.split_once("--uninstall-rustdesk-idd").unwrap();
        for command in [
            "sc delete ",
            "--service-control remove ",
            "reg delete ",
            "netsh ",
        ] {
            assert!(!stopping.contains(command), "{command}");
        }
        let (failure, teardown) = cleanup.split_once("exit /b 1").unwrap();
        assert!(failure.contains("if errorlevel 1 ("));
        assert!(failure.contains("net.exe start "));
        assert!(teardown.contains("sc delete ") || teardown.contains("--service-control remove "));
        assert!(teardown.contains("reg delete HKEY_CLASSES_ROOT"));
        assert!(teardown.contains("netsh advfirewall firewall delete"));
    }
}
