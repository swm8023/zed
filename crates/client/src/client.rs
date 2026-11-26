pub mod platform_info;
mod project_types;

use clock::SystemClock;
use gpui::App;
use http_client::{HttpClientWithUrl, read_proxy_from_env};
use serde::Deserialize;
use settings::{RegisterSetting, Settings};
use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
};
use url::Url;

pub use rpc::*;
pub use project_types::*;

pub static ZED_APP_PATH: LazyLock<Option<PathBuf>> =
    LazyLock::new(|| std::env::var("ZED_APP_PATH").ok().map(PathBuf::from));

pub static ZED_ALWAYS_ACTIVE: LazyLock<bool> =
    LazyLock::new(|| std::env::var("ZED_ALWAYS_ACTIVE").is_ok_and(|e| !e.is_empty()));

#[derive(Deserialize, Default, RegisterSetting)]
pub struct ProxySettings {
    pub proxy: Option<String>,
}

impl ProxySettings {
    pub fn proxy_url(&self) -> Option<Url> {
        self.proxy
            .as_deref()
            .map(str::trim)
            .filter(|input| !input.is_empty())
            .and_then(|input| {
                input
                    .parse::<Url>()
                    .inspect_err(|e| log::error!("Error parsing proxy settings: {}", e))
                    .ok()
            })
            .or_else(read_proxy_from_env)
    }
}

impl Settings for ProxySettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self {
            proxy: content
                .proxy
                .as_deref()
                .map(str::trim)
                .filter(|proxy| !proxy.is_empty())
                .map(ToOwned::to_owned),
        }
    }
}

pub struct Client {
    http: Arc<HttpClientWithUrl>,
}

impl Client {
    pub fn new(
        _clock: Arc<dyn SystemClock>,
        http: Arc<HttpClientWithUrl>,
        _cx: &mut App,
    ) -> Arc<Self> {
        Arc::new(Self { http })
    }

    pub fn production(cx: &mut App) -> Arc<Self> {
        let http = cx.http_client();
        Arc::new(Self {
            http: Arc::new(HttpClientWithUrl::new_url(http.clone(), "", http.proxy().cloned())),
        })
    }

    pub fn http_client(&self) -> Arc<HttpClientWithUrl> {
        self.http.clone()
    }
}

/// prefix for the zed:// url scheme
pub const ZED_URL_SCHEME: &str = "zed";

#[cfg(test)]
mod tests {
    use super::ProxySettings;

    #[test]
    fn proxy_settings_ignore_whitespace() {
        let settings = ProxySettings { proxy: Some(" http://127.0.0.1:8765 ".into()) };
        assert_eq!(settings.proxy_url().unwrap().as_str(), "http://127.0.0.1:8765/");
    }
}
