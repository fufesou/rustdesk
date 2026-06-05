use hbb_common::{
    async_recursion::async_recursion,
    config::{Config, Socks5Server},
    log::{self, info},
    proxy::{Proxy, ProxyScheme},
    tls::{
        get_cached_tls_accept_invalid_cert, get_cached_tls_type, is_plain, upsert_tls_cache,
        TlsType,
    },
};
use reqwest::{blocking::Client as SyncClient, Client as AsyncClient};
use std::time::Duration;

const TLS_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

macro_rules! configure_http_client {
    ($builder:expr, $tls_type:expr, $danger_accept_invalid_cert:expr, $Client: ty) => {{
        // https://github.com/rustdesk/rustdesk/issues/11569
        // https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html#method.no_proxy
        let mut builder = $builder.no_proxy();

        match $tls_type {
            TlsType::Plain => {}
            TlsType::NativeTls => {
                builder = builder.use_native_tls();
                if $danger_accept_invalid_cert {
                    builder = builder.danger_accept_invalid_certs(true);
                }
            }
            TlsType::Rustls => {
                #[cfg(any(target_os = "android", target_os = "ios"))]
                match hbb_common::verifier::client_config($danger_accept_invalid_cert) {
                    Ok(client_config) => {
                        builder = builder.use_preconfigured_tls(client_config);
                    }
                    Err(e) => {
                        hbb_common::log::error!("Failed to get client config: {}", e);
                    }
                }
                #[cfg(not(any(target_os = "android", target_os = "ios")))]
                {
                    builder = builder.use_rustls_tls();
                    if $danger_accept_invalid_cert {
                        builder = builder.danger_accept_invalid_certs(true);
                    }
                }
            }
        }

        let client = if let Some(conf) = Config::get_socks() {
            let proxy_result = Proxy::from_conf(&conf, None);

            match proxy_result {
                Ok(proxy) => {
                    let proxy_setup = match &proxy.intercept {
                        ProxyScheme::Http { host, .. } => {
                            reqwest::Proxy::all(format!("http://{}", host))
                        }
                        ProxyScheme::Https { host, .. } => {
                            reqwest::Proxy::all(format!("https://{}", host))
                        }
                        ProxyScheme::Socks5 { addr, .. } => {
                            reqwest::Proxy::all(&format!("socks5://{}", addr))
                        }
                    };

                    match proxy_setup {
                        Ok(mut p) => {
                            if let Some(auth) = proxy.intercept.maybe_auth() {
                                if !auth.username().is_empty() && !auth.password().is_empty() {
                                    p = p.basic_auth(auth.username(), auth.password());
                                }
                            }
                            builder = builder.proxy(p);
                            builder.build().unwrap_or_else(|e| {
                                info!("Failed to create a proxied client: {}", e);
                                <$Client>::new()
                            })
                        }
                        Err(e) => {
                            info!("Failed to set up proxy: {}", e);
                            <$Client>::new()
                        }
                    }
                }
                Err(e) => {
                    info!("Failed to configure proxy: {}", e);
                    <$Client>::new()
                }
            }
        } else {
            builder.build().unwrap_or_else(|e| {
                info!("Failed to create a client: {}", e);
                <$Client>::new()
            })
        };

        client
    }};
}

pub fn create_http_client(tls_type: TlsType, danger_accept_invalid_cert: bool) -> SyncClient {
    let builder = SyncClient::builder();
    configure_http_client!(builder, tls_type, danger_accept_invalid_cert, SyncClient)
}

pub fn create_http_client_async(
    tls_type: TlsType,
    danger_accept_invalid_cert: bool,
) -> AsyncClient {
    let builder = AsyncClient::builder();
    configure_http_client!(builder, tls_type, danger_accept_invalid_cert, AsyncClient)
}

pub fn get_url_for_tls<'a>(url: &'a str, proxy_conf: &'a Option<Socks5Server>) -> &'a str {
    if is_plain(url) {
        if let Some(conf) = proxy_conf {
            if conf.proxy.starts_with("https://") {
                return &conf.proxy;
            }
        }
    }
    url
}

pub fn create_http_client_with_url(url: &str) -> SyncClient {
    let proxy_conf = Config::get_socks();
    let tls_url = get_url_for_tls(url, &proxy_conf);
    let cached_danger_accept_invalid_cert = get_cached_tls_accept_invalid_cert(tls_url);
    let (tls_type, can_reuse_cached_probe, danger_accept_invalid_cert) =
        probe_config_from_cache(
            get_cached_tls_type(tls_url),
            cached_danger_accept_invalid_cert,
            false,
        );
    create_http_client_with_url_(
        url,
        tls_url,
        tls_type,
        can_reuse_cached_probe,
        danger_accept_invalid_cert,
        cached_danger_accept_invalid_cert,
        false,
    )
}

