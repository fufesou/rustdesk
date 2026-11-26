mod display_target;
mod profile;

use hbb_common::{
    anyhow::{anyhow, Context},
    bail, lazy_static, ResultType,
};
use profile::Profile;
use std::{collections::BTreeMap, sync::Mutex};
use virtual_display::MonitorMode;

const HEADLESS: u32 = 0;
const PEER_INDICES: std::ops::RangeInclusive<u32> = 1..=4;
const DEFAULT_MODE: MonitorMode = MonitorMode {
    width: 1920,
    height: 1080,
    sync: 60,
};

lazy_static::lazy_static! {
    static ref MANAGER: Mutex<Manager> = Mutex::new(Manager::default());
}

struct Monitor {
    profile: Profile,
    name: Option<String>,
}

impl Monitor {
    fn matches_source(&mut self, name: &str) -> bool {
        if self.name.is_none() {
            match self.profile.source() {
                Ok(current) => self.name = current,
                Err(error) => {
                    hbb_common::log::trace!("Virtual display source is unavailable: {error:#}")
                }
            }
        }
        self.name.as_deref() == Some(name)
    }
}

#[derive(Default)]
struct Manager {
    monitors: BTreeMap<u32, Monitor>,
    installed: bool,
}

impl Manager {
    fn install(&mut self) -> ResultType<()> {
        let mut reboot = false;
        virtual_display::install_update_driver(&mut reboot)
            .context("Install RustDesk IDD driver")?;
        if reboot {
            bail!("Restart Windows to finish installing RustDesk IDD");
        }
        self.installed = true;
        Ok(())
    }

    fn plug_in(&mut self, index: u32, modes: &[MonitorMode]) -> ResultType<()> {
        if !self.installed {
            self.install()?;
        }
        if let Some(monitor) = self.monitors.get_mut(&index) {
            monitor.name = Some(monitor.profile.wait_for_source(None)?);
            return Ok(());
        }
        let profile = Profile::new(index);
        profile.attach(if modes.is_empty() {
            std::slice::from_ref(&DEFAULT_MODE)
        } else {
            modes
        })?;
        let mut monitor = Monitor {
            profile,
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
    MANAGER.lock().unwrap().install()
}

pub fn plug_in_headless() -> ResultType<()> {
    MANAGER.lock().unwrap().plug_in(HEADLESS, &[])
}

pub fn get_virtual_displays() -> Vec<u32> {
    MANAGER
        .lock()
        .unwrap()
        .monitors
        .range(PEER_INDICES)
        .map(|(i, _)| *i)
        .collect()
}

pub fn plug_in_index_modes(index: u32, modes: Vec<MonitorMode>) -> ResultType<()> {
    if !PEER_INDICES.contains(&index) {
        bail!("Invalid virtual display index {index}");
    }
    MANAGER.lock().unwrap().plug_in(index, &modes)
}

pub fn plug_in_peer_request(modes: Vec<Vec<MonitorMode>>) -> ResultType<Vec<u32>> {
    let mut manager = MANAGER.lock().unwrap();
    let indices: Vec<_> = PEER_INDICES
        .filter(|i| !manager.monitors.contains_key(i))
        .take(modes.len())
        .collect();
    if indices.len() < modes.len() {
        bail!("No free RustDesk IDD connector");
    }
    for (n, (index, modes)) in indices.iter().zip(&modes).enumerate() {
        if let Err(error) = manager.plug_in(*index, modes) {
            if let Err(cleanup) = manager.detach(&indices[..n]) {
                bail!("{error:#}; rollback failed: {cleanup:#}");
            }
            return Err(error);
        }
    }
    Ok(indices)
}

pub fn plug_out_peer_request(indices: &[u32]) -> ResultType<()> {
    if let Some(index) = indices.iter().find(|i| !PEER_INDICES.contains(i)) {
        bail!("Invalid virtual display index {index}");
    }
    MANAGER.lock().unwrap().detach(indices)
}

pub fn reset_all() -> ResultType<()> {
    let mut manager = MANAGER.lock().unwrap();
    let indices: Vec<_> = manager.monitors.keys().copied().collect();
    manager.detach(&indices)
}

pub fn is_virtual_display(name: &str) -> bool {
    let name = name.trim_end_matches('\0');
    MANAGER
        .lock()
        .unwrap()
        .monitors
        .values_mut()
        .any(|m| m.matches_source(name))
}

pub(crate) fn change_resolution_if_is_virtual_display(
    name: &str,
    (w, h): (u32, u32),
    privacy_active: bool,
) -> ResultType<bool> {
    let requested_name = name.trim_end_matches('\0');
    let mut manager = MANAGER.lock().unwrap();
    let Some(monitor) = manager
        .monitors
        .values_mut()
        .find_map(|m| m.matches_source(requested_name).then_some(m))
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
    if profile::supports_resolution(&current_name, (w, h)) {
        crate::platform::change_resolution(&current_name, w as _, h as _)?;
        return Ok(true);
    }
    if privacy_active {
        bail!("Turn off privacy mode before selecting a resolution that reconnects the virtual display");
    }
    monitor.profile.set_mode(MonitorMode {
        width: w,
        height: h,
        sync: DEFAULT_MODE.sync,
    })?;
    // Profile replacement can assign a different Windows display source, even on the same connector.
    monitor.name = None;
    let active = monitor.profile.wait_for_source(Some((w, h)))?;
    monitor.name = Some(active.clone());
    crate::platform::change_resolution(&active, w as _, h as _)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disconnect_retries_failed_monitors_and_releases_the_others() {
        let mut monitors = BTreeMap::from([(0, false), (1, true), (2, false)]);
        let result = detach_monitors(&mut monitors, &[0, 1, 2], |fails| {
            if *fails {
                bail!("driver busy")
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(monitors.keys().copied().collect::<Vec<_>>(), [1]);
        detach_monitors(&mut monitors, &[1], |_| Ok(())).unwrap();
        assert!(monitors.is_empty());
    }
}
