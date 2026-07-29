## Context

Today, `daily-wallpaper` with no subcommand runs the auto-update fetch/apply body directly (`run_with_raw_args`, `src/lib.rs:798-883`, the `None => {}` fallthrough at line 844). `enable-auto-update` (`create_launchd_plist`, `src/lib.rs:2123`) writes a launchd job whose `ProgramArguments` is `[current_exe, ...raw_args minus "enable-auto-update"]` — i.e. it deliberately reproduces a bare invocation. Every launchd-triggered run today therefore has no subcommand token at all, `OnDemand: true`, `StartInterval: 1800`, stdio redirected to `/tmp/{label}.out`/`.err`, and no controlling terminal.

`enable-display-sync` (`create_display_sync_plist`, `src/lib.rs:2193`) already hardcodes `"display-sync"` into its own `ProgramArguments`, so it has no equivalent ambiguity — this design only touches the auto-update plist and bare-invocation dispatch.

`inquire = "0.7"` is already a dependency (used by `run_choose`), so the parent menu reuses it rather than introducing a new prompt library.

## Goals / Non-Goals

**Goals:**
- Bare `daily-wallpaper` at an interactive terminal shows a menu of exactly Choose / Info / Reapply, dispatching to the identical code paths as the existing subcommands.
- The auto-update fetch/apply body remains reachable and unchanged in behavior, moved behind an explicit, hidden CLI token.
- Every pre-existing `enable-auto-update` installation converges to the new explicit invocation form automatically, without a separate install-time or user-driven migration step.
- `daily-wallpaper` invoked non-interactively (scripts, cron, an old un-migrated launchd job) keeps doing exactly what it does today: the auto-update body, unchanged.

**Non-Goals:**
- No new "run auto-update now" menu item — confirmed with the user, out of scope.
- No change to `choose`/`info`/`reapply`/`enable-display-sync`/`display-sync` behavior.
- No general-purpose config-driven menu customization (fixed 3-item menu is sufficient).
- Not attempting to migrate a plist this tool didn't create (e.g., a hand-rolled cron job invoking `daily-wallpaper` bare) — only plists written by `create_launchd_plist` are self-healed, identified by `settings.plist_filename()`.

## Decisions

**1. Interactive vs. non-interactive: `std::io::IsTerminal` on both stdin and stdout.**
The `None` arm branches on `io::stdin().is_terminal() && io::stdout().is_terminal()`. Requiring both (not just one) avoids showing a menu when either side is redirected (e.g. `daily-wallpaper > log.txt`, or piped input) — matches the launchd case (both redirected to files) and typical script/cron invocations. `std::io::IsTerminal` is stable in std since Rust 1.70, no new dependency needed.
*Alternative considered*: checking only stdin. Rejected — a caller redirecting only stdout (e.g. `daily-wallpaper > out.txt` run by hand) would still get an interactive prompt fighting with redirected output; requiring both sides to be a TTY is the more conservative, safer default for "is a human plausibly watching this."

**2. Explicit hidden subcommand rather than relying on TTY detection alone (Option B from exploration).**
Add a new `CommandArg` variant (e.g. `CommandArg::AutoUpdateRun`) marked `#[value(hide = true)]` so it's parseable but excluded from `--help`/possible-values listings. `create_launchd_plist` always appends this token to `ProgramArguments`, unconditionally — new and re-enabled auto-update jobs are self-documenting and never depend on TTY heuristics to behave correctly. TTY detection is then purely about "what should bare invocation, with no subcommand at all, do for a human" — not about disambiguating auto-update.
*Alternative considered*: TTY-gating alone (no explicit subcommand), which needs zero migration since launchd calls are never a TTY. Rejected per user preference — an explicit subcommand makes the installed plist self-documenting and doesn't leave the auto-update path's correctness implicitly dependent on terminal semantics forever.

