use hbb_common::ResultType;
use std::sync::Mutex;
use windows::Win32::System::{
    Com::CoIncrementMTAUsage,
    WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED},
};

pub(super) struct Apartment;

impl Apartment {
    pub(super) fn initialize() -> ResultType<Self> {
        static MTA_INITIALIZED: Mutex<bool> = Mutex::new(false);
        let mut initialized = MTA_INITIALIZED.lock().unwrap();
        if !*initialized {
            // windows-rs caches factories for the process lifetime. Keep one MTA
            // reference for that lifetime, including between worker threads.
            unsafe { CoIncrementMTAUsage()? };
            *initialized = true;
        }
        drop(initialized);
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { RoUninitialize() };
    }
}
