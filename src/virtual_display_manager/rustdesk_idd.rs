pub(crate) mod display_target;
mod peer;
pub(crate) mod privacy;
mod profile;

pub(crate) use peer::plug_out_monitor;
pub use peer::{plug_in_index_modes, plug_in_peer_request};

use hbb_common::{anyhow::Context, bail, lazy_static, ResultType};
use profile::Profile;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};
use virtual_display::MonitorMode;

const HEADLESS: u32 = 0;
pub(crate) const PEER_INDICES: std::ops::RangeInclusive<u32> = 1..=4;
const DEFAULT_MODE: MonitorMode = MonitorMode {
    width: 1920,
    height: 1080,
    sync: 60,
};

lazy_static::lazy_static! {
    static ref MANAGER: Mutex<Manager> = Mutex::new(Manager::default());
}
static INSTALLED: Mutex<bool> = Mutex::new(false);

#[derive(Clone)]
pub(crate) struct Monitor {
    profile: Arc<Profile>,
}

#[derive(Clone, Default)]
struct Manager {
    monitors: BTreeMap<u32, Monitor>,
}

fn install_driver(force: bool) -> ResultType<()> {
    let mut installed = INSTALLED.lock().unwrap();
    if *installed && !force {
        return Ok(());
    }
    let mut reboot = false;
    virtual_display::install_update_driver(&mut reboot).context("Install RustDesk IDD driver")?;
    if reboot {
        bail!("Restart Windows to finish installing RustDesk IDD");
    }
    *installed = true;
    Ok(())
}

impl Manager {
    fn peer_slots_changed(&self, next: &Self) -> bool {
        PEER_INDICES
            .into_iter()
            .any(|i| match (self.monitors.get(&i), next.monitors.get(&i)) {
                (Some(old), Some(new)) => !Arc::ptr_eq(&old.profile, &new.profile),
                (None, None) => false,
                _ => true,
            })
    }

    fn plug_in(&mut self, index: u32, modes: &[MonitorMode]) -> ResultType<()> {
        install_driver(false)?;
        if let Some(monitor) = self.monitors.get_mut(&index) {
            match monitor.profile.wait_for_source(None) {
                Ok(_) => return Ok(()),
                Err(error) if profile::target_is_gone(&error) => {
                    self.monitors.remove(&index);
                }
                Err(error) => return Err(error),
            }
        }
        let modes = if modes.is_empty() {
            std::slice::from_ref(&DEFAULT_MODE)
        } else {
            modes
        };
        let profile = Profile::new(index).attach(modes, || false)?;
        let monitor = Monitor {
            profile: Arc::new(profile),
        };
        if let Err(error) = monitor.profile.wait_for_source(None) {
            if let Err(cleanup) = monitor.profile.detach() {
                self.monitors.insert(index, monitor);
                bail!("{error:#}; detach connector {index}: {cleanup:#}");
            }
            return Err(error);
        }
        self.monitors.insert(index, monitor);
        Ok(())
    }

    fn detach(&mut self, indices: &[u32]) -> ResultType<()> {
        detach_monitors(&mut self.monitors, indices, |monitor| {
            monitor.profile.detach()
        })
    }
}

fn update<R>(operation: impl FnOnce(&mut Manager) -> R) -> R {
    // Driver writes are serialized. Logins can read the last completed topology during activation.
    static WRITER: Mutex<()> = Mutex::new(());
    let _writer = WRITER.lock().unwrap();
    let mut next = MANAGER.lock().unwrap().clone();
    let result = operation(&mut next);
    let changed = {
        let mut current = MANAGER.lock().unwrap();
        let changed = current.peer_slots_changed(&next);
        *current = next;
        changed
    };
    if changed {
        crate::server::display_service::virtual_display_slots_changed();
    }
    result
}

