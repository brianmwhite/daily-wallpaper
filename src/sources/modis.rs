use crate::{
    download_to_path, ensure_http_success, log_verbose, CacheManager, Result, Settings,
    WallpaperCandidate, WallpaperError, WallpaperSource, METADATA_TIMEOUT,
};
use reqwest::blocking::Client;
use scraper::{Html, Selector};
use serde::Deserialize;
use std::fs;
use url::Url;

use super::{FetchResult, Source, SourceContext};

const MODIS_URL: &str = "https://modis.gsfc.nasa.gov/gallery/individual.php";
const MODIS_GALLERY_BASE: &str = "https://modis.gsfc.nasa.gov/gallery/";

#[derive(Debug, Clone)]
pub struct ModisSettings {
    pub url_override: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct ModisConfig {
    #[serde(default)]
    pub url_override: Option<String>,
}

impl ModisSettings {
    pub fn from_config(config: Option<&crate::AppConfig>) -> Self {
        let url_override = config.and_then(|cfg| cfg.modis.as_ref()?.url_override.clone());
        Self { url_override }
    }
}

pub struct ModisSource;

impl Source for ModisSource {
    fn id(&self) -> WallpaperSource {
        WallpaperSource::Modis
    }

    fn label(&self) -> &'static str {
        "MODIS"
    }

    fn description(&self) -> &'static str {
        "NASA MODIS image of the day"
    }

    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult> {
        let candidate = fetch_modis_candidate(
            ctx.client,
            ctx.cache,
            ctx.settings,
            ctx.date_label,
            &ctx.source_settings.modis,
        )?;
        Ok(FetchResult::single(candidate, false))
    }

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        _ctx: &SourceContext<'_>,
    ) -> Result<&'a WallpaperCandidate> {
        candidates
            .first()
            .ok_or_else(|| WallpaperError::Message("No MODIS candidates found.".into()))
    }
}

struct ModisParsed {
    title: String,
    description: String,
    image_url: String,
    credit: Option<String>,
}

pub(crate) fn fetch_modis_candidate(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
    modis_settings: &ModisSettings,
) -> Result<WallpaperCandidate> {
    if let Some(candidate) = cache.find_candidate(date_label, WallpaperSource::Modis)? {
        if candidate.local_path.exists() && (!settings.force || settings.offline) {
            log_verbose(
                &format!("Using cached MODIS wallpaper for {date_label}"),
                settings,
            );
            return Ok(candidate);
        }
    }

    if settings.offline {
        return Err(WallpaperError::Message(format!(
            "Offline mode enabled; no cached MODIS wallpaper for {date_label}."
        )));
    }

    let page_url = build_modis_url(modis_settings, date_label)?;
    log_verbose(&format!("Fetching MODIS page: {page_url}"), settings);
    let response = client.get(&page_url).timeout(METADATA_TIMEOUT).send()?;
    ensure_http_success(response.status(), &page_url)?;
    let body = response.text()?;

    let parsed = parse_modis_html(&body)?;
    let media_dir = cache.media_dir(date_label, WallpaperSource::Modis);
    fs::create_dir_all(&media_dir)?;
    let file_name = format!("modis_{date_label}.jpg");
    let target_path = media_dir.join(file_name);
    let download = download_to_path(client, &parsed.image_url, &target_path, settings)?;

    let attribution = parsed
        .credit
        .as_deref()
        .map(|credit| format!("Image Credit: {credit}"));

    let candidate = WallpaperCandidate {
        id: format!("modis-{date_label}"),
        source: WallpaperSource::Modis,
        title: Some(parsed.title),
        description: Some(parsed.description),
        attribution,
        info_url: Some(page_url),
        image_url: parsed.image_url,
        local_path: download.path,
        date: date_label.to_string(),
        metadata_xml: None,
        checksum: None,
    };

    cache.upsert_candidate(date_label, candidate.clone())?;
    Ok(candidate)
}

