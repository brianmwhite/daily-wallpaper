# parent-menu

## Purpose

Defines the interactive parent menu shown on bare invocation of `daily-wallpaper` at an interactive terminal, and the conditions under which the auto-update fetch/apply body runs instead.

## Requirements

### Requirement: Interactive parent menu on bare invocation
When `daily-wallpaper` is run with no subcommand and both stdin and stdout are attached to an interactive terminal, the system SHALL present a selection menu offering exactly three items corresponding to the `choose`, `info`, and `reapply` subcommands, and SHALL NOT run the auto-update fetch/apply body directly.

#### Scenario: Bare invocation at an interactive terminal shows the menu
- **WHEN** a user runs `daily-wallpaper` with no subcommand at an interactive terminal (both stdin and stdout are TTYs)
- **THEN** a selection menu is shown with exactly three options representing Choose, Info, and Reapply, and no wallpaper fetch/apply happens before a selection is made

#### Scenario: Selecting a menu item runs the identical subcommand behavior
- **WHEN** a user selects one of the three menu items
- **THEN** the system runs the exact same code path as invoking `daily-wallpaper choose`, `daily-wallpaper info`, or `daily-wallpaper reapply` directly, with no difference in behavior, output, or side effects

### Requirement: Explicit subcommands remain unaffected
Running `daily-wallpaper choose`, `daily-wallpaper info`, or `daily-wallpaper reapply` explicitly SHALL behave exactly as it did before this change, regardless of whether stdin/stdout are a terminal.

#### Scenario: Explicit subcommand bypasses the menu
- **WHEN** a user runs `daily-wallpaper choose` (or `info`, or `reapply`) explicitly, interactively or non-interactively
- **THEN** the corresponding command runs directly with no menu shown, identical to its pre-change behavior

### Requirement: Non-interactive bare invocation runs the auto-update body
When `daily-wallpaper` is run with no subcommand and stdin or stdout is not an interactive terminal, the system SHALL run the auto-update fetch/apply body (unchanged from current behavior) rather than showing a menu.

#### Scenario: Bare invocation with redirected stdio
- **WHEN** `daily-wallpaper` is run with no subcommand and either stdin or stdout is redirected (not a TTY) — for example under launchd, cron, or with output piped/redirected to a file
- **THEN** the system runs the auto-update fetch/apply body exactly as it does today, without attempting to show a menu

### Requirement: Menu failure falls back to the auto-update body
If the interactive menu cannot be displayed or its prompt fails for any reason after being shown, the system SHALL fall back to running the auto-update fetch/apply body rather than hanging or erroring out.

#### Scenario: Menu prompt errors
- **WHEN** the terminal is detected as interactive but the menu prompt returns an error instead of a selection
- **THEN** the system falls back to running the auto-update fetch/apply body instead of exiting with an error or hanging
