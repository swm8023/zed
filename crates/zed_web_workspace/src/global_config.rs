use std::{borrow::Cow, collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context as _, Result};
use assets::Assets;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use gpui::{App, AssetSource, SharedString};
use serde::Deserialize;
use settings::{BaseKeymap, KeymapFile, KeymapFileLoadResult, Settings as _};
use theme::ThemeRegistry;

const ASSET_PREFIX: &str = "web-global/";

#[derive(Deserialize)]
struct ConfigContents {
    settings: Option<String>,
    keymap: Option<String>,
    assets: BTreeMap<String, String>,
}

pub struct GlobalConfig {
    pub settings: Option<String>,
    pub keymap: Option<String>,
    pub assets: GlobalAssets,
}

impl GlobalConfig {
    pub fn parse(contents: &str) -> Result<Self> {
        let contents: ConfigContents = serde_json::from_str(contents)?;
        let assets = contents
            .assets
            .into_iter()
            .map(|(path, payload)| {
                let bytes = STANDARD
                    .decode(payload)
                    .with_context(|| format!("Decoding the global asset {path}"))?;
                Ok((path, bytes))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            settings: contents.settings,
            keymap: contents.keymap,
            assets: GlobalAssets(Arc::new(assets)),
        })
    }

    pub fn load_appearance(&self, cx: &mut App) -> Result<()> {
        let registry = ThemeRegistry::global(cx);
        let mut fonts = Vec::new();
        for (path, bytes) in self.assets.0.iter() {
            if path.starts_with("themes/") || path.starts_with("builtin-themes/") {
                theme_settings::load_user_theme(&registry, bytes)
                    .with_context(|| format!("Loading the global theme {path}"))?;
            } else if path.starts_with("icon_themes/") {
                registry
                    .load_icon_theme(Path::new(path), Path::new(ASSET_PREFIX), bytes.clone())
                    .with_context(|| format!("Loading the global icon theme {path}"))?;
            } else if path.starts_with("fonts/") {
                fonts.push(Cow::Owned(bytes.clone()));
            }
        }
        Assets.load_fonts(cx)?;
        cx.text_system().add_fonts(fonts)?;
        theme_settings::reload_theme(cx);
        theme_settings::reload_icon_theme(cx);
        Ok(())
    }

    pub fn load_keymap(&self, cx: &mut App) -> Result<()> {
        let base = *BaseKeymap::get_global(cx);
        if base != BaseKeymap::None {
            cx.bind_keys(KeymapFile::load_asset_allow_partial_failure(
                settings::DEFAULT_KEYMAP_PATH,
                cx,
            )?);
            if let Some(path) = base.asset_path() {
                cx.bind_keys(KeymapFile::load_asset_allow_partial_failure(path, cx)?);
            }
            cx.bind_keys(KeymapFile::load_asset_allow_partial_failure(
                settings::SPECIFIC_OVERRIDES_KEYMAP_PATH,
                cx,
            )?);
        }
        if let Some(keymap) = &self.keymap {
            match KeymapFile::load(keymap, cx) {
                KeymapFileLoadResult::Success { key_bindings } => cx.bind_keys(key_bindings),
                KeymapFileLoadResult::SomeFailedToLoad {
                    key_bindings,
                    error_message,
                } => {
                    log::warn!("Some global key bindings could not be loaded: {error_message:?}");
                    cx.bind_keys(key_bindings);
                }
                KeymapFileLoadResult::JsonParseFailure { error } => {
                    return Err(error.context("Loading the global keymap"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct GlobalAssets(Arc<BTreeMap<String, Vec<u8>>>);

impl AssetSource for GlobalAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(path) = path.strip_prefix(ASSET_PREFIX) {
            let path = path.strip_prefix("./").unwrap_or(path);
            return Ok(self.0.get(path).cloned().map(Cow::Owned));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if let Some(prefix) = path.strip_prefix(ASSET_PREFIX) {
            return Ok(self
                .0
                .keys()
                .filter(|path| path.starts_with(prefix))
                .map(|path| format!("{ASSET_PREFIX}{path}").into())
                .collect());
        }
        Assets.list(path)
    }
}
