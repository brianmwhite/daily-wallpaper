use chrono::{Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use clap::{ArgAction, Parser, ValueEnum};
use inquire::{InquireError, Select, Text};
use plist::{Dictionary, Value};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json;
use image::image_dimensions;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::collections::HashSet;
#[cfg(target_os = "macos")]
use std::os::raw::c_void;
use thiserror::Error;

mod sources;
use sources::{Source, SourceContext, SourceRegistry, SourceSettings};
mod favorites;
use favorites::{FavoriteEntry, FavoritesManager};

const DEFAULT_RESOLUTIONS: &[&str] = &[
    "1920x1200",
    "1920x1080",
    "1024x768",
    "1280x720",
    "1366x768",
    "UHD",
];
const DEFAULT_PATH: &str = "/usr/local/bin:/usr/local/sbin:/usr/bin:/bin:/usr/sbin:/sbin";
const USER_AGENT: &str = concat!(
    "daily-wallpaper/",
    env!("CARGO_PKG_VERSION")
);
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const IMAGE_TIMEOUT: Duration = Duration::from_secs(60);
const PLIST_BASENAME: &str = "com.thirdember.daily-wallpaper";
const AUTO_UPDATE_RUN_ARG: &str = "auto-update-run";
const CACHE_DIR_NAME: &str = "cache";
const CACHE_INDEX_FILE: &str = "index.json";
const LAST_APPLIED_FILE: &str = "last_applied.json";
const DEFAULT_INFO_WRAP_WIDTH: usize = 80;
const INFO_LABEL_COLOR: &str = "\u{1b}[1;36m";
const INFO_RESET: &str = "\u{1b}[0m";
const INFO_ICON_TITLE: &str = "\u{1f5bc}\u{fe0f}";
const INFO_ICON_ABOUT: &str = "\u{1f4dd}";
const INFO_ICON_CREDIT: &str = "\u{270d}\u{fe0f}";
const INFO_ICON_LINK: &str = "\u{1f517}";

fn source_dir_name(source: WallpaperSource) -> &'static str {
    match source {
        WallpaperSource::Bing => "bing",
        WallpaperSource::Spotlight => "spotlight",
        WallpaperSource::Apod => "apod",
        WallpaperSource::Modis => "modis",
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
enum WallpaperSource {
    Bing,
    Spotlight,
    Apod,
    Modis,
}

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ConfigSource {
    Bing,
    Spotlight,
    Apod,
    Modis,
}

impl From<ConfigSource> for SourceArg {
    fn from(value: ConfigSource) -> Self {
        match value {
            ConfigSource::Bing => SourceArg::Bing,
            ConfigSource::Spotlight => SourceArg::Spotlight,
            ConfigSource::Apod => SourceArg::Apod,
            ConfigSource::Modis => SourceArg::Modis,
        }
    }
}

#[derive(Debug, Clone, ValueEnum, PartialEq, Eq)]
enum SourceArg {
    Bing,
    Spotlight,
    Apod,
    Modis,
}

fn map_source(source: SourceArg) -> WallpaperSource {
    match source {
        SourceArg::Bing => WallpaperSource::Bing,
        SourceArg::Spotlight => WallpaperSource::Spotlight,
        SourceArg::Apod => WallpaperSource::Apod,
        SourceArg::Modis => WallpaperSource::Modis,
    }
}

fn source_label(source: WallpaperSource) -> &'static str {
    match source {
        WallpaperSource::Bing => "Bing",
        WallpaperSource::Spotlight => "Spotlight",
        WallpaperSource::Apod => "APOD",
        WallpaperSource::Modis => "MODIS",
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
    #[error("Downloaded image {width}x{height} below minimum {min_width}x{min_height}")]
    MinResolution {
        width: u32,
        height: u32,
        min_width: u32,
        min_height: u32,
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
    #[error("Canceled by user")]
    Canceled,
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
    picture_dir: PathBuf,
    favorites_dir: PathBuf,
    auto_update_name: String,
    monitor: usize,
    force: bool,
    offline: bool,
    verbose: bool,
    quiet: bool,
    experimental: bool,
    filename: Option<String>,
    source: WallpaperSource,
    prune_cache_days: Option<u32>,
    info_wrap_width: usize,
    info_plain_text: bool,
    refresh_metadata: bool,
    min_resolution: Option<(u32, u32)>,
    log_file: Option<PathBuf>,
    log_file_max_bytes: u64,
    disabled_sources: HashSet<WallpaperSource>,
    date_override: Option<NaiveDate>,
}

#[derive(Debug, Clone)]
pub(crate) struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    fn set(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    fn clear(&self) {
        self.0.store(false, Ordering::SeqCst);
    }

    fn is_set(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Settings {
    fn plist_filename(&self) -> PathBuf {
        launchd_dir().join(format!("{PLIST_BASENAME}-{}.plist", self.auto_update_name))
    }

    fn plist_label(&self) -> String {
        format!("{PLIST_BASENAME}.{}", self.auto_update_name)
    }

    fn display_sync_plist_filename(&self) -> PathBuf {
        launchd_dir().join(format!(
            "{PLIST_BASENAME}-display-sync-{}.plist",
            self.auto_update_name
        ))
    }

    fn display_sync_label(&self) -> String {
        format!("{PLIST_BASENAME}.display-sync.{}", self.auto_update_name)
    }
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
    #[serde(default)]
    checksum: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheIndex {
    date: String,
    candidates: Vec<WallpaperCandidate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceSkip {
    reason: String,
    created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InProgressFetch {
    started_at: u64,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct AppConfig {
    #[serde(default)]
    default_source: Option<ConfigSource>,
    #[serde(default)]
    disabled_sources: Option<Vec<ConfigSource>>,
    #[serde(default)]
    monitor: Option<usize>,
    #[serde(default)]
    all_desktops_experimental: Option<bool>,
    #[serde(default)]
    auto_update_name: Option<String>,
    #[serde(default)]
    prune_cache_days: Option<u32>,
    #[serde(default)]
    picture_dir: Option<PathBuf>,
    #[serde(default)]
    favorites_dir: Option<PathBuf>,
    #[serde(default)]
    verbosity: Option<String>,
    #[serde(default)]
    offline: Option<bool>,
    #[serde(default)]
    spotlight_index: Option<usize>,
    #[serde(default)]
    info_wrap_width: Option<usize>,
    #[serde(default)]
    info_plain_text: Option<bool>,
    #[serde(default)]
    min_resolution: Option<String>,
    #[serde(default)]
    log_file: Option<PathBuf>,
    #[serde(default)]
    log_file_max_kb: Option<u64>,
    #[serde(default)]
    apod_api_key: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    resolutions: Option<Vec<String>>,
    #[serde(default)]
    apod: Option<sources::apod::ApodConfig>,
    #[serde(default)]
    modis: Option<sources::modis::ModisConfig>,
    #[serde(default)]
    bing: Option<sources::bing::BingConfig>,
    #[serde(default)]
    spotlight: Option<sources::spotlight::SpotlightConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastApplied {
    candidate_id: String,
    source: WallpaperSource,
    applied_path: PathBuf,
    applied_at: u64,
    #[serde(default)]
    applied_by_user: bool,
    #[serde(default)]
    date: Option<String>,
}

#[derive(Clone)]
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

    fn skip_path(&self, date: &str, source: WallpaperSource) -> PathBuf {
        self.media_dir(date, source).join("skip.json")
    }

    fn in_progress_path(&self, date: &str, source: WallpaperSource) -> PathBuf {
        self.media_dir(date, source).join("in_progress.json")
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

    fn read_skip(&self, date: &str, source: WallpaperSource) -> Result<Option<SourceSkip>> {
        let path = self.skip_path(date, source);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read(&path)?;
        let skip = serde_json::from_slice::<SourceSkip>(&data)?;
        Ok(Some(skip))
    }

    fn write_skip(&self, date: &str, source: WallpaperSource, reason: &str) -> Result<()> {
        let path = self.skip_path(date, source);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let skip = SourceSkip {
            reason: reason.to_string(),
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let bytes = serde_json::to_vec_pretty(&skip)?;
        write_bytes_atomic(&path, &bytes)?;
        Ok(())
    }

    fn read_in_progress(
        &self,
        date: &str,
        source: WallpaperSource,
    ) -> Result<Option<InProgressFetch>> {
        let path = self.in_progress_path(date, source);
        if !path.exists() {
            return Ok(None);
        }
        let data = fs::read(&path)?;
        let progress = serde_json::from_slice::<InProgressFetch>(&data)?;
        Ok(Some(progress))
    }

    fn write_in_progress(&self, date: &str, source: WallpaperSource) -> Result<()> {
        let path = self.in_progress_path(date, source);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let progress = InProgressFetch {
            started_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };
        let bytes = serde_json::to_vec_pretty(&progress)?;
        write_bytes_atomic(&path, &bytes)?;
        Ok(())
    }

    fn clear_in_progress(&self, date: &str, source: WallpaperSource) {
        let path = self.in_progress_path(date, source);
        let _ = fs::remove_file(&path);
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

    fn find_candidates_by_source(
        &self,
        date: &str,
        source: WallpaperSource,
    ) -> Result<Vec<WallpaperCandidate>> {
        let index = match self.load_index(date)? {
            Some(idx) => idx,
            None => return Ok(Vec::new()),
        };
        Ok(index
            .candidates
            .into_iter()
            .filter(|cand| cand.source == source)
            .collect())
    }

    fn find_candidate_by_id(&self, date: &str, id: &str) -> Result<Option<WallpaperCandidate>> {
        let index = match self.load_index(date)? {
            Some(idx) => idx,
            None => return Ok(None),
        };
        Ok(index.candidates.into_iter().find(|cand| cand.id == id))
    }

    fn find_candidate_any_date(&self, id: &str) -> Result<Option<WallpaperCandidate>> {
        if !self.base_dir.exists() {
            return Ok(None);
        }

        let mut dates: Vec<String> = Vec::new();
        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                dates.push(name.to_string());
            }
        }
        dates.sort();

        for date in dates {
            if let Some(candidate) = self.find_candidate_by_id(&date, id)? {
                return Ok(Some(candidate));
            }
        }

        Ok(None)
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
    pub fn read_last_applied(&self) -> Result<Option<LastApplied>> {
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
    EnableDisplaySync,
    DisableDisplaySync,
    DisplaySync,
    Info,
    #[value(
        name = "choose",
        help = "Interactive picker; Ctrl-C during downloads cancels remaining sources."
    )]
    Choose,
    Reapply,
    #[value(hide = true)]
    AutoUpdateRun,
}


#[derive(Debug, Parser)]
#[command(
    name = "daily-wallpaper",
    version,
    about = "Download the daily wallpapers and apply one to macOS desktops."
)]
struct Cli {
    #[arg(value_enum)]
    command: Option<CommandArg>,

    #[arg(long = "auto-update-name", default_value = "default")]
    auto_update_name: String,

    #[arg(short = 'f', long = "force", action = ArgAction::SetTrue)]
    force: bool,

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

    #[arg(short = 'v', long = "verbose", action = ArgAction::SetTrue, conflicts_with = "quiet")]
    verbose: bool,

    #[arg(long = "offline", action = ArgAction::SetTrue, help = "Use cached wallpapers only; never download.")]
    offline: bool,

    #[arg(
        long = "date",
        help = "Browse a cached date with `choose` (YYYY-MM-DD), or 'pick' to select one interactively. Forces offline mode for the run; never persisted."
    )]
    date: Option<String>,

    #[arg(
        long = "disable-source",
        value_enum,
        action = ArgAction::Append,
        value_delimiter = ',',
        help = "Disable a source (repeatable or comma-separated)."
    )]
    disable_sources: Vec<SourceArg>,

    #[arg(short = 'n', long = "filename")]
    filename: Option<String>,

    #[arg(short = 'p', long = "picturedir")]
    picture_dir: Option<PathBuf>,

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
    let mut clap_args = vec![OsString::from("daily-wallpaper")];
    clap_args.extend(raw_args.iter().map(OsString::from));
    let args = Cli::parse_from(clap_args);
    let config = load_config();

    let ssl = !args.no_ssl && args.ssl;
    let mut source_arg = SourceArg::Bing;
    if let Some(cfg) = &config {
        if let Some(def) = cfg.default_source {
            source_arg = def.into();
        }
    }
    let source = map_source(source_arg);

    let monitor_override = arg_present(&raw_args, &["-m", "--monitor"]);
    let auto_update_override =
        arg_present(&raw_args, &["--auto-update-name", "--auto_update_name"]);
    let prune_override = arg_present(&raw_args, &["--prune-cache-days"]);
    let picture_override = arg_present(&raw_args, &["-p", "--picturedir"]);
    let verbosity_override = args.quiet || args.verbose;
    let offline_override = arg_present(&raw_args, &["--offline"]);
    let disabled_sources = resolve_disabled_sources(config.as_ref(), &args.disable_sources);

    let monitor = {
        let mut monitor_val = args.monitor;
        if let Some(cfg) = &config {
            if !monitor_override {
                if let Some(m) = cfg.monitor {
                    monitor_val = m;
                }
            }
        }
        monitor_val
    };

    let source_settings = SourceSettings::from_config(config.as_ref())?;
    let min_resolution = config
        .as_ref()
        .and_then(|cfg| cfg.min_resolution.as_deref())
        .and_then(parse_resolution);
    let log_file = config
        .as_ref()
        .and_then(|cfg| cfg.log_file.clone())
        .map(|path| path.expand_tilde());
    let log_file_max_bytes = config
        .as_ref()
        .and_then(|cfg| cfg.log_file_max_kb)
        .unwrap_or(5120)
        .saturating_mul(1024);

    let mut quiet = args.quiet;
    let mut verbose = args.verbose;
    if !verbosity_override {
        if let Some(cfg) = &config {
            if let Some(v) = cfg.verbosity.as_deref() {
                match v.to_lowercase().as_str() {
                    "quiet" => {
                        quiet = true;
                        verbose = false;
                    }
                    "verbose" => {
                        verbose = true;
                        quiet = false;
                    }
                    _ => {}
                }
            }
        }
    }

    let mut offline = args.offline;
    if !offline_override {
        if let Some(cfg) = &config {
            offline = cfg.offline.unwrap_or(offline);
        }
    }

    let prune_cache_days = if prune_override {
        args.prune_cache_days
    } else if let Some(cfg) = &config {
        cfg.prune_cache_days.or(args.prune_cache_days)
    } else {
        args.prune_cache_days
    };

    let picture_dir = if picture_override {
        args.picture_dir.clone()
    } else if let Some(cfg) = &config {
        cfg.picture_dir.clone().or(args.picture_dir.clone())
    } else {
        args.picture_dir.clone()
    };

    let favorites_dir = if let Some(cfg) = &config {
        cfg.favorites_dir.clone()
    } else {
        None
    };

    let auto_update_name = if auto_update_override {
        args.auto_update_name.clone()
    } else if let Some(cfg) = &config {
        cfg.auto_update_name
            .clone()
            .unwrap_or(args.auto_update_name.clone())
    } else {
        args.auto_update_name.clone()
    };

    let experimental = if let Some(cfg) = &config {
        cfg.all_desktops_experimental
            .unwrap_or(args.all_desktops_experimental)
    } else {
        args.all_desktops_experimental
    };

    let info_wrap_width = config
        .as_ref()
        .and_then(|cfg| cfg.info_wrap_width)
        .unwrap_or(DEFAULT_INFO_WRAP_WIDTH)
        .max(20);
    let info_plain_text = config
        .as_ref()
        .and_then(|cfg| cfg.info_plain_text)
        .unwrap_or(false);

    let resolved_picture_dir = picture_dir
        .unwrap_or_else(default_picture_dir)
        .expand_tilde();
    let resolved_favorites_dir = favorites_dir
        .unwrap_or_else(|| default_favorites_dir(&resolved_picture_dir))
        .expand_tilde();

    let settings = Settings {
        proto: if ssl { "https".into() } else { "http".into() },
        picture_dir: resolved_picture_dir,
        favorites_dir: resolved_favorites_dir,
        auto_update_name: normalize_auto_update_name(&auto_update_name),
        monitor,
        force: args.force,
        offline,
        verbose,
        quiet,
        experimental,
        filename: args.filename.clone(),
        source,
        prune_cache_days,
        info_wrap_width,
        info_plain_text,
        refresh_metadata: true,
        min_resolution,
        log_file,
        log_file_max_bytes,
        disabled_sources,
        date_override: None,
    };
    set_log_file(settings.log_file.clone(), settings.log_file_max_bytes);

    if settings.offline && settings.force {
        log(
            "Offline mode enabled; ignoring --force for downloads.",
            settings.quiet,
        );
    }

    let cache = CacheManager::new(&settings.picture_dir);
    let favorites = FavoritesManager::new(settings.favorites_dir.clone());
    let client = build_client()?;
    let registry = SourceRegistry::new();

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
        Some(CommandArg::EnableDisplaySync) => {
            create_display_sync_plist(&settings, &raw_args)?;
            log("Display sync enabled.", settings.quiet);
            return Ok(());
        }
        Some(CommandArg::DisableDisplaySync) => {
            remove_display_sync_plist(&settings)?;
            log("Display sync disabled.", settings.quiet);
            return Ok(());
        }
        Some(CommandArg::DisplaySync) => {
            return run_display_sync(&cache, &settings);
        }
        Some(CommandArg::AutoUpdateRun) => {
            return run_auto_update_body(&cache, &settings, &registry, &client, &source_settings);
        }
        Some(CommandArg::Info) => {
            return dispatch_info(&cache, &settings);
        }
        Some(CommandArg::Choose) => {
            return dispatch_choose_maybe_dated(
                &client,
                &cache,
                &favorites,
                &registry,
                &settings,
                &source_settings,
                args.date.as_deref(),
            );
        }
        Some(CommandArg::Reapply) => {
            return dispatch_reapply(&cache, &settings);
        }
        None => {
            if io::stdin().is_terminal() && io::stdout().is_terminal() {
                match prompt_parent_menu() {
                    Ok(choice) => {
                        return run_menu_selection(
                            choice,
                            &client,
                            &cache,
                            &favorites,
                            &registry,
                            &settings,
                            &source_settings,
                            args.date.as_deref(),
                        );
                    }
                    Err(InquireError::OperationCanceled) => {
                        return Ok(());
                    }
                    Err(_) => {}
                }
            }
        }
    }

    self_heal_auto_update_plist(&settings, &raw_args)?;
    run_auto_update_body(&cache, &settings, &registry, &client, &source_settings)
}

fn dispatch_info(cache: &CacheManager, settings: &Settings) -> Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    show_info(cache, settings, &mut handle)
}

fn dispatch_choose(
    client: &Client,
    cache: &CacheManager,
    favorites: &FavoritesManager,
    registry: &SourceRegistry,
    settings: &Settings,
    source_settings: &SourceSettings,
) -> Result<()> {
    ensure_picture_dir(&settings.picture_dir)?;
    ensure_picture_dir(&settings.favorites_dir)?;
    run_choose(client, cache, favorites, registry, settings, source_settings)
}

fn dispatch_reapply(cache: &CacheManager, settings: &Settings) -> Result<()> {
    ensure_picture_dir(&settings.picture_dir)?;
    reapply_last_wallpaper(cache, settings)
}

fn validate_date_arg(cache: &CacheManager, value: &str) -> Result<NaiveDate> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d").map_err(|_| {
        WallpaperError::Message(format!(
            "Invalid date '{value}'. Expected YYYY-MM-DD, or use --date pick to select from cached dates."
        ))
    })?;
    let today = Local::now().date_naive();
    if date > today {
        return Err(WallpaperError::Message(format!(
            "{date} is in the future; cache only holds past days."
        )));
    }
    let available = list_cached_dates(cache, false);
    if available.contains(&date) {
        return Ok(date);
    }
    if available.is_empty() {
        return Err(WallpaperError::Message(
            "No cached wallpapers found for any date yet.".to_string(),
        ));
    }
    let list = available
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(WallpaperError::Message(format!(
        "No cached wallpapers found for {date}. Cached dates available: {list}. Use --date pick to select interactively."
    )))
}

