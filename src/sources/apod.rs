use crate::{
    download_to_path, ensure_http_success, finish_spinner, log, log_verbose, start_spinner,
    unique_temp_path, CacheManager, Result, Settings, WallpaperCandidate, WallpaperError,
    WallpaperSource, METADATA_TIMEOUT,
};
use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::env;
use chrono::{Datelike, Duration as ChronoDuration, NaiveDate};

use super::{FetchResult, Source, SourceContext};

pub const APOD_URL: &str = "https://api.nasa.gov/planetary/apod";
pub const APOD_DEFAULT_KEY: &str = "DEMO_KEY";

#[derive(Debug, Clone)]
pub struct ApodSettings {
    pub api_key: String,
    pub crop: bool,
    pub url_override: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct ApodConfig {
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub crop: Option<bool>,
    #[serde(default)]
    pub url_override: Option<String>,
}

impl ApodSettings {
    pub fn from_config(config: Option<&crate::AppConfig>) -> Self {
        let (api_key, crop, url_override, top_level_key) = if let Some(cfg) = config {
            (
                cfg.apod.as_ref().and_then(|a| a.api_key.clone()),
                cfg.apod.as_ref().and_then(|a| a.crop),
                cfg.apod.as_ref().and_then(|a| a.url_override.clone()),
                cfg.apod_api_key.clone(),
            )
        } else {
            (None, None, None, None)
        };

        let key = api_key
            .or(top_level_key)
            .or_else(|| env::var("NASA_API_KEY").ok())
            .unwrap_or_else(|| APOD_DEFAULT_KEY.to_string());

        Self {
            api_key: key,
            crop: crop.unwrap_or(true),
            url_override,
        }
    }
}

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
        let candidate = fetch_apod_candidate(
            ctx.client,
            ctx.cache,
            ctx.settings,
            ctx.date_label,
            &ctx.source_settings.apod,
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
    apod_settings: &ApodSettings,
) -> Result<WallpaperCandidate> {
    fetch_apod_candidate_with_fallback(
        client,
        cache,
        settings,
        date_label,
        date_label,
        apod_settings,
        true,
    )
}

fn fetch_apod_candidate_with_fallback(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    requested_date_label: &str,
    apod_date_label: &str,
    apod_settings: &ApodSettings,
    allow_fallback: bool,
) -> Result<WallpaperCandidate> {
    let candidate_id = format!("apod-{requested_date_label}");
    if let Some(candidate) = cache.find_candidate_by_id(requested_date_label, &candidate_id)? {
        if candidate.local_path.exists() && (!settings.force || settings.offline) {
            log_verbose(
                &format!("Using cached APOD wallpaper for {}", requested_date_label),
                settings,
            );
            return Ok(candidate);
        }
    }

    if let Some(message) = read_apod_skip(cache, requested_date_label, settings) {
        return Err(WallpaperError::Message(message));
    }

    if settings.offline {
        return Err(WallpaperError::Message(format!(
            "Offline mode enabled; no cached APOD wallpaper for {requested_date_label}."
        )));
    }

    let apod = fetch_apod(client, settings, apod_settings, apod_date_label)?;
    if apod.media_type != "image" {
        if allow_fallback {
            if let Some(fallback_label) = fallback_date_label(apod_date_label) {
                log_verbose(
                    &format!(
                        "APOD media type is not an image; trying {} instead.",
                        fallback_label
                    ),
                    settings,
                );
                return fetch_apod_candidate_with_fallback(
                    client,
                    cache,
                    settings,
                    requested_date_label,
                    &fallback_label,
                    apod_settings,
                    false,
                );
            }
        }
        if !settings.force {
            write_apod_skip(cache, requested_date_label)?;
        }
        return Err(WallpaperError::Message(
            "APOD media type is not an image; skipping.".to_string(),
        ));
    }
    // Prefer HD when available to keep full resolution.
    let image_url = apod.hdurl.clone().unwrap_or(apod.url.clone());
    if image_url.is_empty() {
        return Err(WallpaperError::Message(
            "APOD response missing image URL.".to_string(),
        ));
    }

    let media_dir = cache.media_dir(requested_date_label, WallpaperSource::Apod);
    fs::create_dir_all(&media_dir)?;
    let file_name = format!("apod_{requested_date_label}.jpg");
    let target_path = media_dir.join(file_name);
    let download = download_to_path(client, &image_url, &target_path, settings)?;

    let candidate = WallpaperCandidate {
        id: candidate_id,
        source: WallpaperSource::Apod,
        title: apod.title.clone(),
        description: apod.explanation.clone(),
        attribution: apod.copyright.clone(),
        info_url: None,
        image_url,
        local_path: download.path,
        date: apod_date_label.to_string(),
        metadata_xml: None,
    };
    log_verbose(
        &format!("APOD image URL: {}", candidate.image_url),
        settings,
    );
    if apod_settings.crop {
        let spinner = start_spinner(settings, "Processing APOD image…");
        match crop_and_resize_apod(&candidate.local_path) {
            Ok(()) => finish_spinner(spinner, "Processed APOD image", settings, false),
            Err(err) => {
                finish_spinner(spinner, "", settings, true);
                log(
                    &format!("APOD crop/resize failed; using original image. {err}"),
                    settings.quiet,
                );
            }
        }
    } else {
        log_verbose("APOD cropping disabled; using original image.", settings);
    }

    cache.upsert_candidate(requested_date_label, candidate.clone())?;
    Ok(candidate)
}

fn fetch_apod(
    client: &Client,
    settings: &Settings,
    apod_settings: &ApodSettings,
    date_label: &str,
) -> Result<ApodResponse> {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("api_key", &apod_settings.api_key);
    serializer.append_pair("date", date_label);
    let base = apod_settings.url_override.as_deref().unwrap_or(APOD_URL);
    let url = format!("{base}?{}", serializer.finish());
    log_verbose(&format!("Fetching APOD metadata: {}", url), settings);
    let response = client.get(&url).timeout(METADATA_TIMEOUT).send()?;
    ensure_http_success(response.status(), &url)?;
    let body = response.bytes()?.to_vec();
    let parsed: ApodResponse = serde_json::from_slice(&body)?;
    Ok(parsed)
}

fn apod_skip_path(cache: &CacheManager, date_label: &str) -> std::path::PathBuf {
    cache
        .media_dir(date_label, WallpaperSource::Apod)
        .join("apod_skip.txt")
}

fn read_apod_skip(
    cache: &CacheManager,
    date_label: &str,
    settings: &Settings,
) -> Option<String> {
    if settings.force {
        return None;
    }
    let path = apod_skip_path(cache, date_label);
    if !path.exists() {
        return None;
    }
    log_verbose(
        &format!("Using cached APOD skip for {}", date_label),
        settings,
    );
    fs::read_to_string(path)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some("APOD media type is not an image; skipping.".to_string()))
}

