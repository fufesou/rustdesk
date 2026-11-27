use super::{display_target, ResolutionControl};
use crate::platform::win_device::{
    device_io_control_by_path, get_instance_device_path, DeviceError,
};
use hbb_common::{anyhow::Context, bail, ResultType};
use std::time::{Duration, Instant};
use uuid::Uuid;
use virtual_display::MonitorMode;
use winapi::shared::{
    guiddef::GUID,
    winerror::{
        ERROR_DEVICE_NOT_CONNECTED, ERROR_FILE_NOT_FOUND, ERROR_NOT_FOUND, ERROR_NOT_READY,
        ERROR_NO_MORE_ITEMS, ERROR_SHARING_VIOLATION,
    },
};

const INTERFACE: GUID = GUID {
    Data1: 0x781ef630,
    Data2: 0x72b2,
    Data3: 0x11d2,
    Data4: [0xb8, 0x52, 0x00, 0xc0, 0x4e, 0xaf, 0x52, 0x72],
};
// DeviceCreateWithLifetime uses these enumerator and instance names in SwDeviceCreate.
const DEVICE_INSTANCE: &str = r"SWD\RustDeskIddDriver\RustDeskIddDriver";
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
// Windows remembers the monitor by container ID. The attachment ID belongs to one lease.
const CONNECTOR_UUID_PREFIX: u128 = 0xe3e14b42_a3ca_82ec_ad45_602c00000000;

pub(super) struct Profile {
    device_path: Vec<u16>,
    index: u32,
    id: Uuid,
    attachment: Uuid,
}

impl Profile {
    pub(super) fn new(index: u32) -> Self {
        Self {
            device_path: Vec::new(),
            index,
            id: Uuid::from_u128(CONNECTOR_UUID_PREFIX | u128::from(index)),
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

    pub(super) fn attach(
        mut self,
        modes: &[MonitorMode],
        cancelled: impl Fn() -> bool,
    ) -> ResultType<Self> {
        let packet = self.packet(modes)?;
        let deadline = Instant::now() + DEVICE_TIMEOUT;
        loop {
            if cancelled() {
                bail!("Virtual display preparation cancelled");
            }
            let attach = (|| unsafe {
                if self.device_path.is_empty() {
                    self.device_path = get_instance_device_path(&INTERFACE, DEVICE_INSTANCE)?;
                }
                device_io_control_by_path(&self.device_path, SESSION_PROFILE, &packet, 0)
            })();
            match attach {
                Ok(_) => return Ok(self),
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
        unsafe { device_io_control_by_path(&self.device_path, code, &packet, 0) }?;
        Ok(())
    }

    pub(super) fn target(&self) -> ResultType<Option<display_target::Target>> {
        let mut identity = self.index.to_le_bytes().to_vec();
        identity.extend(self.id.to_bytes_le());
        match unsafe {
            device_io_control_by_path(
                &self.device_path,
                QUERY_COLOR,
                &identity,
                display_target::REPLY_SIZE,
            )
        } {
            Ok(reply) => display_target::target_id(&reply).map(Some),
            Err(error) if device_starting(&error) => Ok(None),
            Err(error) => Err(error).context("Query RustDesk IDD monitor"),
        }
    }

    pub(super) fn source(&self) -> ResultType<Option<String>> {
        match self.target()? {
            Some(target) => display_target::active_source(target),
            None => Ok(None),
        }
    }

    pub(super) fn apply_resolution(
        &self,
        source: &str,
        (width, height): (u32, u32),
        control: &ResolutionControl,
    ) -> ResultType<()> {
        let current = self.source();
        if !control.is_allowed() {
            return Ok(());
        }
        if current?.as_deref() != Some(source) {
            bail!("The virtual display source changed before applying its resolution");
        }
        crate::platform::change_resolution(source, width as _, height as _)
    }

    pub(super) fn no_longer_attached(&self) -> bool {
        self.target().is_err_and(|error| target_is_gone(&error))
    }

    pub(super) fn wait_for_source(&self, size: Option<(u32, u32)>) -> ResultType<String> {
        self.wait_for_source_while(size, || false)
    }

    pub(super) fn wait_for_source_while(
        &self,
        size: Option<(u32, u32)>,
        cancelled: impl Fn() -> bool,
    ) -> ResultType<String> {
        wait_for_activation(|| {
            if cancelled() {
                bail!("Virtual display preparation cancelled");
            }
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

pub(super) fn target_is_gone(error: &hbb_common::anyhow::Error) -> bool {
    // The named query returns NOT_FOUND for both an empty slot and a different owner.
    matches!(error.downcast_ref::<DeviceError>(), Some(DeviceError::WinApiLastErr(api, e))
        if api == "DeviceIoControl" && e.raw_os_error() == Some(ERROR_NOT_FOUND as i32))
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
    fn virtual_display_reconnect_keeps_the_windows_monitor_identity() {
        let first = Profile::new(1).packet(&[]).unwrap();
        let reconnect = Profile::new(1).packet(&[]).unwrap();
        let other = Profile::new(2).packet(&[]).unwrap();
        assert_eq!(&first[24..40], &reconnect[24..40]);
        assert_ne!(&first[24..40], &other[24..40]);
        assert_ne!(&first[..16], &reconnect[..16]);
    }

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