fn pick_cached_date(cache: &CacheManager) -> Result<Option<NaiveDate>> {
    let dates = list_cached_dates(cache, true);
    if dates.is_empty() {
        println!("No cached dates found yet.");
        return Ok(None);
    }
    let labels: Vec<String> = dates.iter().map(|d| d.to_string()).collect();
    match Select::new("Select a cached date", labels).prompt() {
        Ok(label) => Ok(NaiveDate::parse_from_str(&label, "%Y-%m-%d").ok()),
        Err(_) => Ok(None),
    }
}

fn settings_with_date_override(settings: &Settings, date: NaiveDate) -> Settings {
    let mut dated = settings.clone();
    if !dated.offline {
        log(
            &format!("Offline mode forced because --date {date} was used."),
            dated.quiet,
        );
    }
    dated.offline = true;
    dated.date_override = Some(date);
    dated
}

fn dispatch_choose_maybe_dated(
    client: &Client,
    cache: &CacheManager,
    favorites: &FavoritesManager,
    registry: &SourceRegistry,
    settings: &Settings,
    source_settings: &SourceSettings,
    date_arg: Option<&str>,
) -> Result<()> {
    let Some(date_arg) = date_arg else {
        return dispatch_choose(client, cache, favorites, registry, settings, source_settings);
    };
    let chosen = if date_arg.eq_ignore_ascii_case("pick") {
        pick_cached_date(cache)?
    } else {
        Some(validate_date_arg(cache, date_arg)?)
    };
    let Some(date) = chosen else {
        return Ok(());
    };
    let dated_settings = settings_with_date_override(settings, date);
    dispatch_choose(
        client,
        cache,
        favorites,
        registry,
        &dated_settings,
        source_settings,
    )
}

enum ParentMenuChoice {
    Choose,
    Info,
    Reapply,
    BrowseCache,
}

fn prompt_parent_menu() -> std::result::Result<ParentMenuChoice, InquireError> {
    const CHOOSE: &str = "Choose a wallpaper";
    const INFO: &str = "Show info about the current wallpaper";
    const REAPPLY: &str = "Reapply the last wallpaper";
    const BROWSE_CACHE: &str = "Browse cache";

    let selection = Select::new(
        "What would you like to do?",
        vec![CHOOSE, INFO, REAPPLY, BROWSE_CACHE],
    )
    .prompt()?;
    Ok(match selection {
        CHOOSE => ParentMenuChoice::Choose,
        INFO => ParentMenuChoice::Info,
        REAPPLY => ParentMenuChoice::Reapply,
        BROWSE_CACHE => ParentMenuChoice::BrowseCache,
        _ => unreachable!("inquire only returns one of the provided options"),
    })
}

fn run_menu_selection(
    choice: ParentMenuChoice,
    client: &Client,
    cache: &CacheManager,
    favorites: &FavoritesManager,
    registry: &SourceRegistry,
    settings: &Settings,
    source_settings: &SourceSettings,
    date_arg: Option<&str>,
) -> Result<()> {
    match choice {
        ParentMenuChoice::Choose => dispatch_choose_maybe_dated(
            client,
            cache,
            favorites,
            registry,
            settings,
            source_settings,
            date_arg,
        ),
        ParentMenuChoice::Info => dispatch_info(cache, settings),
        ParentMenuChoice::Reapply => dispatch_reapply(cache, settings),
        ParentMenuChoice::BrowseCache => dispatch_choose_maybe_dated(
            client,
            cache,
            favorites,
            registry,
            settings,
            source_settings,
            Some("pick"),
        ),
    }
}

fn self_heal_auto_update_plist(settings: &Settings, raw_args: &[String]) -> Result<()> {
    let plist_path = settings.plist_filename();
    if !plist_path.exists() {
        return Ok(());
    }
    if plist_has_auto_update_run_token(&plist_path)? {
        return Ok(());
    }
    create_launchd_plist(settings, raw_args)
}