**3. Self-healing runs inside the bare, non-interactive fallback path, gated on plist content.**
When `None` + non-interactive is reached (meaning: no explicit subcommand was given, so either an old-style plist called us, or some other non-tty caller did), check whether `settings.plist_filename()` exists; if it does, parse its `ProgramArguments` and check whether the hidden subcommand token is present. If it's missing, this is a stale pre-migration plist — call the existing `create_launchd_plist(&settings, &raw_args)` (which already does unload/write/load) to rewrite it in the new explicit form, then proceed with this run's fetch/apply body exactly as before. The *next* scheduled tick will invoke with the explicit subcommand and skip this check entirely (`CommandArg::AutoUpdateRun` match arm runs directly).
If `plist_filename()` doesn't exist at all, skip the self-heal check — this bare invocation wasn't launchd-triggered by this tool, so there's nothing to migrate; just run the fetch/apply body as today.
*Alternative considered*: a dedicated `--internal-migrate` maintenance pass run from `enable-auto-update`/on every command invocation. Rejected — piggybacking on the existing 30-minute schedule is the only trigger that reliably fires after a binary upgrade with zero extra moving parts, and doesn't add a check to every single CLI invocation (only the already-rare bare/non-interactive path).

**4. Shared dispatch functions, not duplicated logic.**
Extract the bodies of the `CommandArg::Choose`, `CommandArg::Info`, and `CommandArg::Reapply` match arms into standalone functions callable both from the existing match and from the menu-selection branch, so the menu path is byte-for-byte the same code, not a reimplementation.

**5. Menu failure/cancellation fallback.**
If `inquire::Select::prompt()` returns `Err` (e.g. unusual terminal state), fall back to running the auto-update fetch/apply body (same self-heal-then-fetch path used for non-interactive callers) rather than erroring or hanging — confirmed with user. See Open Questions for the distinct case of an explicit user cancel (Esc).

## Risks / Trade-offs

- **[Risk] Downgrading the installed binary after a plist has been migrated to the new explicit subcommand.** An older binary's `CommandArg` enum doesn't know that value and will fail to parse it, breaking auto-update entirely until the plist is regenerated. → *Mitigation*: document that downgrading the binary requires re-running `enable-auto-update` (or `disable-auto-update`) with the older binary immediately after downgrading; not silently handled, since the old binary can't be changed retroactively.
- **[Risk] `settings.plist_filename()` depends on `auto_update_name`.** Multiple concurrently-named schedules (a supported feature per CLAUDE.md) each self-heal independently the next time *their own* tick fires — no cross-schedule sweep. → *Mitigation*: this is acceptable and simpler; each schedule's own invocation carries its own `--auto-update-name`, so it always checks/heals its own plist file.
- **[Risk] A human-run bare invocation on a genuinely rare terminal (TTY present but not fully interactive, e.g. some CI runners or `ssh -tt` wrappers) could see a broken/hanging prompt before `inquire` returns an error.** → *Mitigation*: this is the documented fallback case; if it becomes a real problem, a future change could add an explicit escape hatch (env var or flag) to force non-interactive behavior, but is not needed for this change per current scope.
- **[Trade-off] Self-heal adds a file read + parse on every non-interactive bare invocation**, even ones that don't need healing. This is a single small plist read, negligible relative to network fetches already happening in the same code path.

## Migration Plan

1. Ship the new hidden subcommand, TTY-gated menu, and self-healing check together (they only make sense as one change).
2. No explicit migration script or install-time hook is introduced. Once the user upgrades the installed binary (`cargo install --path .`) at its existing path, the *next* launchd-triggered tick for each existing `enable-auto-update` schedule will run the new binary, detect its own plist lacks the explicit subcommand, rewrite + reload it, and finish that tick's fetch/apply normally. Convergence happens within one `StartInterval` (≤30 min) per schedule, with no user action required.
3. `enable-display-sync` is untouched by this migration (already explicit) — no action needed there.
4. Rollback: if the new behavior needs to be reverted, reinstalling the previous binary version and re-running `enable-auto-update` regenerates a plist that version understands (see downgrade risk above).

## Open Questions

- When the menu is shown and the user explicitly cancels (Esc / Ctrl-C on the prompt) rather than the prompt failing outright, should that (a) exit quietly with no fetch, or (b) fall back to running the auto-update body like a hard prompt failure? Leaning toward (a) — a deliberate cancel is a distinct signal from "couldn't show a menu" — but not yet confirmed with the user.
- Exact hidden subcommand name/value (e.g. `run-auto-update` vs `auto-update-run` vs something less guessable) — cosmetic, to be finalized during implementation.
