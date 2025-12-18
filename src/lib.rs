use chrono::{Duration as ChronoDuration, Local, NaiveDate};
use clap::{ArgAction, Parser, ValueEnum};
use plist::{Dictionary, Value};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use url::form_urlencoded;

const DEFAULT_RESOLUTIONS: &[&str] = &[
    "1920x1200",
    "1920x1080",
    "1024x768",
    "1280x720",
    "1366x768",
    "UHD",
];
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin";
const BING_ARCHIVE_URL: &str = "https://www.bing.com/HPImageArchive.aspx";
const USER_AGENT: &str = concat!(
    "bing-wallpaper-daily-mac-multimonitor/",
    env!("CARGO_PKG_VERSION")
);
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(60);
const PLIST_BASENAME: &str = "com.bing-wallpaper-daily-mac-multimonitor";
const CACHE_DIR_NAME: &str = "cache";
const CACHE_INDEX_FILE: &str = "index.json";
const LAST_APPLIED_FILE: &str = "last_applied.json";

pub type Result<T> = std::result::Result<T, WallpaperError>;

#[derive(Debug, Error)]
pub enum WallpaperError {
    #[error("{0}")]
    Message(String),
    #[error("Unexpected status {status} from {url}")]
    Status { url: String, status: u16 },
    #[error("Failed to download wallpaper at {resolution}: HTTP {status}")]
    DownloadStatus { resolution: String, status: u16 },
    #[error("Failed to download wallpaper at {resolution}")]
    Download {
        resolution: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("Could not parse Bing metadata response.")]
    MetadataParse,
    #[error("Bing response did not include an image URL.")]
    MissingImageUrl,
    #[error("{what} failed (exit {code}). {stderr}")]
    CommandFailed {
        what: &'static str,
        code: i32,
        stderr: String,
    },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error(transparent)]
    Plist(#[from] plist::Error),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
struct Settings {
    proto: String,
    country: Option<String>,
    day: i32,
    picture_dir: PathBuf,
    auto_update_name: String,
    monitor: usize,
    force: bool,
    quiet: bool,
    experimental: bool,
    filename: Option<String>,
    bing_host: String,
}

impl Settings {
    fn plist_filename(&self) -> PathBuf {
        launchd_dir().join(format!("{PLIST_BASENAME}-{}.plist", self.auto_update_name))
    }

    fn plist_label(&self) -> String {
        format!("{PLIST_BASENAME}.{}", self.auto_update_name)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum WallpaperSource {
    Bing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WallpaperCandidate {
    id: String,
    source: WallpaperSource,
    title: Option<String>,
    description: Option<String>,
    attribution: Option<String>,
    info_url: Option<String>,
    image_url: String,
    local_path: PathBuf,
    date: String,
    metadata_xml: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheIndex {
    date: String,
    candidates: Vec<WallpaperCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastApplied {
    candidate_id: String,
    source: WallpaperSource,
    applied_path: PathBuf,
    applied_at: u64,
}

struct CacheManager {
    base_dir: PathBuf,
}

impl CacheManager {
    fn new(picture_dir: &Path) -> Self {
        Self {
            base_dir: picture_dir.join(CACHE_DIR_NAME),
        }
    }

    fn index_path(&self, date: &str) -> PathBuf {
        self.base_dir.join(date).join(CACHE_INDEX_FILE)
    }

    fn last_applied_path(&self) -> PathBuf {
        self.base_dir.join(LAST_APPLIED_FILE)
    }

    fn load_index(&self, date: &str) -> Result<Option<CacheIndex>> {
        let path = self.index_path(date);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read(&path)?;
        let index = serde_json::from_slice::<CacheIndex>(&data)?;
        Ok(Some(index))
    }

    fn upsert_candidate(&self, date: &str, candidate: WallpaperCandidate) -> Result<()> {
        let mut index = self.load_index(date)?.unwrap_or_else(|| CacheIndex {
            date: date.to_string(),
            candidates: Vec::new(),
        });
        if let Some(existing) = index
            .candidates
            .iter_mut()
            .find(|item| item.id == candidate.id)
        {
            *existing = candidate;
        } else {
            index.candidates.push(candidate);
        }
        self.write_index(date, &index)
    }

    fn write_index(&self, date: &str, index: &CacheIndex) -> Result<()> {
        let path = self.index_path(date);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(index)?;
        write_bytes_atomic(&path, &bytes)?;
        Ok(())
    }

    fn find_candidate(
        &self,
        date: &str,
        source: WallpaperSource,
    ) -> Result<Option<WallpaperCandidate>> {
        let index = match self.load_index(date)? {
            Some(idx) => idx,
            None => return Ok(None),
        };
        Ok(index
            .candidates
            .into_iter()
            .find(|cand| cand.source == source))
    }

    fn write_last_applied(&self, data: &LastApplied) -> Result<()> {
        if let Some(parent) = self.last_applied_path().parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(data)?;
        write_bytes_atomic(&self.last_applied_path(), &bytes)?;
        Ok(())
    }

    #[allow(dead_code)]
    fn read_last_applied(&self) -> Result<Option<LastApplied>> {
        let path = self.last_applied_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path)?;
        let value = serde_json::from_slice(&bytes)?;
        Ok(Some(value))
    }
}

#[derive(Debug, Clone, ValueEnum)]
enum CommandArg {
    EnableAutoUpdate,
    DisableAutoUpdate,
    Info,
    Choose,
}

#[derive(Debug, Parser)]
#[command(
    name = "bing-wallpaper-daily-mac-multimonitor",
    version,
    about = "Download the Bing daily wallpaper and apply it to macOS desktops."
)]
struct Cli {
    #[arg(value_enum)]
    command: Option<CommandArg>,

    #[arg(long = "auto-update-name", default_value = "default")]
    auto_update_name: String,

    #[arg(short = 'f', long = "force", action = ArgAction::SetTrue)]
    force: bool,

    #[arg(short = 's', long = "ssl", default_value_t = true, action = ArgAction::SetTrue)]
    ssl: bool,

    #[arg(long = "no-ssl", action = ArgAction::SetTrue, conflicts_with = "ssl")]
    no_ssl: bool,

    #[arg(short = 'q', long = "quiet", action = ArgAction::SetTrue)]
    quiet: bool,

    #[arg(short = 'c', long = "country")]
    country: Option<String>,

    #[arg(short = 'd', long = "day", default_value_t = 0)]
    day: i32,

    #[arg(short = 'n', long = "filename")]
    filename: Option<String>,

    #[arg(short = 'p', long = "picturedir")]
    picture_dir: Option<PathBuf>,

    #[arg(short = 'r', long = "resolution")]
    resolution: Option<String>,

    #[arg(long = "resolutions")]
    resolutions: Vec<String>,

    #[arg(short = 'm', long = "monitor", default_value_t = 0)]
    monitor: usize,

    #[arg(long = "all-desktops-experimental", action = ArgAction::SetTrue)]
    all_desktops_experimental: bool,
}

pub fn run_from_env() -> Result<()> {
    let raw_args: Vec<String> = env::args().skip(1).collect();
    run_with_raw_args(raw_args)
}

fn run_with_raw_args(raw_args: Vec<String>) -> Result<()> {
    let mut clap_args = vec![OsString::from("bing-wallpaper-daily-mac-multimonitor")];
    clap_args.extend(raw_args.iter().map(OsString::from));
    let args = Cli::parse_from(clap_args);

    if args.resolution.is_some() && !args.resolutions.is_empty() {
        return Err(WallpaperError::Message(
            "Provide either --resolution or --resolutions, not both.".to_string(),
        ));
    }

    let resolutions: Vec<String> = if let Some(res) = args.resolution {
        vec![res]
    } else if !args.resolutions.is_empty() {
        args.resolutions.clone()
    } else {
        DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect()
    };

    let ssl = !args.no_ssl && args.ssl;
    let settings = Settings {
        proto: if ssl { "https".into() } else { "http".into() },
        country: args.country.clone(),
        day: args.day,
        picture_dir: args
            .picture_dir
            .unwrap_or_else(default_picture_dir)
            .expand_tilde(),
        auto_update_name: normalize_auto_update_name(&args.auto_update_name),
        monitor: args.monitor,
        force: args.force,
        quiet: args.quiet,
        experimental: args.all_desktops_experimental,
        filename: args.filename.clone(),
        bing_host: "www.bing.com".to_string(),
    };

    let cache = CacheManager::new(&settings.picture_dir);
    let target_date = target_date_for_day(settings.day);
    let date_label = target_date.to_string();
    let client = build_client()?;

    match args.command {
        Some(CommandArg::EnableAutoUpdate) => {
            create_launchd_plist(&settings, &raw_args)?;
            log("Automatic wallpaper update enabled.", settings.quiet);
            return Ok(());
        }
        Some(CommandArg::DisableAutoUpdate) => {
            remove_launchd_plist(&settings)?;
            log("Automatic wallpaper update disabled.", settings.quiet);
            return Ok(());
        }
        Some(CommandArg::Info) => {
            show_info(&settings.picture_dir)?;
            return Ok(());
        }
        Some(CommandArg::Choose) => {
            log(
                "Choose mode will show an interactive list in a future phase; using Bing for now.",
                settings.quiet,
            );
        }
        None => {}
    }

    ensure_picture_dir(&settings.picture_dir)?;

    if !settings.force {
        if let Some(candidate) = cache.find_candidate(&date_label, WallpaperSource::Bing)? {
            if candidate.local_path.exists() {
                log(
                    &format!(
                        "Using cached wallpaper for {} from {:?}",
                        date_label, candidate.source
                    ),
                    settings.quiet,
                );
                ensure_info_file(&settings.picture_dir, &candidate)?;
                apply_wallpaper(
                    &candidate.local_path,
                    &settings,
                    &cache,
                    &candidate.id,
                    candidate.source,
                )?;
                return Ok(());
            }
        }
    }

    let archive_url = build_archive_url(settings.day, settings.country.as_deref());
    let (url_base, metadata_body) = fetch_image_metadata(&client, &archive_url)?;
    let metadata = parse_bing_metadata(&metadata_body)?;

    let mut last_error: Option<WallpaperError> = None;
    for res in resolutions {
        match download_image(&client, &url_base, &res, &settings, &metadata_body) {
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
                    date: date_label.clone(),
                    metadata_xml: Some(String::from_utf8_lossy(&metadata_body).to_string()),
                };

                if settings.experimental && downloaded.skipped {
                    log(
                        "Download skipped; experimental all-desktops update not applied.",
                        settings.quiet,
                    );
                    cache.upsert_candidate(&date_label, candidate)?;
                    return Ok(());
                }

                let result = apply_wallpaper(
                    &downloaded.path,
                    &settings,
                    &cache,
                    &candidate.id,
                    candidate.source,
                );
                cache.upsert_candidate(&date_label, candidate)?;

                if let Err(err) = result {
                    last_error = Some(err);
                    continue;
                }
                return Ok(());
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

fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(IMAGE_TIMEOUT)
        .build()
        .map_err(Into::into)
}

fn default_picture_dir() -> PathBuf {
    home_dir().join("Pictures").join("bing-wallpapers")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn launchd_dir() -> PathBuf {
    home_dir().join("Library").join("LaunchAgents")
}

fn normalize_auto_update_name(name: &str) -> String {
    let cleaned = name.trim();
    let cleaned = if cleaned.is_empty() {
        "default"
    } else {
        cleaned
    };
    cleaned
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn log(message: &str, quiet: bool) {
    if quiet {
        return;
    }
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    println!("{timestamp}: {message}");
}

fn target_date_for_day(day: i32) -> NaiveDate {
    let today = Local::now().date_naive();
    let delta = ChronoDuration::days(day.into());
    today.checked_sub_signed(delta).unwrap_or(today)
}

fn build_archive_url(day: i32, country: Option<&str>) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
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

#[derive(Debug, Default, Clone)]
struct BingMetadata {
    headline: Option<String>,
    copyright: Option<String>,
    copyright_link: Option<String>,
}

fn parse_bing_metadata(body: &[u8]) -> Result<BingMetadata> {
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

fn sanitize_filename(name: &str) -> String {
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
struct DownloadedImage {
    path: PathBuf,
    skipped: bool,
    image_url: String,
}

fn download_image(
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

    let temp_path = unique_temp_path(&target_path);
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

fn unique_temp_path(target_path: &Path) -> PathBuf {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = target_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("wallpaper.jpg");
    target_path.with_file_name(format!("{file_name}.tmp.{pid}.{nanos}"))
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| WallpaperError::Message(format!("Invalid path: {}", path.display())))?;
    fs::create_dir_all(parent)?;

    let temp_path = unique_temp_path(path);
    let write_result = (|| -> Result<()> {
        let mut file = File::create(&temp_path)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temp_path, path)?;
        Ok(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }
    Ok(())
}

fn apply_wallpaper(
    file_path: &Path,
    settings: &Settings,
    cache: &CacheManager,
    candidate_id: &str,
    source: WallpaperSource,
) -> Result<()> {
    let result = if settings.experimental {
        set_wallpaper_experimental(file_path, settings.quiet)
    } else {
        set_wallpaper(file_path, settings.monitor, settings.quiet)
    };

    if result.is_ok() {
        let applied = LastApplied {
            candidate_id: candidate_id.to_string(),
            source,
            applied_path: file_path.to_path_buf(),
            applied_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        cache.write_last_applied(&applied)?;
    }

    result
}

fn ensure_info_file(picture_dir: &Path, candidate: &WallpaperCandidate) -> Result<()> {
    let info_path = picture_dir.join("info.xml");
    if info_path.exists() {
        return Ok(());
    }
    if let Some(xml) = &candidate.metadata_xml {
        write_bytes_atomic(&info_path, xml.as_bytes())?;
    }
    Ok(())
}

fn set_wallpaper(file_path: &Path, monitor: usize, quiet: bool) -> Result<()> {
    let posix_path = file_path.to_string_lossy().replace('"', "\\\"");
    let script = if monitor >= 1 {
        format!(
            r#"
        set tlst to {{}}
        tell application "System Events"
            set tlst to a reference to every desktop
            set picture of item {monitor} of tlst to (POSIX file "{posix_path}")
        end tell
        "#
        )
    } else {
        format!(
            r#"tell application "System Events" to tell every desktop to set picture to (POSIX file "{posix_path}")"#
        )
    };

    log(
        &format!(
            "Setting wallpaper to {} (monitor: {})",
            file_path.display(),
            if monitor < 1 {
                "all".into()
            } else {
                monitor.to_string()
            }
        ),
        quiet,
    );

    run_checked("osascript", &["-e", &script], "osascript")?;
    Ok(())
}

fn set_wallpaper_experimental(file_path: &Path, quiet: bool) -> Result<()> {
    let db_path = home_dir()
        .join("Library")
        .join("Application Support")
        .join("Dock")
        .join("desktoppicture.db");

    if !db_path.exists() {
        return Err(WallpaperError::Message(format!(
            "desktoppicture.db not found at {}",
            db_path.display()
        )));
    }

    log(
        "Writing wallpaper to desktoppicture.db (experimental all desktops)",
        quiet,
    );

    let mut conn = Connection::open(&db_path)?;
    {
        let tx = conn.transaction()?;
        tx.execute(
            "insert into data values (?)",
            [&file_path.to_string_lossy()],
        )?;
        let new_entry: i64 = tx.query_row("select max(rowid) from data;", [], |row| row.get(0))?;
        let pictures = {
            let mut stmt = tx.prepare("select rowid from pictures;")?;
            let rows = stmt
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        tx.execute("delete from preferences;", [])?;
        for pic in pictures {
            tx.execute(
                "insert into preferences (key, data_id, picture_id) values(1, ?, ?)",
                (new_entry, pic),
            )?;
        }
        tx.commit()?;
    }

    run_checked("killall", &["Dock"], "killall Dock")?;
    Ok(())
}

fn create_launchd_plist(settings: &Settings, raw_args: &[String]) -> Result<()> {
    fs::create_dir_all(launchd_dir())?;

    let mut filtered_args: Vec<String> = raw_args.to_owned();
    if let Some(pos) = filtered_args
        .iter()
        .position(|arg| arg == "enable-auto-update")
    {
        filtered_args.remove(pos);
    }

    let current_exe = env::current_exe().map_err(|err| {
        WallpaperError::Message(format!("Unable to determine current executable: {err}"))
    })?;
    let mut program_arguments = vec![current_exe.to_string_lossy().to_string()];
    program_arguments.extend(filtered_args);

    let mut plist_map: Dictionary = Dictionary::new();
    plist_map.insert("Label".into(), Value::String(settings.plist_label()));
    plist_map.insert("OnDemand".into(), Value::Boolean(true));
    plist_map.insert(
        "ProgramArguments".into(),
        Value::Array(program_arguments.into_iter().map(Value::String).collect()),
    );
    let mut env_map: Dictionary = Dictionary::new();
    env_map.insert("PATH".into(), Value::String(DEFAULT_PATH.to_string()));
    plist_map.insert("EnvironmentVariables".into(), Value::Dictionary(env_map));
    plist_map.insert(
        "StandardErrorPath".into(),
        Value::String(format!(
            "/tmp/{PLIST_BASENAME}-{}.err",
            settings.auto_update_name
        )),
    );
    plist_map.insert(
        "StandardOutPath".into(),
        Value::String(format!(
            "/tmp/{PLIST_BASENAME}-{}.out",
            settings.auto_update_name
        )),
    );
    plist_map.insert("StartInterval".into(), Value::Integer(1800.into()));
    plist_map.insert("RunAtLoad".into(), Value::Boolean(true));

    let plist_path = settings.plist_filename();
    let value = Value::Dictionary(plist_map);
    let mut bytes = Vec::new();
    plist::to_writer_xml(&mut bytes, &value)?;
    write_bytes_atomic(&plist_path, &bytes)?;

    let _ = Command::new("launchctl")
        .args(["unload", "-w", plist_path.to_string_lossy().as_ref()])
        .output();
    run_checked(
        "launchctl",
        &["load", "-w", plist_path.to_string_lossy().as_ref()],
        "launchctl load",
    )?;
    Ok(())
}

fn remove_launchd_plist(settings: &Settings) -> Result<()> {
    let plist_path = settings.plist_filename();
    let _ = Command::new("launchctl")
        .args(["unload", "-w", plist_path.to_string_lossy().as_ref()])
        .output();
    let _ = fs::remove_file(&plist_path);
    Ok(())
}

fn show_info(picture_dir: &Path) -> Result<()> {
    let info_path = picture_dir.join("info.xml");
    if !info_path.exists() {
        return Err(WallpaperError::Message(format!(
            "No info.xml found in {}. Run the download first.",
            picture_dir.display()
        )));
    }

    let mut body = Vec::new();
    File::open(&info_path)?.read_to_end(&mut body)?;

    let metadata = parse_bing_metadata(&body)?;

    let mut info = String::new();
    if let Some(headline) = &metadata.headline {
        if !headline.is_empty() {
            info.push_str(headline);
            info.push('\n');
        }
    }
    if let Some(text) = &metadata.copyright {
        info.push_str(text);
    } else {
        info.push_str("Unknown copyright");
    }
    if let Some(link) = &metadata.copyright_link {
        if !link.is_empty() {
            info.push('\n');
            info.push_str(link);
        }
    }

    println!("{info}");
    Ok(())
}

fn ensure_picture_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

fn run_checked(program: &str, args: &[&str], what: &'static str) -> Result<Output> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        return Ok(output);
    }
    let code = output.status.code().unwrap_or(1);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(WallpaperError::CommandFailed { what, code, stderr })
}

trait ExpandTilde {
    fn expand_tilde(self) -> PathBuf;
}

impl ExpandTilde for PathBuf {
    fn expand_tilde(self) -> PathBuf {
        if let Ok(stripped) = self.strip_prefix("~") {
            return home_dir().join(stripped);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use tempfile::tempdir;

    fn make_settings(tmpdir: &Path, filename: Option<&str>, force: bool) -> Settings {
        Settings {
            proto: "https".into(),
            country: None,
            day: 0,
            picture_dir: tmpdir.to_path_buf(),
            auto_update_name: "default".into(),
            monitor: 0,
            force,
            quiet: true,
            experimental: false,
            filename: filename.map(ToString::to_string),
            bing_host: "www.bing.com".into(),
        }
    }

    #[test]
    fn normalize_auto_update_name_cleans() {
        assert_eq!(normalize_auto_update_name("  "), "default");
        assert_eq!(normalize_auto_update_name("foo bar"), "foo-bar");
        assert_eq!(normalize_auto_update_name("Name_1"), "Name_1");
    }

    #[test]
    fn sanitize_filename_behaves_like_python() {
        assert_eq!(sanitize_filename(""), "wallpaper.jpg");
        assert_eq!(sanitize_filename("custom"), "custom.jpg");
        assert_eq!(sanitize_filename("dir/../name.png"), "name.png");
    }

    #[test]
    fn build_archive_url_includes_country() {
        let url = build_archive_url(1, Some("en-US"));
        assert!(url.contains("idx=1"));
        assert!(url.contains("mkt=en-US"));
    }

    #[test]
    fn download_image_skips_existing_without_force() {
        let server = MockServer::start();
        let metadata = b"<xml />";
        let res = "1920x1080";
        let url_base = "/th?id=test";

        let tmpdir = tempdir().unwrap();
        let settings = Settings {
            proto: "http".into(),
            bing_host: server.address().to_string(),
            ..make_settings(tmpdir.path(), None, false)
        };

        let target = tmpdir.path().join(format!(
            "default-{}_{}.jpg",
            url_base.replace("/th?id=", ""),
            res
        ));
        fs::write(&target, b"existing").unwrap();

        let client = build_client().unwrap();
        let _mock = server.mock(|when, then| {
            when.method(GET);
            then.status(200).body("image-bytes");
        });

        let downloaded = download_image(&client, url_base, res, &settings, metadata).unwrap();
        assert!(downloaded.skipped);
        assert_eq!(downloaded.path, target);
        assert_eq!(_mock.hits(), 0);
    }

    #[test]
    fn download_image_success_replaces_old_after_complete() {
        let server = MockServer::start();
        let metadata = b"<xml>meta</xml>";
        let res = "1920x1080";
        let url_base = "/th?id=test";

        let tmpdir = tempdir().unwrap();
        let settings = Settings {
            proto: "http".into(),
            bing_host: server.address().to_string(),
            ..make_settings(tmpdir.path(), None, false)
        };

        let old_wallpaper = tmpdir.path().join("default-old.jpg");
        fs::write(&old_wallpaper, b"old").unwrap();

        let client = build_client().unwrap();
        let _mock = server.mock(|when, then| {
            when.method(GET);
            then.status(200).body("image-bytes");
        });

        let downloaded = download_image(&client, url_base, res, &settings, metadata).unwrap();
        assert!(!downloaded.skipped);
        assert!(downloaded.path.exists());
        assert_eq!(fs::read(&downloaded.path).unwrap(), b"image-bytes");
        assert!(!old_wallpaper.exists());
        assert_eq!(fs::read(tmpdir.path().join("info.xml")).unwrap(), metadata);
        assert_eq!(_mock.hits(), 1);
    }

    #[test]
    fn download_image_http_error_cleans_temp() {
        let server = MockServer::start();
        let metadata = b"meta";
        let res = "1920x1080";
        let url_base = "/th?id=test";

        let tmpdir = tempdir().unwrap();
        let settings = Settings {
            proto: "http".into(),
            bing_host: server.address().to_string(),
            ..make_settings(tmpdir.path(), None, false)
        };

        let target = tmpdir.path().join(format!(
            "default-{}_{}.jpg",
            url_base.replace("/th?id=", ""),
            res
        ));

        let client = build_client().unwrap();
        let _mock = server.mock(|when, then| {
            when.method(GET);
            then.status(404);
        });

        let err = download_image(&client, url_base, res, &settings, metadata).unwrap_err();
        assert!(matches!(err, WallpaperError::DownloadStatus { .. }));
        assert!(!target.exists());

        let temps: Vec<_> = fs::read_dir(tmpdir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .contains(".tmp.")
            })
            .collect();
        assert!(temps.is_empty(), "Temporary files must be cleaned up");
        assert_eq!(_mock.hits(), 1);
    }

    #[test]
    fn cache_manager_upserts_and_loads_candidate() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let candidate = WallpaperCandidate {
            id: "bing-2024-01-01-1080".into(),
            source: WallpaperSource::Bing,
            title: Some("title".into()),
            description: None,
            attribution: Some("copyright".into()),
            info_url: None,
            image_url: "http://example".into(),
            local_path: tmpdir.path().join("wallpaper.jpg"),
            date: "2024-01-01".into(),
            metadata_xml: Some("<xml>meta</xml>".into()),
        };

        cache
            .upsert_candidate("2024-01-01", candidate.clone())
            .unwrap();

        let loaded = cache
            .find_candidate("2024-01-01", WallpaperSource::Bing)
            .unwrap()
            .expect("candidate must exist");
        assert_eq!(loaded.id, candidate.id);
        assert_eq!(loaded.title, candidate.title);
    }

    #[test]
    fn ensure_info_file_restores_missing_info() {
        let tmpdir = tempdir().unwrap();
        let candidate = WallpaperCandidate {
            id: "bing-2024-01-02-uhd".into(),
            source: WallpaperSource::Bing,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "http://example".into(),
            local_path: tmpdir.path().join("wallpaper2.jpg"),
            date: "2024-01-02".into(),
            metadata_xml: Some("<xml>info</xml>".into()),
        };

        let info_path = tmpdir.path().join("info.xml");
        assert!(!info_path.exists());
        ensure_info_file(tmpdir.path(), &candidate).unwrap();
        assert!(info_path.exists());
        let contents = fs::read_to_string(info_path).unwrap();
        assert!(contents.contains("info"));
    }
}