fn plist_program_arguments(path: &Path) -> Result<Vec<String>> {
    let value = Value::from_file(path)?;
    Ok(value
        .as_dictionary()
        .and_then(|dict| dict.get("ProgramArguments"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_string().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

fn plist_has_auto_update_run_token(path: &Path) -> Result<bool> {
    Ok(plist_program_arguments(path)?
        .iter()
        .any(|arg| arg == AUTO_UPDATE_RUN_ARG))
}

fn run_auto_update_body(
    cache: &CacheManager,
    settings: &Settings,
    registry: &SourceRegistry,
    client: &Client,
    source_settings: &SourceSettings,
) -> Result<()> {
    if !settings.force && should_skip_auto_update(cache, settings)? {
        log(
            "Wallpaper already set today; skipping auto update.",
            settings.quiet,
        );
        return Ok(());
    }

    ensure_picture_dir(&settings.picture_dir)?;

    if settings.disabled_sources.contains(&settings.source) {
        return Err(WallpaperError::Message(format!(
            "Source {} is disabled. Remove it from disabled_sources or choose another source.",
            source_label(settings.source)
        )));
    }

    let source = require_source(registry, settings.source)?;
    let date_label = date_label_for(Some(source), settings, source_settings);
    let ctx = SourceContext {
        client,
        cache,
        settings,
        date_label: &date_label,
        source_settings,
        cancel: None,
    };
    let result = run_source(source, &ctx);

    if result.is_ok() {
        if let Some(days) = settings.prune_cache_days {
            prune_cache(cache, days, settings.quiet)?;
        }
    }

    result
}

fn should_skip_auto_update(cache: &CacheManager, _settings: &Settings) -> Result<bool> {
    let Some(last) = cache.read_last_applied()? else {
        return Ok(false);
    };
    let today = Local::now().date_naive().to_string();
    if last.applied_by_user {
        if let Some(applied_at) = Local.timestamp_opt(last.applied_at as i64, 0).single() {
            if applied_at.date_naive().to_string() == today {
                return Ok(true);
            }
        }
    }
    let mut last_date = last.date.clone();
    if last.source == WallpaperSource::Bing {
        let cached_candidate = last
            .date
            .as_deref()
            .and_then(|date| cache.find_candidate_by_id(date, &last.candidate_id).ok())
            .flatten()
            .or_else(|| cache.find_candidate_any_date(&last.candidate_id).ok().flatten());
        if let Some(candidate) = cached_candidate {
            if let Some(xml) = candidate.metadata_xml.as_deref() {
                if let Ok(Some(metadata_date)) = sources::bing::metadata_date_label(xml.as_bytes())
                {
                    last_date = Some(metadata_date);
                }
            }
        }
    }
    if last_date.as_deref() == Some(today.as_str()) {
        return Ok(true);
    }
    if last.source != WallpaperSource::Bing {
        if let Some(applied_at) = Local.timestamp_opt(last.applied_at as i64, 0).single() {
            if applied_at.date_naive().to_string() == today {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(IMAGE_TIMEOUT)
        .build()
        .map_err(Into::into)
}

fn default_picture_dir() -> PathBuf {
    home_dir().join("Pictures").join("daily-wallpapers")
}

fn default_favorites_dir(picture_dir: &Path) -> PathBuf {
    picture_dir.join("favorites")
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn arg_present(args: &[String], flags: &[&str]) -> bool {
    for arg in args {
        for flag in flags {
            if arg == flag || arg.starts_with(&format!("{flag}=")) {
                return true;
            }
        }
    }
    false
}

fn resolve_disabled_sources(
    config: Option<&AppConfig>,
    cli_sources: &[SourceArg],
) -> HashSet<WallpaperSource> {
    let mut disabled = HashSet::new();
    if let Some(cfg) = config {
        if let Some(list) = &cfg.disabled_sources {
            for source in list {
                disabled.insert(map_source((*source).into()));
            }
        }
    }
    for source in cli_sources {
        disabled.insert(map_source(source.clone()));
    }
    disabled
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

fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let normalized = value.trim().to_lowercase();
    let (width, height) = normalized.split_once('x')?;
    let width = width.trim().parse::<u32>().ok()?;
    let height = height.trim().parse::<u32>().ok()?;
    Some((width, height))
}

fn log(message: &str, quiet: bool) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("{timestamp}: {message}");
    write_log_line(&line);
    if quiet {
        return;
    }
    println!("{line}");
}

fn log_verbose(message: &str, settings: &Settings) {
    if settings.quiet || !settings.verbose {
        return;
    }
    log(message, false);
}

pub(crate) fn log_action_start(settings: &Settings, message: &str) {
    let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
    let line = format!("{timestamp}: {message}");
    write_log_line(&line);
    if settings.quiet || !settings.verbose {
        return;
    }
    println!("{line}");
}

#[derive(Clone)]
struct LogConfig {
    path: PathBuf,
    max_bytes: u64,
}

fn log_file_storage() -> &'static Mutex<Option<LogConfig>> {
    static LOG_FILE: OnceLock<Mutex<Option<LogConfig>>> = OnceLock::new();
    LOG_FILE.get_or_init(|| Mutex::new(None))
}

fn set_log_file(path: Option<PathBuf>, max_bytes: u64) {
    let mut guard = log_file_storage().lock().unwrap();
    *guard = path.map(|path| LogConfig { path, max_bytes });
}

fn write_log_line(line: &str) {
    let config = {
        let guard = log_file_storage().lock().unwrap();
        guard.clone()
    };
    let Some(config) = config else {
        return;
    };
    if let Some(parent) = config.path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    rotate_log_file(&config);
    if let Ok(mut file) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn rotate_log_file(config: &LogConfig) {
    let Ok(metadata) = fs::metadata(&config.path) else {
        return;
    };
    if metadata.len() < config.max_bytes {
        return;
    }
    let rotated = PathBuf::from(format!("{}.1", config.path.display()));
    let _ = fs::remove_file(&rotated);
    let _ = fs::rename(&config.path, rotated);
}

fn start_spinner(
    settings: &Settings,
    message: impl Into<String>,
) -> Option<indicatif::ProgressBar> {
    use indicatif::ProgressStyle;

    if settings.verbose || settings.quiet {
        return None;
    }
    let pb = indicatif::ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner} {msg}")
            .unwrap_or_else(|_| ProgressStyle::default_spinner()),
    );
    pb.set_message(message.into());
    pb.enable_steady_tick(Duration::from_millis(120));
    Some(pb)
}

fn finish_spinner(
    spinner: Option<indicatif::ProgressBar>,
    message: &str,
    settings: &Settings,
    clear_only: bool,
) {
    if let Some(pb) = spinner {
        if clear_only {
            pb.finish_and_clear();
        } else if settings.verbose {
            pb.finish_with_message(message.to_string());
        } else {
            pb.finish_and_clear();
        }
    } else if settings.verbose && !settings.quiet && !clear_only {
        log(message, false);
    }
}

fn target_date_for_day(day: i32) -> NaiveDate {
    let today = Local::now().date_naive();
    let delta = ChronoDuration::days(day.into());
    today.checked_sub_signed(delta).unwrap_or(today)
}

fn date_label_for(
    source: Option<&dyn Source>,
    settings: &Settings,
    source_settings: &SourceSettings,
) -> String {
    if let Some(date) = settings.date_override {
        return date.to_string();
    }
    let use_day = source.map(|s| s.supports_day()).unwrap_or(true);
    let target_date = if use_day {
        target_date_for_day(source_settings.bing.day)
    } else {
        Local::now().date_naive()
    };
    target_date.to_string()
}

fn run_source(source: &dyn Source, ctx: &SourceContext<'_>) -> Result<()> {
    let mut fetch_result = source.fetch(ctx)?;
    fetch_result.candidates =
        filter_candidates_by_min_resolution(fetch_result.candidates, ctx.settings)?;
    let candidate = source
        .pick_default(&fetch_result.candidates, ctx)?
        .clone();

    if ctx.settings.experimental && fetch_result.skipped_download {
        log(
            "Download skipped; experimental all-desktops update not applied.",
            ctx.settings.quiet,
        );
        return Ok(());
    }

    apply_wallpaper(
        &candidate.local_path,
        ctx.settings,
        ctx.cache,
        &candidate.id,
        candidate.source,
        Some(&candidate.date),
        false,
    )
}

fn require_source<'a>(registry: &'a SourceRegistry, id: WallpaperSource) -> Result<&'a dyn Source> {
    registry
        .get(id)
        .ok_or_else(|| WallpaperError::Message(format!("Source {:?} is not registered.", id)))
}

fn filter_candidates_by_min_resolution(
    candidates: Vec<WallpaperCandidate>,
    settings: &Settings,
) -> Result<Vec<WallpaperCandidate>> {
    let Some((min_width, min_height)) = settings.min_resolution else {
        return Ok(candidates);
    };

    let mut filtered = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let (width, height) = match image_dimensions(&candidate.local_path) {
            Ok(dim) => dim,
            Err(err) => {
                log_verbose(
                    &format!(
                        "Skipping {} ({}): could not read dimensions: {err}",
                        candidate.id,
                        candidate.local_path.display()
                    ),
                    settings,
                );
                continue;
            }
        };
        if width < min_width || height < min_height {
            log_verbose(
                &format!(
                    "Skipping {} ({}): {}x{} below minimum {}x{}.",
                    candidate.id,
                    candidate.local_path.display(),
                    width,
                    height,
                    min_width,
                    min_height
                ),
                settings,
            );
            continue;
        }
        filtered.push(candidate);
    }

    Ok(filtered)
}

fn run_choose(
    client: &Client,
    cache: &CacheManager,
    favorites: &FavoritesManager,
    registry: &SourceRegistry,
    settings: &Settings,
    source_settings: &SourceSettings,
) -> Result<()> {
    let mut current_settings = settings.clone();
    let mut selected_idx: Option<usize> = None;
    let mut candidates_cache: Vec<WallpaperCandidate> = Vec::new();
    let mut candidates_dirty = true;
    current_settings.refresh_metadata = false;
    let cancel = CancelFlag::new();
    let fetch_active = Arc::new(AtomicBool::new(false));
    if let Err(err) = ctrlc::set_handler({
        let cancel = cancel.clone();
        let fetch_active = fetch_active.clone();
        move || {
            if fetch_active.load(Ordering::SeqCst) {
                cancel.set();
            } else {
                std::process::exit(130);
            }
        }
    }) {
        log(
            &format!("Unable to install Ctrl-C handler: {err}"),
            settings.quiet,
        );
    }

    let normalize_label = |value: &str| {
        value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string()
    };

    loop {
        let favorite_entries = favorites.load_all()?;
        let favorite_ids: HashSet<String> = favorite_entries
            .iter()
            .map(|f| f.id.clone())
            .collect();
        if candidates_dirty {
            cancel.clear();
            fetch_active.store(true, Ordering::SeqCst);
            let fetched = gather_candidates(
                client,
                cache,
                registry,
                &current_settings,
                source_settings,
                &cancel,
            );
            fetch_active.store(false, Ordering::SeqCst);
            candidates_cache =
                filter_candidates_by_min_resolution(fetched?, &current_settings)?;
            // Force should be one-shot in the chooser to avoid repeated re-downloads.
            current_settings.force = false;
            current_settings.refresh_metadata = false;
            candidates_dirty = false;
        }
        let candidates = &candidates_cache;
        if candidates.is_empty() && favorite_entries.is_empty() {
            return Err(WallpaperError::Message(
                "No wallpapers available to choose from.".to_string(),
            ));
        }

        let mut labels: Vec<String> = candidates
            .iter()
            .enumerate()
            .map(|(idx, cand)| {
                let title = cand
                    .title
                    .as_deref()
                    .map(normalize_label)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "(no title)".to_string());
                let attribution = cand
                    .attribution
                    .as_deref()
                    .map(normalize_label)
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(String::new);
                let favorite_marker = if favorite_ids.contains(&cand.id) {
                    " [fav]"
                } else {
                    ""
                };
                format!(
                    "{}: [{}] {}{}{}",
                    idx + 1,
                    source_label(cand.source),
                    title,
                    if attribution.is_empty() {
                        "".to_string()
                    } else {
                        format!(" — {}", attribution)
                    },
                    favorite_marker
                )
            })
            .collect();
        labels.push(format!(
            "{}: [Favorites] ({})",
            candidates.len() + 1,
            favorite_entries.len()
        ));

        let labels_for_prompt = labels.clone();
        let selection = Select::new(
            "Select a wallpaper (arrows + Enter). Choose Preview/Apply next.",
            labels_for_prompt,
        )
        .with_starting_cursor(selected_idx.unwrap_or(0))
        .prompt();

        let (idx, _selected_label) = match selection {
            Ok(label) => {
                let pos = labels.iter().position(|l| l == &label).unwrap_or(0);
                (pos, label)
            }
            Err(_) => return Ok(()),
        };
        selected_idx = Some(idx.min(labels.len().saturating_sub(1)));

        if idx == candidates.len() {
            if favorite_entries.is_empty() {
                println!("No favorites saved yet.");
                continue;
            }
            run_favorites_menu(&favorite_entries, favorites, cache, &current_settings)?;
            continue;
        }

        if let Some(cand) = candidates.get(idx) {
            let mut action_cursor = 0_usize;
            loop {
                let mut actions = vec![
                    "Preview (Quick Look)".to_string(),
                    "Apply".to_string(),
                    "Info".to_string(),
                    "Favorite".to_string(),
                    "Refresh list (force re-download)".to_string(),
                    "Quit chooser".to_string(),
                ];
                if favorite_ids.contains(&cand.id) {
                    actions[3] = "Favorite (already saved)".to_string();
                }

                let action = Select::new("Action", actions)
                .with_starting_cursor(action_cursor)
                .prompt();
                match action {
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
                        // After preview, default to Apply on next prompt.
                        action_cursor = 1;
                        continue;
                    }
                    Ok(choice) if choice.starts_with("Apply") => {
                        match apply_wallpaper(
                            &cand.local_path,
                            &current_settings,
                            cache,
                            &cand.id,
                            cand.source,
                            Some(&cand.date),
                            true,
                        ) {
                            Ok(()) => return Ok(()),
                            Err(err) => println!("Failed to apply wallpaper: {err}"),
                        }
                    }
                    Ok(choice) if choice.starts_with("Info") => {
                        println!();
                    println!("{}", format_candidate_info(cand, settings));
                    println!();
                    wait_for_enter("Press Enter or Esc to return...");
                    continue;
                }
                    Ok(choice) if choice.starts_with("Favorite") => {
                        match favorites.save_favorite(cand) {
                            Ok(_) => println!("Saved to favorites."),
                            Err(err) => println!("Could not favorite: {err}"),
                        }
                        // Refresh favorites on next loop.
                        break;
                    }
                    Ok(choice) if choice.starts_with("Refresh") => {
                        current_settings.force = true;
                        current_settings.refresh_metadata = true;
                        candidates_dirty = true;
                        break;
                    }
                    Ok(choice) if choice.starts_with("Quit") => return Ok(()),
                    Err(InquireError::OperationCanceled) => break,
                    _ => return Ok(()),
                }
            }
        }
    }
}

