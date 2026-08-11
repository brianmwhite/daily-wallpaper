## Context

Every source's `fetch()` (`src/sources/*.rs`) already receives a `date_label: &str` via `SourceContext` (`src/sources/mod.rs`) and uses it for both the cache path (`cache/<date>/<source>/`) and any live API call (APOD's `date` param, MODIS's `db_date` param). "Today" is injected in exactly one place, `date_label_for` (`src/lib.rs:1254`):

```rust
fn date_label_for(source, settings, source_settings) -> String {
    let use_day = source.supports_day();          // true for Bing, Spotlight
    if use_day {
        target_date_for_day(source_settings.bing.day)   // today - N days
    } else {
        Local::now().date_naive()                  // today
    }
}
```

`settings.offline` already forces cache-only behavior end to end, and every source already has a friendly "no cached wallpaper for {date}" skip message baked in when offline and nothing is cached. `prune_cache` (`src/lib.rs` ~2044-2083) already scans `cache/<date>/` folder names and parses them as `NaiveDate` to decide what to delete — the same scan, used to list instead of delete, is what a cached-date picker needs.

Nothing about "today" is persisted anywhere (no config write, no state file), so a per-run date override naturally has no memory across invocations as long as it is never written to `~/.wallpaperconfig` or `last_applied.json`.

## Goals / Non-Goals

**Goals:**
- Let a user browse and favorite (or apply) a wallpaper from a specific past cached date, via `--date YYYY-MM-DD`, `--date pick`, or the new "Browse cache" parent menu item.
- Reuse the existing `choose` flow (`run_choose`/`gather_candidates`) unmodified in shape — only the date and offline-ness fed into it change.
- Fail fast with a specific, actionable message when `--date` can't be satisfied, before any fetch/chooser UI is shown.
- Leave today's default behavior (no `--date`) byte-for-byte unchanged.

**Non-Goals:**
- Live re-fetching a historical day from a source's own archive API. Bing/Spotlight already have an unused day-offset mechanism (`BingSettings.day`, `supports_day()`) for this; wiring it up is a separate, larger feature (bounded by how far back each API's archive goes) and is not part of this change.
- Extending `--date` to `info` or `reapply`. `info`/`reapply` are scoped to "the currently applied wallpaper," a single global record independent of date — a per-date view doesn't map onto them cleanly.
- Relative date labels ("Today", "Yesterday") in the picker — plain ISO dates only, newest first.

## Decisions

**1. `--date` is a global `Cli` flag, not a per-subcommand arg.**
`Cli` today models flags like `--offline`, `--force`, `--monitor` as global args that are only meaningful for certain code paths (e.g. `--monitor` is irrelevant to `info`). `--date` follows that existing precedent rather than introducing per-subcommand arg parsing, which the CLI doesn't currently have. It's only consulted by the `choose` path (explicit `choose` subcommand, or bare-menu → Choose, or the new Browse-cache menu path).

**2. A chosen date forces `settings.offline = true` for that run; it does not add a new "cache-only-for-this-date" mode.**
This means zero source-level code changes: `apod.rs`/`modis.rs`/`bing.rs`/`spotlight.rs` already check `settings.offline` and already emit "no cached wallpaper for {date}" when nothing is found. A log line notes that offline was forced because of `--date`, so it's not a silent behavior change from the user's perspective.

**3. `date_label_for` gains an override parameter that, when set, is returned as-is for every source — bypassing `supports_day`/`target_date_for_day` entirely.**
The existing day-offset math (`target_date_for_day`) converts "N days back" to a date for Bing/Spotlight's own remote archive API. An explicit override is already an absolute date, so there's no offset to compute and no reason to route through the day-offset path — every source, including Bing/Spotlight, just gets handed the literal chosen date as `date_label`, which is exactly what their cache lookups already key on.

**4. Cached-date listing is a new small helper, e.g. `list_cached_dates(cache: &CacheManager) -> Vec<NaiveDate>`, sibling to `prune_cache`.**
Scans `cache/<date>/` folder names, parses with `NaiveDate::parse_from_str(name, "%Y-%m-%d")`, discards unparseable entries (mirrors `prune_cache`'s existing tolerance of stray folders), sorts descending, and excludes today's date. This one function backs three call sites: `--date pick`, the "Browse cache" menu item, and the "nothing cached for that date" error message's suggestion list — so the listing logic and its "exclude today" rule live in exactly one place.

**5. Validation happens before `run_choose` is entered, as a small pre-check in the `--date` dispatch path.**
Three checks, in order, each with its own message (confirmed wording — see spec):
- Parse failure → malformed-date message.
- Parses but `> today` → future-date message.
- Parses, not future, but `list_cached_dates()` (unfiltered by "today exclusion" for this specific check — see Open Question below) does not contain it → not-cached message, listing what *is* available.

This means a typo'd date never reaches the "spin up threads, try each source, hit offline skips" path at all — it fails immediately with a message that tells the user what dates actually exist.

**6. "Browse cache" menu item and `--date pick` share one function that shows the picker and then calls the same `choose`-with-override entry point.**
Avoids two implementations of "prompt with `Select`, handle empty-cache and Esc-cancel, hand off to choose." The menu item is effectively sugar for `--date pick` reached via a different entry point, not a separate feature.

## Risks / Trade-offs

- **[Risk]** Forcing `offline = true` whenever `--date` is set could surprise a user who expected `--date <a Bing archive day still within Bing's live window>` to fetch fresh. → Mitigation: this is explicitly a "browse what's cached" feature per the proposal; the log line makes the forced-offline behavior visible, and live re-fetch is called out as a non-goal rather than a silently missing capability.
- **[Risk]** `list_cached_dates` walks `cache/*` on every `--date pick`/menu/error-path invocation; on a very long-lived cache directory (years of unpruned history) this is an extra directory scan. → Mitigation: this mirrors `prune_cache`'s existing full-directory scan (already run routinely), so the cost profile is already accepted elsewhere in the codebase; no new concern.
- **[Trade-off]** Excluding today from the picker/list means a user who wants "today, but forced offline" can't reach that via `--date`/pick — they'd have to use plain `choose --offline`. → Accepted: that combination already exists today via `--offline` and doesn't need a second path.

## Open Questions

- Should the "not cached" error's suggestion list (`--date` decision #5) include today's date if today happens to have cached candidates, even though the picker itself excludes today? Leaning toward yes (the error message's job is "what dates can satisfy this request," and today can satisfy it) — but flagging since it's a minor asymmetry with the picker's exclusion rule from decision #4.
