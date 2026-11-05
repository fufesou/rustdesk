use super::{get_install_info, installer_shell::launch_elevated};
use hbb_common::{anyhow::anyhow, bail, log, ResultType};
use std::{io, path::Path};

pub(super) fn run() -> ResultType<()> {
    let application = crate::get_app_name();
    crate::platform::validate_install_app_name(&application)?;
    let (_, _, _, installed_executable) = get_install_info();
    let current = std::env::current_exe()?;
    let current = current
        .to_str()
        .ok_or_else(|| anyhow!("Current executable path is not valid Unicode"))?;
    if !current.eq_ignore_ascii_case(&installed_executable) {
        bail!("Run the installed diagnostic before testing native Stop service");
    }
    let broker = super::WIN_TOPMOST_INJECTED_PROCESS_EXE;
    let parameters = format!(
        "--native-service-stop \"{application}\" \"{broker}\" {}",
        std::process::id()
    );
    log::info!("Diagnostic native service stop: existing runner; no CMD, BAT or installer pipe");
    let code = launch_elevated(Path::new(&installed_executable), &parameters, false)?.wait()?;
    log::info!("Native service-stop runner exited with code {code}");
    if code != 0 {
        bail!(
            "Native service-stop runner failed with code {code}: {}",
            io::Error::from_raw_os_error(code as i32)
        );
    }
    Ok(())
}
