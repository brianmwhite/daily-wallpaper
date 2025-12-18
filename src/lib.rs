use chrono::{Duration as ChronoDuration, Local, NaiveDate};
use clap::{ArgAction, Parser, ValueEnum};
use inquire::{InquireError, Select};
use plist::{Dictionary, Value};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json;
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;

mod sources;
use sources::apod::APOD_DEFAULT_KEY;
use sources::spotlight::SPOTLIGHT_COUNT;
use sources::{Source, SourceContext, SourceRegistry};

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
const CACHE_DIR_NAME: &str = "cache";
const CACHE_INDEX_FILE: &str = "index.json";
const LAST_APPLIED_FILE: &str = "last_applied.json";

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
    verbose: bool,
    quiet: bool,
    experimental: bool,
    filename: Option<String>,
    bing_host: String,
    source: WallpaperSource,
    spotlight_index: usize,
    spotlight_url_override: Option<String>,
    apod_api_key: String,
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

#[derive(Debug, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum ConfigSource {
    Bing,
    Spotlight,
    Apod,
}

impl From<ConfigSource> for SourceArg {
    fn from(value: ConfigSource) -> Self {
        match value {
            ConfigSource::Bing => SourceArg::Bing,
            ConfigSource::Spotlight => SourceArg::Spotlight,
            ConfigSource::Apod => SourceArg::Apod,
        }
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
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CacheIndex {
    date: String,
    candidates: Vec<WallpaperCandidate>,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct ApodConfig {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    crop: Option<bool>,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct BingConfig {
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    resolutions: Option<Vec<String>>,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct AppConfig {
    #[serde(default)]
    default_source: Option<ConfigSource>,
    #[serde(default)]
    spotlight_index: Option<usize>,
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
    verbosity: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    resolutions: Option<Vec<String>>,
    #[serde(default)]
    apod: Option<ApodConfig>,
    #[serde(default)]
    bing: Option<BingConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LastApplied {
    candidate_id: String,
    source: WallpaperSource,
    applied_path: PathBuf,
    applied_at: u64,
    #[serde(default)]
    date: Option<String>,
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
    Info,
    Choose,
    Reapply,
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

    #[arg(short = 'v', long = "verbose", action = ArgAction::SetTrue, conflicts_with = "quiet")]
    verbose: bool,

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
    let mut clap_args = vec![OsString::from("daily-wallpaper")];
    clap_args.extend(raw_args.iter().map(OsString::from));
    let args = Cli::parse_from(clap_args);
    let config = load_config();

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
    let mut resolutions: Vec<String> = if let Some(res) = single_resolution {
        vec![res]
    } else if !args.resolutions.is_empty() {
        args.resolutions.clone()
    } else if let Some(cfg) = &config {
        cfg.bing
            .as_ref()
            .and_then(|b| b.resolutions.clone())
            .or_else(|| cfg.resolutions.clone())
            .unwrap_or_else(|| DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect())
    } else {
        DEFAULT_RESOLUTIONS.iter().map(|s| s.to_string()).collect()
    };

    let ssl = !args.no_ssl && args.ssl;
    let mut source_arg = args.source;
    if let Some(cfg) = &config {
        if let Some(def) = cfg.default_source {
            source_arg = def.into();
        }
    }
    let source = map_source(source_arg);
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

    let monitor_override = arg_present(&raw_args, &["-m", "--monitor"]);
    let spotlight_override = arg_present(&raw_args, &["--spotlight-index"]);
    let auto_update_override =
        arg_present(&raw_args, &["--auto-update-name", "--auto_update_name"]);
    let prune_override = arg_present(&raw_args, &["--prune-cache-days"]);
    let picture_override = arg_present(&raw_args, &["-p", "--picturedir"]);
    let verbosity_override = args.quiet || args.verbose;
    let country_override = arg_present(&raw_args, &["-c", "--country"]);
    let resolution_override = !args.resolutions.is_empty() || args.resolution.is_some();
    let apod_crop_override =
        arg_present(&raw_args, &["--no-apod-crop"]) || arg_present(&raw_args, &["--apod-crop"]);

    if let Some(cfg) = &config {
        if !resolution_override {
            resolutions = cfg
                .bing
                .as_ref()
                .and_then(|b| b.resolutions.clone())
                .or_else(|| cfg.resolutions.clone())
                .unwrap_or(resolutions);
        }
    }

    let (monitor, spotlight_index) = {
        let mut monitor_val = args.monitor;
        let mut spotlight_val = args.spotlight_index;
        if let Some(cfg) = &config {
            if !monitor_override {
                if let Some(m) = cfg.monitor {
                    monitor_val = m;
                }
            }
            if !spotlight_override {
                if let Some(idx) = cfg.spotlight_index {
                    spotlight_val = idx;
                }
            }
        }
        (monitor_val, spotlight_val)
    };

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

    let country = if country_override {
        args.country.clone()
    } else if let Some(cfg) = &config {
        cfg.country
            .clone()
            .or_else(|| cfg.bing.as_ref().and_then(|b| b.country.clone()))
    } else {
        args.country.clone()
    };

    let apod_crop = if apod_crop_override {
        args.apod_crop
    } else if let Some(cfg) = &config {
        cfg.apod
            .as_ref()
            .and_then(|a| a.crop)
            .unwrap_or(args.apod_crop)
    } else {
        args.apod_crop
    };

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

    let settings = Settings {
        proto: if ssl { "https".into() } else { "http".into() },
        country,
        day: args.day,
        picture_dir: picture_dir
            .unwrap_or_else(default_picture_dir)
            .expand_tilde(),
        auto_update_name: normalize_auto_update_name(&auto_update_name),
        monitor,
        force: args.force,
        verbose,
        quiet,
        experimental,
        filename: args.filename.clone(),
        bing_host: "www.bing.com".to_string(),
        source,
        spotlight_index,
        spotlight_url_override: None,
        apod_api_key: args
            .apod_api_key
            .clone()
            .or_else(|| {
                config
                    .as_ref()
                    .and_then(|c| c.apod.as_ref().and_then(|a| a.api_key.clone()))
            })
            .or_else(|| load_apod_api_key_from_config())
            .or_else(|| env::var("NASA_API_KEY").ok())
            .unwrap_or_else(|| APOD_DEFAULT_KEY.to_string()),
        apod_url_override: None,
        apod_crop,
        prune_cache_days,
    };

    let cache = CacheManager::new(&settings.picture_dir);
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
        Some(CommandArg::Info) => {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            show_info(&cache, &mut handle)?;
            return Ok(());
        }
        Some(CommandArg::Choose) => {
            ensure_picture_dir(&settings.picture_dir)?;
            return run_choose(&client, &cache, &registry, &settings, resolutions);
        }
        Some(CommandArg::Reapply) => {
            ensure_picture_dir(&settings.picture_dir)?;
            return reapply_last_wallpaper(&cache, &settings);
        }
        None => {}
    }

    ensure_picture_dir(&settings.picture_dir)?;

    let source = require_source(&registry, settings.source)?;
    let date_label = date_label_for(Some(source), &settings);
    let ctx = SourceContext {
        client: &client,
        cache: &cache,
        settings: &settings,
        date_label: &date_label,
        resolutions: &resolutions,
    };
    let result = run_source(source, &ctx);

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
    home_dir().join("Pictures").join("daily-wallpapers")
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

fn log_verbose(message: &str, settings: &Settings) {
    if settings.quiet || !settings.verbose {
        return;
    }
    log(message, false);
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

fn date_label_for(source: Option<&dyn Source>, settings: &Settings) -> String {
    let use_day = source.map(|s| s.supports_day()).unwrap_or(true);
    let target_date = if use_day {
        target_date_for_day(settings.day)
    } else {
        Local::now().date_naive()
    };
    target_date.to_string()
}

fn run_source(source: &dyn Source, ctx: &SourceContext<'_>) -> Result<()> {
    let fetch_result = source.fetch(ctx)?;
    let candidate = source
        .pick_default(&fetch_result.candidates, ctx.settings)?
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
    )
}

fn require_source<'a>(registry: &'a SourceRegistry, id: WallpaperSource) -> Result<&'a dyn Source> {
    registry
        .get(id)
        .ok_or_else(|| WallpaperError::Message(format!("Source {:?} is not registered.", id)))
}

fn run_choose(
    client: &Client,
    cache: &CacheManager,
    registry: &SourceRegistry,
    settings: &Settings,
    resolutions: Vec<String>,
) -> Result<()> {
    let mut current_settings = settings.clone();
    let current_res = resolutions;
    let mut selected_idx: Option<usize> = None;

    loop {
        let candidates =
            gather_candidates(client, cache, registry, &current_settings, &current_res)?;
        // Force should be one-shot in the chooser to avoid repeated re-downloads.
        current_settings.force = false;
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
        .with_starting_cursor(selected_idx.unwrap_or(0))
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
        selected_idx = Some(idx);

        if let Some(cand) = candidates.get(idx) {
            let mut action_cursor = 0_usize;
            loop {
                let action = Select::new(
                    "Action",
                    vec![
                        "Preview (Quick Look)",
                        "Apply",
                        "Refresh list (force re-download)",
                        "Quit chooser",
                    ],
                )
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
                        ) {
                            Ok(()) => return Ok(()),
                            Err(err) => println!("Failed to apply wallpaper: {err}"),
                        }
                    }
                    Ok(choice) if choice.starts_with("Refresh") => {
                        current_settings.force = true;
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

fn gather_candidates(
    client: &Client,
    cache: &CacheManager,
    registry: &SourceRegistry,
    settings: &Settings,
    resolutions: &[String],
) -> Result<Vec<WallpaperCandidate>> {
    let mut result = Vec::new();

    for source in registry.all() {
        let date_label = date_label_for(Some(source), settings);
        let ctx = SourceContext {
            client,
            cache,
            settings,
            date_label: &date_label,
            resolutions,
        };
        match source.fetch(&ctx) {
            Ok(fetched) => result.extend(fetched.candidates),
            Err(err) => log(
                &format!("{} unavailable: {err}", source.label()),
                settings.quiet,
            ),
        }
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

#[derive(Debug)]
struct DownloadedFile {
    path: PathBuf,
}
fn download_to_path(
    client: &Client,
    url: &str,
    target_path: &Path,
    settings: &Settings,
) -> Result<DownloadedFile> {
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

    log_verbose(&format!("Downloading {}", url), settings);
    let spinner = start_spinner(settings, format!("Downloading {}", url));
    let temp_path = unique_temp_path(target_path);
    let download_result = (|| -> Result<()> {
        let _ = fs::remove_file(&temp_path);
        let mut response = client.get(url).timeout(IMAGE_TIMEOUT).send()?;
        ensure_http_success(response.status(), url)?;
        let mut file = File::create(&temp_path)?;
        response.copy_to(&mut file)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temp_path, target_path)?;
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

fn apply_wallpaper(
    file_path: &Path,
    settings: &Settings,
    cache: &CacheManager,
    candidate_id: &str,
    source: WallpaperSource,
    candidate_date: Option<&str>,
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
    )
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

fn format_candidate_info(candidate: &WallpaperCandidate) -> String {
    let mut info = String::new();
    if let Some(title) = candidate.title.as_deref().filter(|t| !t.is_empty()) {
        info.push_str(title);
        info.push('\n');
    }
    if let Some(desc) = candidate
        .description
        .as_deref()
        .filter(|d| !d.is_empty())
    {
        info.push_str(desc);
        info.push('\n');
    }
    let attribution = candidate
        .attribution
        .as_deref()
        .filter(|a| !a.is_empty())
        .unwrap_or("Unknown copyright");
    info.push_str(attribution);
    if let Some(link) = candidate.info_url.as_deref().filter(|l| !l.is_empty()) {
        info.push('\n');
        info.push_str(link);
    }
    info
}

fn show_info<W: Write>(
    cache: &CacheManager,
    mut writer: W,
) -> Result<()> {
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
        let info = format_candidate_info(&candidate);
        writeln!(writer, "{info}")?;
        return Ok(());
    }

    Err(WallpaperError::Message(
        "No metadata found for last applied wallpaper. Run the download first.".to_string(),
    ))
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

fn load_apod_api_key_from_config() -> Option<String> {
    load_config().and_then(|cfg| {
        cfg.apod
            .as_ref()
            .and_then(|a| a.api_key.clone())
            .or_else(|| {
                // Backward compatibility for top-level key.
                let path = home_dir().join(".wallpaperconfig");
                let contents = fs::read_to_string(path).ok()?;
                let parsed: toml::Value = toml::from_str(&contents).ok()?;
                parsed
                    .get("apod_api_key")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
    })
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
            verbose: false,
            quiet: true,
            experimental: false,
            filename: filename.map(ToString::to_string),
            bing_host: "www.bing.com".into(),
            source: WallpaperSource::Bing,
            spotlight_index: 1,
            spotlight_url_override: None,
            apod_api_key: "TEST".into(),
            apod_url_override: None,
            apod_crop: true,
            prune_cache_days: None,
        }
    }

    fn run_source_for_test(
        source_id: WallpaperSource,
        client: &Client,
        cache: &CacheManager,
        settings: &Settings,
        date_label: &str,
        resolutions: &[String],
    ) -> Result<()> {
        let registry = SourceRegistry::new();
        let source = require_source(&registry, source_id)?;
        let ctx = SourceContext {
            client,
            cache,
            settings,
            date_label,
            resolutions,
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

        let downloaded = bing::download_image(&client, url_base, res, &settings, metadata).unwrap();
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

        let downloaded = bing::download_image(&client, url_base, res, &settings, metadata).unwrap();
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

        let err = bing::download_image(&client, url_base, res, &settings, metadata).unwrap_err();
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

        run_source_for_test(
            WallpaperSource::Spotlight,
            &client,
            &cache,
            &settings,
            &date_label,
            &[],
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
            &[],
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

        run_source_for_test(
            WallpaperSource::Apod,
            &client,
            &cache,
            &settings,
            date_label,
            &[],
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
            &[],
        )
        .unwrap();
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

        let err = run_source_for_test(
            WallpaperSource::Apod,
            &client,
            &cache,
            &settings,
            date_label,
            &[],
        )
        .unwrap_err();
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
                date: Some(candidate.date.clone()),
            })
            .unwrap();

        let mut buffer = Vec::new();
        show_info(&cache, &mut buffer).unwrap();
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
        let err = show_info(&cache, &mut buffer).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("previously applied wallpaper"));
    }
}