fn run_favorites_menu(
    favorites: &[FavoriteEntry],
    manager: &FavoritesManager,
    cache: &CacheManager,
    settings: &Settings,
) -> Result<()> {
    let mut fav_cursor: usize = 0;
    loop {
        if favorites.is_empty() {
            println!("No favorites saved yet.");
            return Ok(());
        }
        let labels: Vec<String> = favorites
            .iter()
            .enumerate()
            .map(|(idx, fav)| {
                let title = fav
                    .title
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .unwrap_or("(no title)");
                let attribution = fav
                    .attribution
                    .as_deref()
                    .filter(|t| !t.is_empty())
                    .unwrap_or("");
                format!(
                    "{idx}: [{}] {}{}",
                    source_label(fav.source),
                    title,
                    if attribution.is_empty() {
                        "".to_string()
                    } else {
                        format!(" — {}", attribution)
                    }
                )
            })
            .collect();

        let selection = Select::new("Select a favorite", labels)
            .with_starting_cursor(fav_cursor)
            .prompt();

        let idx = match selection {
            Ok(label) => {
                let parts: Vec<&str> = label.split(':').collect();
                if let Some(num_str) = parts.first() {
                    num_str.parse::<usize>().unwrap_or(0)
                } else {
                    0
                }
            }
            Err(_) => return Ok(()),
        };
        fav_cursor = idx;
        let Some(fav) = favorites.get(idx) else {
            continue;
        };

        let mut action_cursor = 0_usize;
        loop {
            let action = Select::new(
                "Action",
                vec![
                    "Preview (Quick Look)",
                    "Apply",
                    "Info",
                    "Remove from favorites",
                    "Back",
                ],
            )
            .with_starting_cursor(action_cursor)
            .prompt();

            match action {
                Ok(choice) if choice.starts_with("Preview") => {
                    if fav.image_path.exists() {
                        let path_str = fav.image_path.to_string_lossy().to_string();
                        let _ = run_checked("qlmanage", &["-p", &path_str], "Quick Look preview");
                    } else {
                        println!("Favorite image not found: {}", fav.image_path.display());
                    }
                    action_cursor = 1;
                    continue;
                }
                Ok(choice) if choice.starts_with("Apply") => {
                    if !fav.image_path.exists() {
                        println!("Favorite image not found: {}", fav.image_path.display());
                        break;
                    }
                    match apply_wallpaper(
                        &fav.image_path,
                        settings,
                        cache,
                        &fav.id,
                        fav.source,
                        Some(&fav.date),
                        true,
                    ) {
                        Ok(()) => return Ok(()),
                        Err(err) => println!("Failed to apply wallpaper: {err}"),
                    }
                }
                Ok(choice) if choice.starts_with("Info") => {
                    println!();
                    println!("{}", format_favorite_info(fav, settings));
                    println!();
                    wait_for_enter("Press Enter or Esc to return...");
                    continue;
                }
                Ok(choice) if choice.starts_with("Remove") => {
                    if let Err(err) = manager.remove(fav) {
                        println!("Failed to remove favorite: {err}");
                    } else {
                        println!("Removed from favorites.");
                    }
                    return Ok(());
                }
                Ok(choice) if choice.starts_with("Back") => break,
                Err(InquireError::OperationCanceled) => break,
                _ => return Ok(()),
            }
        }
    }
}

fn gather_candidates(
    client: &Client,
    cache: &CacheManager,
    registry: &SourceRegistry,
    settings: &Settings,
    source_settings: &SourceSettings,
    cancel: &CancelFlag,
) -> Result<Vec<WallpaperCandidate>> {
    let mut result = Vec::new();
    let mut skipped_summaries: Vec<String> = Vec::new();
    let sources = registry.all_enabled(&settings.disabled_sources);
    if sources.is_empty() {
        return Err(WallpaperError::Message(
            "All sources are disabled.".to_string(),
        ));
    }

    let skip_reason_for_error = |err: &WallpaperError| -> Option<String> {
        match err {
            WallpaperError::Request(req_err) => {
                if req_err.is_timeout() {
                    Some("request_timeout".to_string())
                } else {
                    Some("request_error".to_string())
                }
            }
            WallpaperError::Status { status, .. } => Some(format!("status_{status}")),
            WallpaperError::DownloadStatus { status, .. } => Some(format!("download_status_{status}")),
            WallpaperError::Download { .. } => Some("download_error".to_string()),
            _ => None,
        }
    };

    for source in sources {
        if cancel.is_set() {
            cancel.clear();
        }
        let date_label = date_label_for(Some(source.as_ref()), settings, source_settings);
        if !settings.force {
            if cache
                .read_in_progress(&date_label, source.id())?
                .is_some()
            {
                let _ = cache.write_skip(&date_label, source.id(), "interrupted");
                cache.clear_in_progress(&date_label, source.id());
                let mut summary =
                    format!("{} skipped (interrupted).", source.label());
                summary.push_str(" Use --force to retry.");
                skipped_summaries.push(summary);
                continue;
            }
        }
        let _ = cache.write_in_progress(&date_label, source.id());
        let spinner = start_spinner(
            settings,
            format!("Fetching {} (Ctrl+C to cancel)…", source.label()),
        );
        let (tx, rx) = mpsc::channel();
        let thread_client = client.clone();
        let thread_cache = cache.clone();
        let thread_settings = settings.clone();
        let thread_source_settings = source_settings.clone();
        let thread_date_label = date_label.clone();
        let thread_cancel = cancel.clone();
        let thread_source = source.clone();
        thread::spawn(move || {
            let ctx = SourceContext {
                client: &thread_client,
                cache: &thread_cache,
                settings: &thread_settings,
                date_label: &thread_date_label,
                source_settings: &thread_source_settings,
                cancel: Some(&thread_cancel),
            };
            let _ = tx.send(thread_source.fetch(&ctx));
        });

        let mut cancel_current = false;
        let fetch_result = loop {
            if cancel.is_set() {
                cancel_current = true;
                break None;
            }
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(res) => break Some(res),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(_) => break Some(Err(WallpaperError::Message(
                    "Fetch thread disconnected unexpectedly.".to_string(),
                ))),
            }
        };

        if cancel_current {
            cache.clear_in_progress(&date_label, source.id());
            finish_spinner(spinner, "", settings, true);
            let _ = cache.write_skip(&date_label, source.id(), "canceled");
            skipped_summaries.push(format!("{} skipped (canceled).", source.label()));
            log(
                &format!("Canceled {}; continuing.", source.label()),
                settings.quiet,
            );
            cancel.clear();
            continue;
        }

        let mut skip_source = false;
        match fetch_result {
            Some(Ok(fetched)) => {
                cache.clear_in_progress(&date_label, source.id());
                finish_spinner(
                    spinner,
                    &format!("Fetched {}", source.label()),
                    settings,
                    false,
                );
                result.extend(fetched.candidates);
            }
            Some(Err(err)) => {
                cache.clear_in_progress(&date_label, source.id());
                finish_spinner(spinner, "", settings, true);
                let mut skip_reason: Option<String> = None;
                let cancel_requested =
                    cancel.is_set() || matches!(err, WallpaperError::Canceled);
                if cancel_requested {
                    let _ = cache.write_skip(&date_label, source.id(), "canceled");
                    skip_reason = Some("canceled".to_string());
                    skip_source = true;
                }
                if !settings.force {
                    if let Ok(Some(skip)) = cache.read_skip(&date_label, source.id()) {
                        skip_reason = Some(skip.reason);
                    }
                }
                if skip_reason.is_none() && !settings.force && !settings.offline {
                    if let Some(reason) = skip_reason_for_error(&err) {
                        let _ = cache.write_skip(&date_label, source.id(), &reason);
                        skip_reason = Some(reason);
                    }
                }
                if let Some(reason) = skip_reason {
                    let mut summary =
                        format!("{} skipped ({reason}).", source.label());
                    if !settings.force {
                        summary.push_str(" Use --force to retry.");
                    }
                    skipped_summaries.push(summary);
                }
                let msg = format!("{} unavailable: {err}", source.label());
                if settings.verbose || !msg.contains("skipped for") {
                    log(&msg, settings.quiet);
                } else {
                    log_verbose(&msg, settings);
                }
            }
            None => {}
        }

        if skip_source {
            log(
                &format!("Canceled {}; continuing.", source.label()),
                settings.quiet,
            );
            cancel.clear();
            continue;
        }
    }

    if !skipped_summaries.is_empty() && !settings.quiet {
        for summary in skipped_summaries {
            log(&summary, settings.quiet);
        }
    }

    Ok(result)
}


