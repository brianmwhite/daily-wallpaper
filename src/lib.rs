use chrono::{Duration as ChronoDuration, Local, NaiveDate};
use clap::{ArgAction, Parser, ValueEnum};
use image::imageops::FilterType;
use image::GenericImageView;
use inquire::Select;
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
const SPOTLIGHT_URL: &str = "https://fd.api.iris.microsoft.com/v4/api/selection";
const APOD_URL: &str = "https://api.nasa.gov/planetary/apod";
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
const SPOTLIGHT_DEFAULT_COUNTRY: &str = "US";
const SPOTLIGHT_DEFAULT_LOCALE: &str = "en-US";
const SPOTLIGHT_COUNT: usize = 3;
const APOD_DEFAULT_KEY: &str = "DEMO_KEY";

fn source_dir_name(source: WallpaperSource) -> &'static str {
    match source {
        WallpaperSource::Bing => "bing",
        WallpaperSource::Spotlight => "spotlight",
        WallpaperSource::Apod => "apod",
    }
}

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
    source: WallpaperSource,
    spotlight_index: usize,
    spotlight_url_override: Option<String>,
    apod_api_key: String,
    apod_hd: bool,
    apod_url_override: Option<String>,
    apod_crop: bool,
    prune_cache_days: Option<u32>,
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
    Spotlight,
    Apod,
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

    fn media_dir(&self, date: &str, source: WallpaperSource) -> PathBuf {
        self.base_dir.join(date).join(source_dir_name(source))
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

    fn find_candidate_by_id(&self, date: &str, id: &str) -> Result<Option<WallpaperCandidate>> {
        let index = match self.load_index(date)? {
            Some(idx) => idx,
            None => return Ok(None),
        };
        Ok(index.candidates.into_iter().find(|cand| cand.id == id))
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

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
enum SourceArg {
    Bing,
    Spotlight,
    Apod,
}

fn map_source(source: SourceArg) -> WallpaperSource {
    match source {
        SourceArg::Bing => WallpaperSource::Bing,
        SourceArg::Spotlight => WallpaperSource::Spotlight,
        SourceArg::Apod => WallpaperSource::Apod,
    }
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

    #[arg(long = "source", value_enum, default_value_t = SourceArg::Bing)]
    source: SourceArg,

    #[arg(
        long = "spotlight-index",
        default_value_t = 1,
        value_parser = clap::value_parser!(usize)
    )]
    spotlight_index: usize,

    #[arg(long = "nasa-api-key")]
    apod_api_key: Option<String>,

    #[arg(long = "apod-hd", action = ArgAction::SetTrue)]
    apod_hd: bool,

    #[arg(
        long = "no-apod-crop",
        action = ArgAction::SetFalse,
        default_value_t = true,
        help = "Disable APOD center-crop/resize to monitor aspect ratio"
    )]
    apod_crop: bool,

    #[arg(
        long = "prune-cache-days",
        value_parser = clap::value_parser!(u32).range(1..=365),
        help = "Remove cached wallpaper days older than this many days (uses `trash`)"
    )]
    prune_cache_days: Option<u32>,

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

    if args.spotlight_index == 0 || args.spotlight_index > SPOTLIGHT_COUNT {
        return Err(WallpaperError::Message(format!(
            "--spotlight-index must be between 1 and {SPOTLIGHT_COUNT}"
        )));
    }

    let single_resolution = args.resolution.clone();
    let resolutions: Vec<String> = if let Some(res) = single_resolution {
        vec![res]
    } else if !args.resolutions.is_empty() {
        args.resolutions.clone()
    } else {
        DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect()
    };

    let ssl = !args.no_ssl && args.ssl;
    let source = map_source(args.source);
    if source != WallpaperSource::Bing
        && (!args.resolutions.is_empty() || args.resolution.is_some())
    {
        log(
            "Ignoring --resolution/--resolutions for non-Bing source.",
            args.quiet,
        );
    }

    if source == WallpaperSource::Spotlight && args.day != 0 {
        log(
            "Spotlight ignores --day; using today's feed instead.",
            args.quiet,
        );
    }

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
        source,
        spotlight_index: args.spotlight_index,
        spotlight_url_override: None,
        apod_api_key: args
            .apod_api_key
            .clone()
            .or_else(|| env::var("NASA_API_KEY").ok())
            .unwrap_or_else(|| APOD_DEFAULT_KEY.to_string()),
        apod_hd: args.apod_hd,
        apod_url_override: None,
        apod_crop: args.apod_crop,
        prune_cache_days: args.prune_cache_days,
    };

    let cache = CacheManager::new(&settings.picture_dir);
    let target_date = if settings.source == WallpaperSource::Spotlight {
        Local::now().date_naive()
    } else {
        target_date_for_day(settings.day)
    };
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
            ensure_picture_dir(&settings.picture_dir)?;
            return run_choose(&client, &cache, &settings, &date_label, resolutions);
        }
        None => {}
    }

    ensure_picture_dir(&settings.picture_dir)?;

    let result = match settings.source {
        WallpaperSource::Bing => run_bing(&client, &cache, &settings, &date_label, resolutions),
        WallpaperSource::Spotlight => run_spotlight(
            &client,
            &cache,
            &settings,
            &date_label,
            settings.spotlight_index,
        ),
        WallpaperSource::Apod => run_apod(&client, &cache, &settings, &date_label),
    };

    if result.is_ok() {
        if let Some(days) = settings.prune_cache_days {
            prune_cache(&cache, days, settings.quiet)?;
        }
    }

    result
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

