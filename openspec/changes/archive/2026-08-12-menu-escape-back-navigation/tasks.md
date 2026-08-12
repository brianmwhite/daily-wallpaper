## 1. `ChooseOutcome` plumbing

- [x] 1.1 Add a `ChooseOutcome { Done, Canceled }` enum in `src/lib.rs`.
- [x] 1.2 Change `run_choose` to return `Result<ChooseOutcome>`: outermost wallpaper-selection `Select` Escape (`Err(_)` at `src/lib.rs:1597`) and the "Quit chooser" action (`src/lib.rs:1679`) return `Ok(ChooseOutcome::Canceled)`; the Apply-success path and any other terminal path return `Ok(ChooseOutcome::Done)`.
- [x] 1.3 Change `pick_cached_date` to return `Result<ChooseOutcome>` in place of `Result<Option<NaiveDate>>` composition, or otherwise signal cancellation distinctly (e.g. `Result<Option<(NaiveDate, ...)>>` plus a separate cancel signal) so `dispatch_choose_maybe_dated` can tell "canceled the date picker" apart from "picked a date." — implemented as a dedicated `PickedDate { Selected, Canceled, Unavailable }` enum rather than reusing `ChooseOutcome` itself, since "no cached dates to show" needed a third state distinct from both.
- [x] 1.4 Update `dispatch_choose` and `dispatch_choose_maybe_dated` to return `Result<ChooseOutcome>`, forwarding `run_choose`'s outcome and mapping a canceled `pick_cached_date` to `Ok(ChooseOutcome::Canceled)` without entering `run_choose`.
- [x] 1.5 Update `run_menu_selection` to return `Result<ChooseOutcome>`: the `Choose` and `BrowseCache` arms forward `dispatch_choose_maybe_dated`'s outcome; the `Info` and `Reapply` arms always map to `Ok(ChooseOutcome::Done)` (or propagate their `Err` unchanged).
- [x] 1.6 **(found during manual testing, 6.3)** Fix `dispatch_choose_maybe_dated`'s `--date pick` branch: canceling the wallpaper list after a date was already picked was propagating `Canceled` straight past the date picker, so Browse cache → pick a date → Escape landed on the parent menu instead of back on the date list. Wrapped the "pick" branch in a loop: `dispatch_choose`'s `Canceled` is caught locally and `continue`s (re-shows `pick_cached_date`); only `pick_cached_date`'s own `Canceled` (nothing picked yet) is returned to the caller. See design.md's "`dispatch_choose_maybe_dated` absorbs one level of `Canceled`" decision, and the updated `browse-cache` spec scenario "Canceling the choose flow after a date is picked returns to the date picker."

## 2. Direct-subcommand call site (unchanged behavior)

- [x] 2.1 Update the `CommandArg::Choose` arm in `run()` to call `dispatch_choose_maybe_dated` and collapse both `ChooseOutcome::Done` and `ChooseOutcome::Canceled` to `Ok(())`, preserving today's exact exit-on-cancel behavior for `daily-wallpaper choose` and `daily-wallpaper choose --date pick`.

## 3. Parent-menu loop

- [x] 3.1 Change the bare-invocation (`None`) branch in `run()` (`src/lib.rs:853-874`) into a loop: on `Ok(choice)` from `prompt_parent_menu()`, call `run_menu_selection`; on `Ok(ChooseOutcome::Canceled)`, `continue` the loop (re-show the parent menu); on `Ok(ChooseOutcome::Done)` or an `Err` from `run_menu_selection`, `return` that result exactly as today.
- [x] 3.2 Keep `Err(InquireError::OperationCanceled)` from `prompt_parent_menu()` itself returning `Ok(())` (exit to terminal), and any other prompt error falling through to the self-heal/auto-update fallback — both unchanged from today.

## 4. Ctrl-C handler re-entrancy fix

- [x] 4.1 Move Ctrl-C handler installation out of `run_choose` into a process-wide one-time registration (installed once before the parent-menu loop begins, and once before the direct `choose`/`choose --date` dispatch), backed by a shared cell (e.g. `Arc<Mutex<Option<(CancelFlag, Arc<AtomicBool>)>>>`) that the handler closure reads through. — implemented as `choose_cancel_state()` (a `OnceLock<Mutex<Option<(CancelFlag, Arc<AtomicBool>)>>>`) plus `ensure_choose_ctrlc_handler()`, called from `run_choose` itself rather than from the two call sites, since `run_choose` is the only place that needs it and both call sites already funnel through it.
- [x] 4.2 Have `run_choose` register its `cancel`/`fetch_active` pair into that shared cell on entry and clear it on exit, so Ctrl-C during a second (or later) `run_choose` invocation in the same process cancels the *current* fetch, not a stale one. — implemented via a `ChooseCancelGuard` RAII type that clears the cell on `Drop`, covering every return path (including early returns via `?`).
- [x] 4.3 Add/adjust a test exercising two sequential `run_choose`-reaching invocations in one process to confirm no `MultipleHandlers` panic/error surfaces and the shared cell doesn't leak state across invocations — `run_choose_reentry_clears_cancel_state_between_invocations`. See the note under Testing Limitation below: this cannot fully prove Ctrl-C is wired to the *second* invocation's flags without a live prompt or signal delivery, which is out of reach for this test harness.

