## MODIFIED Requirements

### Requirement: Interactive parent menu on bare invocation
When `daily-wallpaper` is run with no subcommand and both stdin and stdout are attached to an interactive terminal, the system SHALL present a selection menu offering exactly four items: Choose, Info, Reapply, and Browse cache, and SHALL NOT run the auto-update fetch/apply body directly. After a menu-driven Choose or Browse cache flow is canceled at its outermost prompt (Escape, or the "Quit chooser" action within Choose) before completing, the system SHALL show the parent menu again rather than exiting the process.

#### Scenario: Bare invocation at an interactive terminal shows the menu
- **WHEN** a user runs `daily-wallpaper` with no subcommand at an interactive terminal (both stdin and stdout are TTYs)
- **THEN** a selection menu is shown with exactly four options representing Choose, Info, Reapply, and Browse cache, and no wallpaper fetch/apply happens before a selection is made

#### Scenario: Selecting Choose, Info, or Reapply runs the identical subcommand behavior for completed outcomes
- **WHEN** a user selects Choose, Info, or Reapply from the menu and the flow completes — successfully, or with an error, or (for Choose) by applying a wallpaper — rather than being canceled at its outermost prompt
- **THEN** the system runs the exact same code path as invoking `daily-wallpaper choose`, `daily-wallpaper info`, or `daily-wallpaper reapply` directly, with no difference in behavior, output, or side effects

#### Scenario: Selecting Browse cache shows the cached-date picker then the choose flow
- **WHEN** a user selects Browse cache from the menu
- **THEN** the system shows the same cached-date picker used by `daily-wallpaper choose --date pick`, and upon a date being selected proceeds into the choose flow scoped to that date with offline forced, identically to `daily-wallpaper choose --date <selected-date>`

#### Scenario: Canceling Choose at its outermost prompt returns to the parent menu
- **WHEN** a user selects Choose from the parent menu, and then presses Escape at the wallpaper-selection list, or selects "Quit chooser" from the per-wallpaper Action menu, before applying anything
- **THEN** the system shows the parent menu again instead of exiting the process

#### Scenario: Canceling Browse cache's date picker returns to the parent menu
- **WHEN** a user selects Browse cache from the parent menu, and then presses Escape at the cached-date list before selecting a date
- **THEN** the system shows the parent menu again instead of exiting the process

#### Scenario: Canceling Choose after entering via Browse cache returns to the date picker, not the parent menu
- **WHEN** a user selects Browse cache from the parent menu, picks a cached date, and then presses Escape at the resulting wallpaper-selection list, or selects "Quit chooser", before applying anything
- **THEN** the system returns to the cached-date picker (see the `browse-cache` capability) rather than skipping past it to the parent menu; canceling that date picker in turn (see the scenario above) is what returns to the parent menu

#### Scenario: Escape at the parent menu itself still exits
- **WHEN** a user presses Escape at the top-level parent menu prompt itself (not inside a flow it dispatched to)
- **THEN** the system exits to the terminal quietly, without error, exactly as it did before this change
