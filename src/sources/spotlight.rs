use crate::{
    download_to_path, CacheManager, Result, Settings, WallpaperCandidate, WallpaperError,
    WallpaperSource, METADATA_TIMEOUT,
};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use std::fs;

use super::{FetchResult, Source, SourceContext};

pub struct SpotlightSource;

pub const SPOTLIGHT_URL: &str = "https://fd.api.iris.microsoft.com/v4/api/selection";
pub const SPOTLIGHT_DEFAULT_COUNTRY: &str = "US";
pub const SPOTLIGHT_DEFAULT_LOCALE: &str = "en-US";
pub const SPOTLIGHT_COUNT: usize = 3;

impl Source for SpotlightSource {
    fn id(&self) -> WallpaperSource {
        WallpaperSource::Spotlight
    }

    fn label(&self) -> &'static str {
        "Spotlight"
    }

    fn description(&self) -> &'static str {
        "Windows Spotlight image feed"
    }

    fn supports_day(&self) -> bool {
        false
    }

    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult> {
        let candidates =
            fetch_spotlight_candidates(ctx.client, ctx.cache, ctx.settings, ctx.date_label)?;
        Ok(FetchResult {
            candidates,
            skipped_download: false,
        })
    }

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        settings: &Settings,
    ) -> Result<&'a WallpaperCandidate> {
        let idx = settings.spotlight_index.saturating_sub(1);
        candidates.get(idx).ok_or_else(|| {
            WallpaperError::Message(format!(
                "Requested Spotlight index {} not available; {} images found.",
                settings.spotlight_index,
                candidates.len()
            ))
        })
    }
}

#[derive(Debug, Deserialize)]
struct SpotlightResponse {
    #[serde(default)]
    batchrsp: Option<SpotlightBatch>,
}

#[derive(Debug, Deserialize)]
struct SpotlightBatch {
    #[serde(default)]
    items: Vec<SpotlightItemWrapper>,
}

#[derive(Debug, Deserialize)]
struct SpotlightItemWrapper {
    item: SpotlightItem,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SpotlightItem {
    RawString(String),
    Object(SpotlightPayload),
}

#[derive(Debug, Deserialize, Clone)]
struct SpotlightPayload {
    #[serde(default)]
    ad: SpotlightAd,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct SpotlightAd {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    copyright: Option<String>,
    #[serde(default, rename = "ctaUri")]
    cta_uri: Option<String>,
    #[serde(default, rename = "landscapeImage")]
    landscape_image: Option<SpotlightImage>,
    #[serde(default, rename = "entityId")]
    _entity_id: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct SpotlightImage {
    asset: String,
}

pub(crate) fn fetch_spotlight_candidates(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
) -> Result<Vec<WallpaperCandidate>> {
    if !settings.force {
        if let Some(index) = cache.load_index(date_label)? {
            let existing: Vec<_> = index
                .candidates
                .into_iter()
                .filter(|c| c.source == WallpaperSource::Spotlight && c.local_path.exists())
                .collect();
            if !existing.is_empty() {
                return Ok(existing);
            }
        }
    }

    let url = build_spotlight_url(settings);
    let response = client.get(url.clone()).timeout(METADATA_TIMEOUT).send()?;
    if response.status() != StatusCode::OK {
        return Err(WallpaperError::Status {
            url,
            status: response.status().as_u16(),
        });
    }
    let body = response.bytes()?.to_vec();
    let payloads = parse_spotlight_payloads(&body)?;
    if payloads.is_empty() {
        return Err(WallpaperError::Message(
            "Spotlight response did not include any images.".to_string(),
        ));
    }

    let media_dir = cache.media_dir(date_label, WallpaperSource::Spotlight);
    fs::create_dir_all(&media_dir)?;

    let mut candidates = Vec::new();
    for (idx, payload) in payloads.into_iter().take(SPOTLIGHT_COUNT).enumerate() {
        let Some(image) = payload.ad.landscape_image.clone() else {
            continue;
        };
        let asset_url = image.asset;
        let ordinal = idx + 1;
        let file_name = format!("spotlight_{date_label}_{ordinal}.jpg");
        let local_path = media_dir.join(file_name);

        let download = download_to_path(
            client,
            &asset_url,
            &local_path,
            settings.force,
            settings.quiet,
        )?;

        let candidate_id = format!("spotlight-{date_label}-{ordinal}");
        let candidate = WallpaperCandidate {
            id: candidate_id,
            source: WallpaperSource::Spotlight,
            title: payload.ad.title.clone(),
            description: payload.ad.description.clone(),
            attribution: payload.ad.copyright.clone(),
            info_url: payload
                .ad
                .cta_uri
                .as_ref()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(strip_edge_scheme),
            image_url: asset_url,
            local_path: download.path,
            date: date_label.to_string(),
            metadata_xml: None,
        };
        cache.upsert_candidate(date_label, candidate.clone())?;
        candidates.push(candidate);
    }

    Ok(candidates)
}

pub(crate) fn build_spotlight_url(settings: &Settings) -> String {
    if let Some(override_url) = &settings.spotlight_url_override {
        return override_url.clone();
    }
    let (country, locale) = match &settings.country {
        Some(c) => {
            let parts: Vec<&str> = c.split('-').collect();
            let country = parts
                .first()
                .map(|s| s.to_uppercase())
                .unwrap_or_else(|| SPOTLIGHT_DEFAULT_COUNTRY.to_string());
            (country, c.clone())
        }
        None => (
            SPOTLIGHT_DEFAULT_COUNTRY.to_string(),
            SPOTLIGHT_DEFAULT_LOCALE.to_string(),
        ),
    };
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("placement", "88000820");
    serializer.append_pair("bcnt", &SPOTLIGHT_COUNT.to_string());
    serializer.append_pair("country", &country);
    serializer.append_pair("locale", &locale);
    serializer.append_pair("fmt", "json");
    format!("{SPOTLIGHT_URL}?{}", serializer.finish())
}

fn parse_spotlight_payloads(body: &[u8]) -> Result<Vec<SpotlightPayload>> {
    let parsed: SpotlightResponse = serde_json::from_slice(body)?;
    let Some(batch) = parsed.batchrsp else {
        return Ok(Vec::new());
    };
    let mut items = Vec::new();
    for wrapper in batch.items {
        match wrapper.item {
            SpotlightItem::RawString(raw) => {
                if let Ok(obj) = serde_json::from_str::<SpotlightPayload>(&raw) {
                    items.push(obj);
                }
            }
            SpotlightItem::Object(obj) => items.push(obj),
        }
    }
    Ok(items)
}

fn strip_edge_scheme(url: &str) -> String {
    let trimmed = url.trim();
    if let Some(rest) = trimmed.strip_prefix("microsoft-edge:") {
        rest.to_string()
    } else {
        trimmed.to_string()
    }
}
