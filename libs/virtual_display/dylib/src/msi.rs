use windows::{
    core::{w, HSTRING},
    Win32::{
        Foundation::{ERROR_INSTALL_FAILURE, ERROR_SUCCESS},
        System::{
            ApplicationInstallationAndServicing::{
                MsiCloseHandle, MsiCreateRecord, MsiProcessMessage, MsiRecordSetStringW,
                INSTALLMESSAGE, INSTALLMESSAGE_ERROR, INSTALLMESSAGE_INFO, MSIHANDLE,
            },
            DataExchange::GlobalAddAtomW,
        },
    },
};

#[no_mangle]
pub extern "system" fn RemoveRustDeskIdd(install: MSIHANDLE) -> u32 {
    let mut reboot = false;
    match virtual_display::uninstall_test_driver(&mut reboot) {
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
