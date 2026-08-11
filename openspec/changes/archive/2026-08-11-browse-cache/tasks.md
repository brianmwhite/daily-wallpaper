## 1. Cached-date listing helper

- [x] 1.1 Add `list_cached_dates(cache: &CacheManager) -> Vec<NaiveDate>` in `src/lib.rs`, sibling to `prune_cache`: scan `cache/<date>/` folder names, parse with `NaiveDate::parse_from_str(name, "%Y-%m-%d")`, discard unparseable entries, sort descending.
- [x] 1.2 Add a variant/param to exclude today's date (used by the picker) vs. include it (used by the "not cached" error's suggestion list, per design.md's Open Question — default to including today in the error suggestion list).
- [x] 1.3 Unit tests: empty cache dir, stray non-date folders ignored, descending sort order, today-exclusion behavior for both call modes.

## 2. Date override plumbing

- [x] 2.1 Add `--date <VALUE>` to `Cli` global args in `src/lib.rs` (`Option<String>`, same pattern as existing global flags).
- [x] 2.2 Add a date-override field threaded alongside `Settings` into `dispatch_choose`/`run_choose`/`gather_candidates` (not stored in `Settings` itself if that risks it leaking into anything persisted — confirm no serialization path touches it).
- [x] 2.3 Update `date_label_for` to accept the override and, when present, return it directly for every source, bypassing `supports_day`/`target_date_for_day`.
- [x] 2.4 When an override is present, force `settings.offline = true` for that run and log a line explaining offline was forced because of `--date`.
- [x] 2.5 Confirm (via existing/new test) that omitting `--date` produces byte-for-byte identical behavior to before this change.

## 3. `--date` validation and error messages

- [x] 3.1 Implement the three-way validation (malformed / future / not-cached) that runs before any fetch or chooser UI, using `list_cached_dates` for the not-cached message's suggestion list.
- [x] 3.2 Implement exact message wording from specs/browse-cache/spec.md for all three failure modes, including the "cache entirely empty" variant of the not-cached message.
- [x] 3.3 Wire `--date pick` to skip direct-value validation and instead invoke the picker (task 4).
- [x] 3.4 Tests: malformed value, future date, well-formed date with nothing cached (non-empty suggestion list), well-formed date with an entirely empty cache (empty-state message).

## 4. Cached-date picker (shared)

- [x] 4.1 Implement a shared function (e.g. `pick_cached_date`) that shows an `inquire::Select` of `list_cached_dates` (today excluded), plain ISO strings, newest first.
- [x] 4.2 Handle empty list: print a plain "no cached dates yet" message and return without showing a selection prompt.
- [x] 4.3 Handle Esc/cancel: exit quietly, matching existing chooser cancel behavior (`prompt_parent_menu`'s `InquireError::OperationCanceled` handling).
- [x] 4.4 On selection, hand off into the same choose-with-override path used by `--date <value>` (offline forced, `date_label_for` override set).

## 5. Parent menu integration

- [x] 5.1 Add `Browse cache` as a fourth option in `prompt_parent_menu`'s `Select` and a corresponding `ParentMenuChoice::BrowseCache` variant.
- [x] 5.2 Wire `ParentMenuChoice::BrowseCache` in `run_menu_selection` to call the shared picker (task 4) followed by the choose flow.
- [x] 5.3 Update/extend existing parent-menu tests (mirroring `run_menu_selection_info_matches_dispatch_info_error_path` style) to cover the fourth item and its picker hand-off.

## 6. Verification

- [x] 6.1 Manual pass: `choose --date <cached date>`, `choose --date pick`, `choose --date <bad value>`, `choose --date <future date>`, `choose --date <uncached date>`, bare menu → Browse cache, all against a real `~/Pictures/daily-wallpapers` cache.
- [x] 6.2 Confirm no new writes to `~/.wallpaperconfig` or `last_applied.json` occur as a result of using `--date` (grep/diff config and last_applied before/after).
- [x] 6.3 `cargo test` full suite passes; `cargo build` clean.
