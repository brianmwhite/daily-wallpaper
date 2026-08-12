## MODIFIED Requirements

### Requirement: `--date pick` shows an interactive cached-date picker
When `--date pick` is passed, the system SHALL present an interactive selection of cached dates (excluding today), and upon selection SHALL proceed exactly as if `--date <selected-date>` had been passed. Canceling this picker (e.g. Esc) SHALL exit quietly without error when the picker was reached via `daily-wallpaper choose --date pick` directly; when the picker was instead reached via the parent menu's Browse cache item, canceling it SHALL return to the parent menu instead of exiting the process. Canceling the *choose flow* that follows a picked date (Escape at the wallpaper-selection list, or "Quit chooser") SHALL return to this date picker, showing it again, rather than propagating past it — this holds regardless of whether `--date pick` was reached directly or via the parent menu; only canceling the date picker itself (with no date yet picked) propagates further.

#### Scenario: Picking a date from the list
- **WHEN** a user runs `daily-wallpaper choose --date pick` and at least one non-today date is cached
- **THEN** a selection list of cached dates is shown, plain ISO date strings only, newest first, with no relative labels, and selecting one proceeds into the choose flow scoped to that date with offline forced

#### Scenario: Canceling the choose flow after a date is picked returns to the date picker
- **WHEN** a user runs `daily-wallpaper choose --date pick` (directly, or via the parent menu's Browse cache item), picks a cached date, and then presses Escape at the resulting wallpaper-selection list, or selects "Quit chooser", before applying anything
- **THEN** the system shows the cached-date picker again — the same list of cached dates — rather than exiting (direct invocation) or returning to the parent menu (menu-driven invocation)

#### Scenario: No cached dates available to pick
- **WHEN** a user runs `daily-wallpaper choose --date pick` and no cached dates other than today exist
- **THEN** the system prints a plain message that there are no cached dates yet and exits without attempting to show an empty selection list

#### Scenario: Canceling the picker when invoked directly
- **WHEN** a user runs `daily-wallpaper choose --date pick` directly and cancels the selection prompt (e.g. Esc)
- **THEN** the system exits quietly without error and without entering the choose flow

#### Scenario: Canceling the picker when reached via the parent menu
- **WHEN** a user reaches the cached-date picker via the parent menu's Browse cache item and cancels the selection prompt (e.g. Esc)
- **THEN** the system returns to the parent menu instead of exiting the process, and does not enter the choose flow
