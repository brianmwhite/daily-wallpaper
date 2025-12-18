# Wallpaper Sources + Interactive Selection Plan

## Goals

- Add additional wallpaper sources while retaining existing default behavior (Bing daily wallpaper when run with no extra args).
- Provide an interactive “choose” flow where the user can:
  - See a list of available wallpapers (5 total by default: Bing, 3× Windows Spotlight, NASA APOD).
  - View short descriptions/attribution for each item.
  - Preview an image via macOS Quick Look (`qlmanage -p <file>`).
  - Select one wallpaper to apply.
- Provide non-interactive options for “just set something quickly” (suitable for `launchd` auto-update).
- Cache downloads and metadata once per day; subsequent runs the same day use cached files unless forced.
- Keep the implementation robust and maintainable (clear module boundaries, structured metadata, predictable caching).

Non-goals (initially):
- A full “gallery browser” UI, search, or paging beyond the 5-item default set.
- Supporting non-image APOD media types (video) beyond graceful handling (skip or fallback).

## Current State (Baseline)

- `src/lib.rs` implements: Bing metadata fetch, image download, wallpaper apply (AppleScript / optional Dock DB update), `launchd` plist enable/disable, and `info` output.
- Images and `info.xml` are saved under `~/Pictures/bing-wallpapers/` (or `--picturedir`).
- The tool already has `--force` and robust atomic writes.

## Proposed UX / CLI

### Keep current behavior

- Running `bing-wallpaper-daily-mac-multimonitor` without extra args continues to fetch Bing and set it (using cache if same day).
- Decision: go with **Option A** (`choose` subcommand). Until the interactive list is built (Phase 4), it will behave like the default download/apply flow.

### New: interactive selection

Add one of the following (choose one approach during Phase 1):

**Option A (preferred): new explicit subcommand**
- `bing-wallpaper-daily-mac-multimonitor choose [options]`

**Option B: flag-based on existing default command**
- `bing-wallpaper-daily-mac-multimonitor --choose [options]`

In both cases, the interactive UI:
- Ensures candidates are downloaded (or loaded from cache).
- Prints a numbered list of 5 candidates with:
  - Source label (Bing / Spotlight / APOD)
  - Title/headline
  - Attribution/copyright
  - Optional 1-line description snippet
- Lets the user:
  - Type a number to select+apply
  - Type `p <n>` to Quick Look preview candidate `n`
  - Type `r` to refresh (ignore cache for this run)
  - Type `q` to quit without changing wallpaper

Quick Look detail:
- Use `qlmanage -p <file>` to open the preview.
- Implementation should `spawn()` and `wait()` (blocking) so the user closes Quick Look and returns to the prompt, or optionally `spawn()` without waiting if we want non-blocking preview (decide in Phase 3).

### New: non-interactive source selection

Add flags that allow picking without a prompt:

- `--source <bing|spotlight|apod>` (default `bing`)
- For Spotlight, add one of:
  - `--spotlight-index <1..=3>` (default `1`)
  - or `--spotlight-count <n>` paired with `--spotlight-pick <index|random>`
- For APOD:
  - `--apod-hd` (prefer `hdurl` when present)
  - `--nasa-api-key <key>` or `NASA_API_KEY` env var (default to `DEMO_KEY` with rate-limit caveat)

Caching behavior:
- Default: use today’s cache if present.
- `--force`: re-download even if cached for today.

### `info` behavior

Extend `info` so it can display metadata for:
- The last applied wallpaper (regardless of source), or
- A specific cached candidate (optional enhancement).

Suggestion:
- Write a small “last applied” marker file under the picture dir (e.g. `last_applied.json`) containing the chosen candidate ID and file path, so `info` stays source-agnostic.

## Data Model / Architecture

### Core types

Introduce a small internal model that all sources map onto:

- `WallpaperCandidate`
  - `id`: stable ID for caching (source + date + unique identifier)
  - `source`: enum (`Bing`, `Spotlight`, `Apod`)
  - `date`: local date used for caching (today, or `--day` for Bing/APOD)
  - `title`: display title/headline
  - `description`: optional long or short description
  - `attribution`: copyright/author
  - `info_url`: optional link (Bing detail URL, Spotlight “learn more”, APOD page)
  - `image_url`: remote URL to download
  - `local_path`: resolved cache path for the downloaded file
  - `mime`: optional (if known)

### Source abstraction

Create a `sources` module with a simple trait:

- `trait WallpaperSource { fn fetch_candidates(&self, ctx: &FetchContext) -> Result<Vec<WallpaperCandidate>>; }`

Where `FetchContext` includes:
- `client`: reqwest blocking client
- `date`: target date (today or day-offset date)
- `picture_dir`: base cache dir
- source-specific options (country for Bing, bcnt for Spotlight, API key for APOD)

### Caching

Store per-day cache under a predictable directory:

- `picturedir/cache/<YYYY-MM-DD>/`
  - `bing/…`
  - `spotlight/…`
  - `apod/…`
  - `index.json` (all candidates metadata for that day)

Download strategy:
- When running, load `index.json` for today if present and not `--force`.
- If missing/stale, fetch from the network, produce candidates, download images, then write `index.json` atomically.
- Keep downloads atomic (temp file + rename) as the current code already does.

This “index.json” makes it easy to show the list instantly on subsequent runs without re-parsing remote responses.

Implementation notes:
- Add `serde` + `serde_json` for `index.json` and `last_applied.json`.
- Keep file names deterministic:
  - `bing_<date>_<resolution>.jpg` (or use the Bing `urlBase` suffix)
  - `spotlight_<date>_<slot>.jpg`
  - `apod_<date>.jpg`

