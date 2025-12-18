# Bing Wallpaper Daily for macOS (multi-monitor)

Rust CLI that downloads Bing/Spotlight/NASA APOD wallpapers to `~/Pictures/daily-wallpapers/` and applies them across all desktops or a specific monitor on macOS.

- Multi-monitor aware with per-monitor targeting or all desktops
- Sources: Bing Daily (default), Windows Spotlight (3 per day via `--spotlight-index`), NASA APOD (images only, optional HD, optional crop)
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
bing-wallpaper-daily-mac-multimonitor
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
  bing-wallpaper-daily-mac-multimonitor [options]
  ```

## Usage

- Download today’s Bing wallpaper to the default directory and apply to all desktops (default behavior):

  ```sh
  bing-wallpaper-daily-mac-multimonitor
  ```

- Apply to a specific monitor (e.g., second display):

  ```sh
  bing-wallpaper-daily-mac-multimonitor --monitor 2
  ```

- Choose interactively between Bing, Spotlight, and APOD for today (Quick Look previews):

  ```sh
  bing-wallpaper-daily-mac-multimonitor choose
  ```

Wallpapers and `info.xml` land in `~/Pictures/daily-wallpapers/` unless you override with `--picturedir`.

## Automatic updates (`launchd`)

Create a LaunchAgent that refreshes every 30 minutes (runs at load):

```sh
bing-wallpaper-daily-mac-multimonitor enable-auto-update [options]
```

Use `--auto-update-name <name>` to keep multiple schedules (one plist per name). Disable a job with:

```sh
bing-wallpaper-daily-mac-multimonitor disable-auto-update --auto-update-name <name>
```

Tip: install first (`cargo install --path .`) so `launchd` references a stable binary path.

## Show wallpaper info

After a download, display the Bing headline and copyright for the saved wallpaper:

```sh
bing-wallpaper-daily-mac-multimonitor info
```

## Configuration (`~/.wallpaperconfig`)

```toml
# Default source when not set on CLI: bing | spotlight | apod
default_source = "bing"

# Target monitor (0 = all)
monitor = 0

# Name for auto-update job and saved files
auto_update_name = "default"

# Prune cache after successful run (days)
prune_cache_days = 14

# Default download directory
picture_dir = "~/Pictures/daily-wallpapers"

# Verbosity: quiet | normal | verbose
verbosity = "normal"

# Spotlight settings
spotlight_index = 1

[bing]
country = "en-US"
resolutions = ["1920x1200", "1920x1080", "1366x768", "UHD"]

[apod]
api_key = "your-nasa-api-key"
crop = true
```

- CLI flags and environment variables override config values.
- `apod.api_key` can also be provided as a top-level `apod_api_key = "..."` (backward compatibility).
- `resolutions` apply to Bing only; Spotlight ignores them; APOD always downloads full resolution.
- `verbosity` sets the default; `--quiet`/`--verbose` still take precedence.

## Adding a new wallpaper source (for contributors)

The pipeline is already generic: sources implement `Source`, return `WallpaperCandidate`s, and are registered in `SourceRegistry`. To add a new source with minimal `src/lib.rs` churn:

1) Create `src/sources/<name>.rs` implementing `Source` (follow `apod.rs`/`spotlight.rs`). Reuse helpers: `download_to_path`, `ensure_http_success`, `CacheManager::media_dir`, `SourceContext`.
2) Register it in `SourceRegistry::new()` in `src/sources/mod.rs`.
3) Add a `WallpaperSource` variant and wire the CLI: update `source_dir_name`, `SourceArg` + `map_source` so the CLI can select it.
4) If it needs config, extend `AppConfig` in `src/lib.rs` and pull defaults the same way Bing/APOD/Spotlight do; keep overrides via CLI/env.
5) Add tests (e.g., with `httpmock`) so the new source doesn’t hit real networks.

All chooser/apply/cache logic is source-agnostic; new sources should not require further changes to `src/lib.rs` beyond registration and optional config wiring.

## CLI options

```
  enable-auto-update             Write and load a launchd plist for periodic updates.
  disable-auto-update            Unload and remove the launchd plist.
  info                           Print the headline and copyright of the last download.
  choose                         Interactive picker (arrows/Enter) for Bing + Spotlight (3) + APOD; preview via Quick Look.

  --source <bing|spotlight|apod> Wallpaper source (default: bing).
  --spotlight-index <1-3>        Which Spotlight image to apply (default: 1).
  --nasa-api-key <key>           NASA API key for APOD (default: DEMO_KEY or NASA_API_KEY env).
  --apod-hd                      Prefer the APOD HD image when available.
  --no-apod-crop                 Disable APOD center-crop/resize to monitor aspect ratio (default: enabled).
  --prune-cache-days <n>         After a successful run, delete cached days older than <n> days.
  --auto-update-name <name>      Name for the auto-update job (default: default).
  -f --force                     Force download even if the file already exists.
  -s --ssl                       Communicate with bing.com over HTTPS (default; use --no-ssl to opt out).
  --no-ssl                       Communicate with bing.com over HTTP (not recommended).
  -q --quiet                     Suppress log messages.
  -c --country <country-code>    Market/region code (en-US, cs-CZ, ...).
  -d --day <number>              Day offset (0=today, 1=yesterday...). Default: 0.
  -n --filename <file name>      Custom filename for the downloaded picture.
  -p --picturedir <picture dir>  Download directory [default: ~/Pictures/daily-wallpapers/].
  -r --resolution <resolution>   Single resolution to try.
  --resolutions <resolutions>    List of resolutions to try (e.g. --resolutions 1920x1200 UHD).
  -m --monitor <num>             Apply wallpaper only to a specific monitor (1,2,3...).
  --all-desktops-experimental    Write directly to desktoppicture.db for all desktops.
                                 Known issue: minimized apps are removed from Dock.
  --version                      Show version.
  -h --help                      Show help.
```

## Notes and tips

- Default resolutions are tried in order: `1920x1200`, `1920x1080`, `1024x768`, `1280x720`, `1366x768`, `UHD`.
- Use `--auto-update-name` to run multiple schedules (different monitors, days, or countries).
- The experimental `--all-desktops-experimental` flag writes to `~/Library/Application Support/Dock/desktoppicture.db`; if something breaks, delete that file and restart the Dock.
- For local development without installing, run `./run.sh ...` (calls `cargo run --`).
- Spotlight ignores `--day` and always fetches the current feed; Bing respects `--day`. Same-day reruns reuse cached files unless `--force` is given.
- APOD respects `--day`, skips non-image media, defaults to the NASA DEMO_KEY (supply your own key or set `NASA_API_KEY` to avoid rate limits), and center-crops/resizes to your primary display’s aspect ratio by default (disable with `--no-apod-crop`).
- `choose` downloads/caches today’s Bing, Spotlight, and APOD candidates (if available), lets you navigate with arrows, preview via Quick Look, refresh, or apply.
- Use `--prune-cache-days <n>` to delete cached days older than `<n>` after a successful run.

## Development

Run the test suite:

```sh
cargo test
```

## Future ideas / TODOs
- Test auto-apply flows.

Project originally forked from https://github.com/lpikora/bing-wallpaper-daily-mac-multimonitor
