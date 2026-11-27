use super::Display;
use crate::virtual_display_manager::rustdesk_idd::display_target::{
    configuration, path_target, source_name, Configuration, Target,
};
use hbb_common::{anyhow::Context, bail, ResultType};
use winapi::{
    shared::winerror::{ERROR_BAD_CONFIGURATION, ERROR_INVALID_PARAMETER},
    um::wingdi::DISPLAYCONFIG_PATH_ACTIVE,
};
use windows::Win32::Devices::Display::{
    SetDisplayConfig, DISPLAYCONFIG_PATH_INFO, QDC_ALL_PATHS, QDC_ONLY_ACTIVE_PATHS,
    SDC_ALLOW_CHANGES, SDC_APPLY, SDC_SAVE_TO_DATABASE, SDC_TOPOLOGY_EXTEND,
    SDC_USE_DATABASE_CURRENT, SDC_USE_SUPPLIED_DISPLAY_CONFIG, SET_DISPLAY_CONFIG_FLAGS,
};

pub(super) struct RestoreLayout {
    saved: Configuration,
    physical: Vec<Target>,
}

impl RestoreLayout {
    pub(super) fn capture(displays: &[Display]) -> ResultType<Self> {
        let saved = configuration(QDC_ONLY_ACTIVE_PATHS)?
            .context("Display topology changed while enabling privacy mode")?;
        let sources: Vec<_> = saved
            .0
            .iter()
            .map(|path| source_name(path).map(|name| (name, path_target(path))))
            .collect::<std::io::Result<_>>()?;
        let mut physical = Vec::new();
        for display in displays {
            let mut targets = sources
                .iter()
                .filter(|(name, _)| {
                    display.name.split(|c| *c == 0).next() == name.split(|c| *c == 0).next()
                })
                .map(|(_, target)| *target);
            physical.push(
                targets
                    .next()
                    .context("Cannot identify a physical display path for privacy mode")?,
            );
            physical.extend(targets);
        }
        Ok(Self { saved, physical })
    }

    fn connected_paths(&self) -> ResultType<Vec<DISPLAYCONFIG_PATH_INFO>> {
        let (available, _) = configuration(QDC_ALL_PATHS)?
            .context("Display topology changed while restoring privacy mode")?;
        let available: Vec<_> = available
            .into_iter()
            .filter(|p| p.targetInfo.targetAvailable.as_bool())
            .collect();
        if !available.iter().any(|p| {
            let target = path_target(p);
            self.physical.contains(&target)
                || !self.saved.0.iter().any(|s| path_target(s) == target)
        }) {
            bail!("No physical display is connected; keeping the privacy display");
        }
        Ok(available)
    }

    pub(super) fn restore(&self) -> ResultType<()> {
        self.apply(&self.connected_paths()?)?;
        let (current, _) = configuration(QDC_ALL_PATHS)?
            .context("Display topology changed after restoring privacy mode")?;
        if !connected_targets_restored(&self.physical, &current)
            || !super::has_active_physical_display()
        {
            bail!("Windows has not restored the connected physical desktop");
        }
        Ok(())
    }

    fn apply(&self, available: &[DISPLAYCONFIG_PATH_INFO]) -> ResultType<()> {
        let (saved, modes) = &self.saved;
        let topology = if paths_still_available(saved, available) {
            // CCD restores the original source-to-target associations even if GDI names changed.
            let status = unsafe {
                SetDisplayConfig(
                    Some(saved),
                    Some(modes),
                    SDC_APPLY
                        | SDC_USE_SUPPLIED_DISPLAY_CONFIG
                        | SDC_SAVE_TO_DATABASE
                        | SDC_ALLOW_CHANGES,
                )
            };
            if saved_layout_applied(status)? {
                return Ok(());
            }
            connected_topology(&self.connected_paths()?)
        } else {
            hbb_common::throttled_log!(
                super::STATE_CHECK_LOG_INTERVAL,
                info,
                "The saved display paths changed; Windows will arrange the connected displays"
            );
            connected_topology(available)
        };
        let status = unsafe { SetDisplayConfig(None, None, SDC_APPLY | topology) };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status))
                .context("Restore the physical desktop");
        }
        Ok(())
    }
}