fn probe_config_from_cache(
    cached_tls_type: Option<TlsType>,
    cached_danger_accept_invalid_cert: Option<bool>,
    force_strict_tls: bool,
) -> (TlsType, bool, Option<bool>) {
    if force_strict_tls {
        let can_reuse_cached_probe =
            cached_tls_type.is_some() && cached_danger_accept_invalid_cert == Some(false);
        let tls_type = if can_reuse_cached_probe {
            cached_tls_type.unwrap_or(TlsType::Rustls)
        } else {
            TlsType::Rustls
        };
        (tls_type, can_reuse_cached_probe, Some(false))
    } else {
        let can_reuse_cached_probe =
            cached_tls_type.is_some() && cached_danger_accept_invalid_cert.is_some();
        let tls_type = cached_tls_type.unwrap_or(TlsType::Rustls);
        (
            tls_type,
            can_reuse_cached_probe,
            cached_danger_accept_invalid_cert,
        )
    }
}

pub fn create_http_client_with_url_strict(url: &str) -> SyncClient {
    let proxy_conf = Config::get_socks();
    let tls_url = get_url_for_tls(url, &proxy_conf);
    let cached_danger_accept_invalid_cert = get_cached_tls_accept_invalid_cert(tls_url);
    let (tls_type, can_reuse_cached_probe, danger_accept_invalid_cert) =
        probe_config_from_cache(
            get_cached_tls_type(tls_url),
            cached_danger_accept_invalid_cert,
            true,
        );
    create_http_client_with_url_(
        url,
        tls_url,
        tls_type,
        can_reuse_cached_probe,
        danger_accept_invalid_cert,
        cached_danger_accept_invalid_cert,
        true,
    )
}

fn next_danger_accept_invalid_cert(
    current_danger_accept_invalid_cert: Option<bool>,
    original_danger_accept_invalid_cert: Option<bool>,
    force_strict_tls: bool,
) -> Option<bool> {
    if force_strict_tls && current_danger_accept_invalid_cert == Some(false) {
        Some(false)
    } else {
        original_danger_accept_invalid_cert
    }
}

fn create_http_client_with_url_(
    url: &str,
    tls_url: &str,
    tls_type: TlsType,
    can_reuse_cached_probe: bool,
    danger_accept_invalid_cert: Option<bool>,
    original_danger_accept_invalid_cert: Option<bool>,
    force_strict_tls: bool,
) -> SyncClient {
    let mut client = create_http_client(tls_type, danger_accept_invalid_cert.unwrap_or(false));
    if can_reuse_cached_probe {
        return client;
    }
    let send_result = client.head(url).timeout(TLS_PROBE_TIMEOUT).send();
    if let Err(e) = send_result {
        if e.is_request() {
            match (tls_type, can_reuse_cached_probe, danger_accept_invalid_cert) {
                (TlsType::Rustls, _, None) => {
                    log::warn!(
                        "Failed to connect to server {} with rustls-tls: {:?}, trying accept invalid cert",
                        tls_url,
                        e
                    );
                    client = create_http_client_with_url_(
                        url,
                        tls_url,
                        tls_type,
                        can_reuse_cached_probe,
                        Some(true),
                        original_danger_accept_invalid_cert,
                        force_strict_tls,
                    );
                }
                (TlsType::Rustls, false, Some(_)) => {
                    log::warn!(
                        "Failed to connect to server {} with rustls-tls: {:?}, trying native-tls",
                        tls_url,
                        e
                    );
                    let next_danger_accept_invalid_cert = next_danger_accept_invalid_cert(
                        danger_accept_invalid_cert,
                        original_danger_accept_invalid_cert,
                        force_strict_tls,
                    );
                    client = create_http_client_with_url_(
                        url,
                        tls_url,
                        TlsType::NativeTls,
                        can_reuse_cached_probe,
                        next_danger_accept_invalid_cert,
                        next_danger_accept_invalid_cert,
                        force_strict_tls,
                    );
                }
                (TlsType::NativeTls, _, None) => {
                    log::warn!(
                        "Failed to connect to server {} with native-tls: {:?}, trying accept invalid cert",
                        tls_url,
                        e
                    );
                    client = create_http_client_with_url_(
                        url,
                        tls_url,
                        tls_type,
                        can_reuse_cached_probe,
                        Some(true),
                        original_danger_accept_invalid_cert,
                        force_strict_tls,
                    );
                }
                _ => {
                    log::error!(
                        "Failed to connect to server {} with {:?}, err: {:?}.",
                        tls_url,
                        tls_type,
                        e
                    );
                }
            }
        } else {
            log::warn!(
                "Failed to connect to server {} with {:?}, err: {}.",
                tls_url,
                tls_type,
                e
            );
        }
    } else {
        log::info!(
            "Successfully connected to server {} with {:?}",
            tls_url,
            tls_type
        );
        upsert_tls_cache(
            tls_url,
            tls_type,
            danger_accept_invalid_cert.unwrap_or(false),
        );
    }
    client
}

