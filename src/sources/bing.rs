use crate::{
    ensure_info_file, log, write_bytes_atomic, CacheManager, Result, Settings, WallpaperCandidate,
    WallpaperError, WallpaperSource, DEFAULT_RESOLUTIONS, IMAGE_TIMEOUT, METADATA_TIMEOUT,
};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use roxmltree;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::{FetchResult, Source, SourceContext};

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
        let resolutions: Vec<String> = if ctx.resolutions.is_empty() {
            DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect()
        } else {
            ctx.resolutions.to_vec()
        };

        let fetched = fetch_bing_candidate(
            ctx.client,
            ctx.cache,
            ctx.settings,
            ctx.date_label,
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
        _settings: &Settings,
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
    resolutions: Vec<String>,
) -> Result<FetchedCandidate> {
    if !settings.force {
        if let Some(candidate) = cache.find_candidate(date_label, WallpaperSource::Bing)? {
            if candidate.local_path.exists() {
                log(
                    &format!(
                        "Using cached wallpaper for {} from {:?}",
                        date_label, candidate.source
                    ),
                    settings.quiet,
                );
                ensure_info_file(&settings.picture_dir, &candidate)?;
                return Ok(FetchedCandidate {
                    candidate,
                    skipped_download: true,
                });
            }
        }
    }

    let archive_url = build_archive_url(settings.day, settings.country.as_deref());
    let (url_base, metadata_body) = fetch_image_metadata(client, &archive_url)?;
    let metadata = parse_bing_metadata(&metadata_body)?;

    let mut last_error: Option<WallpaperError> = None;
    for res in resolutions {
        match download_image(client, &url_base, &res, settings, &metadata_body) {
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

    let status = response.status();
    if status != StatusCode::OK {
        return Err(WallpaperError::Status {
            url: archive_url.to_string(),
            status: status.as_u16(),
        });
    }

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
    settings: &Settings,
    metadata_body: &[u8],
) -> Result<DownloadedImage> {
    let file_url_with_res = format!("{url_base}_{resolution}.jpg");
    let file_url = format!(
        "{}://{}/{}",
        settings.proto,
        settings.bing_host,
        file_url_with_res.trim_start_matches('/')
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
        log(
            &format!("Skipping download, already present: {name}"),
            settings.quiet,
        );
        return Ok(DownloadedImage {
            path: target_path,
            skipped: true,
            image_url: file_url,
        });
    }

    log(
        &format!("Downloading {resolution} from {file_url}"),
        settings.quiet,
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
        if status != StatusCode::OK {
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
        return Err(err);
    }

    Ok(DownloadedImage {
        path: target_path,
        skipped: false,
        image_url: file_url,
    })
}
