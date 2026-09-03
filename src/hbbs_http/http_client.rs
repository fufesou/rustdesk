use hbb_common::{
    async_recursion::async_recursion,
    bail,
    config::{Config, Socks5Server},
    log::{self, info},
    proxy::{Proxy, ProxyScheme},
    tls::{
        get_cached_tls_accept_invalid_cert, get_cached_tls_type, is_plain, upsert_tls_cache,
        TlsType,
    },
    ResultType,
};
use reqwest::{blocking::Client as SyncClient, Client as AsyncClient};
use std::time::Duration;

// Strict probing may try Rustls and NativeTLS; each backend gets its own full timeout.
const STRICT_HTTP_PROBE_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct HttpClientProbeOptions {
    tls_type: TlsType,
    is_tls_type_cached: bool,
    danger_accept_invalid_cert: Option<bool>,
    original_danger_accept_invalid_cert: Option<bool>,
    attempt_timeout: Option<Duration>,
}

#[derive(Clone, Copy)]
enum HttpClientProbeRetry {
    AcceptInvalidCert(HttpClientProbeOptions),
    NativeTls(HttpClientProbeOptions),
}

impl HttpClientProbeRetry {
    fn options(self) -> HttpClientProbeOptions {
        match self {
            Self::AcceptInvalidCert(options) | Self::NativeTls(options) => options,
        }
    }
}

fn next_http_client_probe(options: HttpClientProbeOptions) -> Option<HttpClientProbeRetry> {
    match (
        options.tls_type,
        options.is_tls_type_cached,
        options.danger_accept_invalid_cert,
    ) {
        (TlsType::Rustls, _, None) | (TlsType::NativeTls, _, None) => Some(
            HttpClientProbeRetry::AcceptInvalidCert(HttpClientProbeOptions {
                danger_accept_invalid_cert: Some(true),
                ..options
            }),
        ),
        (TlsType::Rustls, false, Some(_)) => {
            Some(HttpClientProbeRetry::NativeTls(HttpClientProbeOptions {
                tls_type: TlsType::NativeTls,
                danger_accept_invalid_cert: options.original_danger_accept_invalid_cert,
                ..options
            }))
        }
        _ => None,
    }
}

fn log_http_client_probe_retry(tls_url: &str, error: &reqwest::Error, retry: HttpClientProbeRetry) {
    match retry {
        HttpClientProbeRetry::AcceptInvalidCert(options) => log::warn!(
            "Failed to connect to server {} with {:?}: {:?}, trying accept invalid cert",
            tls_url,
            options.tls_type,
            error
        ),
        HttpClientProbeRetry::NativeTls(_) => log::warn!(
            "Failed to connect to server {} with rustls-tls: {:?}, trying native-tls",
            tls_url,
            error
        ),
    }
}

fn cache_successful_http_probe(tls_url: &str, options: HttpClientProbeOptions) {
    log::info!(
        "Successfully connected to server {} with {:?}",
        tls_url,
        options.tls_type
    );
    upsert_tls_cache(
        tls_url,
        options.tls_type,
        options.danger_accept_invalid_cert.unwrap_or(false),
    );
}

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
    let tls_type = get_cached_tls_type(tls_url);
    let is_tls_type_cached = tls_type.is_some();
    let tls_type = tls_type.unwrap_or(TlsType::Rustls);
    let tls_danger_accept_invalid_cert = get_cached_tls_accept_invalid_cert(tls_url);
    create_http_client_with_url_(
        url,
        tls_url,
        HttpClientProbeOptions {
            tls_type,
            is_tls_type_cached,
            danger_accept_invalid_cert: tls_danger_accept_invalid_cert,
            original_danger_accept_invalid_cert: tls_danger_accept_invalid_cert,
            attempt_timeout: None,
        },
    )
}

pub fn create_http_client_with_url_strict(url: &str) -> ResultType<SyncClient> {
    let parsed_url = url::Url::parse(url)?;
    if parsed_url.scheme() != "https" {
        bail!("Strict HTTP client requires HTTPS: {}", url);
    }
    let proxy_conf = Config::get_socks();
    let tls_url = get_url_for_tls(url, &proxy_conf);
    let cached_tls_type = get_cached_tls_type(tls_url);
    let cached_danger_accept_invalid_cert = get_cached_tls_accept_invalid_cert(tls_url);
    let can_reuse_cached_probe =
        cached_tls_type.is_some() && cached_danger_accept_invalid_cert == Some(false);
    let tls_type = if can_reuse_cached_probe {
        cached_tls_type.unwrap_or(TlsType::Rustls)
    } else {
        TlsType::Rustls
    };
    Ok(create_http_client_with_url_(
        url,
        tls_url,
        HttpClientProbeOptions {
            tls_type,
            is_tls_type_cached: can_reuse_cached_probe,
            danger_accept_invalid_cert: Some(false),
            original_danger_accept_invalid_cert: Some(false),
            attempt_timeout: Some(STRICT_HTTP_PROBE_ATTEMPT_TIMEOUT),
        },
    ))
}

