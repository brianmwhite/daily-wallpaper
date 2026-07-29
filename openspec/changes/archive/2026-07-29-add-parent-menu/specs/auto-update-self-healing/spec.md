## ADDED Requirements

### Requirement: Auto-update body is reachable via an explicit hidden subcommand
The auto-update fetch/apply body SHALL be invocable through a dedicated CLI subcommand that is excluded from `--help` and other user-facing command listings, in addition to remaining reachable via bare, non-interactive invocation (for backward compatibility with not-yet-migrated installations).

#### Scenario: Hidden subcommand runs the auto-update body
- **WHEN** `daily-wallpaper` is invoked with the hidden auto-update subcommand
- **THEN** the system runs the same fetch/apply logic (including `should_skip_auto_update` checks) as today's bare-invocation auto-update behavior, and the hidden subcommand does not appear in `daily-wallpaper --help` output

### Requirement: enable-auto-update always writes the explicit subcommand
`enable-auto-update` SHALL write a launchd job whose `ProgramArguments` includes the hidden auto-update subcommand explicitly, for both newly-created and re-enabled (re-run) schedules.

#### Scenario: Fresh enable-auto-update
- **WHEN** a user runs `daily-wallpaper enable-auto-update` (with any combination of flags) on a system with no prior auto-update schedule of that name
- **THEN** the generated launchd plist's `ProgramArguments` includes the current executable path followed by the hidden auto-update subcommand and the user's other flags, in a form that no longer depends on bare invocation

#### Scenario: Re-running enable-auto-update on an existing schedule
- **WHEN** a user runs `daily-wallpaper enable-auto-update` again for an `--auto-update-name` that already has an installed plist
- **THEN** the plist is rewritten and reloaded with the hidden auto-update subcommand present in `ProgramArguments`, replacing any prior bare-invocation form

### Requirement: Stale auto-update plists self-heal on their next scheduled run
When a bare, non-interactive invocation occurs and a launchd plist for the current `--auto-update-name` exists on disk but its `ProgramArguments` does not include the hidden auto-update subcommand, the system SHALL rewrite and reload that plist to the explicit form before or after completing that run's fetch/apply work, without requiring separate user action or a distinct install-time step.

#### Scenario: Old-style plist triggers self-heal
- **WHEN** a launchd job created by a previous version of `daily-wallpaper` (whose `ProgramArguments` has no subcommand token) triggers a bare, non-interactive run of the upgraded binary
- **THEN** the system detects that the on-disk plist for that `--auto-update-name` is missing the hidden subcommand, regenerates and reloads the plist with the explicit form, and still completes the current run's fetch/apply exactly as it would have before this change

#### Scenario: Already-migrated plist does not get rewritten every run
- **WHEN** the hidden auto-update subcommand is invoked directly (because a plist was already migrated)
- **THEN** the system does not perform any plist self-heal check on that invocation — it proceeds directly to the fetch/apply body

#### Scenario: Bare non-interactive invocation with no installed plist
- **WHEN** a bare, non-interactive invocation occurs but `enable-auto-update` was never run for the current `--auto-update-name` (no plist file exists)
- **THEN** the system skips the self-heal check entirely and runs the fetch/apply body exactly as it does today, without creating or modifying any plist