#[derive(Debug)]
struct DownloadedFile {
    path: PathBuf,
}
fn download_to_path(
    client: &Client,
    url: &str,
    target_path: &Path,
    settings: &Settings,
    cancel: Option<&CancelFlag>,
) -> Result<DownloadedFile> {
    if settings.offline {
        return Err(WallpaperError::Message(
            "Offline mode is enabled; downloads are disabled.".to_string(),
        ));
    }
    if target_path.exists() && !settings.force {
        log_verbose(
            &format!(
                "Skipping download, already present: {}",
                target_path.display()
            ),
            settings,
        );
        return Ok(DownloadedFile {
            path: target_path.to_path_buf(),
        });
    }

    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let message = format!("Downloading {}", url);
    log_action_start(settings, &message);
    let spinner = start_spinner(settings, message);
    let temp_path = unique_temp_path(target_path);
    let download_result = (|| -> Result<()> {
        let _ = fs::remove_file(&temp_path);
        let mut response = client.get(url).timeout(IMAGE_TIMEOUT).send()?;
        ensure_http_success(response.status(), url)?;
        let mut file = File::create(&temp_path)?;
        let mut buf = [0u8; 32 * 1024];
        loop {
            if cancel.is_some_and(|flag| flag.is_set()) {
                return Err(WallpaperError::Canceled);
            }
            let bytes_read = response.read(&mut buf)?;
            if bytes_read == 0 {
                break;
            }
            file.write_all(&buf[..bytes_read])?;
        }
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temp_path, target_path)?;
        if let Err(err) = enforce_min_resolution(target_path, settings) {
            let _ = fs::remove_file(target_path);
            return Err(err);
        }
        Ok(())
    })();

    if let Err(err) = download_result {
        let _ = fs::remove_file(&temp_path);
        finish_spinner(spinner, "", settings, true);
        return Err(err);
    }

    finish_spinner(
        spinner,
        &format!("Downloaded {}", target_path.display()),
        settings,
        false,
    );

    Ok(DownloadedFile {
        path: target_path.to_path_buf(),
    })
}

pub(crate) fn read_response_bytes_with_cancel(
    mut response: reqwest::blocking::Response,
    cancel: Option<&CancelFlag>,
) -> Result<Vec<u8>> {
    let mut buf = [0u8; 32 * 1024];
    let mut out = Vec::new();
    loop {
        if cancel.is_some_and(|flag| flag.is_set()) {
            return Err(WallpaperError::Canceled);
        }
        let bytes_read = response.read(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        out.extend_from_slice(&buf[..bytes_read]);
    }
    Ok(out)
}

pub(crate) fn enforce_min_resolution(path: &Path, settings: &Settings) -> Result<()> {
    let Some((min_width, min_height)) = settings.min_resolution else {
        return Ok(());
    };

    let (width, height) = match image_dimensions(path) {
        Ok(dim) => dim,
        Err(err) => {
            log_verbose(
                &format!(
                    "Could not read image dimensions for {}: {err}",
                    path.display()
                ),
                settings,
            );
            return Err(WallpaperError::Message(format!(
                "Unable to read image dimensions for {}.",
                path.display()
            )));
        }
    };

    if width < min_width || height < min_height {
        log_verbose(
            &format!(
                "Downloaded image {}x{} below minimum {}x{}; skipping.",
                width, height, min_width, min_height
            ),
            settings,
        );
            return Err(WallpaperError::MinResolution {
                width,
                height,
                min_width,
                min_height,
            });
        }

    Ok(())
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

fn list_cached_dates(cache: &CacheManager, exclude_today: bool) -> Vec<NaiveDate> {
    let today = Local::now().date_naive();
    let mut dates = Vec::new();
    let Ok(entries) = fs::read_dir(&cache.base_dir) else {
        return dates;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(date) = NaiveDate::parse_from_str(name, "%Y-%m-%d") else {
            continue;
        };
        if exclude_today && date == today {
            continue;
        }
        let has_candidates = cache
            .load_index(name)
            .ok()
            .flatten()
            .map(|index| !index.candidates.is_empty())
            .unwrap_or(false);
        if !has_candidates {
            continue;
        }
        dates.push(date);
    }
    dates.sort_by(|a, b| b.cmp(a));
    dates
}

fn apply_wallpaper(
    file_path: &Path,
    settings: &Settings,
    cache: &CacheManager,
    candidate_id: &str,
    source: WallpaperSource,
    candidate_date: Option<&str>,
    applied_by_user: bool,
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
            applied_by_user,
            date: candidate_date.map(|d| d.to_string()),
        };
        cache.write_last_applied(&applied)?;
    }

    result
}

fn reapply_last_wallpaper(cache: &CacheManager, settings: &Settings) -> Result<()> {
    let Some(last) = cache.read_last_applied()? else {
        return Err(WallpaperError::Message(
            "No previously applied wallpaper found. Choose a wallpaper first.".to_string(),
        ));
    };
    if !last.applied_path.exists() {
        return Err(WallpaperError::Message(format!(
            "Previously applied wallpaper is missing: {}",
            last.applied_path.display()
        )));
    }

    log(
        &format!(
            "Reapplying last wallpaper ({:?}) from {}",
            last.source,
            last.applied_path.display()
        ),
        settings.quiet,
    );

    apply_wallpaper(
        &last.applied_path,
        settings,
        cache,
        &last.candidate_id,
        last.source,
        last.date.as_deref(),
        true,
    )
}

fn ensure_info_file(candidate: &WallpaperCandidate) -> Result<()> {
    let Some(parent) = candidate.local_path.parent() else {
        return Ok(());
    };
    let info_path = parent.join("info.xml");
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

fn auto_update_program_arguments(current_exe: &str, raw_args: &[String]) -> Vec<String> {
    let mut filtered_args: Vec<String> = raw_args.to_owned();
    filtered_args.retain(|arg| arg != "enable-auto-update" && arg != AUTO_UPDATE_RUN_ARG);

    let mut program_arguments = vec![current_exe.to_string(), AUTO_UPDATE_RUN_ARG.to_string()];
    program_arguments.extend(filtered_args);
    program_arguments
}

fn create_launchd_plist(settings: &Settings, raw_args: &[String]) -> Result<()> {
    fs::create_dir_all(launchd_dir())?;

    let current_exe = env::current_exe().map_err(|err| {
        WallpaperError::Message(format!("Unable to determine current executable: {err}"))
    })?;
    let program_arguments =
        auto_update_program_arguments(&current_exe.to_string_lossy(), raw_args);

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

fn create_display_sync_plist(settings: &Settings, raw_args: &[String]) -> Result<()> {
    fs::create_dir_all(launchd_dir())?;

    let mut filtered_args: Vec<String> = raw_args.to_owned();
    if let Some(pos) = filtered_args
        .iter()
        .position(|arg| arg == "enable-display-sync")
    {
        filtered_args.remove(pos);
    }

    let current_exe = env::current_exe().map_err(|err| {
        WallpaperError::Message(format!("Unable to determine current executable: {err}"))
    })?;
    let mut program_arguments = vec![
        current_exe.to_string_lossy().to_string(),
        "display-sync".to_string(),
    ];
    program_arguments.extend(filtered_args);

    let mut plist_map: Dictionary = Dictionary::new();
    plist_map.insert(
        "Label".into(),
        Value::String(settings.display_sync_label()),
    );
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
            "/tmp/{PLIST_BASENAME}-display-sync-{}.err",
            settings.auto_update_name
        )),
    );
    plist_map.insert(
        "StandardOutPath".into(),
        Value::String(format!(
            "/tmp/{PLIST_BASENAME}-display-sync-{}.out",
            settings.auto_update_name
        )),
    );
    plist_map.insert("KeepAlive".into(), Value::Boolean(true));
    plist_map.insert("RunAtLoad".into(), Value::Boolean(true));

    let plist_path = settings.display_sync_plist_filename();
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

fn remove_display_sync_plist(settings: &Settings) -> Result<()> {
    let plist_path = settings.display_sync_plist_filename();
    let _ = Command::new("launchctl")
        .args(["unload", "-w", plist_path.to_string_lossy().as_ref()])
        .output();
    let _ = fs::remove_file(&plist_path);
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_display_sync(cache: &CacheManager, settings: &Settings) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let cache = cache.clone();
    let settings = settings.clone();
    let quiet = settings.quiet;

    std::thread::spawn(move || {
        for _ in rx {
            log("Display change detected; reapplying wallpaper.", settings.quiet);
            if let Err(err) = reapply_last_wallpaper(&cache, &settings) {
                log(&format!("Display sync reapply failed: {err}"), settings.quiet);
            }
        }
    });

    let sender = Box::new(tx);
    let sender_ptr = Box::into_raw(sender) as *mut c_void;

    let err = unsafe { CGDisplayRegisterReconfigurationCallback(display_reconfig_callback, sender_ptr) };
    if err != 0 {
        return Err(WallpaperError::Message(format!(
            "Display sync failed to register callback (error {err})."
        )));
    }

    log(
        "Display sync running; reapplying wallpaper on monitor changes.",
        quiet,
    );
    unsafe {
        CFRunLoopRun();
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn run_display_sync(_cache: &CacheManager, _settings: &Settings) -> Result<()> {
    Err(WallpaperError::Message(
        "Display sync is only supported on macOS.".to_string(),
    ))
}

#[cfg(target_os = "macos")]
type CGDirectDisplayID = u32;
#[cfg(target_os = "macos")]
type CGDisplayChangeSummaryFlags = u32;
#[cfg(target_os = "macos")]
type CGError = i32;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayRegisterReconfigurationCallback(
        callback: extern "C" fn(CGDirectDisplayID, CGDisplayChangeSummaryFlags, *mut c_void),
        user_info: *mut c_void,
    ) -> CGError;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRunLoopRun();
}

#[cfg(target_os = "macos")]
extern "C" fn display_reconfig_callback(
    _display: CGDirectDisplayID,
    _flags: CGDisplayChangeSummaryFlags,
    user_info: *mut c_void,
) {
    if user_info.is_null() {
        return;
    }
    let sender = unsafe { &*(user_info as *const std::sync::mpsc::Sender<()>) };
    let _ = sender.send(());
}


fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
            continue;
        }
        if line.len() + 1 + word.len() > width {
            lines.push(line);
            line = word.to_string();
        } else {
            line.push(' ');
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn format_info_line(
    icon: &str,
    label: &str,
    value: &str,
    width: usize,
    plain: bool,
) -> Vec<String> {
    let clean_value = value.replace(['\n', '\r'], " ");
    let prefix_plain = if icon.is_empty() {
        format!("{label}: ")
    } else {
        format!("{icon} {label}: ")
    };
    let prefix_styled = if plain {
        prefix_plain.clone()
    } else if icon.is_empty() {
        format!("{INFO_LABEL_COLOR}{label}:{INFO_RESET} ")
    } else {
        format!("{INFO_LABEL_COLOR}{icon} {label}:{INFO_RESET} ")
    };
    let prefix_len = prefix_plain.chars().count();
    if width <= prefix_len + 8 {
        return vec![format!("{prefix_styled}{clean_value}")];
    }

    let content_width = width.saturating_sub(prefix_len).max(8);
    let wrapped = wrap_text(&clean_value, content_width);
    let indent = " ".repeat(prefix_len);
    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                format!("{prefix_styled}{line}")
            } else {
                format!("{indent}{line}")
            }
        })
        .collect()
}

fn format_basic_info(
    title: Option<&str>,
    description: Option<&str>,
    attribution: Option<&str>,
    info_url: Option<&str>,
    width: usize,
    plain: bool,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    let title_icon = if plain { "" } else { INFO_ICON_TITLE };
    let about_icon = if plain { "" } else { INFO_ICON_ABOUT };
    let credit_icon = if plain { "" } else { INFO_ICON_CREDIT };
    let link_icon = if plain { "" } else { INFO_ICON_LINK };
    if let Some(title) = title.filter(|t| !t.is_empty()) {
        lines.extend(format_info_line(title_icon, "Title", title, width, plain));
    }
    if let Some(desc) = description.filter(|d| !d.is_empty()) {
        lines.extend(format_info_line(about_icon, "About", desc, width, plain));
    }
    let attribution = attribution
        .filter(|a| !a.is_empty())
        .unwrap_or("Unknown copyright");
    lines.extend(format_info_line(credit_icon, "Credit", attribution, width, plain));
    if let Some(link) = info_url.filter(|l| !l.is_empty()) {
        lines.extend(format_info_line(link_icon, "Link", link, width, plain));
    }
    lines.join("\n")
}