fn build_modis_url(settings: &ModisSettings, date_label: &str) -> Result<String> {
    let base = settings.url_override.as_deref().unwrap_or(MODIS_URL);
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("db_date", date_label);
    let url = format!("{base}?{}", serializer.finish());
    Ok(url)
}

fn parse_modis_html(html: &str) -> Result<ModisParsed> {
    let document = Html::parse_document(html);

    let title_sel = Selector::parse("div.option h5 b").map_err(|_| {
        WallpaperError::Message("Unable to build MODIS title selector".into())
    })?;
    let title = document
        .select(&title_sel)
        .next()
        .map(|node| normalize_whitespace(&node.text().collect::<String>()))
        .and_then(|text| text.split_once(" - ").map(|(_, title)| title.to_string()).or(Some(text)))
        .filter(|text| !text.is_empty())
        .ok_or_else(|| WallpaperError::Message("MODIS title not found".into()))?;

    let paragraph_sel = Selector::parse("div.option p").map_err(|_| {
        WallpaperError::Message("Unable to build MODIS paragraph selector".into())
    })?;
    let mut description: Option<String> = None;
    let mut credit: Option<String> = None;

    for node in document.select(&paragraph_sel) {
        let text = normalize_whitespace(&node.text().collect::<String>());
        if text.is_empty() {
            continue;
        }
        if text.contains("Image Credit:") {
            if let Some((_, tail)) = text.split_once("Image Credit:") {
                let value = tail.trim();
                if !value.is_empty() {
                    credit = Some(value.to_string());
                }
            }
            continue;
        }
        if text.starts_with("Image Facts") {
            continue;
        }
        if description.is_none() {
            description = Some(text);
        }
    }

    let description = description.ok_or_else(|| {
        WallpaperError::Message("MODIS description not found".into())
    })?;

    let link_sel = Selector::parse("a").map_err(|_| {
        WallpaperError::Message("Unable to build MODIS link selector".into())
    })?;
    let mut image_url: Option<String> = None;
    for node in document.select(&link_sel) {
        if let Some(href) = node.value().attr("href") {
            if href.contains("_500m.jpg") {
                image_url = Some(resolve_modis_url(href)?);
                break;
            }
        }
    }

    let image_url = image_url.ok_or_else(|| {
        WallpaperError::Message("MODIS 500m image URL not found".into())
    })?;

    Ok(ModisParsed {
        title,
        description,
        image_url,
        credit,
    })
}

fn resolve_modis_url(href: &str) -> Result<String> {
    if href.starts_with("http://") || href.starts_with("https://") {
        return Ok(href.to_string());
    }

    let base = Url::parse(MODIS_GALLERY_BASE)
        .map_err(|_| WallpaperError::Message("MODIS base URL is invalid".into()))?;
    base.join(href)
        .map(|url| url.to_string())
        .map_err(|_| WallpaperError::Message("Unable to resolve MODIS image URL".into()))
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modis_html_extracts_fields() {
        let html = include_str!("../../docs/MODIS Web.html");
        let parsed = parse_modis_html(html).unwrap();
        assert_eq!(
            parsed.title,
            "Late Spring Bloom in the Great Australian Bight"
        );
        assert_eq!(
            parsed.description,
            "The Great Australian Bight is an embayment of the Indian Ocean along the southern coast of Australia. The expansive stretch of water has a depth ranging from just 15 meters (49 feet) to 6,000 meters (19,685 feet) and provides habitat for a wide range of species, including commercially important fish species such as the Australian Sardine and Southern Bluefin Tuna. Several endangered marine species also use the Bight as a home or breeding grounds, such as the Blue Whale, the Southern Right Whale, and the Australian Sea Lion."
        );
        assert_eq!(
            parsed.image_url,
            "http://modis.gsfc.nasa.gov/gallery/images/image12192025_500m.jpg"
        );
        assert_eq!(
            parsed.credit.as_deref(),
            Some("MODIS Land Rapid Response Team, NASA GSFC")
        );
    }
}
