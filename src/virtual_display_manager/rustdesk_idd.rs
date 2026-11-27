pub(crate) mod display_target;
mod peer;
pub(crate) mod privacy;
mod profile;

pub(crate) use peer::plug_out_monitor;
pub use peer::{plug_in_index_modes, plug_in_peer_request};

use hbb_common::{
    anyhow::{anyhow, Context},
    bail, lazy_static, ResultType,
};
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
struct Monitor {
    profile: Arc<Profile>,
    name: Option<String>,
}

impl Monitor {
    fn refresh_source(&mut self) -> bool {
        match self.profile.source() {
            Ok(name) => self.name = name,
            Err(error) if profile::target_is_gone(&error) => {
                self.name = None;
                return false;
            }
            Err(error) => {
                hbb_common::log::trace!("Virtual display source is unavailable: {error:#}")
            }
        }
        true
    }

    fn matches_source(&mut self, name: &str, refresh_on_mismatch: bool) -> bool {
        if self.name.is_none() || (refresh_on_mismatch && self.name.as_deref() != Some(name)) {
            self.refresh_source();
        }
        self.name.as_deref() == Some(name)
    }
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
                Ok(name) => {
                    monitor.name = Some(name);
                    return Ok(());
                }
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
        let mut monitor = Monitor {
            profile: Arc::new(profile),
            name: None,
        };
        match monitor.profile.wait_for_source(None) {
            Ok(name) => monitor.name = Some(name),
            Err(error) => {
                if let Err(cleanup) = monitor.profile.detach() {
                    self.monitors.insert(index, monitor);
                    bail!("{error:#}; detach connector {index}: {cleanup:#}");
                }
                return Err(error);
            }
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
        .iter_mut()
        .filter_map(|(&i, m)| (m.refresh_source() && PEER_INDICES.contains(&i)).then_some(i))
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

pub fn is_virtual_display(name: &str) -> bool {
    let name = name.trim_end_matches('\0');
    MANAGER
        .lock()
        .unwrap()
        .monitors
        .values_mut()
        .any(|m| m.matches_source(name, false))
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

pub(crate) fn change_resolution_if_is_virtual_display(
    name: &str,
    (w, h): (u32, u32),
    control: &ResolutionControl,
) -> ResultType<bool> {
    let requested_name = name.trim_end_matches('\0');
    update(|manager| {
        if !control.is_allowed() {
            return Ok(true);
        }
        let Some((index, monitor)) = manager
            .monitors
            .iter_mut()
            .find_map(|(&i, m)| m.matches_source(requested_name, true).then_some((i, m)))
        else {
            return Ok(false);
        };
        let current_name = monitor.profile.wait_for_source(None)?;
        if current_name != requested_name {
            monitor.name = Some(current_name);
            return Err(anyhow!(
                "The virtual display changed before applying its resolution"
            ));
        }
        let current = crate::platform::current_resolution(&current_name)?;
        if (current.width as u32, current.height as u32) == (w, h) {
            return Ok(true);
        }
        let supported = profile::supports_resolution(&current_name, (w, h));
        if !control.is_allowed() {
            return Ok(true);
        }
        if supported {
            crate::platform::change_resolution(&current_name, w as _, h as _)?;
            return Ok(true);
        }
        if control.privacy_active {
            bail!("Turn off privacy mode before selecting a resolution that reconnects the virtual display");
        }
        // Even a failed replacement can recreate the old mode under a new GDI name.
        monitor.name = None;
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
        monitor.name = Some(active.clone());
        if control.is_allowed() {
            crate::platform::change_resolution(&active, w as _, h as _)?;
        }
        Ok(true)
    })
}

#[cfg(test)]
mod tests;