fn format_candidate_info(candidate: &WallpaperCandidate, settings: &Settings) -> String {
    format_basic_info(
        candidate.title.as_deref(),
        candidate.description.as_deref(),
        candidate.attribution.as_deref(),
        candidate.info_url.as_deref(),
        settings.info_wrap_width,
        settings.info_plain_text,
    )
}

fn format_favorite_info(favorite: &FavoriteEntry, settings: &Settings) -> String {
    format_basic_info(
        favorite.title.as_deref(),
        favorite.description.as_deref(),
        favorite.attribution.as_deref(),
        favorite.info_url.as_deref(),
        settings.info_wrap_width,
        settings.info_plain_text,
    )
}

fn show_info<W: Write>(cache: &CacheManager, settings: &Settings, mut writer: W) -> Result<()> {
    let Some(last) = cache.read_last_applied()? else {
        return Err(WallpaperError::Message(
            "No previously applied wallpaper found. Run the download first.".to_string(),
        ));
    };

    let candidate = if let Some(date) = last.date.as_deref() {
        match cache.find_candidate_by_id(date, &last.candidate_id)? {
            Some(found) => Some(found),
            None => cache.find_candidate_any_date(&last.candidate_id)?,
        }
    } else {
        cache.find_candidate_any_date(&last.candidate_id)?
    };

    if let Some(candidate) = candidate {
        let info = format_candidate_info(&candidate, settings);
        writeln!(writer, "{info}")?;
        return Ok(());
    }

    Err(WallpaperError::Message(
        "No metadata found for last applied wallpaper. Run the download first.".to_string(),
    ))
}

fn wait_for_enter(prompt: &str) {
    let result = Text::new(prompt).prompt();
    match result {
        Ok(_) => {}
        Err(InquireError::OperationCanceled) | Err(InquireError::OperationInterrupted) => {}
        Err(_) => {}
    }
}

fn ensure_picture_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

pub(crate) fn ensure_http_success(status: StatusCode, url: &str) -> Result<()> {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return Err(WallpaperError::Message(format!(
            "Rate limited by {} (HTTP 429). Please retry later.",
            url
        )));
    }
    if status != StatusCode::OK {
        return Err(WallpaperError::Status {
            url: url.to_string(),
            status: status.as_u16(),
        });
    }
    Ok(())
}