fn saved_layout_applied(status: i32) -> ResultType<bool> {
    if status == 0 {
        return Ok(true);
    }
    let error = std::io::Error::from_raw_os_error(status);
    if matches!(
        status as u32,
        ERROR_BAD_CONFIGURATION | ERROR_INVALID_PARAMETER
    ) {
        hbb_common::throttled_log!(
            super::STATE_CHECK_LOG_INTERVAL,
            warn,
            "Windows rejected the saved display configuration ({error}); arranging the connected displays"
        );
        return Ok(false);
    }
    Err(error).context("Restore the physical desktop")
}

fn connected_targets_restored(saved: &[Target], current: &[DISPLAYCONFIG_PATH_INFO]) -> bool {
    saved.iter().all(|target| {
        let mut paths = current.iter().filter(|p| path_target(p) == *target);
        !paths
            .clone()
            .any(|p| p.targetInfo.targetAvailable.as_bool())
            || paths.any(|p| p.flags & DISPLAYCONFIG_PATH_ACTIVE != 0)
    })
}

fn connected_topology(available: &[DISPLAYCONFIG_PATH_INFO]) -> SET_DISPLAY_CONFIG_FLAGS {
    if available
        .windows(2)
        .any(|pair| path_target(&pair[0]) != path_target(&pair[1]))
    {
        SDC_TOPOLOGY_EXTEND
    } else {
        SDC_USE_DATABASE_CURRENT
    }
}

fn paths_still_available(
    saved: &[DISPLAYCONFIG_PATH_INFO],
    available: &[DISPLAYCONFIG_PATH_INFO],
) -> bool {
    saved.iter().all(|old| {
        available.iter().any(|now| {
            path_target(now) == path_target(old)
                && now.sourceInfo.adapterId == old.sourceInfo.adapterId
                && now.sourceInfo.id == old.sourceInfo.id
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_restore_rearranges_rejected_modes_and_propagates_access_errors() {
        use winapi::shared::winerror::ERROR_ACCESS_DENIED;

        assert!(saved_layout_applied(0).unwrap());
        assert!(!saved_layout_applied(ERROR_BAD_CONFIGURATION as i32).unwrap());
        assert!(!saved_layout_applied(ERROR_INVALID_PARAMETER as i32).unwrap());
        let error = saved_layout_applied(ERROR_ACCESS_DENIED as i32).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn privacy_restore_accepts_an_unplugged_monitor_but_requires_a_connected_one() {
        let mut laptop = DISPLAYCONFIG_PATH_INFO::default();
        laptop.flags = DISPLAYCONFIG_PATH_ACTIVE;
        laptop.targetInfo.targetAvailable = true.into();
        let mut dock = DISPLAYCONFIG_PATH_INFO::default();
        dock.targetInfo.id = 1;
        let saved = [path_target(&laptop), path_target(&dock)];

        assert!(connected_targets_restored(&saved, &[laptop, dock]));
        dock.targetInfo.targetAvailable = true.into();
        assert!(!connected_targets_restored(&saved, &[laptop, dock]));
    }

    #[test]
    fn restore_layout_selects_windows_extend_after_undocking_or_reassigning_a_source() {
        let path = |target, source| {
            let mut p = DISPLAYCONFIG_PATH_INFO::default();
            p.targetInfo.id = target;
            p.sourceInfo.id = source;
            p
        };
        let saved = [path(1, 0), path(2, 1), path(3, 2)];
        assert!(paths_still_available(&saved, &saved));
        assert!(!paths_still_available(&saved, &[saved[0], saved[2]]));
        assert_eq!(
            connected_topology(&[saved[0], saved[2]]),
            SDC_TOPOLOGY_EXTEND
        );
        assert_eq!(
            connected_topology(&[saved[0], path(1, 2)]),
            SDC_USE_DATABASE_CURRENT
        );
        assert!(!paths_still_available(
            &saved,
            &[saved[0], saved[1], path(3, 4)]
        ));
    }
}
