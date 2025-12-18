use crate::{
    download_to_path, log, unique_temp_path, CacheManager, Result, Settings, WallpaperCandidate,
    WallpaperError, WallpaperSource, METADATA_TIMEOUT,
};
use image::imageops::FilterType;
use image::GenericImageView;
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::process::Command;

use super::{FetchResult, Source, SourceContext};

pub const APOD_URL: &str = "https://api.nasa.gov/planetary/apod";
pub const APOD_DEFAULT_KEY: &str = "DEMO_KEY";

pub struct ApodSource;

impl Source for ApodSource {
    fn id(&self) -> WallpaperSource {
        WallpaperSource::Apod
    }

    fn label(&self) -> &'static str {
        "APOD"
    }

    fn description(&self) -> &'static str {
        "NASA Astronomy Picture of the Day"
    }

    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult> {
        let candidate = fetch_apod_candidate(ctx.client, ctx.cache, ctx.settings, ctx.date_label)?;
        Ok(FetchResult::single(candidate, false))
    }

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        _settings: &Settings,
    ) -> Result<&'a WallpaperCandidate> {
        candidates
            .first()
            .ok_or_else(|| WallpaperError::Message("No APOD candidates found.".into()))
    }
}

#[derive(Debug, Deserialize)]
struct ApodResponse {
    #[serde(default)]
    url: String,
    #[serde(default)]
    hdurl: Option<String>,
    #[serde(default, rename = "media_type")]
    media_type: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    explanation: Option<String>,
    #[serde(default)]
    copyright: Option<String>,
}

pub(crate) fn fetch_apod_candidate(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
) -> Result<WallpaperCandidate> {
    let candidate_id = format!("apod-{date_label}");
    if !settings.force {
        if let Some(candidate) = cache.find_candidate_by_id(date_label, &candidate_id)? {
            if candidate.local_path.exists() {
                log(
                    &format!("Using cached APOD wallpaper for {}", date_label),
                    settings.quiet,
                );
                return Ok(candidate);
            }
        }
    }

    let apod = fetch_apod(client, settings, date_label)?;
    if apod.media_type != "image" {
        return Err(WallpaperError::Message(
            "APOD media type is not an image; skipping.".to_string(),
        ));
    }
    let image_url = if settings.apod_hd {
        apod.hdurl.clone().unwrap_or(apod.url.clone())
    } else {
        apod.url.clone()
    };
    if image_url.is_empty() {
        return Err(WallpaperError::Message(
            "APOD response missing image URL.".to_string(),
        ));
    }

    let media_dir = cache.media_dir(date_label, WallpaperSource::Apod);
    fs::create_dir_all(&media_dir)?;
    let file_name = format!("apod_{date_label}.jpg");
    let target_path = media_dir.join(file_name);
    let download = download_to_path(
        client,
        &image_url,
        &target_path,
        settings.force,
        settings.quiet,
    )?;

    let candidate = WallpaperCandidate {
        id: candidate_id,
        source: WallpaperSource::Apod,
        title: apod.title.clone(),
        description: apod.explanation.clone(),
        attribution: apod.copyright.clone(),
        info_url: None,
        image_url,
        local_path: download.path,
        date: date_label.to_string(),
        metadata_xml: None,
    };
    if settings.apod_crop {
        if let Err(err) = crop_and_resize_apod(&candidate.local_path) {
            log(
                &format!("APOD crop/resize failed; using original image. {err}"),
                settings.quiet,
            );
        }
    }

    cache.upsert_candidate(date_label, candidate.clone())?;
    Ok(candidate)
}

fn fetch_apod(client: &Client, settings: &Settings, date_label: &str) -> Result<ApodResponse> {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("api_key", &settings.apod_api_key);
    serializer.append_pair("date", date_label);
    let base = settings.apod_url_override.as_deref().unwrap_or(APOD_URL);
    let url = format!("{base}?{}", serializer.finish());
    let response = client.get(&url).timeout(METADATA_TIMEOUT).send()?;
    if response.status() != StatusCode::OK {
        return Err(WallpaperError::Status {
            url,
            status: response.status().as_u16(),
        });
    }
    let body = response.bytes()?.to_vec();
    let parsed: ApodResponse = serde_json::from_slice(&body)?;
    Ok(parsed)
}

fn crop_and_resize_apod(path: &Path) -> Result<()> {
    let img = image::open(path)
        .map_err(|err| WallpaperError::Message(format!("Unable to read APOD image: {err}")))?;
    let (orig_w, orig_h) = img.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return Ok(());
    }

    let (target_w, target_h) = detect_primary_display_size().unwrap_or((16, 9));
    let target_aspect = target_w as f32 / target_h as f32;
    let orig_aspect = orig_w as f32 / orig_h as f32;

    let (crop_w, crop_h) = if orig_aspect > target_aspect {
        let new_w = (orig_h as f32 * target_aspect).round().max(1.0) as u32;
        (new_w.min(orig_w), orig_h)
    } else {
        let new_h = (orig_w as f32 / target_aspect).round().max(1.0) as u32;
        (orig_w, new_h.min(orig_h))
    };
    let x0 = (orig_w - crop_w) / 2;
    let y0 = (orig_h - crop_h) / 2;
    let cropped = img.crop_imm(x0, y0, crop_w, crop_h);

    let processed = if target_w > 0 && target_h > 0 {
        image::imageops::resize(&cropped, target_w, target_h, FilterType::Lanczos3)
    } else {
        cropped.to_rgba8()
    };

    let temp_path = unique_temp_path(path);
    processed.save(&temp_path).map_err(|err| {
        WallpaperError::Message(format!("Unable to save processed APOD image: {err}"))
    })?;
    if let Ok(f) = fs::File::open(&temp_path) {
        let _ = f.sync_all();
    }
    fs::rename(&temp_path, path)?;
    Ok(())
}

fn detect_primary_display_size() -> Option<(u32, u32)> {
    let output = Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let displays = value.get("SPDisplaysDataType")?.as_array()?;
    let first = displays.first()?;
    let ndrv = first.get("spdisplays_ndrvs")?.as_array()?.first()?;
    let res_str = ndrv.get("spdisplays_resolution")?.as_str()?;
    parse_resolution(res_str)
}

pub(crate) fn parse_resolution(res_str: &str) -> Option<(u32, u32)> {
    let parts: Vec<&str> = res_str
        .split(|c| c == 'x' || c == '×' || c == ',')
        .collect();
    if parts.len() < 2 {
        return None;
    }
    let w = parts.get(0)?.trim().parse().ok()?;
    let h = parts.get(1)?.trim().parse().ok()?;
    Some((w, h))
}