## Source Details

### Bing (existing)

- Continue using current metadata fetch (`HPImageArchive.aspx`) and resolution fallback logic.
- Map existing fields into `WallpaperCandidate`:
  - headline/title, copyright, info URL (if present), and chosen image URL.
- Cache the “best available” file that was successfully downloaded for the day.

### Windows Spotlight (new)

- Use the endpoint documented in the repo’s “future ideas” list (and validate with the existing `spotlight-sample.json`).
- Support `bcnt=3` to retrieve three items.
- Extract fields for display:
  - `title`, `description`, `copyright`
  - image URL (prefer `landscapeImage.asset` on macOS)
- Download all three images once per day into cache.

Edge cases:
- Some entries may have missing fields; ensure display falls back gracefully (e.g. “(no title)”).
- Avoid duplicate images if the API returns repeats (dedupe by image URL).

### NASA APOD (new)

- Use NASA APOD API endpoint from README future ideas.
- Use `nasa-sample.json` as example of response from the api
- API key:
  - Default to `DEMO_KEY` unless `NASA_API_KEY` env var or CLI arg provided.
  - Document rate limits and recommend user key for frequent auto-updates.
- Handle `media_type`:
  - If `image`: use `hdurl` when `--apod-hd` is set (fallback to `url`).
  - If `video`: initial behavior: skip APOD candidate with a clear message in interactive UI; non-interactive mode falls back to Bing (or errors if user explicitly requested APOD).

## Phase Plan

### Phase 1 — CLI + model scaffolding (backwards compatible)

- Decide whether to add `choose` as a subcommand or a flag; keep default behavior unchanged.
- Introduce `WallpaperCandidate`, `WallpaperSource`, `FetchContext`, and a new cache layout under `picturedir/cache/<date>/…`.
- Add `serde` + `serde_json` dependencies.
- Add `last_applied.json` marker written after successfully setting wallpaper.
- Add a small internal “cache manager” module responsible for:
  - computing cache paths
  - reading/writing `index.json` atomically
  - loading cached candidates (today only)

Deliverable:
- Tool still sets Bing by default, but now writes/reads today’s cache index.

### Phase 2 — Windows Spotlight source

- Implement `sources::spotlight`:
  - Fetch JSON (bcnt=3)
  - Parse into 3 candidates
  - Download all images (atomic), write into cache
- Add unit tests with `httpmock` using a minimized version of `sample.json`.

Deliverable:
- Non-interactive `--source spotlight --spotlight-index N` works and uses cache.

### Phase 3 — NASA APOD source

- Implement `sources::apod`:
  - Fetch JSON
  - Parse image URL + metadata
  - Download image (atomic)
  - Handle video case gracefully
- Add unit tests with mocked APOD responses (image + video).

Deliverable:
- Non-interactive `--source apod` works and uses cache.

### Phase 4 — Interactive chooser + Quick Look preview

- Add interactive prompt flow using either:
  - `dialoguer` (classic, lightweight), or
  - `inquire` (nice UX), or
  - a minimal custom stdin loop (no new dependency).
- Provide commands:
  - select by number to apply
  - preview with `qlmanage -p`
  - refresh (ignore cache)
  - quit
- Ensure the chooser works cleanly with `--quiet` (likely disable chooser if quiet; or ignore quiet and still prompt—decide).

Deliverable:
- `choose` lists 5 candidates and can preview/apply.

### Phase 5 — Integrations, polish, docs, and safety

- Update `README.md` with:
  - new sources and flags
  - caching behavior
  - APOD API key guidance
  - interactive chooser instructions and Quick Look key commands
- Extend `info`:
  - show metadata for `last_applied.json`
  - optionally allow `info --today` to show today’s cached list
- Validate that `enable-auto-update` continues to behave sensibly:
  - auto-update should default to non-interactive mode
  - interactive chooser should not be used in `launchd` contexts
- Confirm delete behavior: any removal of cached files should use the `trash` command rather than `rm` (if we add cache pruning).

Deliverable:
- End-to-end feature complete with tests and clear docs.

## Open Questions / Decisions to Make Early

- Should Spotlight and APOD respect `--day`? (Recommended: Bing/APOD yes; Spotlight no, treat as “today feed”.)
- Should “5 options” be fixed or configurable? (Recommended: fixed initial set; configurable later.)
- How should “non-interactive APOD video” behave?
  - fallback to Bing automatically vs error out when `--source apod` is explicit
- Do we want automatic cache pruning (e.g. keep last N days)? If yes, implement with `trash` and make it opt-in.

Decisions made after follow-up:
- Use **Option A** (`choose` subcommand).
- No video support (APOD video days are skipped/errored).
- Keep 5 options fixed initially.
- Bing and APOD respect `--day`; Spotlight ignores `--day` (treats as “today feed”).
- Cache pruning should be opt-in and use the `trash` command.

## Suggested Acceptance Criteria

- Running with no args behaves exactly like before (Bing daily wallpaper, cached per day).
- `choose` shows 5 candidates and allows preview via Quick Look and selection to apply.
- `--source spotlight --spotlight-index 2` applies Spotlight #2 without prompting.
- `--source apod` applies APOD image for today (or errors clearly for non-image).
- Same-day runs do not hit the network unless `--force` is provided.
- Tests cover at least:
  - cache index read/write
  - spotlight JSON parsing for 3 candidates
  - apod parsing for image/video
  - “uses cache when present” behavior