fn load_config() -> Option<AppConfig> {
    let path = home_dir().join(".wallpaperconfig");
    let contents = fs::read_to_string(path).ok()?;
    toml::from_str(&contents).ok()
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
    use crate::sources::bing;
    use crate::favorites::FavoritesManager;
    use httpmock::Method::GET;
    use httpmock::MockServer;
    use tempfile::tempdir;

    fn make_settings(tmpdir: &Path, filename: Option<&str>, force: bool) -> Settings {
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
            filename: filename.map(ToString::to_string),
            source: WallpaperSource::Bing,
            prune_cache_days: None,
            info_wrap_width: DEFAULT_INFO_WRAP_WIDTH,
            info_plain_text: false,
            refresh_metadata: true,
            min_resolution: None,
            log_file: None,
            log_file_max_bytes: 5120 * 1024,
            disabled_sources: HashSet::new(),
            date_override: None,
        }
    }

    fn run_source_for_test(
        source_id: WallpaperSource,
        client: &Client,
        cache: &CacheManager,
        settings: &Settings,
        date_label: &str,
        source_settings: &SourceSettings,
    ) -> Result<()> {
        let registry = SourceRegistry::new();
        let source = require_source(&registry, source_id)?;
        let ctx = SourceContext {
            client,
            cache,
            settings,
            date_label,
            source_settings,
            cancel: None,
        };
        run_source(source, &ctx)
    }

    #[test]
    fn normalize_auto_update_name_cleans() {
        assert_eq!(normalize_auto_update_name("  "), "default");
        assert_eq!(normalize_auto_update_name("foo bar"), "foo-bar");
        assert_eq!(normalize_auto_update_name("Name_1"), "Name_1");
    }

    #[test]
    fn sanitize_filename_behaves_like_python() {
        assert_eq!(bing::sanitize_filename(""), "wallpaper.jpg");
        assert_eq!(bing::sanitize_filename("custom"), "custom.jpg");
        assert_eq!(bing::sanitize_filename("dir/../name.png"), "name.png");
    }

    #[test]
    fn build_archive_url_includes_country() {
        let url = bing::build_archive_url(1, Some("en-US"));
        assert!(url.contains("idx=1"));
        assert!(url.contains("mkt=en-US"));
    }

    #[test]
    fn download_image_skips_existing_without_force() {
        let server = MockServer::start();
        let metadata = b"<xml />";
        let res = "1920x1080";
        let url_base = "/th?id=test";
        let date_label = "2024-01-01";

        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let settings = Settings {
            proto: "http".into(),
            ..make_settings(tmpdir.path(), None, false)
        };
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.bing.host = server.address().to_string();

        let target_dir = cache.media_dir(date_label, WallpaperSource::Bing);
        let target = target_dir.join(format!(
            "default-{}_{}.jpg",
            url_base.replace("/th?id=", ""),
            res
        ));
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(&target, b"existing").unwrap();

        let client = build_client().unwrap();
        let _mock = server.mock(|when, then| {
            when.method(GET);
            then.status(200).body("image-bytes");
        });

        let downloaded = bing::download_image(
            &client,
            url_base,
            res,
            &source_settings.bing,
            "en-US",
            &settings,
            metadata,
            &cache,
            date_label,
            None,
        )
        .unwrap();
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
        let date_label = "2024-01-01";

        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let settings = Settings {
            proto: "http".into(),
            ..make_settings(tmpdir.path(), None, false)
        };
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.bing.host = server.address().to_string();

        let target_dir = cache.media_dir(date_label, WallpaperSource::Bing);
        fs::create_dir_all(&target_dir).unwrap();
        let old_wallpaper = target_dir.join("default-old.jpg");
        fs::write(&old_wallpaper, b"old").unwrap();

        let client = build_client().unwrap();
        let _mock = server.mock(|when, then| {
            when.method(GET);
            then.status(200).body("image-bytes");
        });

        let downloaded = bing::download_image(
            &client,
            url_base,
            res,
            &source_settings.bing,
            "en-US",
            &settings,
            metadata,
            &cache,
            date_label,
            None,
        )
        .unwrap();
        assert!(!downloaded.skipped);
        assert!(downloaded.path.exists());
        assert_eq!(fs::read(&downloaded.path).unwrap(), b"image-bytes");
        assert!(!old_wallpaper.exists());
        assert_eq!(fs::read(target_dir.join("info.xml")).unwrap(), metadata);
        assert_eq!(_mock.hits(), 1);
    }

    #[test]
    fn download_image_http_error_cleans_temp() {
        let server = MockServer::start();
        let metadata = b"meta";
        let res = "1920x1080";
        let url_base = "/th?id=test";
        let date_label = "2024-01-01";

        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let settings = Settings {
            proto: "http".into(),
            ..make_settings(tmpdir.path(), None, false)
        };
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.bing.host = server.address().to_string();

        let target_dir = cache.media_dir(date_label, WallpaperSource::Bing);
        let target = target_dir.join(format!(
            "default-{}_{}.jpg",
            url_base.replace("/th?id=", ""),
            res
        ));

        let client = build_client().unwrap();
        let _mock = server.mock(|when, then| {
            when.method(GET);
            then.status(404);
        });

        let err = bing::download_image(
            &client,
            url_base,
            res,
            &source_settings.bing,
            "en-US",
            &settings,
            metadata,
            &cache,
            date_label,
            None,
        )
        .unwrap_err();
        assert!(matches!(err, WallpaperError::DownloadStatus { .. }));
        assert!(!target.exists());

        let temps: Vec<_> = fs::read_dir(target_dir)
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
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.spotlight.index = 2;
        source_settings.spotlight.url_override = Some(api_url.clone());

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = Local::now().date_naive().to_string();

        run_source_for_test(
            WallpaperSource::Spotlight,
            &client,
            &cache,
            &settings,
            &date_label,
            &source_settings,
        )
        .unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img1_mock.hits(), 1);
        assert_eq!(img2_mock.hits(), 1);
        assert_eq!(img3_mock.hits(), 1);

        // Second run should reuse cache and skip network.
        run_source_for_test(
            WallpaperSource::Spotlight,
            &client,
            &cache,
            &settings,
            &date_label,
            &source_settings,
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
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.apod.api_key = "TEST".into();
        source_settings.apod.url_override = Some(api_url);
        source_settings.apod.crop = false;

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = "2024-01-01";

        run_source_for_test(
            WallpaperSource::Apod,
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings,
        )
        .unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img_mock.hits(), 1);

        // second run should reuse cache
        run_source_for_test(
            WallpaperSource::Apod,
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings,
        )
        .unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img_mock.hits(), 1);
    }

    #[test]
    fn apod_offline_reuses_cache_without_network() {
        let server = MockServer::start();
        let api_url = server.url("/apod");
        let img_url = server.url("/image.jpg");

        let api_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/apod")
                .query_param("api_key", "TEST")
                .query_param("date", "2024-01-03");
            then.status(200).body(
                r#"{
                "url":"IMAGE_URL",
                "media_type":"image",
                "title":"Nebula 2",
                "explanation":"desc 2"
            }"#
                .replace("IMAGE_URL", &img_url),
            );
        });
        let img_mock = server.mock(|when, then| {
            when.method(GET).path("/image.jpg");
            then.status(200).body("img-bytes-2");
        });

        let tmpdir = tempdir().unwrap();
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.source = WallpaperSource::Apod;
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.apod.api_key = "TEST".into();
        source_settings.apod.url_override = Some(api_url);
        source_settings.apod.crop = false;

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = "2024-01-03";

        run_source_for_test(
            WallpaperSource::Apod,
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings,
        )
        .unwrap();
        assert_eq!(api_mock.hits(), 1);
        assert_eq!(img_mock.hits(), 1);

        settings.offline = true;
        settings.force = true;

        run_source_for_test(
            WallpaperSource::Apod,
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings,
        )
        .unwrap();
        assert_eq!(api_mock.hits(), 1, "metadata fetch must be skipped offline");
        assert_eq!(img_mock.hits(), 1, "image download must be skipped offline");
    }

    #[test]
    fn apod_offline_errors_without_cache() {
        let server = MockServer::start();
        let api_url = server.url("/apod");
        let img_url = server.url("/image.jpg");

        let api_mock = server.mock(|when, then| {
            when.method(GET)
                .path("/apod")
                .query_param("api_key", "TEST")
                .query_param("date", "2024-01-04");
            then.status(200).body(
                r#"{
                "url":"IMAGE_URL",
                "media_type":"image",
                "title":"Nebula 3",
                "explanation":"desc 3"
            }"#
                .replace("IMAGE_URL", &img_url),
            );
        });
        let img_mock = server.mock(|when, then| {
            when.method(GET).path("/image.jpg");
            then.status(200).body("img-bytes-3");
        });

        let tmpdir = tempdir().unwrap();
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.source = WallpaperSource::Apod;
        settings.offline = true;
        settings.force = true;
        let mut source_settings = SourceSettings::from_config(None).unwrap();
        source_settings.apod.api_key = "TEST".into();
        source_settings.apod.url_override = Some(api_url);
        source_settings.apod.crop = false;

        let cache = CacheManager::new(tmpdir.path());
        let client = build_client().unwrap();
        let date_label = "2024-01-04";

        let err = run_source_for_test(
            WallpaperSource::Apod,
            &client,
            &cache,
            &settings,
            date_label,
            &source_settings,
        )
        .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("Offline mode enabled"));
        assert_eq!(api_mock.hits(), 0);
        assert_eq!(img_mock.hits(), 0);
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
            checksum: None,
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
        let cache = CacheManager::new(tmpdir.path());
        let date_label = "2024-01-02";
        let media_dir = cache.media_dir(date_label, WallpaperSource::Bing);
        let candidate = WallpaperCandidate {
            id: "bing-2024-01-02-uhd".into(),
            source: WallpaperSource::Bing,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "http://example".into(),
            local_path: media_dir.join("wallpaper2.jpg"),
            date: date_label.into(),
            metadata_xml: Some("<xml>info</xml>".into()),
            checksum: None,
        };

        let info_path = media_dir.join("info.xml");
        assert!(!info_path.exists());
        ensure_info_file(&candidate).unwrap();
        assert!(info_path.exists());
        let contents = fs::read_to_string(info_path).unwrap();
        assert!(contents.contains("info"));
    }

    #[test]
    fn show_info_uses_last_applied_candidate_metadata() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let candidate = WallpaperCandidate {
            id: "spotlight-2024-02-01-1".into(),
            source: WallpaperSource::Spotlight,
            title: Some("Spotlight Title".into()),
            description: Some("Spotlight description".into()),
            attribution: Some("Copyright 2024".into()),
            info_url: Some("https://example.com/info".into()),
            image_url: "http://example.com/image.jpg".into(),
            local_path: tmpdir.path().join("spotlight.jpg"),
            date: "2024-02-01".into(),
            metadata_xml: None,
            checksum: None,
        };

        cache
            .upsert_candidate(&candidate.date, candidate.clone())
            .unwrap();
        cache
            .write_last_applied(&LastApplied {
                candidate_id: candidate.id.clone(),
                source: candidate.source,
                applied_path: candidate.local_path.clone(),
                applied_at: 0,
                applied_by_user: false,
                date: Some(candidate.date.clone()),
            })
            .unwrap();

        let mut buffer = Vec::new();
        let settings = make_settings(tmpdir.path(), None, false);
        show_info(&cache, &settings, &mut buffer).unwrap();
        let output = String::from_utf8(buffer).unwrap();
        assert!(output.contains("Spotlight Title"));
        assert!(output.contains("Spotlight description"));
        assert!(output.contains("Copyright 2024"));
        assert!(output.contains("https://example.com/info"));
    }

    #[test]
    fn show_info_errors_without_last_applied() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let mut buffer = Vec::new();
        let settings = make_settings(tmpdir.path(), None, false);
        let err = show_info(&cache, &settings, &mut buffer).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("previously applied wallpaper"));
    }

    #[test]
    fn auto_update_does_not_skip_bing_when_metadata_not_today() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let startdate = yesterday.format("%Y%m%d").to_string();
        let metadata_xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><images><image><startdate>{}</startdate></image></images>",
            startdate
        );
        let candidate = WallpaperCandidate {
            id: format!("bing-{}-en-US-1920x1080", yesterday),
            source: WallpaperSource::Bing,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://example.com/image.jpg".into(),
            local_path: tmpdir.path().join("wallpaper.jpg"),
            date: yesterday.to_string(),
            metadata_xml: Some(metadata_xml),
            checksum: None,
        };

        cache
            .upsert_candidate(&candidate.date, candidate.clone())
            .unwrap();
        cache
            .write_last_applied(&LastApplied {
                candidate_id: candidate.id.clone(),
                source: candidate.source,
                applied_path: candidate.local_path.clone(),
                applied_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                applied_by_user: false,
                date: Some(candidate.date.clone()),
            })
            .unwrap();

        let settings = make_settings(tmpdir.path(), None, false);
        assert!(!should_skip_auto_update(&cache, &settings).unwrap());
    }

    #[test]
    fn auto_update_skips_bing_when_user_applied_today() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let startdate = yesterday.format("%Y%m%d").to_string();
        let metadata_xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><images><image><startdate>{}</startdate></image></images>",
            startdate
        );
        let candidate = WallpaperCandidate {
            id: format!("bing-{}-en-US-1920x1080", yesterday),
            source: WallpaperSource::Bing,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://example.com/image.jpg".into(),
            local_path: tmpdir.path().join("wallpaper.jpg"),
            date: yesterday.to_string(),
            metadata_xml: Some(metadata_xml),
            checksum: None,
        };

        cache
            .upsert_candidate(&candidate.date, candidate.clone())
            .unwrap();
        cache
            .write_last_applied(&LastApplied {
                candidate_id: candidate.id.clone(),
                source: candidate.source,
                applied_path: candidate.local_path.clone(),
                applied_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                applied_by_user: true,
                date: Some(candidate.date.clone()),
            })
            .unwrap();

        let settings = make_settings(tmpdir.path(), None, false);
        assert!(should_skip_auto_update(&cache, &settings).unwrap());
    }

    #[test]
    fn auto_update_skips_bing_when_metadata_is_today() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let today = Local::now().date_naive();
        let startdate = today.format("%Y%m%d").to_string();
        let metadata_xml = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><images><image><startdate>{}</startdate></image></images>",
            startdate
        );
        let candidate = WallpaperCandidate {
            id: format!("bing-{}-en-US-1920x1080", today),
            source: WallpaperSource::Bing,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://example.com/image.jpg".into(),
            local_path: tmpdir.path().join("wallpaper.jpg"),
            date: today.to_string(),
            metadata_xml: Some(metadata_xml),
            checksum: None,
        };

        cache
            .upsert_candidate(&candidate.date, candidate.clone())
            .unwrap();
        cache
            .write_last_applied(&LastApplied {
                candidate_id: candidate.id.clone(),
                source: candidate.source,
                applied_path: candidate.local_path.clone(),
                applied_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                applied_by_user: false,
                date: Some(candidate.date.clone()),
            })
            .unwrap();

        let settings = make_settings(tmpdir.path(), None, false);
        assert!(should_skip_auto_update(&cache, &settings).unwrap());
    }

    #[test]
    fn auto_update_skips_non_bing_when_applied_today() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let yesterday = (Local::now().date_naive() - ChronoDuration::days(1)).to_string();
        let candidate = WallpaperCandidate {
            id: format!("spotlight-{}-1", yesterday),
            source: WallpaperSource::Spotlight,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://example.com/spotlight.jpg".into(),
            local_path: tmpdir.path().join("spotlight.jpg"),
            date: yesterday.clone(),
            metadata_xml: None,
            checksum: None,
        };

        cache
            .upsert_candidate(&candidate.date, candidate.clone())
            .unwrap();
        cache
            .write_last_applied(&LastApplied {
                candidate_id: candidate.id.clone(),
                source: candidate.source,
                applied_path: candidate.local_path.clone(),
                applied_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                applied_by_user: false,
                date: Some(candidate.date.clone()),
            })
            .unwrap();

        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.source = WallpaperSource::Spotlight;
        assert!(should_skip_auto_update(&cache, &settings).unwrap());
    }

    fn dummy_candidate(tmpdir: &Path, id: &str) -> WallpaperCandidate {
        let img_path = tmpdir.join(format!("{id}.png"));
        let buffer = image::ImageBuffer::from_pixel(10, 10, image::Rgba([1u8, 2u8, 3u8, 255u8]));
        buffer
            .save_with_format(&img_path, image::ImageFormat::Png)
            .unwrap();
        WallpaperCandidate {
            id: id.to_string(),
            source: WallpaperSource::Bing,
            title: Some("Title".into()),
            description: Some("Desc".into()),
            attribution: Some("Attr".into()),
            info_url: Some("https://example.com/info".into()),
            image_url: "https://example.com/img.jpg".into(),
            local_path: img_path,
            date: "2024-01-01".into(),
            metadata_xml: Some("<xml>meta</xml>".into()),
            checksum: None,
        }
    }

    #[test]
    fn favorites_save_load_and_remove() {
        let tmpdir = tempdir().unwrap();
        let favorites_dir = tmpdir.path().join("favorites");
        let manager = FavoritesManager::new(favorites_dir.clone());
        let candidate = dummy_candidate(tmpdir.path(), "fav1");

        let saved = manager.save_favorite(&candidate).unwrap();
        assert!(saved.image_path.exists());
        let stem = Path::new(&saved.stored_filename)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert!(favorites_dir.join(format!("{stem}.json")).exists());

        let loaded = manager.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, candidate.id);
        assert!(loaded[0].image_path.exists());

        manager.remove(&loaded[0]).unwrap();
        let after = manager.load_all().unwrap();
        assert!(after.is_empty());
    }

    #[test]
    fn favorites_prevent_duplicates() {
        let tmpdir = tempdir().unwrap();
        let manager = FavoritesManager::new(tmpdir.path().join("favorites"));
        let candidate = dummy_candidate(tmpdir.path(), "dup");

        manager.save_favorite(&candidate).unwrap();
        let err = manager.save_favorite(&candidate).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("already in favorites"));
    }

    #[test]
    fn auto_update_run_is_hidden_from_help() {
        let help = <Cli as clap::CommandFactory>::command().render_long_help().to_string();
        assert!(!help.contains(AUTO_UPDATE_RUN_ARG));
    }

    #[test]
    fn auto_update_program_arguments_fresh_enable_includes_hidden_subcommand() {
        let args = auto_update_program_arguments(
            "/usr/local/bin/daily-wallpaper",
            &[
                "enable-auto-update".to_string(),
                "--auto-update-name".to_string(),
                "work".to_string(),
            ],
        );
        assert_eq!(
            args,
            vec![
                "/usr/local/bin/daily-wallpaper",
                AUTO_UPDATE_RUN_ARG,
                "--auto-update-name",
                "work",
            ]
        );
    }

    #[test]
    fn auto_update_program_arguments_rerun_over_existing_schedule_stays_explicit() {
        // Simulate re-running `enable-auto-update` a second time for a name that
        // already has an installed plist: raw_args again carries the subcommand
        // token clap parsed, and the rebuilt ProgramArguments must still contain
        // exactly one copy of the hidden subcommand, never a duplicate.
        let first = auto_update_program_arguments(
            "/usr/local/bin/daily-wallpaper",
            &["enable-auto-update".to_string()],
        );
        assert_eq!(first, vec!["/usr/local/bin/daily-wallpaper", AUTO_UPDATE_RUN_ARG]);

        let rerun = auto_update_program_arguments(
            "/usr/local/bin/daily-wallpaper",
            &["enable-auto-update".to_string()],
        );
        assert_eq!(rerun, vec!["/usr/local/bin/daily-wallpaper", AUTO_UPDATE_RUN_ARG]);
    }

    #[test]
    fn auto_update_program_arguments_never_duplicates_hidden_token() {
        // If raw_args already contains the hidden token (e.g. a stale plist's
        // ProgramArguments being reused), it must not be duplicated.
        let args = auto_update_program_arguments(
            "/usr/local/bin/daily-wallpaper",
            &[AUTO_UPDATE_RUN_ARG.to_string()],
        );
        assert_eq!(args, vec!["/usr/local/bin/daily-wallpaper", AUTO_UPDATE_RUN_ARG]);
    }

    fn write_fake_plist(path: &Path, program_arguments: &[&str]) {
        let mut plist_map: Dictionary = Dictionary::new();
        plist_map.insert(
            "ProgramArguments".into(),
            Value::Array(
                program_arguments
                    .iter()
                    .map(|s| Value::String(s.to_string()))
                    .collect(),
            ),
        );
        let value = Value::Dictionary(plist_map);
        let mut bytes = Vec::new();
        plist::to_writer_xml(&mut bytes, &value).unwrap();
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn plist_has_auto_update_run_token_detects_old_style_plist() {
        let tmpdir = tempdir().unwrap();
        let plist_path = tmpdir.path().join("old-style.plist");
        write_fake_plist(&plist_path, &["/usr/local/bin/daily-wallpaper"]);
        assert!(!plist_has_auto_update_run_token(&plist_path).unwrap());
    }

    #[test]
    fn plist_has_auto_update_run_token_detects_migrated_plist() {
        let tmpdir = tempdir().unwrap();
        let plist_path = tmpdir.path().join("migrated.plist");
        write_fake_plist(
            &plist_path,
            &["/usr/local/bin/daily-wallpaper", AUTO_UPDATE_RUN_ARG],
        );
        assert!(plist_has_auto_update_run_token(&plist_path).unwrap());
    }

    #[test]
    fn self_heal_skips_when_no_plist_exists() {
        let tmpdir = tempdir().unwrap();
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.auto_update_name = unique_auto_update_name("no-plist");
        // No plist exists on disk for this auto_update_name, so self-heal must
        // be a no-op: no file is created and no launchctl call is attempted.
        self_heal_auto_update_plist(&settings, &[]).unwrap();
        assert!(!settings.plist_filename().exists());
    }

    fn unique_auto_update_name(label: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("test-{label}-{nanos}")
    }

    #[test]
    fn auto_update_run_direct_dispatch_never_touches_plist() {
        // CommandArg::AutoUpdateRun's match arm calls run_auto_update_body
        // directly and never calls self_heal_auto_update_plist, so a stale,
        // pre-migration plist on disk for this auto_update_name must be left
        // completely untouched by that code path.
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let yesterday = (Local::now().date_naive() - ChronoDuration::days(1)).to_string();
        let candidate = WallpaperCandidate {
            id: format!("spotlight-{}-1", yesterday),
            source: WallpaperSource::Spotlight,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://example.com/spotlight.jpg".into(),
            local_path: tmpdir.path().join("spotlight.jpg"),
            date: yesterday.clone(),
            metadata_xml: None,
            checksum: None,
        };
        cache
            .upsert_candidate(&candidate.date, candidate.clone())
            .unwrap();
        cache
            .write_last_applied(&LastApplied {
                candidate_id: candidate.id.clone(),
                source: candidate.source,
                applied_path: candidate.local_path.clone(),
                applied_at: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                applied_by_user: false,
                date: Some(candidate.date.clone()),
            })
            .unwrap();

        // Applied today (non-Bing), so should_skip_auto_update short-circuits
        // run_auto_update_body before any network access.
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.source = WallpaperSource::Spotlight;
        settings.auto_update_name = unique_auto_update_name("direct-hidden");

        let plist_path = settings.plist_filename();
        fs::create_dir_all(plist_path.parent().unwrap()).unwrap();
        write_fake_plist(&plist_path, &["/usr/local/bin/daily-wallpaper"]);
        let before = fs::read(&plist_path).unwrap();

        let registry = SourceRegistry::new();
        let client = build_client().unwrap();
        let source_settings = SourceSettings::from_config(None).unwrap();
        run_auto_update_body(&cache, &settings, &registry, &client, &source_settings).unwrap();

        let after = fs::read(&plist_path).unwrap();
        assert_eq!(before, after);

        let _ = fs::remove_file(&plist_path);
    }

    #[test]
    fn run_menu_selection_info_matches_dispatch_info_error_path() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let settings = make_settings(tmpdir.path(), None, false);

        let via_menu = run_menu_selection(
            ParentMenuChoice::Info,
            &build_client().unwrap(),
            &cache,
            &FavoritesManager::new(tmpdir.path().join("favorites")),
            &SourceRegistry::new(),
            &settings,
            &SourceSettings::from_config(None).unwrap(),
            None,
        );
        let via_dispatch = dispatch_info(&cache, &settings);

        let via_menu_msg = via_menu.unwrap_err().to_string();
        let via_dispatch_msg = via_dispatch.unwrap_err().to_string();
        assert_eq!(via_menu_msg, via_dispatch_msg);
    }

    #[test]
    fn run_menu_selection_reapply_matches_dispatch_reapply_error_path() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let settings = make_settings(tmpdir.path(), None, false);

        let via_menu = run_menu_selection(
            ParentMenuChoice::Reapply,
            &build_client().unwrap(),
            &cache,
            &FavoritesManager::new(tmpdir.path().join("favorites")),
            &SourceRegistry::new(),
            &settings,
            &SourceSettings::from_config(None).unwrap(),
            None,
        );
        let via_dispatch = dispatch_reapply(&cache, &settings);

        let via_menu_msg = via_menu.unwrap_err().to_string();
        let via_dispatch_msg = via_dispatch.unwrap_err().to_string();
        assert_eq!(via_menu_msg, via_dispatch_msg);
    }

    #[test]
    fn explicit_choose_info_reapply_subcommands_parse_unaffected() {
        let cli = Cli::parse_from(["daily-wallpaper", "info"]);
        assert!(matches!(cli.command, Some(CommandArg::Info)));
        let cli = Cli::parse_from(["daily-wallpaper", "choose"]);
        assert!(matches!(cli.command, Some(CommandArg::Choose)));
        let cli = Cli::parse_from(["daily-wallpaper", "reapply"]);
        assert!(matches!(cli.command, Some(CommandArg::Reapply)));
        let cli = Cli::parse_from(["daily-wallpaper", "auto-update-run"]);
        assert!(matches!(cli.command, Some(CommandArg::AutoUpdateRun)));
        let cli = Cli::parse_from(["daily-wallpaper"]);
        assert!(cli.command.is_none());
    }

    fn insert_dummy_candidate(cache: &CacheManager, date: &str, source: WallpaperSource) {
        let candidate = WallpaperCandidate {
            id: format!("{source:?}-{date}"),
            source,
            title: None,
            description: None,
            attribution: None,
            info_url: None,
            image_url: "https://example.com/image.jpg".into(),
            local_path: PathBuf::from(format!("/tmp/{source:?}-{date}.jpg")),
            date: date.to_string(),
            metadata_xml: None,
            checksum: None,
        };
        cache.upsert_candidate(date, candidate).unwrap();
    }

    #[test]
    fn cli_parses_date_flag_direct_and_pick() {
        let cli = Cli::parse_from(["daily-wallpaper", "choose", "--date", "2026-08-10"]);
        assert_eq!(cli.date.as_deref(), Some("2026-08-10"));
        let cli = Cli::parse_from(["daily-wallpaper", "choose", "--date", "pick"]);
        assert_eq!(cli.date.as_deref(), Some("pick"));
        let cli = Cli::parse_from(["daily-wallpaper", "choose"]);
        assert_eq!(cli.date, None);
    }

    #[test]
    fn list_cached_dates_ignores_non_date_and_empty_folders() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let today = Local::now().date_naive();
        let with_candidate = (today - ChronoDuration::days(2)).to_string();
        let empty_index = (today - ChronoDuration::days(3)).to_string();

        insert_dummy_candidate(&cache, &with_candidate, WallpaperSource::Bing);
        fs::create_dir_all(cache.base_dir.join(&empty_index)).unwrap();
        fs::create_dir_all(cache.base_dir.join("not-a-date")).unwrap();

        let dates = list_cached_dates(&cache, false);
        assert_eq!(dates, vec![NaiveDate::parse_from_str(&with_candidate, "%Y-%m-%d").unwrap()]);
    }

    #[test]
    fn list_cached_dates_sorts_descending_and_can_exclude_today() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let today = Local::now().date_naive();
        let yesterday = today - ChronoDuration::days(1);
        let two_days_ago = today - ChronoDuration::days(2);

        insert_dummy_candidate(&cache, &today.to_string(), WallpaperSource::Bing);
        insert_dummy_candidate(&cache, &yesterday.to_string(), WallpaperSource::Bing);
        insert_dummy_candidate(&cache, &two_days_ago.to_string(), WallpaperSource::Bing);

        let including_today = list_cached_dates(&cache, false);
        assert_eq!(including_today, vec![today, yesterday, two_days_ago]);

        let excluding_today = list_cached_dates(&cache, true);
        assert_eq!(excluding_today, vec![yesterday, two_days_ago]);
    }

    #[test]
    fn validate_date_arg_rejects_malformed_date() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let err = validate_date_arg(&cache, "2026-13-40").unwrap_err();
        assert_eq!(
            err.to_string(),
            "Invalid date '2026-13-40'. Expected YYYY-MM-DD, or use --date pick to select from cached dates."
        );
    }

    #[test]
    fn validate_date_arg_rejects_future_date() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let future = (Local::now().date_naive() + ChronoDuration::days(1)).to_string();
        let err = validate_date_arg(&cache, &future).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("{future} is in the future; cache only holds past days.")
        );
    }

    #[test]
    fn validate_date_arg_errors_when_nothing_cached_lists_available_dates() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let today = Local::now().date_naive();
        let cached = today - ChronoDuration::days(1);
        let requested = today - ChronoDuration::days(2);
        insert_dummy_candidate(&cache, &cached.to_string(), WallpaperSource::Bing);

        let err = validate_date_arg(&cache, &requested.to_string()).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "No cached wallpapers found for {requested}. Cached dates available: {cached}. Use --date pick to select interactively."
            )
        );
    }

    #[test]
    fn validate_date_arg_errors_plainly_when_cache_entirely_empty() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let requested = (Local::now().date_naive() - ChronoDuration::days(1)).to_string();

        let err = validate_date_arg(&cache, &requested).unwrap_err();
        assert_eq!(err.to_string(), "No cached wallpapers found for any date yet.");
    }

    #[test]
    fn validate_date_arg_accepts_cached_past_date() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let cached = Local::now().date_naive() - ChronoDuration::days(1);
        insert_dummy_candidate(&cache, &cached.to_string(), WallpaperSource::Bing);

        assert_eq!(validate_date_arg(&cache, &cached.to_string()).unwrap(), cached);
    }

    #[test]
    fn date_override_bypasses_today_and_day_offset() {
        let tmpdir = tempdir().unwrap();
        let mut settings = make_settings(tmpdir.path(), None, false);
        let source_settings = SourceSettings::from_config(None).unwrap();
        let override_date = NaiveDate::parse_from_str("2020-01-15", "%Y-%m-%d").unwrap();
        settings.date_override = Some(override_date);

        assert_eq!(
            date_label_for(None, &settings, &source_settings),
            "2020-01-15"
        );
    }

    #[test]
    fn settings_with_date_override_forces_offline_and_sets_date() {
        let tmpdir = tempdir().unwrap();
        let settings = make_settings(tmpdir.path(), None, false);
        assert!(!settings.offline);
        let date = Local::now().date_naive() - ChronoDuration::days(1);

        let dated = settings_with_date_override(&settings, date);
        assert!(dated.offline);
        assert_eq!(dated.date_override, Some(date));
    }

    #[test]
    fn run_menu_selection_browse_cache_with_empty_cache_returns_ok_without_prompting() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let settings = make_settings(tmpdir.path(), None, false);

        let result = run_menu_selection(
            ParentMenuChoice::BrowseCache,
            &build_client().unwrap(),
            &cache,
            &FavoritesManager::new(tmpdir.path().join("favorites")),
            &SourceRegistry::new(),
            &settings,
            &SourceSettings::from_config(None).unwrap(),
            None,
        );

        assert!(result.is_ok());
    }

    #[test]
    fn dispatch_choose_maybe_dated_without_date_arg_matches_plain_dispatch_choose() {
        let tmpdir = tempdir().unwrap();
        let cache = CacheManager::new(tmpdir.path());
        let favorites = FavoritesManager::new(tmpdir.path().join("favorites"));
        let mut settings = make_settings(tmpdir.path(), None, false);
        settings.offline = true;

        let via_helper = dispatch_choose_maybe_dated(
            &build_client().unwrap(),
            &cache,
            &favorites,
            &SourceRegistry::new(),
            &settings,
            &SourceSettings::from_config(None).unwrap(),
            None,
        );
        let via_dispatch = dispatch_choose(
            &build_client().unwrap(),
            &cache,
            &favorites,
            &SourceRegistry::new(),
            &settings,
            &SourceSettings::from_config(None).unwrap(),
        );

        assert_eq!(
            via_helper.is_err(),
            via_dispatch.is_err()
        );
    }
}
