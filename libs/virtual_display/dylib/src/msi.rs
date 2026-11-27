use hbb_common::{anyhow::Context, bail, ResultType};
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
use windows::{
    core::{w, HSTRING, PWSTR},
    Win32::{
        Foundation::{ERROR_INSTALL_FAILURE, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR},
        System::{
            ApplicationInstallationAndServicing::{
                MsiCloseHandle, MsiCreateRecord, MsiGetPropertyW, MsiProcessMessage,
                MsiRecordSetStringW, INSTALLMESSAGE, INSTALLMESSAGE_ERROR, INSTALLMESSAGE_INFO,
                MSIHANDLE,
            },
            DataExchange::GlobalAddAtomW,
        },
    },
};

#[no_mangle]
pub extern "system" fn RemoveRustDeskIdd(install: MSIHANDLE) -> u32 {
    let mut reboot = false;
    match installed_exe(install)
        .and_then(|path| virtual_display::uninstall_test_driver_for(&path, &mut reboot))
    {
        Ok(()) => {
            if reboot {
                // WiX's CheckRebootRequired reads this WcaUtil atom after InstallFinalize.
                if unsafe { GlobalAddAtomW(w!("WcaDeferredActionRequiresReboot")) } == 0 {
                    let error = std::io::Error::last_os_error();
                    report(
                        install,
                        INSTALLMESSAGE_ERROR,
                        &format!("Cannot mark restart required: {error}"),
                    );
                    return ERROR_INSTALL_FAILURE.0;
                }
                report(
                    install,
                    INSTALLMESSAGE_INFO,
                    "Restart Windows to finish removing the RustDesk display driver",
                );
            }
            ERROR_SUCCESS.0
        }
        Err(error) => {
            report(install, INSTALLMESSAGE_ERROR, &format!("{error:#}"));
            ERROR_INSTALL_FAILURE.0
        }
    }
}

fn installed_exe(install: MSIHANDLE) -> ResultType<PathBuf> {
    let property = w!("CustomActionData");
    let mut size = 0;
    let mut empty = 0u16;
    let status =
        unsafe { MsiGetPropertyW(install, property, Some(PWSTR(&mut empty)), Some(&mut size)) };
    if status != ERROR_MORE_DATA.0 {
        WIN32_ERROR(status)
            .ok()
            .context("Read IDD uninstall action data")?;
        bail!("Missing installed executable in IDD uninstall action data");
    }
    size += 1;
    let mut value = vec![0u16; size as usize];
    let status = unsafe {
        MsiGetPropertyW(
            install,
            property,
            Some(PWSTR(value.as_mut_ptr())),
            Some(&mut size),
        )
    };
    WIN32_ERROR(status)
        .ok()
        .context("Read installed executable for IDD uninstall")?;
    Ok(OsString::from_wide(&value[..size as usize]).into())
}

fn report(install: MSIHANDLE, level: INSTALLMESSAGE, detail: &str) {
    unsafe {
        let record = MsiCreateRecord(1);
        if record.0 != 0 {
            MsiRecordSetStringW(record, 0, w!("RustDesk IDD: [1]"));
            MsiRecordSetStringW(record, 1, &HSTRING::from(detail));
            MsiProcessMessage(install, level, record);
            MsiCloseHandle(record);
        }
    }
}
