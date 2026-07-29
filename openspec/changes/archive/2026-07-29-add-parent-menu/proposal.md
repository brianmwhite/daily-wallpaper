## Why

Running `daily-wallpaper` with no subcommand today silently does something a first-time or infrequent user wouldn't guess: it runs the auto-update fetch-and-apply logic. There's no discoverable entry point that says "pick one of: choose a wallpaper, see what's applied, or reapply the last one." Adding an interactive parent menu for the three everyday commands (`choose`, `info`, `reapply`) makes bare invocation self-explanatory for a human at a terminal, without changing what any of the three existing subcommands do.

The catch: bare invocation is also the exact command line that `enable-auto-update` installs into the launchd job's `ProgramArguments` (`create_launchd_plist`, `src/lib.rs:2123`) — every scheduled background run today calls the binary with zero subcommand. Introducing a menu there would hijack every existing auto-update installation the moment this ships, popping an interactive prompt into a headless launchd job (stdio redirected to `/tmp/*.out`/`.err`, no controlling terminal) every 30 minutes.

## What Changes

- Bare `daily-wallpaper` (no subcommand), when run at an interactive terminal, shows a selection menu (via the existing `inquire` dependency, consistent with the `choose` picker's UX) offering exactly the three everyday commands: **Choose**, **Info**, **Reapply**. Selecting one runs the identical code path as running that subcommand directly — no behavior differs from today's `daily-wallpaper choose|info|reapply`.
- The auto-update fetch/apply body (today's bare-invocation behavior — `should_skip_auto_update` + fetch + apply) becomes reachable through a new hidden CLI subcommand that is not shown in `--help` and not part of the public command surface. This is **BREAKING** for anything outside this codebase that invokes `daily-wallpaper` with no arguments expecting the silent auto-update behavior (e.g., a user's own hand-written cron entry that predates `enable-auto-update`); the documented, supported path (`enable-auto-update`) migrates itself automatically (see below).
- `enable-auto-update` (`create_launchd_plist`) always writes the new hidden subcommand explicitly into the launchd job's `ProgramArguments`, so newly-created or regenerated auto-update jobs never depend on TTY detection to behave correctly.
- **Self-healing migration**: any invocation that lands in the bare, non-interactive path (i.e., an old-style installed launchd job still calling the binary with no subcommand) transparently regenerates and reloads its own launchd plist to the new explicit form before or after doing its normal fetch/apply work, then continues operating exactly as before on this run. No user action, no separate installer step, and no explicit "on install" hook is required — the existing 30-minute schedule is itself the migration trigger, so every pre-existing installation converges to the explicit form within one scheduling tick after the user upgrades the binary.
- If the interactive menu prompt cannot be shown or is interrupted/fails for any reason (edge-case terminal environments), fall back to running the auto-update fetch/apply body rather than hanging or erroring — bare invocation should never leave the user with nothing happening.
- `enable-display-sync` / `display-sync` are unaffected — that plist already hardcodes an explicit `display-sync` subcommand in `ProgramArguments` and has no bare-invocation ambiguity.

## Capabilities

### New Capabilities
- `parent-menu`: interactive selection menu shown on bare invocation at a terminal, offering Choose/Info/Reapply, with graceful fallback to the auto-update body when a menu can't be shown.
- `auto-update-self-healing`: the hidden explicit auto-update subcommand, `enable-auto-update` always emitting it, and the transparent one-tick migration of any pre-existing launchd job still using the old bare-invocation form.

### Modified Capabilities
(none — no existing `openspec/specs/` capabilities exist yet in this repo)

## Impact

- `src/lib.rs`: `CommandArg` enum (new hidden variant), `run_with_raw_args` dispatch (`None` arm gains TTY branching + self-heal check), `create_launchd_plist` (always inject the hidden subcommand into `ProgramArguments`), new plist-staleness-detection helper.
- Existing tests around `should_skip_auto_update` / auto-update dispatch (`src/lib.rs`) need a companion path exercised through the new hidden subcommand instead of bare args; existing `enable-auto-update` plist-content tests need updating to assert the hidden subcommand is present.
- No config schema, source plugin, or cache layout changes.
- CLAUDE.md's description of bare-invocation behavior and the auto-update skip-logic notes should be updated once implemented (tracked in tasks, not in this proposal).