fn run_bing(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
    resolutions: Vec<String>,
) -> Result<()> {
    let fetched = fetch_bing_candidate(client, cache, settings, date_label, resolutions)?;
    if settings.experimental && fetched.skipped_download {
        log(
            "Download skipped; experimental all-desktops update not applied.",
            settings.quiet,
        );
        return Ok(());
    }

    apply_wallpaper(
        &fetched.candidate.local_path,
        settings,
        cache,
        &fetched.candidate.id,
        fetched.candidate.source,
    )
}

struct FetchedCandidate {
    candidate: WallpaperCandidate,
    skipped_download: bool,
}

fn fetch_bing_candidate(
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

fn run_choose(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
    resolutions: Vec<String>,
) -> Result<()> {
    let mut current_settings = settings.clone();
    let current_res = resolutions;

    loop {
        let candidates =
            gather_candidates(client, cache, &current_settings, date_label, &current_res)?;
        if candidates.is_empty() {
            return Err(WallpaperError::Message(
                "No wallpapers available to choose from.".to_string(),
            ));
        }

        let labels: Vec<String> = candidates
            .iter()
            .enumerate()
            .map(|(idx, cand)| {
                let title = cand
                    .title
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .unwrap_or("(no title)");
                let attribution = cand
                    .attribution
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .unwrap_or("");
                format!(
                    "{idx}: [{}] {}{}",
                    source_label(cand.source),
                    title,
                    if attribution.is_empty() {
                        "".to_string()
                    } else {
                        format!(" — {}", attribution)
                    }
                )
            })
            .collect();

        let selection = Select::new(
            "Select a wallpaper (arrows + Enter). Choose Preview/Apply next.",
            labels,
        )
        .prompt();

        let idx = match selection {
            Ok(label) => {
                // labels are formatted as "{idx}: ..."
                let parts: Vec<&str> = label.split(':').collect();
                if let Some(num_str) = parts.first() {
                    num_str.parse::<usize>().unwrap_or(0)
                } else {
                    0
                }
            }
            Err(_) => return Ok(()),
        };

        if let Some(cand) = candidates.get(idx) {
            let action = Select::new(
                "Action",
                vec![
                    "Apply",
                    "Preview (Quick Look)",
                    "Refresh list (force re-download)",
                    "Quit chooser",
                ],
            )
            .prompt();
            match action {
                Ok(choice) if choice.starts_with("Apply") => {
                    match apply_wallpaper(
                        &cand.local_path,
                        &current_settings,
                        cache,
                        &cand.id,
                        cand.source,
                    ) {
                        Ok(()) => return Ok(()),
                        Err(err) => println!("Failed to apply wallpaper: {err}"),
                    }
                }
                Ok(choice) if choice.starts_with("Preview") => {
                    if cand.local_path.exists() {
                        let path_str = cand.local_path.to_string_lossy().to_string();
                        let _ = run_checked(
                            "qlmanage",
                            &["-p", path_str.as_str()],
                            "Quick Look preview",
                        );
                    } else {
                        println!("File not found for preview: {}", cand.local_path.display());
                    }
                }
                Ok(choice) if choice.starts_with("Refresh") => {
                    current_settings.force = true;
                    continue;
                }
                _ => return Ok(()),
            }
        }
    }
}

fn gather_candidates(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
    resolutions: &[String],
) -> Result<Vec<WallpaperCandidate>> {
    let mut result = Vec::new();

    match fetch_bing_candidate(client, cache, settings, date_label, resolutions.to_vec()) {
        Ok(fetched) => result.push(fetched.candidate),
        Err(err) => log(&format!("Bing unavailable: {err}"), settings.quiet),
    }

    match fetch_spotlight_candidates(client, cache, settings, date_label) {
        Ok(cands) => result.extend(cands),
        Err(err) => log(&format!("Spotlight unavailable: {err}"), settings.quiet),
    }

    match fetch_apod_candidate(client, cache, settings, date_label) {
        Ok(cand) => result.push(cand),
        Err(err) => log(&format!("APOD unavailable: {err}"), settings.quiet),
    }

    Ok(result)
}

fn source_label(source: WallpaperSource) -> &'static str {
    match source {
        WallpaperSource::Bing => "Bing",
        WallpaperSource::Spotlight => "Spotlight",
        WallpaperSource::Apod => "APOD",
    }
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

fn build_spotlight_url(settings: &Settings) -> String {
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
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("placement", "88000820");
    serializer.append_pair("bcnt", &SPOTLIGHT_COUNT.to_string());
    serializer.append_pair("country", &country);
    serializer.append_pair("locale", &locale);
    serializer.append_pair("fmt", "json");
    format!("{SPOTLIGHT_URL}?{}", serializer.finish())
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

#[derive(Debug)]
struct DownloadedFile {
    path: PathBuf,
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

fn download_to_path(
    client: &Client,
    url: &str,
    target_path: &Path,
    force: bool,
    quiet: bool,
) -> Result<DownloadedFile> {
    if target_path.exists() && !force {
        log(
            &format!(
                "Skipping download, already present: {}",
                target_path.display()
            ),
            quiet,
        );
        return Ok(DownloadedFile {
            path: target_path.to_path_buf(),
        });
    }

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }

    log(&format!("Downloading {}", url), quiet);
    let temp_path = unique_temp_path(target_path);
    let download_result = (|| -> Result<()> {
        let _ = fs::remove_file(&temp_path);
        let mut response = client.get(url).timeout(IMAGE_TIMEOUT).send()?;
        let status = response.status();
        if status != StatusCode::OK {
            return Err(WallpaperError::Status {
                url: url.to_string(),
                status: status.as_u16(),
            });
        }
        let mut file = File::create(&temp_path)?;
        response.copy_to(&mut file)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temp_path, target_path)?;
        Ok(())
    })();

    if let Err(err) = download_result {
        let _ = fs::remove_file(&temp_path);
        return Err(err);
    }

    Ok(DownloadedFile {
        path: target_path.to_path_buf(),
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

fn run_spotlight(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
    pick_index: usize,
) -> Result<()> {
    let candidates = fetch_spotlight_candidates(client, cache, settings, date_label)?;
    if candidates.is_empty() {
        return Err(WallpaperError::Message(
            "No Spotlight images available.".to_string(),
        ));
    }
    let idx = pick_index.saturating_sub(1);
    if idx >= candidates.len() {
        return Err(WallpaperError::Message(format!(
            "Requested Spotlight index {} not available; {} images found.",
            pick_index,
            candidates.len()
        )));
    }
    let candidate = &candidates[idx];
    apply_wallpaper(
        &candidate.local_path,
        settings,
        cache,
        &candidate.id,
        candidate.source,
    )
}

fn fetch_spotlight_candidates(
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

fn run_apod(
    client: &Client,
    cache: &CacheManager,
    settings: &Settings,
    date_label: &str,
) -> Result<()> {
    let candidate = fetch_apod_candidate(client, cache, settings, date_label)?;
    apply_wallpaper(
        &candidate.local_path,
        settings,
        cache,
        &candidate.id,
        candidate.source,
    )
}

fn fetch_apod_candidate(
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
    let mut serializer = form_urlencoded::Serializer::new(String::new());
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
    if let Ok(f) = File::open(&temp_path) {
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

fn parse_resolution(res_str: &str) -> Option<(u32, u32)> {
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

fn prune_cache(cache: &CacheManager, keep_days: u32, quiet: bool) -> Result<()> {
    let cache_root = &cache.base_dir;
    if !cache_root.exists() {
        return Ok(());
    }

    let today = Local::now().date_naive();
    let cutoff = today
        .checked_sub_signed(ChronoDuration::days((keep_days.saturating_sub(1)) as i64))
        .unwrap_or(today);

    for entry in fs::read_dir(cache_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(folder_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(folder_name, "%Y-%m-%d") else {
            continue;
        };
        if date < cutoff {
            log(
                &format!(
                    "Pruning cache entry {} (older than {} days)",
                    path.display(),
                    keep_days
                ),
                quiet,
            );
            if let Err(err) = fs::remove_dir_all(&path) {
                log(&format!("Failed to prune {}: {err}", path.display()), quiet);
            }
        }
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
            source: WallpaperSource::Bing,
            spotlight_index: 1,
            spotlight_url_override: None,
            apod_api_key: "TEST".into(),
            apod_hd: false,
            apod_url_override: None,
            apod_crop: true,
            prune_cache_days: None,
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
    fn spotlight_downloads_and_reuses_cache() {
        let server = MockServer::start();
        let api_url = server.url("/spotlight");
        let img1 = server.url("/img1.jpg");
        let img2 = server.url("/img2.jpg");
        let img3 = server.url("/img3.jpg");

        let raw_payload = format!(
            r#"{{"ad":{{"title":"One","description":"D1","copyright":"C1","ctaUri":"https://example.com/1","landscapeImage":{{"asset":"{img1}"}}, "entityId":"raw1"}}}}"#
        );
        let raw_payload_string = serde_json::to_string(&raw_payload).unwrap();

        let body = format!(
            r#"{{
            "batchrsp": {{
                "items": [
                    {{ "item": {raw_payload_string} }},
                    {{ "item": {{ "ad": {{ "title": "Two", "landscapeImage": {{ "asset": "{img2}" }}, "entityId": "id2" }} }} }},
                    {{ "item": {{ "ad": {{ "title": "Three", "landscapeImage": {{ "asset": "{img3}" }}, "entityId": "id3" }} }} }}
                ]
            }}
        }}"#
        );

        let api_mock = server.mock(|when, then| {
            when.method(GET).path("/spotlight");
            then.status(200).body(body.clone());
        });
        let img1_mock = server.mock(|when, then| {
            when.method(GET).path("/img1.jpg");
            then.status(200).body("img1");
        });
        let img2_mock = server.mock(|when, then| {
            when.method(GET).path("/img2.jpg");
            then.status(200).body("img2");
        });
        let img3_mock = server.mock(|when, then| {
            when.method(GET).path("/img3.jpg");
            then.status(200).body("img3");
        });

        let tmpdir = tempdir().unwrap();
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.source = WallpaperSource::Spotlight;
        settings.spotlight_index = 2;
        settings.spotlight_url_override = Some(api_url.clone());

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = Local::now().date_naive().to_string();

        run_spotlight(
            &client,
            &cache,
            &settings,
            &date_label,
            settings.spotlight_index,
        )
        .unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img1_mock.hits(), 1);
        assert_eq!(img2_mock.hits(), 1);
        assert_eq!(img3_mock.hits(), 1);

        // Second run should reuse cache and skip network.
        run_spotlight(
            &client,
            &cache,
            &settings,
            &date_label,
            settings.spotlight_index,
        )
        .unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img1_mock.hits(), 1);
        assert_eq!(img2_mock.hits(), 1);
        assert_eq!(img3_mock.hits(), 1);
    }

    #[test]
    fn apod_downloads_and_uses_cache() {
        let server = MockServer::start();
        let api_url = server.url("/apod");
        let img_url = server.url("/image.jpg");

        let api_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/apod")
                .query_param("api_key", "TEST")
                .query_param("date", "2024-01-01");
            then.status(200).body(
                r#"{
                "url":"IMAGE_URL",
                "media_type":"image",
                "title":"Nebula",
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
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.source = WallpaperSource::Apod;
        settings.apod_api_key = "TEST".into();
        settings.apod_url_override = Some(api_url);
        settings.apod_crop = false;

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = "2024-01-01";

        run_apod(&client, &cache, &settings, date_label).unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img_mock.hits(), 1);

        // second run should reuse cache
        run_apod(&client, &cache, &settings, date_label).unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img_mock.hits(), 1);
    }

    #[test]
    fn apod_errors_on_video() {
        let server = MockServer::start();
        let api_url = server.url("/apod");
        let api_mock = server.mock(|when, then| {
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

        let tmpdir = tempdir().unwrap();
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.source = WallpaperSource::Apod;
        settings.apod_api_key = "TEST".into();
        settings.apod_url_override = Some(api_url);
        settings.apod_crop = false;

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = "2024-01-02";

        let err = run_apod(&client, &cache, &settings, date_label).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not an image"));
        assert_eq!(api_mock.hits(), 1);
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
