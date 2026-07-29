## 1. Hidden auto-update subcommand

- [x] 1.1 Add a new `CommandArg` variant for the auto-update body (e.g. `AutoUpdateRun`), marked `#[value(hide = true)]` so it's parseable but omitted from `--help`/possible-values.
- [x] 1.2 In `run_with_raw_args`, add a match arm for the new variant that runs exactly the logic currently in the `None` fallthrough (the `should_skip_auto_update` + fetch/apply body at `src/lib.rs:847-882`), unchanged.

## 2. Shared dispatch functions for menu reuse

- [x] 2.1 Extract the bodies of the `CommandArg::Choose`, `CommandArg::Info`, and `CommandArg::Reapply` match arms into standalone functions so they can be called both from the existing match and from the new menu-selection branch, with no behavior difference.

## 3. `enable-auto-update` always emits the explicit subcommand

- [x] 3.1 Update `create_launchd_plist` (`src/lib.rs:2123`) to always insert the hidden auto-update subcommand token into `ProgramArguments`, for both fresh and re-run invocations (mirroring how `create_display_sync_plist` already hardcodes `"display-sync"`).
- [x] 3.2 Update/add tests asserting the generated plist's `ProgramArguments` contains the hidden subcommand token, covering both a fresh `enable-auto-update` and a re-run over an existing schedule.

## 4. Plist self-heal for stale (pre-migration) installations

- [x] 4.1 Add a helper that reads `settings.plist_filename()` (if it exists), parses `ProgramArguments` via the `plist` crate, and reports whether the hidden auto-update subcommand token is present.
- [x] 4.2 In the bare-invocation, non-interactive path, call this helper: if the plist file doesn't exist, skip straight to the fetch/apply body (no auto-update was ever enabled for this `--auto-update-name`); if it exists but lacks the token, call `create_launchd_plist(&settings, &raw_args)` to rewrite+reload it, then continue with this run's fetch/apply body exactly as before.
- [~] 4.3 Add a test simulating an old-style plist (no subcommand in `ProgramArguments`) on disk, invoking the bare/non-interactive path, and asserting the plist is rewritten to include the hidden subcommand while the run's fetch/apply behavior is unchanged. (Partial — see note below.)
- [x] 4.4 Add a test for the "no plist exists" case confirming no plist is created and behavior matches today's bare-invocation auto-update path.
- [x] 4.5 Add a test confirming that invoking the hidden subcommand directly (already-migrated case) performs no self-heal file check/rewrite.

## 5. Interactive parent menu

- [x] 5.1 In the `None` (bare) arm of `run_with_raw_args`, branch on `io::stdin().is_terminal() && io::stdout().is_terminal()`.
- [x] 5.2 When interactive: show an `inquire::Select` menu with exactly three items (Choose / Info / Reapply); on selection, call the corresponding shared dispatch function from task 2.1.
- [x] 5.3 When the prompt returns `Err` (menu couldn't be shown/failed), fall back to the non-interactive path (self-heal check + fetch/apply body from section 4), rather than erroring or hanging.
- [x] 5.4 Resolve the open question from design.md on explicit user cancel (Esc) behavior — decide whether it exits quietly or falls back to the auto-update body — and implement accordingly.
- [x] 5.5 When non-interactive (either stdin or stdout not a TTY), route to the self-heal + fetch/apply path from section 4, matching current bare-invocation behavior.

## 6. Tests for menu dispatch

- [x] 6.1 Add tests (or a testable seam, e.g. injecting a fake interactivity check / prompt result) verifying that selecting each of the three menu options invokes the identical shared dispatch function as the corresponding explicit subcommand.
- [x] 6.2 Add a test verifying explicit `choose`/`info`/`reapply` subcommands are unaffected by the menu changes.

## 7. Documentation

- [x] 7.1 Update CLAUDE.md's description of bare-invocation behavior and auto-update dispatch (the "Command dispatch" and "Auto-update skip logic" sections) to reflect the hidden subcommand and self-healing migration.
- [x] 7.2 Update README (if it documents bare invocation or `enable-auto-update` internals) to match.
