use super::{get_install_info, installer_shell::launch_elevated};
use hbb_common::{bail, log, ResultType};
use std::{io, path::Path};

const HELPER_FILENAME: &str = "rustdesk-service-stop.exe";

pub(super) fn run() -> ResultType<()> {
    let application = crate::get_app_name();
    crate::platform::validate_install_app_name(&application)?;
    let (_, _, _, installed_executable) = get_install_info();
    // Use the installed payload, as the existing native SCM command does.
    let helper = Path::new(&installed_executable).with_file_name(HELPER_FILENAME);
    if !helper.is_file() {
        bail!(
            "Native service-stop helper is missing at {}. Install diagnostic K before testing Stop service.",
            helper.display()
        );
    }
    let broker = super::WIN_TOPMOST_INJECTED_PROCESS_EXE;
    let parameters = format!("\"{application}\" \"{broker}\" {}", std::process::id());
    log::info!(
        "Diagnostic native service stop: installed helper {}; no CMD, BAT or installer pipe",
        helper.display()
    );
    let code = launch_elevated(&helper, &parameters, false)?.wait()?;
    log::info!("Native service-stop helper exited with code {code}");
    if code != 0 {
        bail!(
            "Native service-stop helper failed with code {code}: {}",
            io::Error::from_raw_os_error(code as i32)
        );
    }
    Ok(())
}