fn detach_monitors<T>(
    monitors: &mut BTreeMap<u32, T>,
    indices: &[u32],
    detach: impl Fn(&T) -> ResultType<()>,
) -> ResultType<()> {
    let mut errors = Vec::new();
    for index in indices {
        if let Some(monitor) = monitors.get(index) {
            match detach(monitor) {
                Ok(()) => {
                    monitors.remove(index);
                }
                Err(error) => errors.push(format!("connector {index}: {error:#}")),
            }
        }
    }
    if !errors.is_empty() {
        bail!(
            "Failed to detach RustDesk IDD monitors: {}",
            errors.join(", ")
        );
    }
    Ok(())
}

pub fn install_update_driver() -> ResultType<()> {
    install_driver(true)
}

pub fn plug_in_headless() -> ResultType<()> {
    update(|m| m.plug_in(HEADLESS, &[]))
}

pub fn get_virtual_displays() -> Vec<u32> {
    MANAGER
        .lock()
        .unwrap()
        .monitors
        .iter()
        .filter_map(|(&i, m)| {
            (PEER_INDICES.contains(&i) && !m.profile.no_longer_attached()).then_some(i)
        })
        .collect()
}

pub fn plug_out_peer_request(indices: &[u32]) -> ResultType<()> {
    if let Some(index) = indices
        .iter()
        .find(|i| !PEER_INDICES.contains(i) && **i != privacy::CONNECTOR)
    {
        bail!("Invalid virtual display index {index}");
    }
    update(|m| m.detach(indices))
}

pub fn reset_all() -> ResultType<()> {
    update(|manager| {
        let indices: Vec<_> = manager.monitors.keys().copied().collect();
        manager.detach(&indices)
    })
}

pub(crate) fn resolution_targets() -> BTreeMap<String, Monitor> {
    let monitors = MANAGER.lock().unwrap().monitors.clone();
    monitors
        .into_values()
        .filter_map(|monitor| match monitor.profile.source() {
            Ok(Some(name)) => Some((name, monitor)),
            Ok(None) => None,
            Err(error) => {
                hbb_common::log::trace!("Virtual display source is unavailable: {error:#}");
                None
            }
        })
        .collect()
}

pub(crate) fn is_driver_display(name: &str) -> bool {
    super::windows::get_device_names(Some(super::RUSTDESK_IDD_DEVICE_STRING))
        .iter()
        .any(|source| source.trim_end_matches('\0') == name.trim_end_matches('\0'))
}

pub(crate) struct ResolutionControl {
    pub(crate) keyboard: Weak<()>,
    pub(crate) privacy_active: bool,
}

impl ResolutionControl {
    pub(crate) fn is_allowed(&self) -> bool {
        self.keyboard.strong_count() != 0
    }
}

pub(crate) fn change_resolution(
    target: Monitor,
    (w, h): (u32, u32),
    control: &ResolutionControl,
) -> ResultType<()> {
    update(|manager| {
        if !control.is_allowed() {
            return Ok(());
        }
        let Some((index, monitor)) = manager
            .monitors
            .iter()
            .find_map(|(&i, m)| Arc::ptr_eq(&m.profile, &target.profile).then_some((i, m)))
        else {
            bail!("Requested virtual display is no longer connected");
        };
        let current_name = monitor.profile.wait_for_source(None)?;
        let current = crate::platform::current_resolution(&current_name)?;
        if (current.width as u32, current.height as u32) == (w, h) {
            return Ok(());
        }
        let supported = profile::supports_resolution(&current_name, (w, h));
        if !control.is_allowed() {
            return Ok(());
        }
        if supported {
            crate::platform::change_resolution(&current_name, w as _, h as _)?;
            return Ok(());
        }
        if control.privacy_active {
            bail!("Turn off privacy mode before selecting a resolution that reconnects the virtual display");
        }
        if let Err(error) = monitor.profile.set_mode(MonitorMode {
            width: w,
            height: h,
            sync: DEFAULT_MODE.sync,
        }) {
            if monitor.profile.no_longer_attached() {
                manager.monitors.remove(&index);
            }
            return Err(error);
        }
        let active = monitor.profile.wait_for_source(Some((w, h)))?;
        if control.is_allowed() {
            crate::platform::change_resolution(&active, w as _, h as _)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests;
