## Why

Wallpapers roll out of easy reach the moment the calendar flips: today's cache is only reachable through the normal `choose` flow, so if you didn't favorite something yesterday, there's currently no way back to it short of digging through the `cache/<date>/<source>/` folders by hand. The fetch pipeline already threads a `date_label` through every source's cache lookup and live fetch, so browsing a past cached day is a small, well-contained addition rather than a new pipeline.

## What Changes

- Add a `--date <VALUE>` CLI flag, meaningful when combined with `choose` (or the bare interactive menu routing to Choose):
  - `--date YYYY-MM-DD` browses that specific cached date: forces offline (cache-only) behavior for the run and forces every source's date lookup to that date instead of today.
  - `--date pick` shows an interactive picker of cached dates, then proceeds into the same choose flow with the selected date.
  - Validates the date up front (before entering the chooser) with a distinct, actionable message for each failure mode: malformed input, a future date, or a well-formed date with nothing cached anywhere.
- Add a fourth top-level parent menu item, "Browse cache", alongside the existing Choose/Info/Reapply items. Selecting it shows the same cached-date picker as `--date pick`, then proceeds into the same choose flow with the selected date.
- Add a shared cached-date listing helper (dates only, newest first, no relative labels like "Today"/"Yesterday", today's own date excluded since it's already reachable via plain Choose) used by both the `--date pick` flag path and the new menu item.
- No persistence: the chosen date is never written to `~/.wallpaperconfig`, `last_applied.json`, or any other state. Every invocation without `--date` behaves exactly as today.

## Capabilities

### New Capabilities
- `browse-cache`: the `--date` flag (direct date and `pick` modes), its validation/error messages, the cached-date listing helper, and how a chosen date flows into the existing choose loop offline and date-scoped.

### Modified Capabilities
- `parent-menu`: the bare-invocation interactive menu grows from exactly three items (Choose, Info, Reapply) to four (adding Browse cache), and selecting Browse cache runs the cached-date picker followed by the choose flow rather than one of the three existing subcommand code paths directly.

## Impact

- `src/lib.rs`: `Cli`/global args (new `--date` flag), `date_label_for` (accept an override), `dispatch_choose`/`run_choose`/`gather_candidates` call sites (thread the override + forced offline through), `prompt_parent_menu`/`ParentMenuChoice`/`run_menu_selection` (new menu item), a new shared cached-date listing helper (sibling logic to `prune_cache`'s existing folder scan).
- No changes to `src/sources/*` — sources already accept an arbitrary `date_label` for both cache and live lookups.
- No changes to `FavoritesManager` — favoriting a candidate from a past date already works once the candidate is in the chooser's candidate list.
- No changes to config schema, `last_applied.json` schema, or auto-update skip logic.
