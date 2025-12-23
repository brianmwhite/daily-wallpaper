use crate::{
    ensure_http_success, ensure_info_file, finish_spinner, log, log_verbose, start_spinner,
    write_bytes_atomic, CacheManager, Result, Settings, WallpaperCandidate, WallpaperError,
    WallpaperSource, DEFAULT_RESOLUTIONS, IMAGE_TIMEOUT, METADATA_TIMEOUT,
};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use serde::Deserialize;
use roxmltree;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::{FetchResult, Source, SourceContext};

#[derive(Debug, Clone)]
pub struct BingSettings {
    pub host: String,
    pub countries: Vec<String>,
    pub resolutions: Vec<String>,
    pub day: i32,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct BingConfig {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub countries: Option<Vec<String>>,
    #[serde(default)]
    pub resolutions: Option<Vec<String>>,
}

impl BingSettings {
    pub fn from_config(config: Option<&crate::AppConfig>) -> Self {
        let countries = config
            .and_then(|cfg| cfg.bing.as_ref().and_then(|b| b.countries.clone()))
            .filter(|list| !list.is_empty())
            .or_else(|| {
                config.and_then(|cfg| {
                    cfg.bing
                        .as_ref()
                        .and_then(|b| b.country.clone())
                        .or_else(|| cfg.country.clone())
                        .map(|country| vec![country])
                })
            })
            .unwrap_or_else(|| vec!["en-US".to_string()]);

        let mut normalized: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for country in countries {
            let trimmed = country.trim();
            if trimmed.is_empty() {
                continue;
            }
            if seen.insert(trimmed.to_string()) {
                normalized.push(trimmed.to_string());
            }
        }
        if normalized.is_empty() {
            normalized.push("en-US".to_string());
        }

        let resolutions = config
            .and_then(|cfg| {
                cfg.bing
                    .as_ref()
                    .and_then(|b| b.resolutions.clone())
                    .or_else(|| cfg.resolutions.clone())
            })
            .unwrap_or_else(|| DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect());

        Self {
            host: "www.bing.com".to_string(),
            countries: normalized,
            resolutions,
            day: 0,
        }
    }
}

pub struct BingSource;

impl Source for BingSource {
    fn id(&self) -> WallpaperSource {
        WallpaperSource::Bing
    }

    fn label(&self) -> &'static str {
        "Bing"
    }

    fn description(&self) -> &'static str {
        "Bing daily wallpaper"
    }

    fn fetch(&self, ctx: &SourceContext<'_>) -> Result<FetchResult> {
        if !ctx.settings.force {
            if let Some(skip) = ctx.cache.read_skip(ctx.date_label, WallpaperSource::Bing)? {
                log_verbose(
                    &format!(
                        "Skipping Bing for {} ({}).",
                        ctx.date_label, skip.reason
                    ),
                    ctx.settings,
                );
                return Err(WallpaperError::Message(format!(
                    "Bing skipped for {}.",
                    ctx.date_label
                )));
            }
        }
        let resolutions = if ctx.source_settings.bing.resolutions.is_empty() {
            DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect()
        } else {
            ctx.source_settings.bing.resolutions.clone()
        };

        let countries = if ctx.source_settings.bing.countries.is_empty() {
            vec!["en-US".to_string()]
        } else {
            ctx.source_settings.bing.countries.clone()
        };

        let mut candidates = Vec::new();
        let mut skipped_download = true;
        let mut last_error: Option<WallpaperError> = None;
        let mut seen_images = std::collections::HashSet::new();

        for country in countries {
            match fetch_bing_candidate(
                ctx.client,
                ctx.cache,
                ctx.settings,
                ctx.date_label,
                &ctx.source_settings.bing,
                &country,
                resolutions.clone(),
            ) {
                Ok(fetched) => {
                    if !fetched.skipped_download {
                        skipped_download = false;
                    }
                    let dedupe_key = candidate_dedupe_key(&fetched.candidate);
                    if seen_images.insert(dedupe_key) {
                        candidates.push(fetched.candidate);
                    }
                }
                Err(err) => {
                    last_error = Some(err);
                }
            }
        }

        if candidates.is_empty() {
            return Err(last_error.unwrap_or_else(|| {
                WallpaperError::Message("No Bing candidates found.".to_string())
            }));
        }

        Ok(FetchResult {
            candidates,
            skipped_download,
        })
    }

    fn pick_default<'a>(
        &self,
        candidates: &'a [WallpaperCandidate],
        _ctx: &SourceContext<'_>,
    ) -> Result<&'a WallpaperCandidate> {
        candidates
            .first()
            .ok_or_else(|| crate::WallpaperError::Message("No Bing candidates found.".into()))
    }
}

