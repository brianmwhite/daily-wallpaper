## MODIFIED Requirements

### Requirement: Interactive parent menu on bare invocation
When `daily-wallpaper` is run with no subcommand and both stdin and stdout are attached to an interactive terminal, the system SHALL present a selection menu offering exactly four items: Choose, Info, Reapply, and Browse cache, and SHALL NOT run the auto-update fetch/apply body directly.

#### Scenario: Bare invocation at an interactive terminal shows the menu
- **WHEN** a user runs `daily-wallpaper` with no subcommand at an interactive terminal (both stdin and stdout are TTYs)
- **THEN** a selection menu is shown with exactly four options representing Choose, Info, Reapply, and Browse cache, and no wallpaper fetch/apply happens before a selection is made

#### Scenario: Selecting Choose, Info, or Reapply runs the identical subcommand behavior
- **WHEN** a user selects Choose, Info, or Reapply from the menu
- **THEN** the system runs the exact same code path as invoking `daily-wallpaper choose`, `daily-wallpaper info`, or `daily-wallpaper reapply` directly, with no difference in behavior, output, or side effects

#### Scenario: Selecting Browse cache shows the cached-date picker then the choose flow
- **WHEN** a user selects Browse cache from the menu
- **THEN** the system shows the same cached-date picker used by `daily-wallpaper choose --date pick`, and upon a date being selected proceeds into the choose flow scoped to that date with offline forced, identically to `daily-wallpaper choose --date <selected-date>`

### Requirement: Explicit subcommands remain unaffected
Running `daily-wallpaper choose`, `daily-wallpaper info`, or `daily-wallpaper reapply` explicitly SHALL behave exactly as it did before this change, regardless of whether stdin/stdout are a terminal. There is no standalone `browse-cache` subcommand; browsing cache from outside the menu is reached only via `daily-wallpaper choose --date <value>`.

#### Scenario: Explicit subcommand bypasses the menu
- **WHEN** a user runs `daily-wallpaper choose` (or `info`, or `reapply`) explicitly, interactively or non-interactively
- **THEN** the corresponding command runs directly with no menu shown, identical to its pre-change behavior
