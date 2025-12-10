# Python CLI to download and set the Bing Daily Wallpaper on macOS

This project now ships as a Python CLI (managed with [uv](https://docs.astral.sh/uv/)). It downloads the Bing Daily Wallpaper to `~/Pictures/bing-wallpapers/` and sets it across all desktops or a specific monitor on macOS.

## Requirements

- macOS
- Python 3.9+ and `uv` (`curl -Ls https://astral.sh/uv/install.sh | sh`)

## Quick start

- Run once without installing:

  ```sh
  uvx bing-wallpaper-daily-mac-multimonitor
  ```

- Install as a reusable tool (puts the CLI on your PATH):

  ```sh
  uv tool install bing-wallpaper-daily-mac-multimonitor
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
- The legacy `bing-wallpaper.sh` remains in the repo for reference, but the Python CLI is the supported entrypoint going forward.
- The previous npm distribution is no longer required; install/run via `uv` instead.
- For local development without installing, run `./run.sh ...` (uses `uv run --project <repo>` when available, falls back to `PYTHONPATH=src python -m bing_wallpaper.cli`).

## Development and tests

- Run the test suite with uv (uses an isolated env):

  ```sh
  UV_CACHE_DIR=/tmp/uv-cache uv run python -m unittest tests/test_cli.py
  ```

- Or run with system Python from the repo root:

  ```sh
  PYTHONPATH=src python -m unittest tests/test_cli.py
  ```