const BING_ARCHIVE_URL: &str = "https://www.bing.com/HPImageArchive.aspx";

pub(crate) struct FetchedCandidate {
    pub candidate: WallpaperCandidate,
    pub skipped_download: bool,
}

pub(crate) fn fetch_bing_candidate(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
    bing_settings: &BingSettings,
    country: &str,
    resolutions: Vec<String>,
) -> Result<FetchedCandidate> {
    if let Some(mut candidate) =
        find_cached_candidate_for_country(cache, date_label, country, &resolutions)?
    {
        if candidate.local_path.exists() && (!settings.force || settings.offline) {
            if candidate.checksum.is_none() {
                if let Ok(Some(checksum)) = compute_checksum(&candidate.local_path) {
                    candidate.checksum = Some(checksum);
                    cache.upsert_candidate(date_label, candidate.clone())?;
                }
            }
            if !settings.refresh_metadata && !settings.offline {
                log_verbose(
                    &format!(
                        "Using cached Bing wallpaper for {} ({})",
                        date_label, country
                    ),
                    settings,
                );
                ensure_info_file(&candidate)?;
                return Ok(FetchedCandidate {
                    candidate,
                    skipped_download: true,
                });
            }
            if settings.offline {
                log_verbose(
                    &format!(
                        "Using cached Bing wallpaper for {} ({})",
                        date_label, country
                    ),
                    settings,
                );
                ensure_info_file(&candidate)?;
                return Ok(FetchedCandidate {
                    candidate,
                    skipped_download: true,
                });
            }

            let cached_date = candidate.metadata_xml.as_deref().and_then(|xml| {
                metadata_date_label(xml.as_bytes())
                    .ok()
                    .and_then(|date| date)
            });
            if let Some(cached_date) = cached_date {
                if cached_date == date_label {
                    log_verbose(
                        &format!(
                            "Using cached Bing wallpaper for {} ({})",
                            date_label, country
                        ),
                        settings,
                    );
                    ensure_info_file(&candidate)?;
                    return Ok(FetchedCandidate {
                        candidate,
                        skipped_download: true,
                    });
                }
                log_verbose(
                    &format!(
                        "Cached Bing metadata date {cached_date} does not match {date_label}; checking for update."
                    ),
                    settings,
                );
            } else {
                log_verbose(
                    "Cached Bing metadata missing startdate; checking for update.",
                    settings,
                );
            }
        }
    }

    if settings.offline {
        return Err(WallpaperError::Message(format!(
            "Offline mode enabled; no cached Bing wallpaper for {date_label} ({country})."
        )));
    }

    let archive_url = build_archive_url(bing_settings.day, Some(country));
    log_verbose(
        &format!("Fetching Bing metadata ({country}): {}", archive_url),
        settings,
    );
    let (url_base, metadata_body) = fetch_image_metadata(client, &archive_url)?;
    let metadata = parse_bing_metadata(&metadata_body)?;
    let metadata_date = metadata_date_label(&metadata_body).ok().and_then(|date| date);

    let mut last_error: Option<WallpaperError> = None;
        for res in resolutions {
            match download_image(
                client,
                &url_base,
                &res,
            bing_settings,
            country,
            settings,
            &metadata_body,
                cache,
                date_label,
            ) {
                Ok(downloaded) => {
                let candidate_date =
                    metadata_date.clone().unwrap_or_else(|| date_label.to_string());
                let candidate = WallpaperCandidate {
                    id: format!("bing-{date_label}-{country}-{res}"),
                    source: WallpaperSource::Bing,
                    title: metadata.headline.clone(),
                    description: None,
                    attribution: metadata.copyright.clone(),
                    info_url: metadata.copyright_link.clone(),
                    image_url: downloaded.image_url.clone(),
                    local_path: downloaded.path.clone(),
                    date: candidate_date,
                    metadata_xml: Some(String::from_utf8_lossy(&metadata_body).to_string()),
                    checksum: downloaded.checksum.clone(),
                };

                cache.upsert_candidate(date_label, candidate.clone())?;

                return Ok(FetchedCandidate {
                    candidate,
                    skipped_download: downloaded.skipped,
                });
                }
                Err(err) => {
                    if matches!(err, WallpaperError::MinResolution { .. }) {
                        let _ = cache.write_skip(date_label, WallpaperSource::Bing, "min_resolution");
                        return Err(err);
                    }
                    if let WallpaperError::DownloadStatus { status: 404, .. } = err {
                        log_verbose(
                            &format!("Resolution {res} not available; skipping."),
                            settings,
                    );
                } else {
                    log(&format!("Resolution {res} failed: {err}"), settings.quiet);
                    last_error = Some(err);
                }
            }
        }
    }

    if let Some(err) = last_error {
        Err(err)
    } else {
        Err(WallpaperError::Message(
            "Unable to download wallpaper for any resolution.".to_string(),
        ))
    }
}

