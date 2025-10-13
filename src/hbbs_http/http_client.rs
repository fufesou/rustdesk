use hbb_common::{
    config::Config,
    log::{self, info},
    proxy::{Proxy, ProxyScheme},
    tls::{get_cached_tls_type, upsert_tls_type, TlsType},
};
use reqwest::{blocking::Client as SyncClient, Client as AsyncClient};

macro_rules! configure_http_client {
    ($builder:expr, $tls_type:expr, $Client: ty) => {{
        // https://github.com/rustdesk/rustdesk/issues/11569
        // https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html#method.no_proxy
        let mut builder = $builder.no_proxy();

        match $tls_type {
            Some(TlsType::Plain) => {}
            None | Some(TlsType::NativeTls) => {
                builder = builder.use_native_tls();
            }
            Some(TlsType::Rustls) => {
                builder = builder.use_rustls_tls();
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
                        Ok(p) => {
                            // to-do: tls choice for proxy??
                            builder = builder.proxy(p);
                            if let Some(auth) = proxy.intercept.maybe_auth() {
                                let basic_auth =
                                    format!("Basic {}", auth.get_basic_authorization());
                                if let Ok(auth) = basic_auth.parse() {
                                    builder = builder.default_headers(
                                        vec![(reqwest::header::PROXY_AUTHORIZATION, auth)]
                                            .into_iter()
                                            .collect(),
                                    );
                                }
                            }
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

pub fn create_http_client(tls_type: Option<TlsType>) -> SyncClient {
    let builder = SyncClient::builder();
    configure_http_client!(builder, tls_type, SyncClient)
}

pub fn create_http_client_async(tls_type: Option<TlsType>) -> AsyncClient {
    let builder = AsyncClient::builder();
    configure_http_client!(builder, tls_type, AsyncClient)
}

pub fn create_http_client_with_url(url: &str) -> SyncClient {
    let tls_type = get_cached_tls_type(url);
    let mut client = create_http_client(tls_type);
    if let Err(e) = client.head(url).send() {
        if matches!(tls_type, None) && e.is_request() {
            log::warn!(
                "Failed to connect to server {} with native-tls: {}. Trying rustls-tls",
                url,
                e
            );
            client = create_http_client(Some(TlsType::Rustls));
            if let Err(e2) = client.head(url).send() {
                log::warn!(
                    "Failed to connect to server {} with rustls-tls: {}. Keep using rustls-tls",
                    url,
                    e2
                );
            } else {
                log::info!("Successfully switched to rustls-tls");
                upsert_tls_type(url, Some(TlsType::Rustls));
            }
        } else {
            log::warn!(
                "Failed to connect to server {} with native-tls: {}. Keep using native-tls",
                url,
                e
            );
        }
    } else {
        log::info!("Successfully connected to server {} with native-tls", url);
        upsert_tls_type(url, Some(TlsType::NativeTls));
    }
    client
}

pub async fn create_http_client_async_with_url(url: &str) -> AsyncClient {
    let tls_type = get_cached_tls_type(url);
    let mut client = create_http_client_async(tls_type);
    if let Err(e) = client.head(url).send().await {
        if matches!(tls_type, None) && e.is_request() {
            log::warn!(
                "Failed to connect to server {} with native-tls: {}. Trying rustls-tls",
                url,
                e
            );
            client = create_http_client_async(Some(TlsType::Rustls));
            if let Err(e2) = client.head(url).send().await {
                log::warn!(
                    "Failed to connect to server {} with rustls-tls: {}. Keep using rustls-tls",
                    url,
                    e2
                );
            } else {
                log::info!("Successfully switched to rustls-tls");
                upsert_tls_type(url, Some(TlsType::Rustls));
            }
        } else {
            log::warn!(
                "Failed to connect to server {} with native-tls: {}. Keep using native-tls",
                url,
                e
            );
        }
    } else {
        log::info!("Successfully connected to server {} with native-tls", url);
        upsert_tls_type(url, Some(TlsType::NativeTls));
    }
    client
}
