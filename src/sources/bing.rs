use crate::{
    ensure_http_success, ensure_info_file, finish_spinner, log, log_verbose, start_spinner,
    write_bytes_atomic, CacheManager, Result, Settings, WallpaperCandidate, WallpaperError,
    WallpaperSource, DEFAULT_RESOLUTIONS, IMAGE_TIMEOUT, METADATA_TIMEOUT,
};
use reqwest::blocking::Client;
use serde::Deserialize;
use roxmltree;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{FetchResult, Source, SourceContext};

#[derive(Debug, Clone)]
pub struct BingSettings {
    pub host: String,
    pub country: Option<String>,
    pub resolutions: Vec<String>,
    pub day: i32,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub struct BingConfig {
    #[serde(default)]
    pub country: Option<String>,
    #[serde(default)]
    pub resolutions: Option<Vec<String>>,
}

impl BingSettings {
    pub fn from_config(config: Option<&crate::AppConfig>) -> Self {
        let country = config
            .and_then(|cfg| {
                cfg.country
                    .clone()
                    .or_else(|| cfg.bing.as_ref().and_then(|b| b.country.clone()))
            });

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
            country,
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
        let resolutions = if ctx.source_settings.bing.resolutions.is_empty() {
            DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect()
        } else {
            ctx.source_settings.bing.resolutions.clone()
        };

        let fetched = fetch_bing_candidate(
            ctx.client,
            ctx.cache,
            ctx.settings,
            ctx.date_label,
            &ctx.source_settings.bing,
            resolutions,
        )?;
        Ok(FetchResult::single(
            fetched.candidate,
            fetched.skipped_download,
        ))
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
    resolutions: Vec<String>,
) -> Result<FetchedCandidate> {
    if let Some(candidate) = cache.find_candidate(date_label, WallpaperSource::Bing)? {
        if candidate.local_path.exists() && (!settings.force || settings.offline) {
            log_verbose(
                &format!(
                    "Using cached wallpaper for {} from {:?}",
                    date_label, candidate.source
                ),
                settings,
            );
            ensure_info_file(&settings.picture_dir, &candidate)?;
            return Ok(FetchedCandidate {
                candidate,
                skipped_download: true,
            });
        }
    }

    if settings.offline {
        return Err(WallpaperError::Message(format!(
            "Offline mode enabled; no cached Bing wallpaper for {date_label}."
        )));
    }

    let archive_url = build_archive_url(bing_settings.day, bing_settings.country.as_deref());
    log_verbose(
        &format!("Fetching Bing metadata: {}", archive_url),
        settings,
    );
    let (url_base, metadata_body) = fetch_image_metadata(client, &archive_url)?;
    let metadata = parse_bing_metadata(&metadata_body)?;

    let mut last_error: Option<WallpaperError> = None;
    for res in resolutions {
        match download_image(
            client,
            &url_base,
            &res,
            bing_settings,
            settings,
            &metadata_body,
        ) {
            Ok(downloaded) => {
                let candidate = WallpaperCandidate {
                    id: format!("bing-{date_label}-{res}"),
                    source: WallpaperSource::Bing,
                    title: metadata.headline.clone(),
                    description: None,
                    attribution: metadata.copyright.clone(),
                    info_url: metadata.copyright_link.clone(),
                    image_url: downloaded.image_url.clone(),
                    local_path: downloaded.path.clone(),
                    date: date_label.to_string(),
                    metadata_xml: Some(String::from_utf8_lossy(&metadata_body).to_string()),
                };

                cache.upsert_candidate(date_label, candidate.clone())?;

                return Ok(FetchedCandidate {
                    candidate,
                    skipped_download: downloaded.skipped,
                });
            }
            Err(err) => {
                log(&format!("Resolution {res} failed: {err}"), settings.quiet);
                last_error = Some(err);
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

#[derive(Debug, Default, Clone)]
pub(crate) struct BingMetadata {
    pub headline: Option<String>,
    pub copyright: Option<String>,
    pub copyright_link: Option<String>,
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
}

pub(crate) fn download_image(
    client: &Client,
    url_base: &str,
    resolution: &str,
    bing_settings: &BingSettings,
    settings: &Settings,
    metadata_body: &[u8],
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
    let filename_local = format!("{}-{filename_local}", settings.auto_update_name);
    let target_path = settings.picture_dir.join(filename_local);

    if target_path.exists() && !settings.force {
        let name = target_path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_else(|| target_path.to_string_lossy());
        log_verbose(
            &format!("Skipping download, already present: {name}"),
            settings,
        );
        return Ok(DownloadedImage {
            path: target_path,
            skipped: true,
            image_url: file_url,
        });
    }

    log_verbose(
        &format!("Downloading {resolution} from {file_url}"),
        settings,
    );
    let spinner = start_spinner(
        settings,
        format!("Downloading Bing {resolution} wallpaper…"),
    );

    let temp_path = crate::unique_temp_path(&target_path);
    let download_result = (|| -> Result<()> {
        let _ = fs::remove_file(&temp_path);

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

        for entry in fs::read_dir(&settings.picture_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path == target_path {
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) == Some("jpg") {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with(&format!("{}-", settings.auto_update_name)) {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }

        fs::rename(&temp_path, &target_path)?;
        write_bytes_atomic(&settings.picture_dir.join("info.xml"), metadata_body)?;
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

    Ok(DownloadedImage {
        path: target_path,
        skipped: false,
        image_url: file_url,
    })
}
