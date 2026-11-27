use super::{install_driver, update, Monitor, Profile, DEFAULT_MODE};
use crate::virtual_display_manager::{windows::get_device_names, RUSTDESK_IDD_DEVICE_STRING};
use hbb_common::{
    anyhow::Context,
    bail, log,
    tokio::sync::{Mutex, MutexGuard},
    ResultType,
};

// The driver exposes ten connectors. Zero is headless and 1–4 are selectable by peers.
pub(crate) const CONNECTOR: u32 = 5;
// Keep peer display changes out of the interval between preparation and privacy activation.
pub(crate) static TOPOLOGY: Mutex<()> = Mutex::const_new(());

pub(crate) struct PreparedMonitor {
    monitor: Option<Monitor>,
    _reservation: MutexGuard<'static, ()>,
}

impl PreparedMonitor {
    pub(crate) fn prepare(cancelled: impl Fn() -> bool) -> ResultType<Self> {
        let reservation = TOPOLOGY
            .try_lock()
            .context("Another virtual display change is in progress")?;
        if cancelled() {
            bail!("Privacy mode preparation cancelled");
        }
        if !get_device_names(Some(RUSTDESK_IDD_DEVICE_STRING)).is_empty() {
            return Ok(Self {
                monitor: None,
                _reservation: reservation,
            });
        }
        if get_device_names(None).is_empty() {
            bail!(crate::privacy_mode::NO_PHYSICAL_DISPLAYS);
        }
        install_driver(false)?;
        super::plug_out_peer_request(&[CONNECTOR])?;
        let profile = Profile::new(CONNECTOR).attach(&[DEFAULT_MODE], &cancelled)?;
        let name = profile.wait_for_source_while(None, cancelled);
        let prepared = Self {
            monitor: Some(Monitor {
                profile: profile.into(),
            }),
            _reservation: reservation,
        };
        name?;
        Ok(prepared)
    }

    pub(crate) fn adopt(mut self) {
        if let Some(monitor) = self.monitor.take() {
            update(|m| m.monitors.insert(CONNECTOR, monitor));
        }
    }
}

impl Drop for PreparedMonitor {
    fn drop(&mut self) {
        if let Some(monitor) = self.monitor.take() {
            if let Err(error) = monitor.profile.detach() {
                log::error!("Failed to detach the cancelled privacy display: {error:#}");
                update(|m| m.monitors.insert(CONNECTOR, monitor));
            }
        }
    }
}