## 5. Tests

**Testing limitation discovered during implementation:** `inquire::Select::prompt()` has no public hook for scripting keystrokes, and no test anywhere in this codebase drives a live prompt (confirmed: zero `.prompt()` calls exist under `#[cfg(test)] mod tests`). Actually triggering `ChooseOutcome::Canceled` requires a real Escape keypress in a real terminal, which this test harness cannot produce without a disproportionate new dependency (e.g. a PTY-based end-to-end harness) that's out of scope for this change. Tasks 5.1–5.3 as originally scoped (assert `Canceled` is produced by a live Escape) are therefore covered by manual verification (section 6) instead; automated coverage below is everything reachable without a live prompt.

- [x] 5.1 / 5.3 (descoped to manual — see Testing Limitation above and section 6.2–6.4). Automated coverage added instead: `pick_cached_date_with_empty_cache_returns_unavailable` and the tightened assertion in 5.7 confirm the non-prompt paths through the same return-type plumbing that Escape/"Quit chooser" also flow through.
- [x] 5.2 Covered as `pick_cached_date_with_empty_cache_returns_unavailable` — exercises `pick_cached_date`'s non-`Selected` outcome deterministically (no cached dates ⇒ `PickedDate::Unavailable`, no prompt shown); the `Canceled` variant itself falls under the same live-prompt limitation as 5.1.
- [x] 5.4 Existing test `dispatch_choose_maybe_dated_without_date_arg_matches_plain_dispatch_choose` still passes unmodified against the new `Result<ChooseOutcome>` signature, confirming the direct-dispatch path's behavior is unchanged.
- [x] 5.5 Descoped: the bare-invocation loop lives inside `run_with_raw_args`, which no existing test calls directly (it reads the real `~/.wallpaperconfig` via `load_config()`, which is why the test suite has always exercised `dispatch_*`/`run_menu_selection` directly instead — consistent precedent, not a new gap). Covered by manual verification 6.2–6.4 instead.
- [x] 5.6 Confirmed: `run_menu_selection_info_matches_dispatch_info_error_path` and `run_menu_selection_reapply_matches_dispatch_reapply_error_path` pass unmodified against the new signature.
- [x] 5.7 Tightened `run_menu_selection_browse_cache_with_empty_cache_returns_ok_without_prompting` to assert `Ok(ChooseOutcome::Done)` specifically (was a looser `is_ok()`).
- [x] Added `run_choose_reentry_clears_cancel_state_between_invocations` (see 4.3) as new coverage not in the original task list, guarding the Ctrl-C re-entrancy fix's state-cell lifecycle.

## 6. Manual verification

- [x] 6.1 Run `cargo build` and `cargo test`. — 51/51 tests pass, `cargo clippy --all-targets` shows no new warnings (all pre-existing, unrelated to this change).
- [x] 6.2 Manually run `./run.sh` (bare, interactive) → Choose → Escape at the wallpaper list → confirm the parent menu reappears (not the shell prompt). — confirmed by user.
- [x] 6.3 Manually run `./run.sh` (bare, interactive) → Browse cache → Escape at the date list → confirm the parent menu reappears. — confirmed by user.
- [x] 6.4 Manually run `./run.sh` (bare, interactive) → Choose → pick a wallpaper → "Quit chooser" from the Action menu → confirm the parent menu reappears. — confirmed by user.
- [x] 6.5 Manually run `./run.sh choose` directly → Escape at the wallpaper list → confirm it exits straight to the terminal (no menu). — confirmed by user.
- [x] 6.6 Manually run `./run.sh choose --date pick` directly → Escape at the date list → confirm it exits straight to the terminal. — confirmed by user.
- [x] 6.7 Manually verify Ctrl-C still cancels an in-progress fetch after looping Choose → Escape → menu → Choose a second time in the same session. — confirmed by user.
- [x] 6.8 **(new, added after user testing found 1.6's gap)** Manually run `./run.sh` (bare, interactive) → Browse cache → pick a cached date → Escape at the resulting wallpaper list → confirm it returns to the date picker (not the parent menu); Escape again from there should then return to the parent menu. Repeat directly via `./run.sh choose --date pick` → pick a date → Escape at the wallpaper list → confirm it returns to the date picker and exits to the terminal only on a second Escape. — confirmed by user.

**Note:** 6.2–6.8 require an interactive terminal session with real keyboard input, which isn't available in this environment — all confirmed by the user.