pub async fn create_http_client_async_with_url_strict(url: &str) -> AsyncClient {
    let proxy_conf = Config::get_socks();
    let tls_url = get_url_for_tls(url, &proxy_conf);
    let cached_danger_accept_invalid_cert = get_cached_tls_accept_invalid_cert(tls_url);
    let (tls_type, can_reuse_cached_probe, danger_accept_invalid_cert) =
        probe_config_from_cache(
            get_cached_tls_type(tls_url),
            cached_danger_accept_invalid_cert,
            true,
        );
    create_http_client_async_with_url_(
        url,
        tls_url,
        tls_type,
        can_reuse_cached_probe,
        danger_accept_invalid_cert,
        cached_danger_accept_invalid_cert,
        true,
    )
    .await
}

#[async_recursion]
async fn create_http_client_async_with_url_(
    url: &str,
    tls_url: &str,
    tls_type: TlsType,
    can_reuse_cached_probe: bool,
    danger_accept_invalid_cert: Option<bool>,
    original_danger_accept_invalid_cert: Option<bool>,
    force_strict_tls: bool,
) -> AsyncClient {
    let mut client =
        create_http_client_async(tls_type, danger_accept_invalid_cert.unwrap_or(false));
    if can_reuse_cached_probe {
        return client;
    }
    let send_result = client.head(url).timeout(TLS_PROBE_TIMEOUT).send().await;
    if let Err(e) = send_result {
        if e.is_request() {
            match (tls_type, can_reuse_cached_probe, danger_accept_invalid_cert) {
                (TlsType::Rustls, _, None) => {
                    log::warn!(
                        "Failed to connect to server {} with rustls-tls: {:?}, trying accept invalid cert",
                        tls_url,
                        e
                    );
                    client = create_http_client_async_with_url_(
                        url,
                        tls_url,
                        tls_type,
                        can_reuse_cached_probe,
                        Some(true),
                        original_danger_accept_invalid_cert,
                        force_strict_tls,
                    )
                    .await;
                }
                (TlsType::Rustls, false, Some(_)) => {
                    log::warn!(
                        "Failed to connect to server {} with rustls-tls: {:?}, trying native-tls",
                        tls_url,
                        e
                    );
                    let next_danger_accept_invalid_cert = next_danger_accept_invalid_cert(
                        danger_accept_invalid_cert,
                        original_danger_accept_invalid_cert,
                        force_strict_tls,
                    );
                    client = create_http_client_async_with_url_(
                        url,
                        tls_url,
                        TlsType::NativeTls,
                        can_reuse_cached_probe,
                        next_danger_accept_invalid_cert,
                        next_danger_accept_invalid_cert,
                        force_strict_tls,
                    )
                    .await;
                }
                (TlsType::NativeTls, _, None) => {
                    log::warn!(
                        "Failed to connect to server {} with native-tls: {:?}, trying accept invalid cert",
                        tls_url,
                        e
                    );
                    client = create_http_client_async_with_url_(
                        url,
                        tls_url,
                        tls_type,
                        can_reuse_cached_probe,
                        Some(true),
                        original_danger_accept_invalid_cert,
                        force_strict_tls,
                    )
                    .await;
                }
                _ => {
                    log::error!(
                        "Failed to connect to server {} with {:?}, err: {:?}.",
                        tls_url,
                        tls_type,
                        e
                    );
                }
            }
        } else {
            log::warn!(
                "Failed to connect to server {} with {:?}, err: {}.",
                tls_url,
                tls_type,
                e
            );
        }
    } else {
        log::info!(
            "Successfully connected to server {} with {:?}",
            tls_url,
            tls_type
        );
        upsert_tls_cache(
            tls_url,
            tls_type,
            danger_accept_invalid_cert.unwrap_or(false),
        );
    }
    client
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_tls_ignores_cached_tls_type_when_policy_changes() {
        let (tls_type, can_reuse_cached_probe, danger_accept_invalid_cert) =
            probe_config_from_cache(Some(TlsType::NativeTls), Some(true), true);

        assert!(matches!(tls_type, TlsType::Rustls));
        assert!(!can_reuse_cached_probe);
        assert_eq!(danger_accept_invalid_cert, Some(false));
    }

    #[test]
    fn cached_tls_type_is_reused_when_policy_matches() {
        let (tls_type, can_reuse_cached_probe, danger_accept_invalid_cert) =
            probe_config_from_cache(Some(TlsType::NativeTls), Some(false), true);

        assert!(matches!(tls_type, TlsType::NativeTls));
        assert!(can_reuse_cached_probe);
        assert_eq!(danger_accept_invalid_cert, Some(false));
    }
}
