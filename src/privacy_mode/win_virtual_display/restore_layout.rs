use super::Display;
use crate::virtual_display_manager::rustdesk_idd::display_target::{
    configuration, path_target, source_name, Configuration, Target,
};
use hbb_common::{anyhow::Context, bail, ResultType};
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

    pub(super) fn restore(&self) -> ResultType<()> {
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
        self.apply(&available)?;
        let (active, _) = configuration(QDC_ONLY_ACTIVE_PATHS)?
            .context("Display topology changed after restoring privacy mode")?;
        let missing = self.physical.iter().any(|t| {
            available.iter().any(|p| path_target(p) == *t)
                && !active.iter().any(|p| path_target(p) == *t)
        });
        if missing || !super::has_active_physical_display() {
            bail!("Windows has not restored the connected physical desktop");
        }
        Ok(())
    }

    fn apply(&self, available: &[DISPLAYCONFIG_PATH_INFO]) -> ResultType<()> {
        let (saved, modes) = &self.saved;
        let status = if paths_still_available(saved, available) {
            // CCD restores the original source-to-target associations even if GDI names changed.
            unsafe {
                SetDisplayConfig(
                    Some(saved),
                    Some(modes),
                    SDC_APPLY
                        | SDC_USE_SUPPLIED_DISPLAY_CONFIG
                        | SDC_SAVE_TO_DATABASE
                        | SDC_ALLOW_CHANGES,
                )
            }
        } else {
            hbb_common::throttled_log!(
                super::STATE_CHECK_LOG_INTERVAL,
                info,
                "The saved display paths changed; Windows will arrange the connected displays"
            );
            unsafe { SetDisplayConfig(None, None, SDC_APPLY | connected_topology(available)) }
        };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status))
                .context("Restore the physical desktop");
        }
        Ok(())
    }
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