fn write_apod_skip(cache: &CacheManager, date_label: &str) -> Result<()> {
    let path = apod_skip_path(cache, date_label);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, "APOD media type is not an image; skipping.")?;
    Ok(())
}

fn fallback_date_label(date_label: &str) -> Option<String> {
    let date = NaiveDate::parse_from_str(date_label, "%Y-%m-%d").ok()?;
    let year = date.year();
    let fallback = date
        .with_year(year - 1)
        .or_else(|| date.checked_sub_signed(ChronoDuration::days(365)))?;
    Some(fallback.to_string())
}

fn crop_and_resize_apod(path: &Path) -> Result<()> {
    let img = image::open(path)
        .map_err(|err| WallpaperError::Message(format!("Unable to read APOD image: {err}")))?;
    let (orig_w, orig_h) = img.dimensions();
    if orig_w == 0 || orig_h == 0 {
        return Ok(());
    }

    let detected = detect_primary_display_size();
    if detected.is_none() {
        // No display info: keep original size but ensure RGB + JPEG.
        let rgb = img.to_rgb8();
        let temp_path = unique_temp_path(path);
        rgb.save_with_format(&temp_path, ImageFormat::Jpeg)
            .map_err(|err| {
                WallpaperError::Message(format!("Unable to save processed APOD image: {err}"))
            })?;
        if let Ok(f) = fs::File::open(&temp_path) {
            let _ = f.sync_all();
        }
        fs::rename(&temp_path, path)?;
        return Ok(());
    }
    let (target_w, target_h) = detected.unwrap();
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

    let processed: image::DynamicImage = if target_w <= orig_w && target_h <= orig_h {
        image::DynamicImage::ImageRgba8(image::imageops::resize(
            &cropped,
            target_w,
            target_h,
            FilterType::Lanczos3,
        ))
    } else {
        // Avoid upscaling; keep cropped size.
        image::DynamicImage::ImageRgba8(cropped.to_rgba8())
    };

    let processed_rgb = processed.to_rgb8();

    let temp_path = unique_temp_path(path);
    processed_rgb
        .save_with_format(&temp_path, ImageFormat::Jpeg)
        .map_err(|err| {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::SourceSettings;
    use crate::{build_client, CacheManager, Settings, WallpaperSource, DEFAULT_INFO_WRAP_WIDTH};
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use image::{ImageBuffer, Rgba};
    use std::path::Path;
    use tempfile::tempdir;

    fn make_settings(tmpdir: &Path, force: bool) -> Settings {
        Settings {
            proto: "https".into(),
            picture_dir: tmpdir.to_path_buf(),
            favorites_dir: tmpdir.join("favorites"),
            auto_update_name: "default".into(),
            monitor: 0,
            force,
            offline: false,
            verbose: false,
            quiet: true,
            experimental: false,
            filename: None,
            source: WallpaperSource::Apod,
            prune_cache_days: None,
            info_wrap_width: DEFAULT_INFO_WRAP_WIDTH,
            info_plain_text: false,
        }
    }

    #[test]
    fn crop_and_resize_handles_rgba_by_saving_jpeg() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("input.png");

        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(10, 10, |_, _| Rgba([10, 20, 30, 255]));
        img.save(&path).unwrap();

        crop_and_resize_apod(&path).unwrap();
        let loaded = image::ImageReader::open(&path)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .decode()
            .unwrap();
        assert!(!loaded.color().has_alpha(), "should be RGB after re-save");
    }

    #[test]
    fn apod_falls_back_one_year_on_video() {
        let server = MockServer::start();
        let api_url = server.url("/apod");
        let img_url = server.url("/image.jpg");

        let api_mock_today = server.mock(|when, then| {
            when.method(GET)
                .path("/apod")
                .query_param("api_key", "TEST")
                .query_param("date", "2024-01-02");
            then.status(200).body(
                r#"{
                "url":"https://example.com/video.mp4",
                "media_type":"video",
                "title":"vid"
            }"#,
            );
        });
        let api_mock_fallback = server.mock(|when, then| {
            when.method(GET)
                .path("/apod")
                .query_param("api_key", "TEST")
                .query_param("date", "2023-01-02");
            then.status(200).body(
                r#"{
                "url":"IMAGE_URL",
                "media_type":"image",
                "title":"nebula",
                "explanation":"desc"
            }"#
                .replace("IMAGE_URL", &img_url),
            );
        });
        let img_mock = server.mock(|when, then| {
            when.method(GET).path("/image.jpg");
            then.status(200).body("img-bytes");
        });

        let tmpdir = tempdir().unwrap();
        let settings = make_settings(tmpdir.path(), false);
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.apod.api_key = "TEST".into();
        source_settings.apod.url_override = Some(api_url);
        source_settings.apod.crop = false;

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = "2024-01-02";

        let candidate = fetch_apod_candidate(
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings.apod,
        )
        .unwrap();

        assert_eq!(candidate.date, "2023-01-02");
        assert_eq!(api_mock_today.hits(), 1);
        assert_eq!(api_mock_fallback.hits(), 1);
        assert_eq!(img_mock.hits(), 1);

        let candidate = fetch_apod_candidate(
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings.apod,
        )
        .unwrap();
        assert_eq!(candidate.date, "2023-01-02");
        assert_eq!(api_mock_today.hits(), 1);
        assert_eq!(api_mock_fallback.hits(), 1);
        assert_eq!(img_mock.hits(), 1);
    }

    #[test]
    fn apod_errors_on_video() {
        let server = MockServer::start();
        let api_url = server.url("/apod");
        let api_mock_today = server.mock(|when, then| {
            when.method(GET)
                .path("/apod")
                .query_param("api_key", "TEST")
                .query_param("date", "2024-01-02");
            then.status(200).body(
                r#"{
                "url":"https://example.com/video.mp4",
                "media_type":"video",
                "title":"vid"
            }"#,
            );
        });
        let api_mock_fallback = server.mock(|when, then| {
            when.method(GET)
                .path("/apod")
                .query_param("api_key", "TEST")
                .query_param("date", "2023-01-02");
            then.status(200).body(
                r#"{
                "url":"https://example.com/video2.mp4",
                "media_type":"video",
                "title":"vid2"
            }"#,
            );
        });

        let tmpdir = tempdir().unwrap();
        let settings = make_settings(tmpdir.path(), false);
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.apod.api_key = "TEST".into();
        source_settings.apod.url_override = Some(api_url);
        source_settings.apod.crop = false;

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = "2024-01-02";

        let err = fetch_apod_candidate(
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings.apod,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not an image"));
        assert_eq!(api_mock_today.hits(), 1);
        assert_eq!(api_mock_fallback.hits(), 1);

        let err = fetch_apod_candidate(
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings.apod,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not an image"));
        assert_eq!(api_mock_today.hits(), 1);
        assert_eq!(api_mock_fallback.hits(), 1);
    }
}
