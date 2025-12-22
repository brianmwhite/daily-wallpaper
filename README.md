# Daily Wallpaper for macOS (multi-monitor)

## About

- A Rust-based CLI that downloads Bing, Windows Spotlight, and NASA APOD wallpapers to `~/Pictures/daily-wallpapers/`, with support for applying them across all desktops or to a specific monitor on macOS.
- This project originated as a fork of
  [bing-wallpaper-daily-mac-multimonitor](https://github.com/lpikora/bing-wallpaper-daily-mac-multimonitor)
  by Lukas Pikora, but has since been fully rewritten in Rust and expanded to support multiple wallpaper sources.
- Windows Spotlight API details were informed by the
  [Spotlight-Downloader Public](https://github.com/ORelio/Spotlight-Downloader)
  project.
- Multi-monitor aware with per-monitor targeting or all desktops
- Sources: Bing Daily (default), Windows Spotlight (3 per day), NASA APOD (images only, optional HD, optional crop via config)
- Interactive chooser with Quick Look previews
- `launchd` integration for scheduled updates
- Configurable defaults via `~/.wallpaperconfig` (TOML)

## Requirements

- macOS
- Rust toolchain (`rustup` recommended)

## Installation

Install the CLI on your `PATH`:

```sh
cargo install --path .
daily-wallpaper
```

## Quick start

- Run once from the repo (no install):

  ```sh
  ./run.sh [options]
  # or
  cargo run -- [options]
  ```

- After installing, just call the binary:

  ```sh
  daily-wallpaper [options]
  ```

## Usage

- Download today’s Bing wallpaper to the default directory and apply to all desktops (default behavior):

  ```sh
  daily-wallpaper
  ```

- Apply to a specific monitor (e.g., second display):

  ```sh
  daily-wallpaper --monitor 2
  ```

- Choose interactively between Bing, Spotlight, and APOD for today (Quick Look previews):

  ```sh
  daily-wallpaper choose
  ```

Wallpapers and metadata are stored under `~/Pictures/daily-wallpapers/cache/<date>/` unless you override with `--picturedir`.

## Automatic updates (`launchd`)

Create a LaunchAgent that refreshes every 30 minutes (runs at load):

```sh
daily-wallpaper enable-auto-update [options]
```

Use `--auto-update-name <name>` to keep multiple schedules (one plist per name). Disable a job with:

```sh
daily-wallpaper disable-auto-update --auto-update-name <name>
```

Tip: install first (`cargo install --path .`) so `launchd` references a stable binary path.

## Display sync (`launchd`)

Create a LaunchAgent that listens for display/monitor changes and reapplies the last wallpaper:

```sh
daily-wallpaper enable-display-sync [options]
```

This uses CoreGraphics display-change notifications (event-driven, no polling).

Disable it with:

```sh
daily-wallpaper disable-display-sync --auto-update-name <name>
```

## Show wallpaper info

After a download, display the Bing headline and copyright for the saved wallpaper:

```sh
daily-wallpaper info
```

## Configuration (`~/.wallpaperconfig`)

```toml
# Default source: bing | spotlight | apod
default_source = "bing"

# Target monitor (0 = all)
monitor = 0

# Name for auto-update job and saved files
auto_update_name = "default"

# Prune cache after successful run (days)
prune_cache_days = 14

# Default download directory
picture_dir = "~/Pictures/daily-wallpapers"

# Favorites directory (defaults to <picture_dir>/favorites)
favorites_dir = "~/Pictures/daily-wallpapers/favorites"

# Verbosity: quiet | normal | verbose
verbosity = "normal"

# Cache-only mode; skip network and reuse existing downloads
offline = false

# Spotlight settings
spotlight_index = 1

# Wrap width for info output (used by `info` and the chooser)
info_wrap_width = 80

# Render info output without colors or emojis
info_plain_text = false

[bing]
country = "en-US"
resolutions = ["1920x1200", "1920x1080", "1366x768", "UHD"]

[apod]
api_key = "your-nasa-api-key"
crop = true
```

- CLI flags and environment variables override config values where applicable (e.g., `--monitor`, `--picturedir`, `NASA_API_KEY`).
- `apod.api_key` can also be provided as a top-level `apod_api_key = "..."` (backward compatibility).
- `resolutions` apply to Bing only; Spotlight ignores them; APOD always downloads full resolution.
- `verbosity` sets the default; `--quiet`/`--verbose` still take precedence.

## Adding a new wallpaper source (for contributors)

The pipeline is already generic: sources implement `Source`, return `WallpaperCandidate`s, and are registered in `SourceRegistry`. To add a new source with minimal `src/lib.rs` churn:

1) Create `src/sources/<name>.rs` implementing `Source` (follow `apod.rs`/`spotlight.rs`). Reuse helpers: `download_to_path`, `ensure_http_success`, `CacheManager::media_dir`, `SourceContext`.
2) Add a per-source settings struct + config struct in that module and wire it into `SourceSettings` in `src/sources/mod.rs` (so `SourceContext` carries your settings). Keep any validation inside the source module.
3) Register the source in `SourceRegistry::new()` in `src/sources/mod.rs`, and add a `WallpaperSource` + `SourceArg` mapping in `src/lib.rs` so it can be selected.
4) If the source needs config, extend `AppConfig` with an optional section (e.g., `[yoursource]`) and have your module’s `Settings::from_config` pull defaults (env/legacy keys as needed).
5) Add tests (e.g., with `httpmock`) so the new source doesn’t hit real networks.