fn create_http_client_with_url_(
    url: &str,
    tls_url: &str,
    options: HttpClientProbeOptions,
) -> SyncClient {
    let client = create_http_client(
        options.tls_type,
        options.danger_accept_invalid_cert.unwrap_or(false),
    );
    if options.is_tls_type_cached && options.original_danger_accept_invalid_cert.is_some() {
        return client;
    }
    let request = client.head(url);
    let probe_result = if let Some(attempt_timeout) = options.attempt_timeout {
        request.timeout(attempt_timeout).send()
    } else {
        request.send()
    };
    match probe_result {
        Ok(_) => {
            cache_successful_http_probe(tls_url, options);
            client
        }
        Err(error) if !error.is_request() => {
            log::warn!(
                "Failed to connect to server {} with {:?}, err: {}.",
                tls_url,
                options.tls_type,
                error
            );
            client
        }
        Err(error) => match next_http_client_probe(options) {
            Some(retry) => {
                log_http_client_probe_retry(tls_url, &error, retry);
                create_http_client_with_url_(url, tls_url, retry.options())
            }
            None => {
                log::error!(
                    "Failed to connect to server {} with {:?}, err: {:?}.",
                    tls_url,
                    options.tls_type,
                    error
                );
                client
            }
        },
    }
}

pub async fn create_http_client_async_with_url(url: &str) -> AsyncClient {
    let proxy_conf = Config::get_socks();
    let tls_url = get_url_for_tls(url, &proxy_conf);
    let tls_type = get_cached_tls_type(tls_url);
    let is_tls_type_cached = tls_type.is_some();
    let tls_type = tls_type.unwrap_or(TlsType::Rustls);
    let danger_accept_invalid_cert = get_cached_tls_accept_invalid_cert(tls_url);
    create_http_client_async_with_url_(
        url,
        tls_url,
        HttpClientProbeOptions {
            tls_type,
            is_tls_type_cached,
            danger_accept_invalid_cert,
            original_danger_accept_invalid_cert: danger_accept_invalid_cert,
            attempt_timeout: None,
        },
    )
    .await
}

pub async fn create_http_client_async_with_url_strict(url: &str) -> ResultType<AsyncClient> {
    let parsed_url = url::Url::parse(url)?;
    if parsed_url.scheme() != "https" {
        bail!("Strict HTTP client requires HTTPS: {}", url);
    }
    let proxy_conf = Config::get_socks();
    let tls_url = get_url_for_tls(url, &proxy_conf);
    let cached_tls_type = get_cached_tls_type(tls_url);
    let cached_danger_accept_invalid_cert = get_cached_tls_accept_invalid_cert(tls_url);
    let can_reuse_cached_probe =
        cached_tls_type.is_some() && cached_danger_accept_invalid_cert == Some(false);
    let tls_type = if can_reuse_cached_probe {
        cached_tls_type.unwrap_or(TlsType::Rustls)
    } else {
        TlsType::Rustls
    };
    Ok(create_http_client_async_with_url_(
        url,
        tls_url,
        HttpClientProbeOptions {
            tls_type,
            is_tls_type_cached: can_reuse_cached_probe,
            danger_accept_invalid_cert: Some(false),
            original_danger_accept_invalid_cert: Some(false),
            attempt_timeout: Some(STRICT_HTTP_PROBE_ATTEMPT_TIMEOUT),
        },
    )
    .await)
}

#[async_recursion]
async fn create_http_client_async_with_url_(
    url: &str,
    tls_url: &str,
    options: HttpClientProbeOptions,
) -> AsyncClient {
    let client = create_http_client_async(
        options.tls_type,
        options.danger_accept_invalid_cert.unwrap_or(false),
    );
    if options.is_tls_type_cached && options.original_danger_accept_invalid_cert.is_some() {
        return client;
    }
    let request = client.head(url);
    let probe_result = if let Some(attempt_timeout) = options.attempt_timeout {
        request.timeout(attempt_timeout).send().await
    } else {
        request.send().await
    };
    match probe_result {
        Ok(_) => {
            cache_successful_http_probe(tls_url, options);
            client
        }
        Err(error) => match next_http_client_probe(options) {
            Some(retry) => {
                log_http_client_probe_retry(tls_url, &error, retry);
                create_http_client_async_with_url_(url, tls_url, retry.options()).await
            }
            None => {
                log::error!(
                    "Failed to connect to server {} with {:?}, err: {:?}.",
                    tls_url,
                    options.tls_type,
                    error
                );
                client
            }
        },
    }
}
