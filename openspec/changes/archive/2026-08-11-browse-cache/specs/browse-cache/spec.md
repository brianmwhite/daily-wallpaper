## ADDED Requirements

### Requirement: `--date` flag browses a specific cached date
When `--date YYYY-MM-DD` is passed alongside `choose` (or bare invocation routes to Choose), the system SHALL run the choose flow scoped to that date: every source's cache lookup and fetch SHALL use the given date instead of today, and the run SHALL be forced into offline (cache-only) mode regardless of whether `--offline` was also passed.

#### Scenario: Browsing a valid cached past date
- **WHEN** a user runs `daily-wallpaper choose --date 2026-08-10` and `2026-08-10` has at least one cached candidate
- **THEN** the chooser shows candidates from `2026-08-10`'s cache only, no network requests are made, and Favorite/Apply/Info actions behave identically to a normal choose session

#### Scenario: Forced offline is logged
- **WHEN** a user runs `daily-wallpaper choose --date 2026-08-10` without also passing `--offline`
- **THEN** the system logs that offline mode was forced because `--date` was used, before attempting to gather candidates

### Requirement: `--date pick` shows an interactive cached-date picker
When `--date pick` is passed, the system SHALL present an interactive selection of cached dates (excluding today), and upon selection SHALL proceed exactly as if `--date <selected-date>` had been passed.

#### Scenario: Picking a date from the list
- **WHEN** a user runs `daily-wallpaper choose --date pick` and at least one non-today date is cached
- **THEN** a selection list of cached dates is shown, plain ISO date strings only, newest first, with no relative labels, and selecting one proceeds into the choose flow scoped to that date with offline forced

#### Scenario: No cached dates available to pick
- **WHEN** a user runs `daily-wallpaper choose --date pick` and no cached dates other than today exist
- **THEN** the system prints a plain message that there are no cached dates yet and exits without attempting to show an empty selection list

#### Scenario: Canceling the picker
- **WHEN** a user runs `daily-wallpaper choose --date pick` and cancels the selection prompt (e.g. Esc)
- **THEN** the system exits quietly without error and without entering the choose flow

### Requirement: `--date` validates before any fetch or chooser UI is shown
The system SHALL validate a direct `--date YYYY-MM-DD` value before starting any source fetches or displaying the chooser, and SHALL fail with one of three specific messages depending on the failure:

- Malformed value (not a valid `YYYY-MM-DD` date): `Invalid date '<value>'. Expected YYYY-MM-DD, or use --date pick to select from cached dates.`
- Well-formed but in the future (later than today): `<date> is in the future; cache only holds past days.`
- Well-formed, not in the future, but no source has any cached candidate for that date: `No cached wallpapers found for <date>. Cached dates available: <comma-separated dates, newest first>. Use --date pick to select interactively.` — if no dates are cached at all, the system SHALL state that plainly instead of showing an empty list.

#### Scenario: Malformed date value
- **WHEN** a user runs `daily-wallpaper choose --date 2026-13-40`
- **THEN** the system exits with the malformed-value message and does not attempt any fetch or show the chooser

#### Scenario: Future date value
- **WHEN** a user runs `daily-wallpaper choose --date <a date later than today>`
- **THEN** the system exits with the future-date message and does not attempt any fetch or show the chooser

#### Scenario: Well-formed date with nothing cached
- **WHEN** a user runs `daily-wallpaper choose --date <a valid past date that has no cache folder or an empty one>`
- **THEN** the system exits with the not-cached message, listing whichever cached dates do exist (newest first), and does not show the chooser

#### Scenario: Well-formed date with nothing cached anywhere
- **WHEN** a user runs `daily-wallpaper choose --date <a valid past date>` and the cache is entirely empty (no dates cached at all)
- **THEN** the system exits with a message plainly stating no dates are cached, rather than listing an empty set

### Requirement: A chosen date is never persisted
The system SHALL NOT write the `--date` value (direct or picked) to `~/.wallpaperconfig`, `last_applied.json`, or any other on-disk state. Any subsequent invocation without `--date` SHALL behave identically to invocations before this change.

#### Scenario: Next run defaults back to today
- **WHEN** a user runs `daily-wallpaper choose --date 2026-08-10` and later runs `daily-wallpaper choose` (or bare invocation) with no `--date`
- **THEN** the later run operates on today's date exactly as it did before this change, with no trace of the prior `--date` value influencing it
