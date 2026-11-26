use super::{
    connect_root_service, digest, exchange, ClientConnection, Request, Token, CLIENT, TIMEOUT_MS,
};
#[cfg(not(windows))]
use super::{is_service_account, libc};
use hbb_common::{
    anyhow::anyhow,
    rand::{rngs::OsRng, RngCore},
    tokio, ResultType,
};

async fn unlock(mut stream: ClientConnection, candidate: Option<String>) -> ResultType<bool> {
    let mut token = Token::default();
    OsRng
        .try_fill_bytes(&mut token)
        .map_err(|err| anyhow!("Failed to generate PIN grant: {err}"))?;
    let key = digest(&token);
    let ok = match candidate {
        Some(pin) => exchange(&mut stream, Request::Verify(pin, Some(key))).await?,
        None => {
            #[cfg(not(windows))]
            {
                let uid = unsafe { libc::geteuid() };
                tokio::task::spawn_blocking(move || {
                    crate::platform::unix_approval::request("--approve-unlock-pin", uid, &key)
                })
                .await??;
            }
            #[cfg(windows)]
            {
                // The SCM service handles one request per pipe. Release it while UAC is open.
                drop(stream);
                tokio::task::spawn_blocking(move || {
                    crate::platform::windows::unlock_pin::request_approval(&key)
                })
                .await??;
                stream = connect_root_service(TIMEOUT_MS).await?;
            }
            exchange(&mut stream, Request::Authorized(token)).await?
        }
    };
    if ok {
        *CLIENT.lock().unwrap() = Some(token);
    }
    Ok(ok)
}

#[tokio::main(flavor = "current_thread")]
pub(crate) async fn gui_request(action: &str) -> ResultType<String> {
    use std::io::ErrorKind::{ConnectionRefused, NotFound};
    let mut stream = match connect_root_service(TIMEOUT_MS).await {
        Ok(stream) => stream,
        Err(err) => {
            #[cfg(not(windows))]
            let io = err
                .downcast_ref::<crate::trusted_devices::authority::Unavailable>()
                .and_then(|error| error.0.downcast_ref::<std::io::Error>());
            #[cfg(windows)]
            let io = err.downcast_ref::<std::io::Error>();
            if action == "unlock-method"
                && io.is_some_and(|e| matches!(e.kind(), NotFound | ConnectionRefused))
            {
                return Ok("offline".into());
            }
            return Err(err);
        }
    };
    #[cfg(not(windows))]
    let root = is_service_account();
    #[cfg(windows)]
    let root = crate::platform::is_elevated(None)?;
    let ok = match action {
        "unlock-method" if root => return Ok("root".into()),
        "unlock-method" => {
            let configured = exchange(&mut stream, Request::Configured).await?;
            return Ok(if configured { "pin" } else { "admin" }.into());
        }
        "pin-set" => exchange(&mut stream, Request::Configured).await?,
        "admin" => root || unlock(stream, None).await?,
        _ => match action.split_once(':') {
            Some(("pin", pin)) => unlock(stream, Some(pin.to_owned())).await?,
            Some(("check-pin", pin)) => {
                exchange(&mut stream, Request::Verify(pin.to_owned(), None)).await?
            }
            _ => return Err(anyhow!("Unknown unlock PIN request")),
        },
    };
    Ok(ok.to_string())
}
