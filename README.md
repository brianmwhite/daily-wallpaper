# Rust CLI to download and set the Bing Daily Wallpaper on macOS

This project now ships as a Rust CLI. It downloads wallpapers to `~/Pictures/bing-wallpapers/` and sets them across all desktops or a specific monitor on macOS. Sources supported today:
- Bing Daily (default)
- Windows Spotlight (3 images per day; choose with `--spotlight-index`)
- NASA APOD (images only; use `--apod-hd` to prefer the HD URL)

## Requirements

- macOS
- Rust toolchain (`rustup` recommended)

## Quick start

- Run once from the repo without installing:

  ```sh
  ./run.sh [options]
  # or
  cargo run -- [options]
  ```

- Install the CLI locally so it is on your `PATH`:

  ```sh
  cargo install --path .
  bing-wallpaper-daily-mac-multimonitor
  ```

## Automatic daily updates (launchd)

Create a LaunchAgent that refreshes the wallpaper every 30 minutes (run at load enabled):

```sh
bing-wallpaper-daily-mac-multimonitor enable-auto-update [options]
```

Use `--auto-update-name <name>` to keep multiple schedules (one plist per name). Disable a job with:

```sh
bing-wallpaper-daily-mac-multimonitor disable-auto-update --auto-update-name <name>
```

Tip: install the tool (`uv tool install ...`) before enabling auto updates so launchd has a stable binary to call.

## Show wallpaper info

After a download has run, display the Bing headline + copyright for the saved wallpaper:

```sh
bing-wallpaper-daily-mac-multimonitor info
```

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
  -p --picturedir <picture dir>  Download directory [default: ~/Pictures/bing-wallpapers/].
  -r --resolution <resolution>   Single resolution to try.
  --resolutions <resolutions>    List of resolutions to try (e.g. --resolutions 1920x1200 UHD).
  -m --monitor <num>             Apply wallpaper only to a specific monitor (1,2,3...).
  --all-desktops-experimental    Write directly to desktoppicture.db for all desktops.
                                 Known issue: minimized apps are removed from Dock.
  --version                      Show version.
  -h --help                      Show help.
```

### Notes and tips

- Default resolutions are tried in order: `1920x1200`, `1920x1080`, `1024x768`, `1280x720`, `1366x768`, `UHD`.
- Use `--auto-update-name` to run multiple schedules (different monitors, days, or countries).
- The experimental `--all-desktops-experimental` flag writes to `~/Library/Application Support/Dock/desktoppicture.db`. If something breaks, delete that file and restart the Dock.
- Wallpapers and `info.xml` are saved under `~/Pictures/bing-wallpapers/` unless overridden with `--picturedir`.
- For local development without installing, run `./run.sh ...` (calls `cargo run --`).
- Spotlight ignores `--day` and always fetches the current feed; Bing respects `--day`. Same-day reruns reuse cached files unless `--force` is given.
- APOD respects `--day`, skips non-image media, defaults to the NASA DEMO_KEY (supply your own key or set `NASA_API_KEY` to avoid rate limits), and center-crops/resizes to your primary display’s aspect ratio by default (disable with `--no-apod-crop`).
- `choose` downloads/caches today’s Bing, Spotlight, and APOD candidates (if available), shows a list you can navigate with arrows, lets you preview with Quick Look, refresh, or apply.
- Use `--prune-cache-days <n>` to delete cached days older than `<n>` after a successful run.

## Development and tests

- Run the test suite:

  ```sh
  cargo test
  ```
## Future Ideas
- Retrieve spotlight images using https://fd.api.iris.microsoft.com/v4/api/selection?&placement=88000820&bcnt=4&country=US&locale=en-US&fmt=json. Ref https://github.com/ORelio/Spotlight-Downloader/blob/master/SpotlightAPI.md
- Retrieve NASA 
https://apod.nasa.gov/apod/astropix.html
https://api.nasa.gov/planetary/apod?api_key=DEMO_KEY
