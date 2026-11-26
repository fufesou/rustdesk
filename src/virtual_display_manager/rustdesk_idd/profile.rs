use super::display_target;
use crate::platform::win_device::{device_io_control, DeviceError};
use hbb_common::{anyhow::Context, bail, ResultType};
use std::time::{Duration, Instant};
use uuid::Uuid;
use virtual_display::MonitorMode;
use winapi::shared::{
    guiddef::GUID,
    winerror::{
        ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_NOT_READY, ERROR_NO_MORE_ITEMS,
        ERROR_SHARING_VIOLATION,
    },
};

const INTERFACE: GUID = GUID {
    Data1: 0x781ef630,
    Data2: 0x72b2,
    Data3: 0x11d2,
    Data4: [0xb8, 0x52, 0x00, 0xc0, 0x4e, 0xaf, 0x52, 0x72],
};
// RustDeskIddDriver/DisplayProfile.h uses METHOD_BUFFERED with read and write access.
const fn profile_ioctl(function: u32) -> u32 {
    (winapi::um::winioctl::FILE_DEVICE_UNKNOWN << 16) | (3 << 14) | (function << 2)
}
const SESSION_PROFILE: u32 = profile_ioctl(0x805);
const SESSION_MODES: u32 = profile_ioctl(0x806);
const QUERY_COLOR: u32 = profile_ioctl(0x803);
const PROFILE_VERSION: u32 = 1;
const ENABLED: u32 = 1;
const DEVICE_TIMEOUT: Duration = Duration::from_secs(20);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Profile {
    index: u32,
    id: Uuid,
    attachment: Uuid,
}

impl Profile {
    pub(super) fn new(index: u32) -> Self {
        Self {
            index,
            id: Uuid::new_v4(),
            attachment: Uuid::new_v4(),
        }
    }

    fn packet(&self, modes: &[MonitorMode]) -> ResultType<Vec<u8>> {
        let mut packet = Vec::new();
        packet.extend(self.attachment.to_bytes_le());
        packet.extend(PROFILE_VERSION.to_le_bytes());
        packet.extend(self.index.to_le_bytes());
        packet.extend(self.id.to_bytes_le());
        let flags = if modes.is_empty() { 0 } else { ENABLED };
        packet.extend(flags.to_le_bytes());
        packet.extend(u32::try_from(modes.len())?.to_le_bytes());
        packet.extend(0u32.to_le_bytes()); // Driver-generated EDID.
        for mode in modes {
            for field in [mode.width, mode.height, mode.sync, 1] {
                packet.extend(field.to_le_bytes());
            }
        }
        Ok(packet)
    }

    pub(super) fn attach(&self, modes: &[MonitorMode]) -> ResultType<()> {
        let packet = self.packet(modes)?;
        let deadline = Instant::now() + DEVICE_TIMEOUT;
        loop {
            match unsafe { device_io_control(&INTERFACE, SESSION_PROFILE, &packet, 0) } {
                Ok(_) => return Ok(()),
                Err(error) if device_starting(&error) && Instant::now() < deadline => {
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(error) => return Err(error).context("Attach RustDesk IDD monitor"),
            }
        }
    }

    pub(super) fn detach(&self) -> ResultType<()> {
        self.submit(SESSION_PROFILE, &[])
    }

    pub(super) fn set_mode(&self, mode: MonitorMode) -> ResultType<()> {
        self.submit(SESSION_MODES, &[mode])
    }

    fn submit(&self, code: u32, modes: &[MonitorMode]) -> ResultType<()> {
        let packet = self.packet(modes)?;
        unsafe { device_io_control(&INTERFACE, code, &packet, 0) }?;
        Ok(())
    }

    pub(super) fn source(&self) -> ResultType<Option<String>> {
        let mut identity = self.index.to_le_bytes().to_vec();
        identity.extend(self.id.to_bytes_le());
        match unsafe {
            device_io_control(
                &INTERFACE,
                QUERY_COLOR,
                &identity,
                display_target::REPLY_SIZE,
            )
        } {
            Ok(reply) => display_target::active_source(&reply),
            Err(error) if device_starting(&error) => Ok(None),
            Err(error) => Err(error).context("Query RustDesk IDD monitor"),
        }
    }

    pub(super) fn wait_for_source(&self, size: Option<(u32, u32)>) -> ResultType<String> {
        wait_for_activation(|| {
            let Some(name) = self.source()? else {
                return Ok(None);
            };
            let ready = match size {
                Some(size) => supports_resolution(&name, size),
                None => crate::platform::current_resolution(&name).is_ok(),
            };
            Ok(ready.then_some(name))
        })
        .with_context(|| {
            format!(
                "Waiting for RustDesk IDD connector {} with mode {:?}",
                self.index, size
            )
        })
    }
}

pub(super) fn supports_resolution(name: &str, size: (u32, u32)) -> bool {
    crate::platform::resolutions(name)
        .iter()
        .any(|r| (r.width as u32, r.height as u32) == size)
}

fn wait_for_activation(
    mut query: impl FnMut() -> ResultType<Option<String>>,
) -> ResultType<String> {
    let deadline = Instant::now() + DEVICE_TIMEOUT;
    loop {
        if let Some(active) = query()? {
            return Ok(active);
        }
        if Instant::now() >= deadline {
            bail!("Windows did not activate the virtual display before the deadline");
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn device_starting(error: &DeviceError) -> bool {
    let DeviceError::WinApiLastErr(_, error) = error else {
        return false;
    };
    matches!(
        error.raw_os_error().map(|n| n as u32),
        Some(
            ERROR_NOT_READY
                | ERROR_NO_MORE_ITEMS
                | ERROR_FILE_NOT_FOUND
                | ERROR_DEVICE_NOT_CONNECTED
                | ERROR_SHARING_VIOLATION
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_a_monitor_that_activates_after_three_enumerations() {
        let mut sources = [None, None, None, Some("DISPLAY7".to_owned())].into_iter();
        let name = wait_for_activation(|| Ok(sources.next().unwrap())).unwrap();
        assert_eq!(name, "DISPLAY7");
    }

    #[test]
    fn custom_size_is_advertised_in_the_session_profile() {
        let profile = Profile::new(2);
        let packet = profile
            .packet(&[MonitorMode {
                width: 1920,
                height: 1200,
                sync: 60,
            }])
            .unwrap();
        assert_eq!(&packet[..16], &profile.attachment.to_bytes_le());
        assert_eq!(&packet[24..40], &profile.id.to_bytes_le());
        let fields: Vec<_> = packet[40..]
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        assert_eq!(fields, [ENABLED, 1, 0, 1920, 1200, 60, 1]);
        assert_eq!(profile.packet(&[]).unwrap()[40..], [0; 12]);
    }
}
