use hbb_common::{anyhow::anyhow, bail, ResultType};
use std::{
    cell::Cell,
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
};
use winapi::{
    shared::{windef::POINT, winerror::S_OK},
    um::{
        shellscalingapi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        winuser::{MonitorFromPoint, CURSORINFO, MONITOR_DEFAULTTONULL, USER_DEFAULT_SCREEN_DPI},
    },
};

#[derive(Clone, Copy)]
struct Capture {
    id: u64,
    handle: u64,
    scale: f64,
}

thread_local! {
    static CAPTURE: Cell<Option<Capture>> = const { Cell::new(None) };
}

pub(super) fn capture(info: &CURSORINFO) -> ResultType<u64> {
    let scale = monitor_scale(info.ptScreenPos)?;
    let handle = info.hCursor as u64;
    // A shared OS handle can remain unchanged when the pointer crosses DPI boundaries.
    let mut hash = DefaultHasher::new();
    (handle, scale.to_bits()).hash(&mut hash);
    const MAX_CURSOR_ID: u64 = (1 << 53) - 1;
    let id = hash.finish() % MAX_CURSOR_ID + 1;
    CAPTURE.with(|capture| capture.set(Some(Capture { id, handle, scale })));
    Ok(id)
}

pub(super) fn captured(id: u64) -> ResultType<(u64, f64)> {
    // The cursor service reads the bitmap on the same thread as its ID poll.
    // Keep the density from that poll instead of sampling a possibly different monitor.
    CAPTURE
        .with(|capture| capture.get())
        .filter(|capture| capture.id == id)
        .map(|capture| (capture.handle, capture.scale))
        .ok_or_else(|| anyhow!("Windows cursor capture does not match requested ID {id}"))
}

fn monitor_scale(point: POINT) -> ResultType<f64> {
    unsafe {
        let monitor = MonitorFromPoint(point, MONITOR_DEFAULTTONULL);
        if monitor.is_null() {
            bail!(
                "No monitor at Windows cursor position ({}, {})",
                point.x,
                point.y
            );
        }
        let (mut dpi_x, mut dpi_y) = (0, 0);
        let result = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        if result != S_OK {
            bail!("Failed to query Windows cursor DPI: HRESULT {result:#x}");
        }
        if dpi_x == 0 || dpi_y != dpi_x {
            bail!("Invalid Windows cursor DPI ({dpi_x}, {dpi_y})");
        }
        Ok(f64::from(dpi_x) / f64::from(USER_DEFAULT_SCREEN_DPI))
    }
}