fn find_cached_candidate_for_country(
    cache: &CacheManager,
    date_label: &str,
    country: &str,
    resolutions: &[String],
) -> Result<Option<WallpaperCandidate>> {
    let candidates = cache.find_candidates_by_source(date_label, WallpaperSource::Bing)?;
    let prefix = format!("bing-{date_label}-{country}-");
    let mut matches: Vec<WallpaperCandidate> = candidates
        .into_iter()
        .filter(|candidate| candidate.id.starts_with(&prefix))
        .collect();

    if matches.is_empty() && country == "en-US" {
        if let Some(candidate) = cache.find_candidate(date_label, WallpaperSource::Bing)? {
            matches.push(candidate);
        }
    }

    if matches.is_empty() {
        return Ok(None);
    }

    let cache_dir = cache.media_dir(date_label, WallpaperSource::Bing);
    let mut preferred: Vec<WallpaperCandidate> = matches
        .iter()
        .cloned()
        .filter(|candidate| candidate.local_path.exists())
        .collect();

    if preferred.is_empty() {
        return Ok(matches.into_iter().next());
    }

    let in_cache: Vec<WallpaperCandidate> = preferred
        .iter()
        .cloned()
        .filter(|candidate| candidate.local_path.starts_with(&cache_dir))
        .collect();
    if !in_cache.is_empty() {
        preferred = in_cache;
    }

    let resolution_rank = |candidate: &WallpaperCandidate| -> usize {
        let res = candidate
            .id
            .rsplitn(2, '-')
            .next()
            .unwrap_or_default();
        resolutions
            .iter()
            .position(|item| item == res)
            .unwrap_or(usize::MAX)
    };

    preferred.sort_by_key(|candidate| resolution_rank(candidate));
    Ok(preferred.into_iter().next())
}

#[derive(Debug, Default, Clone)]
pub(crate) struct BingMetadata {
    pub headline: Option<String>,
    pub copyright: Option<String>,
    pub copyright_link: Option<String>,
}

fn candidate_dedupe_key(candidate: &WallpaperCandidate) -> String {
    if let Some(checksum) = candidate.checksum.as_deref() {
        return checksum.to_string();
    }

    if let Some(xml) = candidate.metadata_xml.as_deref() {
        if let Ok(Some(url_base)) = parse_xml_text(xml.as_bytes(), "urlBase") {
            return url_base;
        }
        if let Ok(Some(hash)) = parse_xml_text(xml.as_bytes(), "hsh") {
            return hash;
        }
    }

    if let Some(start) = candidate.image_url.find("/th?id=") {
        let tail = &candidate.image_url[start..];
        if let Some((base, _)) = tail.split_once('_') {
            return base.to_string();
        }
        return tail.to_string();
    }

    candidate.image_url.clone()
}

