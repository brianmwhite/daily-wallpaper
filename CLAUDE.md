# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## About

Rust CLI (`daily-wallpaper`) that downloads daily wallpapers (Bing, Windows Spotlight, NASA APOD, MODIS) to `~/Pictures/daily-wallpapers/` and applies them across macOS desktops/monitors. Multi-monitor aware, has an interactive chooser with Quick Look previews, a favorites system, and `launchd` integration for scheduled auto-updates and display-change re-apply.

## Commands

```sh
cargo build                 # build
cargo run -- [options]      # run from source (or ./run.sh [options])
cargo test                  # run full test suite (all tests are inline #[cfg(test)] modules; tests/ dir is unused)
cargo test <test_name>      # run a single test by name substring, e.g. cargo test spotlight_downloads_and_reuses_cache
cargo install --path .      # install stable binary (needed before enable-auto-update/enable-display-sync so launchd has a stable path)
```

Version bump helper (feature-gated, not installed by default):

```sh
scripts/bump_version.sh [major|minor|patch]
# or: cargo run --features bump-version --bin bump_version -- [major|minor|patch]
```

There is no separate lint command configured beyond standard `cargo build`/`clippy` if available; there's no CI config in this repo to mirror.

## Architecture

Almost all logic lives in `src/lib.rs` (~3400 lines); `src/main.rs` just calls `daily_wallpaper::run_from_env()`. Read `src/lib.rs` top-to-bottom rather than expecting a clean module split — CLI parsing, config, cache, apply logic, launchd plist generation, and the interactive chooser are all here.

**Source plugin pattern** (`src/sources/mod.rs` + `src/sources/{bing,spotlight,apod,modis}.rs`): each wallpaper provider implements the `Source` trait (`id`, `label`, `fetch`, optional `pick_default`) and is registered in `SourceRegistry::new()`. `SourceContext` bundles the shared `Client`, `CacheManager`, `Settings`, target date, and per-source `SourceSettings` that gets passed into `fetch`. This pipeline is intentionally source-agnostic — adding a new source (see README's "Adding a new wallpaper source" section) means: implement `Source` in a new `src/sources/<name>.rs`, add its settings/config structs and wire them into `SourceSettings`/`AppConfig`, register it in `SourceRegistry::new()`, add a `WallpaperSource`/`SourceArg` mapping in `lib.rs`, and add `httpmock`-based tests — no other `lib.rs` changes should be needed.

**Cache layout** (`CacheManager`, `src/lib.rs`): under `<picture_dir>/cache/<date>/<source>/` — `index.json` (a `CacheIndex` of `WallpaperCandidate`s for that day/source), per-source skip markers (`SourceSkip`, when a source was deliberately skipped) and in-progress markers (`InProgressFetch`, for interrupted fetches), plus a top-level `last_applied.json` (`LastApplied`) tracking what's currently on the desktop, including whether the user (vs. auto-update) applied it and when.

**Auto-update skip logic** (`should_skip_auto_update`, called from `run_auto_update_body`, `src/lib.rs`): auto-update must not clobber a wallpaper the user deliberately applied today. It checks `LastApplied.applied_by_user` + today's date first; for Bing specifically it also cross-checks the cached candidate's metadata XML date (via `sources::bing::metadata_date_label`) since Bing's "today" can lag the local date. This logic was added/fixed recently (see recent commits) — be careful not to regress it when touching auto-update or Bing metadata handling.

**Config** (`AppConfig`, loaded via `load_config()` from `~/.wallpaperconfig` TOML): merges with CLI flags, where CLI flags win. Per-source settings (`[bing]`, `[apod]`, `[spotlight]`) are optional TOML sections deserialized into each source module's own `*Config`/`*Settings` types, converted via `Settings::from_config`/`*Settings::from_config`. `disabled_sources` in config is unioned with `--disable-source`.

**Command dispatch**: `Cli`/`CommandArg` (clap, derive API) define the subcommands (`choose`, `info`, `reapply`, `enable-auto-update`, `disable-auto-update`, `enable-display-sync`, `disable-display-sync`, `display-sync`) plus a hidden `auto-update-run` variant (`#[value(hide = true)]`, parseable but excluded from `--help`) and global flags (`--monitor`, `--picturedir`, `--offline`, `--disable-source`, etc.). `run_with_raw_args` is the actual entry point (`run_from_env` just forwards `std::env::args()`), which makes it possible to unit-test CLI behavior by calling it directly with custom arg vectors.

Bare invocation (no subcommand) branches on `io::stdin().is_terminal() && io::stdout().is_terminal()`: at an interactive terminal it shows an `inquire::Select` menu of the three everyday commands (Choose/Info/Reapply, via shared `dispatch_choose`/`dispatch_info`/`dispatch_reapply` functions also used by the explicit subcommands — see `run_menu_selection`); an explicit Esc cancel exits quietly, any other menu failure falls through to the non-interactive path. Non-interactive bare invocation (scripts, cron, `launchd`) runs `run_auto_update_body` (the fetch/apply body, also reachable directly via the hidden `auto-update-run` subcommand) after a self-heal check (`self_heal_auto_update_plist`) — see launchd integration below.

**launchd integration**: `create_launchd_plist`/`create_display_sync_plist` write plists (named via `--auto-update-name`, allowing multiple concurrent schedules) that re-invoke the built binary — auto-update polls periodically; display-sync instead registers a `CGDisplayRegisterReconfigurationCallback` (macOS CoreGraphics FFI, gated by `#[cfg(target_os = "macos")]`) to reapply the last wallpaper on display/monitor changes rather than polling. `create_launchd_plist` always writes the hidden `auto-update-run` subcommand explicitly into `ProgramArguments` (via `auto_update_program_arguments`), so new/re-enabled schedules never depend on bare-invocation TTY detection. Pre-existing plists from older versions (bare invocation, no subcommand token) self-heal automatically: the non-interactive bare-invocation path checks `settings.plist_filename()` via `plist_has_auto_update_run_token`, and if it's missing the token, calls `create_launchd_plist` to rewrite + reload it before continuing that run's fetch/apply — convergence happens on each schedule's next tick after a binary upgrade, no separate migration step needed.

**Favorites** (`src/favorites.rs`, `FavoritesManager`/`FavoriteEntry`): copies a candidate's image + per-file JSON metadata into `favorites_dir`, independent of the dated cache (not pruned by `--prune-cache-days`), with duplicate prevention.

## Testing notes

- Network-dependent sources are tested with `httpmock` (see `apod_downloads_and_uses_cache`, `spotlight_downloads_and_reuses_cache` in `src/lib.rs`, and tests in `src/sources/apod.rs`/`bing.rs`/`modis.rs`) — new sources should follow this pattern rather than hitting real APIs.
- `tempfile` is used throughout for isolated `picture_dir`/`favorites_dir` per test.
- When changing auto-update skip behavior, the relevant existing tests are `auto_update_does_not_skip_bing_when_metadata_not_today`, `auto_update_skips_bing_when_user_applied_today`, `auto_update_skips_bing_when_metadata_is_today`, `auto_update_skips_non_bing_when_applied_today` in `src/lib.rs`.
