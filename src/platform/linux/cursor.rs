use hbb_common::{anyhow::Context, bail, log, ResultType};
use std::{
    cell::{Cell, RefCell},
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::{Duration, Instant},
};
use x11rb::{
    protocol::xproto::{Atom, ConnectionExt},
    rust_connection::RustConnection,
    NONE,
};

mod xsettings;

const X11_SETTINGS_REFRESH_INTERVAL: Duration = Duration::from_millis(250);

struct X11Settings {
    connection: RustConnection,
    screen: usize,
    selection: Atom,
    property: Atom,
}

thread_local! {
    static SETTINGS: RefCell<Option<X11Settings>> = const { RefCell::new(None) };
    static X11_SCALE: Cell<Option<f64>> = const { Cell::new(Some(0.0)) };
    static X11_SCALE_LAST_CHECK: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub(super) fn cache_id(id: u64, scale: f64) -> u64 {
    // Legacy Web decoders require JS-safe integers; zero is the service's initial ID.
    const MAX_CURSOR_ID: u64 = (1 << 53) - 1;
    if scale == 0.0 {
        return id;
    }
    let mut hash = DefaultHasher::new();
    (id, scale.to_bits()).hash(&mut hash);
    hash.finish() % MAX_CURSOR_ID + 1
}

pub(super) fn x11_cursor_id(id: u64) -> u64 {
    if X11_SCALE_LAST_CHECK.with(|last| {
        if last
            .get()
            .is_some_and(|checked| checked.elapsed() < X11_SETTINGS_REFRESH_INTERVAL)
        {
            return true;
        }
        last.set(Some(Instant::now()));
        false
    }) {
        return cache_id(id, x11_cursor_scale());
    }
    let scale = X11_SCALE.with(|last| match x11_scale() {
        Ok(scale) => {
            last.set(Some(scale));
            scale
        }
        Err(err) => {
            // XSETTINGS is optional; warn once per failure streak without
            // turning valid XFixes cursor updates into service errors.
            if last.replace(None).is_some() {
                log::warn!("Failed to read XSETTINGS cursor density; using unknown density: {err}");
            }
            0.0
        }
    });
    cache_id(id, scale)
}

pub(super) fn x11_cursor_scale() -> f64 {
    // The cursor service reads the ID and bitmap on the same thread. Reuse
    // that poll's density even if the settings manager changes between them.
    X11_SCALE.with(|last| last.get().unwrap_or(0.0))
}

pub(super) fn x11_scale() -> ResultType<f64> {
    if !super::is_x11() {
        return Ok(0.0);
    }
    SETTINGS.with(|settings| {
        let mut state = settings.try_borrow_mut()?;
        if state.is_none() {
            let (connection, screen) = x11rb::connect(None)?;
            *state = Some(X11Settings {
                connection,
                screen,
                selection: NONE,
                property: NONE,
            });
        }
        let settings = state.as_mut().context("Missing XSETTINGS connection")?;
        let result = read_settings(settings);
        if result.is_err() {
            *state = None;
        }
        result
    })
}

fn read_settings(settings: &mut X11Settings) -> ResultType<f64> {
    let connection = &settings.connection;
    if settings.selection == NONE {
        settings.selection = connection
            .intern_atom(true, format!("_XSETTINGS_S{}", settings.screen).as_bytes())?
            .reply()?
            .atom;
    }
    if settings.selection == NONE {
        return Ok(0.0);
    }
    let owner = connection
        .get_selection_owner(settings.selection)?
        .reply()?
        .owner;
    if owner == NONE {
        return Ok(0.0);
    }
    if settings.property == NONE {
        settings.property = connection
            .intern_atom(true, b"_XSETTINGS_SETTINGS")?
            .reply()?
            .atom;
    }
    let property = settings.property;
    let reply = connection
        .get_property(false, owner, property, property, 0, u32::MAX)?
        .reply()?;
    if reply.format != 8 || reply.bytes_after != 0 {
        bail!("Incomplete XSETTINGS property");
    }
    // Xft/DPI includes text scaling; it is not the cursor's pixel density.
    // Zero explicitly keeps the existing policy on desktops without a window scale.
    Ok(xsettings::scale(&reply.value)?.unwrap_or(0.0))
}

#[cfg(feature = "drm")]
pub(super) fn drm_snapshot<T>(
    f: impl Fn(&crate::server::drm_capturer::DrmCursorData) -> T,
) -> ResultType<Option<(T, f64)>> {
    crate::server::drm_capturer::drm_cursor_snapshot(f)
        .map(|(cursor, display)| {
            // A hidden cursor or an unavailable display probe has no density metadata.
            let scale = display
                .as_ref()
                .map(wayland_scale)
                .transpose()?
                .unwrap_or(0.0);
            Ok((cursor, scale))
        })
        .transpose()
}

#[cfg(feature = "drm")]
fn wayland_scale(display: &base::platform::linux::WaylandDisplayInfo) -> ResultType<f64> {
    // Missing logical geometry means unknown density, as with older senders.
    let Some((logical_width, logical_height)) = display.logical_size else {
        return Ok(0.0);
    };
    if logical_width <= 0 || logical_height <= 0 || display.width <= 0 || display.height <= 0 {
        bail!("Invalid Wayland cursor display dimensions");
    }
    // Logical geometry is already rotated; the physical mode dimensions are not.
    let width = if matches!(display.transform, 90 | 270) {
        display.height
    } else {
        display.width
    };
    let scale = f64::from(width) / f64::from(logical_width);
    // Mutter can report physical desktop coordinates even at 2x/3x output scale.
    // Keep geometry-derived fractional densities for logically scaled desktops.
    Ok(if scale == 1.0 && display.scale_factor > 1 {
        f64::from(display.scale_factor)
    } else {
        scale
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_cursor_polls_reuse_density_until_refresh() {
        const TEST_TIMEOUT: Duration = Duration::from_secs(60);
        // Keep the cache fresh without depending on test scheduling or sleeps.
        let fresh = Instant::now() + TEST_TIMEOUT;
        X11_SCALE.with(|last| last.set(Some(2.0)));
        X11_SCALE_LAST_CHECK.with(|last| last.set(Some(fresh)));
        SETTINGS.with(|settings| {
            // Any settings read must fail while borrowed; cached polls must not read.
            let _borrow = settings.borrow_mut();
            for id in [1, 2] {
                assert_eq!(x11_cursor_id(id), cache_id(id, 2.0));
            }
            X11_SCALE_LAST_CHECK.with(|last| {
                last.set(Some(Instant::now() - X11_SETTINGS_REFRESH_INTERVAL));
            });
            assert_eq!(x11_cursor_id(1), cache_id(1, 0.0));
            assert_eq!(x11_cursor_scale(), 0.0);
        });
    }

    #[test]
    fn cursor_cache_ids_fit_legacy_web_numbers() {
        for cursor in [1, 123, u64::MAX] {
            assert_eq!(cache_id(cursor, 0.0), cursor);
            for scale in [1.0, 1.25, 2.0] {
                assert!((1..=9_007_199_254_740_991).contains(&cache_id(cursor, scale)));
            }
        }
    }
}