fn compute_checksum(path: &Path) -> Result<Option<String>> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(err.into());
        }
    };
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    let digest = hasher.finalize();
    Ok(Some(format!("{:x}", digest)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn candidate_dedupe_key_uses_url_base_from_metadata() {
        let xml = include_str!("../../docs/bing.xml");
        let candidate = WallpaperCandidate {
            id: "bing-2025-12-22-en-US-1920x1080".into(),
            source: WallpaperSource::Bing,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://www.bing.com/th?id=OHR.NutcrackerAnkara_EN-US5537620581_1920x1080.jpg".into(),
            local_path: PathBuf::from("dummy.jpg"),
            date: "2025-12-22".into(),
            metadata_xml: Some(xml.to_string()),
            checksum: None,
        };

        let key = candidate_dedupe_key(&candidate);
        assert_eq!(key, "/th?id=OHR.NutcrackerAnkara_EN-US5537620581");
    }

    #[test]
    fn candidate_dedupe_key_prefers_checksum_when_file_exists() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallpaper.jpg");
        fs::write(&path, b"same-image").unwrap();

        let checksum = compute_checksum(&path).unwrap().unwrap();
        let candidate = WallpaperCandidate {
            id: "bing-2025-12-22-es-1920x1080".into(),
            source: WallpaperSource::Bing,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://www.bing.com/th?id=OHR.DifferentId_1920x1080.jpg".into(),
            local_path: path.clone(),
            date: "2025-12-22".into(),
            metadata_xml: None,
            checksum: Some(checksum.clone()),
        };

        let key = candidate_dedupe_key(&candidate);
        assert_eq!(key, checksum);
    }
}

pub(crate) fn build_archive_url(day: i32, country: Option<&str>) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("format", "xml");
    serializer.append_pair("idx", &day.to_string());
    serializer.append_pair("n", "1");
    if let Some(c) = country {
        serializer.append_pair("mkt", c);
    }
    format!("{BING_ARCHIVE_URL}?{}", serializer.finish())
}

fn fetch_image_metadata(client: &Client, archive_url: &str) -> Result<(String, Vec<u8>)> {
    let response = client.get(archive_url).timeout(METADATA_TIMEOUT).send()?;
    ensure_http_success(response.status(), archive_url)?;

    let body = response.bytes()?.to_vec();
    let url_base = parse_xml_text(&body, "urlBase")?.ok_or(WallpaperError::MissingImageUrl)?;
    Ok((url_base, body))
}

pub(crate) fn parse_bing_metadata(body: &[u8]) -> Result<BingMetadata> {
    let text = std::str::from_utf8(body).map_err(|_| WallpaperError::MetadataParse)?;
    let doc = roxmltree::Document::parse(text).map_err(|_| WallpaperError::MetadataParse)?;
    let headline = doc
        .descendants()
        .find(|n| n.has_tag_name("headline"))
        .and_then(|n| n.text())
        .map(|t| t.to_string());
    let copyright = doc
        .descendants()
        .find(|n| n.has_tag_name("copyright"))
        .and_then(|n| n.text())
        .map(|t| t.to_string());
    let copyright_link = doc
        .descendants()
        .find(|n| n.has_tag_name("copyrightlink"))
        .and_then(|n| n.text())
        .map(|t| t.to_string());
    Ok(BingMetadata {
        headline,
        copyright,
        copyright_link,
    })
}

fn parse_xml_text(body: &[u8], tag: &str) -> Result<Option<String>> {
    let text = std::str::from_utf8(body).map_err(|_| WallpaperError::MetadataParse)?;
    let doc = roxmltree::Document::parse(text).map_err(|_| WallpaperError::MetadataParse)?;
    Ok(doc
        .descendants()
        .find(|node| node.has_tag_name(tag))
        .and_then(|node| node.text().map(|t| t.to_string())))
}

fn normalize_startdate(startdate: &str) -> Option<String> {
    let trimmed = startdate.trim();
    if trimmed.len() != 8 || !trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(format!(
        "{}-{}-{}",
        &trimmed[0..4],
        &trimmed[4..6],
        &trimmed[6..8]
    ))
}

pub(crate) fn metadata_date_label(body: &[u8]) -> Result<Option<String>> {
    let startdate = parse_xml_text(body, "startdate")?;
    Ok(startdate.and_then(|raw| normalize_startdate(&raw)))
}

