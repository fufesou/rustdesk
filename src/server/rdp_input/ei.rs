use base::config::keys::OPTION_ALLOW_LIBEI;
use context::Context;
use dbus::{blocking::SyncConnection, Path};
use hbb_common::{anyhow::anyhow, bail, config::Config, log, tokio, ResultType};
use scrap::wayland::{
    pipewire::{get_portal, PwStreamInfo, RDP_SESSION_INFO},
    remote_desktop_portal::OrgFreedesktopPortalRemoteDesktop,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

mod context;
mod device;
mod ffi;
mod keyboard;
mod keymap;
mod mouse;

type Shared = Arc<Mutex<Context>>;
type Selection = Result<Option<Shared>, String>;

#[derive(Debug)]
pub(crate) struct InitializationError(String);

impl std::fmt::Display for InitializationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "libei initialization failed: {}", self.0)
    }
}

impl std::error::Error for InitializationError {}

struct Session {
    connection: Arc<SyncConnection>,
    path: Path<'static>,
    stream: Option<PwStreamInfo>,
    resolution: Arc<Mutex<Option<(usize, usize)>>>,
}

struct Selected {
    connection: Weak<SyncConnection>,
    path: Path<'static>,
    result: Selection,
}

lazy_static::lazy_static! {
    static ref SELECTED: Mutex<Option<Selected>> = Mutex::new(None);
}

pub(crate) async fn create() -> ResultType<Option<(keyboard::Keyboard, mouse::Mouse)>> {
    let session = snapshot()?;
    tokio::task::spawn_blocking(move || {
        let Some(context) =
            select(&session).map_err(|error| InitializationError(error.to_string()))?
        else {
            return Ok(None);
        };
        let stream = session
            .stream
            .ok_or_else(|| anyhow!("Portal capture has no stream"))?;
        Ok(Some((
            keyboard::Keyboard {
                context: context.clone(),
            },
            mouse::Mouse {
                context,
                stream,
                resolution: session.resolution,
            },
        )))
    })
    .await?
}

fn snapshot() -> ResultType<Session> {
    let info = RDP_SESSION_INFO.lock().unwrap();
    let info = info
        .as_ref()
        .ok_or_else(|| anyhow!("Portal session is unavailable"))?;
    let stream = info.streams.first().cloned();
    Ok(Session {
        connection: info.conn.clone(),
        path: info.session.clone(),
        stream,
        resolution: info.resolution.clone(),
    })
}

fn select(session: &Session) -> ResultType<Option<Shared>> {
    let mut selected = SELECTED.lock().unwrap();
    if let Some(previous) = selected.as_ref() {
        let same_connection = previous
            .connection
            .upgrade()
            .is_some_and(|connection| Arc::ptr_eq(&connection, &session.connection));
        if same_connection && previous.path == session.path {
            return previous.result.clone().map_err(|error| anyhow!(error));
        }
    }
    let result = if Config::get_option(OPTION_ALLOW_LIBEI) == "Y" {
        connect(session).map(Some)
    } else {
        log::info!("Wayland Portal input backend: D-Bus (libei is disabled)");
        Ok(None)
    };
    let result = result.map_err(|error| error.to_string());
    *selected = Some(Selected {
        connection: Arc::downgrade(&session.connection),
        path: session.path.clone(),
        result: result.clone(),
    });
    result.map_err(|error| anyhow!(error))
}

fn connect(session: &Session) -> ResultType<Shared> {
    if session.stream.is_none() {
        bail!("Portal capture has no stream for libei pointer input");
    }
    let portal = get_portal(&session.connection);
    if portal.version()? < 2 {
        bail!("This Portal does not support ConnectToEIS; disable libei and restart RustDesk to use D-Bus input");
    }
    let api = Arc::new(ffi::Api::load().map_err(|error| {
        anyhow!("Cannot load libei.so.1: {error}; install the libei runtime or disable libei, then restart RustDesk")
    })?);
    let fd = portal.connect_to_eis(&session.path, HashMap::new())?;
    let context = Context::new(api, fd)?;
    log::info!("Wayland Portal input backend: libei");
    Ok(Arc::new(Mutex::new(context)))
}

pub(crate) fn is_keyboard(enigo: &mut enigo::Enigo) -> bool {
    enigo
        .get_custom_keyboard()
        .as_ref()
        .is_some_and(|keyboard| keyboard.as_any().is::<keyboard::Keyboard>())
}

fn checked<T>(result: ResultType<T>) -> ResultType<T> {
    if let Err(error) = &result {
        hbb_common::throttled_log!(Duration::from_secs(5), error, "libei input failed: {error}");
    }
    result
}

fn report(result: ResultType<()>) {
    let _ = checked(result);
}
