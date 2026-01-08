pub mod apod;
pub mod bing;
pub mod modis;
pub mod spotlight;

use crate::{CacheManager, CancelFlag, Result, Settings, WallpaperCandidate, WallpaperSource};
use reqwest::blocking::Client;
use std::collections::HashSet;
use std::sync::Arc;

pub struct SourceContext<'a> {
    pub client: &'a Client,
    pub cache: &'a CacheManager,
    pub settings: &'a Settings,
    pub date_label: &'a str,
    pub source_settings: &'a SourceSettings,
    pub cancel: Option<&'a CancelFlag>,
}

#[derive(Debug)]
pub struct FetchResult {
    pub candidates: Vec<WallpaperCandidate>,
    pub skipped_download: bool,
}

impl FetchResult {
    pub fn single(candidate: WallpaperCandidate, skipped_download: bool) -> Self {
        Self {
            candidates: vec![candidate],
            skipped_download,
        }
    }
}

pub trait Source: Send + Sync {
    fn id(&self) -> WallpaperSource;
    fn label(&self) -> &'static str;
    #[allow(dead_code)]
    fn description(&self) -> &'static str;
    fn supports_day(&self) -> bool {
        true
    }
    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult>;

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        _ctx: &SourceContext<'_>,
    ) -> Result<&'a WallpaperCandidate> {
        candidates
            .first()
            .ok_or_else(|| crate::WallpaperError::Message("No candidates available.".into()))
    }
}

pub struct SourceRegistry {
    sources: Vec<Arc<dyn Source>>,
}

impl Default for SourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceRegistry {
    pub fn new() -> Self {
        Self {
            sources: vec![
                Arc::new(bing::BingSource),
                Arc::new(spotlight::SpotlightSource),
                Arc::new(apod::ApodSource),
                Arc::new(modis::ModisSource),
            ],
        }
    }

    pub fn get(&self, id: WallpaperSource) -> Option<&dyn Source> {
        self.sources
            .iter()
            .find(|src| src.id() == id)
            .map(|boxed| boxed.as_ref())
    }

    pub fn all_enabled(&self, disabled: &HashSet<WallpaperSource>) -> Vec<Arc<dyn Source>> {
        self.sources
            .iter()
            .filter(|src| !disabled.contains(&src.id()))
            .cloned()
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct SourceSettings {
    pub bing: bing::BingSettings,
    pub spotlight: spotlight::SpotlightSettings,
    pub apod: apod::ApodSettings,
    pub modis: modis::ModisSettings,
}

impl SourceSettings {
    pub fn from_config(config: Option<&crate::AppConfig>) -> crate::Result<Self> {
        Ok(Self {
            bing: bing::BingSettings::from_config(config),
            spotlight: spotlight::SpotlightSettings::from_config(config)?,
            apod: apod::ApodSettings::from_config(config),
            modis: modis::ModisSettings::from_config(config),
        })
    }
}
