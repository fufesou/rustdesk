use super::{display_target, profile, update, Manager, PEER_INDICES};
use hbb_common::{anyhow::Context, bail, ResultType};
use virtual_display::MonitorMode;

pub fn plug_in_index_modes(index: u32, modes: Vec<MonitorMode>) -> ResultType<()> {
    // Released Amyuni clients replay one anonymous request per saved monitor.
    if index == 0 {
        return plug_in_peer_request(vec![modes]).map(|_| ());
    }
    if !PEER_INDICES.contains(&index) {
        bail!("Invalid virtual display index {index}");
    }
    update(|m| m.plug_in(index, &modes))
}

pub fn plug_in_peer_request(modes: Vec<Vec<MonitorMode>>) -> ResultType<Vec<u32>> {
    update(|manager| {
        manager
            .monitors
            .retain(|i, m| !PEER_INDICES.contains(i) || !m.profile.no_longer_attached());
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
    })
}

pub(crate) fn plug_out_monitor(index: i32, force: bool) -> ResultType<()> {
    update(|manager| unplug(manager, index, force))
}

fn unplug(manager: &mut Manager, index: i32, force: bool) -> ResultType<()> {
    let indices: Vec<u32> =
        if index == super::super::IDD_PLUG_OUT_ALL_INDEX || (index == 0 && !force) {
            manager
                .monitors
                .range(PEER_INDICES)
                .map(|(&i, _)| i)
                .collect()
        } else if PEER_INDICES.contains(&(index as u32)) {
            vec![index as u32]
        } else {
            bail!("Invalid virtual display index {index}");
        };
    if force || indices.is_empty() {
        return manager.detach(&indices);
    }
    let mut requested = Vec::new();
    for index in indices {
        if let Some(monitor) = manager.monitors.get(&index) {
            match monitor.profile.target() {
                Ok(Some(target)) => requested.push((index, target)),
                Ok(None) => bail!("Virtual display target is unavailable"),
                Err(error) if profile::target_is_gone(&error) => {
                    manager.monitors.remove(&index);
                }
                Err(error) => return Err(error),
            }
        }
    }
    if requested.is_empty() {
        return Ok(());
    }
    let paths =
        display_target::active_paths()?.context("Display topology changed, retry unplug")?;
    let active: Vec<_> = paths.iter().map(display_target::path_target).collect();
    let indices = removable(&requested, &active)?;
    if index == 0 {
        // The count-based minus button removes one removable peer monitor.
        return manager.detach(indices.last().copied().as_slice());
    }
    manager.detach(&indices)
}

fn removable<T: PartialEq>(requested: &[(u32, T)], active: &[T]) -> ResultType<Vec<u32>> {
    let mut indices: Vec<_> = requested.iter().map(|(index, _)| *index).collect();
    if active.iter().all(|t| requested.iter().any(|(_, r)| r == t)) {
        if let Some((keep, _)) = requested.iter().find(|(_, r)| active.contains(r)) {
            if indices.len() == 1 {
                bail!("This only virtual display cannot be plugged out.");
            }
            indices.retain(|i| i != keep);
        }
    }
    Ok(indices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_virtual_display_controls_report_the_count_after_unplug() {
        let mut additions =
            r#"{"idd_impl":"rustdesk_idd","rustdesk_virtual_displays":[1],"is_installed":true}"#
                .to_owned();
        crate::virtual_display_manager::use_anonymous_display_controls(&mut additions).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&additions).unwrap(),
            serde_json::json!({"idd_impl": "amyuni_idd", "amyuni_virtual_displays": 1, "is_installed": true})
        );
    }

    #[test]
    fn manual_removal_keeps_one_active_target_when_all_are_requested() {
        let requested = [(1, "inactive"), (2, "active"), (3, "clone")];
        assert_eq!(removable(&requested, &["active", "clone"]).unwrap(), [1, 3]);
        assert!(removable(&requested[1..2], &["active"]).is_err());
        assert_eq!(
            removable(&requested, &["active", "physical"]).unwrap(),
            [1, 2, 3]
        );
    }
}