All chooser/apply/cache logic is source-agnostic; new sources should not require further changes to `src/lib.rs` beyond registration and optional config wiring.

## CLI options

```
  enable-auto-update             Write and load a launchd plist for periodic updates.
  disable-auto-update            Unload and remove the launchd plist.
  enable-display-sync            Write and load a launchd plist that reapplies wallpaper on display changes.
  disable-display-sync           Unload and remove the display sync launchd plist.
  info                           Print the headline and copyright of the last download.
  choose                         Interactive picker (arrows/Enter) for Bing + Spotlight (3) + APOD; preview via Quick Look.
                                 Also includes a Favorites list for saved wallpapers.

  --prune-cache-days <n>         After a successful run, delete cached days older than <n> days.
  --offline                      Use cached wallpapers only; never download or hit the network.
  --auto-update-name <name>      Name for the auto-update job (default: default).
  -f --force                     Force download even if the file already exists.
  -s --ssl                       Communicate with bing.com over HTTPS (default; use --no-ssl to opt out).
  --no-ssl                       Communicate with bing.com over HTTP (not recommended).
  -q --quiet                     Suppress log messages.
  -n --filename <file name>      Custom filename for the downloaded picture.
  -p --picturedir <picture dir>  Download directory [default: ~/Pictures/daily-wallpapers/].
  -m --monitor <num>             Apply wallpaper only to a specific monitor (1,2,3...).
  --all-desktops-experimental    Write directly to desktoppicture.db for all desktops.
                                 Known issue: minimized apps are removed from Dock.
  --version                      Show version.
  -h --help                      Show help.
```

## Notes and tips

- Default resolutions are tried in order: `1920x1200`, `1920x1080`, `1024x768`, `1280x720`, `1366x768`, `UHD`.
- Use `--auto-update-name` to run multiple schedules (e.g., different monitors).
- The experimental `--all-desktops-experimental` flag writes to `~/Library/Application Support/Dock/desktoppicture.db`; if something breaks, delete that file and restart the Dock.
- For local development without installing, run `./run.sh ...` (calls `cargo run --`).
- APOD skips non-image media, defaults to the NASA DEMO_KEY (supply your own key or set `NASA_API_KEY` or `[apod].api_key` to avoid rate limits), and center-crops/resizes to your primary display’s aspect ratio by default (toggle with `[apod].crop` in config).
- `choose` downloads/caches today’s Bing, Spotlight, and APOD candidates (if available), lets you navigate with arrows, preview via Quick Look, refresh, or apply.
- Use `--prune-cache-days <n>` to delete cached days older than `<n>` after a successful run.
- Use `--offline` or `offline = true` in `~/.wallpaperconfig` to reuse cached wallpapers and avoid all network calls.
- Favorites: inside the `choose` flow, you can mark a candidate as a favorite (copies image + per-file metadata into `favorites_dir`) and browse/apply/remove favorites via the `Favorites` option. Favorites are offline-friendly and not pruned.

## Development

Run the test suite:

```sh
cargo test
```

## Future ideas / TODOs