pub(crate) fn sanitize_filename(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return "wallpaper.jpg".to_string();
    }

    let path = Path::new(trimmed);
    let base = path
        .file_name()
        .unwrap_or_else(|| OsStr::new("wallpaper.jpg"))
        .to_string_lossy()
        .to_string();
    if Path::new(&base).extension().is_some() {
        base
    } else {
        format!("{base}.jpg")
    }
}

#[derive(Debug)]
pub(crate) struct DownloadedImage {
    pub path: PathBuf,
    pub skipped: bool,
    pub image_url: String,
    pub checksum: Option<String>,
}

pub(crate) fn download_image(
    client: &Client,
    url_base: &str,
    resolution: &str,
    bing_settings: &BingSettings,
    country: &str,
    settings: &Settings,
    metadata_body: &[u8],
    cache: &CacheManager,
    date_label: &str,
) -> Result<DownloadedImage> {
    let file_url_with_res = format!("{url_base}_{resolution}.jpg");
    let file_url = format!(
        "{}://{}/{}",
        settings.proto,
        bing_settings.host,
        file_url_with_res.trim_start_matches('/')
    );
    log_verbose(
        &format!("Bing image URL ({}): {}", resolution, file_url),
        settings,
    );

    let filename_local = if let Some(name) = &settings.filename {
        sanitize_filename(name)
    } else {
        file_url_with_res.replace("/th?id=", "")
    };
    let cleanup_prefix = if bing_settings.countries.len() > 1 {
        format!("{}-{}", settings.auto_update_name, country)
    } else {
        settings.auto_update_name.clone()
    };
    let filename_local = format!("{cleanup_prefix}-{filename_local}");
    let target_dir = cache.media_dir(date_label, WallpaperSource::Bing);
    let target_path = target_dir.join(filename_local);

    if target_path.exists() && !settings.force {
        let name = target_path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_else(|| target_path.to_string_lossy());
        log_verbose(
            &format!("Skipping download, already present: {name}"),
            settings,
        );
        let checksum = compute_checksum(&target_path)?;
        return Ok(DownloadedImage {
            path: target_path,
            skipped: true,
            image_url: file_url,
            checksum,
        });
    }

    log_verbose(
        &format!("Downloading {resolution} from {file_url}"),
        settings,
    );
    let message = format!("Downloading Bing {resolution} wallpaper…");
    crate::log_action_start(settings, &message);
    let spinner = start_spinner(settings, message);

    let temp_path = crate::unique_temp_path(&target_path);
    let download_result = (|| -> Result<()> {
        let _ = fs::remove_file(&temp_path);
        fs::create_dir_all(&target_dir)?;

        let mut response = client
            .get(&file_url)
            .timeout(IMAGE_TIMEOUT)
            .send()
            .map_err(|err| WallpaperError::Download {
                resolution: resolution.to_string(),
                source: err,
            })?;

        let status = response.status();
        if let Err(_err) = ensure_http_success(status, &file_url) {
            return Err(WallpaperError::DownloadStatus {
                resolution: resolution.to_string(),
                status: status.as_u16(),
            });
        }

        let mut file = File::create(&temp_path)?;
        response
            .copy_to(&mut file)
            .map_err(|err| WallpaperError::Download {
                resolution: resolution.to_string(),
                source: err,
            })?;
        file.flush()?;
        file.sync_all()?;

        for entry in fs::read_dir(&target_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path == target_path {
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) == Some("jpg") {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with(&format!("{cleanup_prefix}-")) {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }

        fs::rename(&temp_path, &target_path)?;
        if let Err(err) = crate::enforce_min_resolution(&target_path, settings) {
            let _ = fs::remove_file(&target_path);
            return Err(err);
        }
        write_bytes_atomic(&target_dir.join("info.xml"), metadata_body)?;
        Ok(())
    })();

    if let Err(err) = download_result {
        let _ = fs::remove_file(&temp_path);
        finish_spinner(spinner, "", settings, true);
        return Err(err);
    }

    finish_spinner(
        spinner,
        &format!("Downloaded Bing {resolution} wallpaper"),
        settings,
        false,
    );

    let checksum = compute_checksum(&target_path)?;

    Ok(DownloadedImage {
        path: target_path,
        skipped: false,
        image_url: file_url,
        checksum,
    })
}
