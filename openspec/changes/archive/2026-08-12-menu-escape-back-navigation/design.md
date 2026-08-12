## Context

`daily-wallpaper`'s bare invocation shows a parent menu (`prompt_parent_menu`, `src/lib.rs:1000`) that dispatches to the same functions used by the explicit subcommands (`dispatch_choose`, `dispatch_info`, `dispatch_reapply`, `dispatch_choose_maybe_dated`). This sharing is intentional and documented in the `parent-menu` spec ("runs the exact same code path... with no difference in behavior"). Today, the outermost prompt of the two flows that have one — `run_choose`'s wallpaper-selection `Select` and `pick_cached_date`'s date-selection `Select` — treats any prompt failure (including a plain Escape) identically regardless of how the flow was entered: it returns `Ok(())`, which unwinds all the way to `main` and exits the process.

Everything nested *inside* those flows already distinguishes levels correctly: the per-candidate Action menu and the Favorites menu both treat Escape as "go back to the enclosing loop," not "exit the process." The gap is specifically the seam between the parent menu and the top of each flow it dispatches to.

## Goals / Non-Goals

**Goals:**
- Canceling (Escape, or "Quit chooser") the outermost prompt of Choose or Browse cache returns to the parent menu when that flow was entered from the parent menu.
- The same cancel action exits to the terminal, unchanged, when the flow was entered directly via `daily-wallpaper choose` or `daily-wallpaper choose --date pick`.
- Within Browse cache specifically, canceling the wallpaper list *after* a date has been picked returns to the date list, not past it — the date list absorbs one level of cancellation before `Canceled` is allowed to reach the parent menu (or, for direct invocation, the terminal). This holds the same way regardless of entry point.
- No change to any prompt nested deeper than the outermost one (Action menu, Favorites menu, Favorites Action menu) — their existing "back one level" behavior is preserved exactly.
- No change to Reapply/Info, which have no prompts.
- Ctrl-C during a fetch still cancels that fetch correctly even if the user has looped back into Choose more than once in the same process.

**Non-Goals:**
- Changing what happens after a *successful* action (e.g. Apply) — that continues to exit the process, from any entry point.
- Making Reapply/Info errors loop back to the menu instead of propagating (explicitly deferred per proposal).
- Persisting or remembering menu position across process invocations — the loop is in-memory only, for the lifetime of one bare invocation.
- Any change to non-interactive (non-TTY) behavior, launchd integration, or CLI flag parsing.

## Decisions

### Outcome bubbles up; caller decides what it means

Introduce an outcome type returned by the call chain that terminates in the two outermost prompts:

```rust
enum ChooseOutcome {
    Done,       // completed normally: applied, an unrecoverable state, etc.
    Canceled,   // user backed out at the outermost prompt (Escape or "Quit chooser")
}
```

`run_choose` and `pick_cached_date` (via `dispatch_choose`, `dispatch_choose_maybe_dated`) return `Result<ChooseOutcome>` instead of `Result<()>`. On Escape or "Quit chooser" at the outermost prompt, they return `Ok(ChooseOutcome::Canceled)` — the same value regardless of how they were entered. The two call sites interpret it differently:

- The direct-subcommand arm in `run()` (`CommandArg::Choose`) treats both `Done` and `Canceled` as "the command is finished" → `Ok(())`. No behavior change from today.
- `run_menu_selection`, called from the parent-menu loop, propagates `Canceled` distinctly so the bare-invocation loop in `run()` can re-show `prompt_parent_menu()` instead of returning.

**Alternative considered:** thread a "which context am I in" flag *down* from `run()` into `dispatch_choose_maybe_dated` → `dispatch_choose` → `run_choose`, and have the Escape handler itself decide `Ok(())` vs. some sentinel based on the flag. Rejected because it forces every function on the call chain to accept and forward a parameter whose only purpose is to be consulted three or four calls deep, and it couples `run_choose`/`pick_cached_date` to knowledge of their caller's identity. Bubbling the outcome up keeps those functions ignorant of context (matching how they're already shared verbatim between both entry points today) and puts the interpretation only at the two places that actually differ.

### `dispatch_choose_maybe_dated` absorbs one level of `Canceled` for the "pick" path

Initial implementation had `dispatch_choose_maybe_dated`'s `--date pick` branch call `pick_cached_date` once, then call `dispatch_choose` once and forward whatever it returned. This meant Escape at the wallpaper list — reached only after a date was already picked — produced the exact same `Canceled` as Escape at the date list itself, so both looked identical to every caller. Via the parent menu this skipped a level: canceling the wallpaper list jumped straight back to the parent menu instead of back to the date list the user had just picked from, discovered via manual testing after the initial implementation.

Fixed by wrapping the "pick" branch in a loop: a `Canceled` returned from `dispatch_choose` (i.e. from the wallpaper list, after a date was already picked) is caught locally and `continue`s the loop — re-showing `pick_cached_date` — rather than being returned to the caller. Only a `Canceled` from `pick_cached_date` itself (nothing picked yet) is returned outward. This is the same "outcome bubbles up" pattern applied one level deeper: `dispatch_choose_maybe_dated` is now itself a caller that interprets `Canceled` from what it calls, exactly as `run_menu_selection`/`run()` interpret `Canceled` from it. No context flag needed here either — the behavior is identical whether `--date pick` was reached directly or via the parent menu, since the date-list-to-wallpaper-list nesting is intrinsic to Browse cache itself, not to how Browse cache was entered.

### "Quit chooser" and Escape are the same outcome

The explicit "Quit chooser" action in the Action-select-adjacent top-level actions list and Escape at the wallpaper list both produce `ChooseOutcome::Canceled`. No separate "quit vs. cancel" distinction is introduced, per the proposal's resolution of this question. This keeps the outcome enum binary and avoids a third code path.

### Parent-menu loop lives in `run()`, not in a new function

The bare-invocation branch in `run()` (`src/lib.rs:853-874`) changes from a single `prompt_parent_menu()` → `run_menu_selection()` call into a `loop`. On `Ok(ChooseOutcome::Canceled)` (or the equivalent for Info/Reapply/BrowseCache — see below) from `run_menu_selection`, the loop continues, re-showing `prompt_parent_menu()`. On `Err(InquireError::OperationCanceled)` from `prompt_parent_menu()` itself, the loop returns `Ok(())` (unchanged today). On any other prompt error, the loop breaks out to the existing self-heal/auto-update fallback (unchanged today).

Since Info and Reapply have no prompts, `run_menu_selection`'s `Info`/`Reapply` arms always produce `Done` (never `Canceled`) — they run once and either return successfully or propagate an `Err`, exactly as today; an `Err` still exits the process (per proposal, out of scope to change). Only the `Choose` and `BrowseCache` arms can produce `Canceled`.

### `ctrlc::set_handler` re-entrancy

`run_choose` installs a Ctrl-C handler on every call (`src/lib.rs:1485`) that closes over that call's local `cancel`/`fetch_active` `Arc`s. The `ctrlc` crate permits only one handler per process; a second `set_handler` call returns `Err(MultipleHandlers)`, which `run_choose` already handles by logging and continuing — but the *first* call's handler, closing over now-stale flags, remains installed and active. This was unreachable before this change (process exited before `run_choose` could run twice); the parent-menu loop makes it reachable (Choose → Escape → menu → Choose again, all in one process).

Fix: move the Ctrl-C handler installation out of `run_choose` and install it once, before the parent-menu loop begins (and, separately, once before the direct-subcommand dispatch, mirroring where it's needed today). The handler needs a way to reach whichever `cancel`/`fetch_active` pair is "live" for the current `run_choose` invocation — the simplest approach is a `Arc<Mutex<Option<(CancelFlag, Arc<AtomicBool>)>>>` (or similar shared cell) that `run_choose` registers itself into on entry and clears on exit, with the process-wide handler reading through the cell. This keeps `set_handler` a true one-time call while letting each `run_choose` entry supply fresh flags.

**Alternative considered:** leave `set_handler` inside `run_choose` but make it a no-op after the first call (track "already installed" in a `OnceLock`/`static`). Rejected because the *flags* still need to be current for the active invocation, not just "a handler exists" — a no-op second call would leave Ctrl-C wired to the first invocation's now-dead `cancel`/`fetch_active`, which is the actual bug, not just a log spam issue.

## Risks / Trade-offs

- **[Risk]** Widening `Result<()>` to `Result<ChooseOutcome>` touches several function signatures (`run_choose`, `pick_cached_date`, `dispatch_choose`, `dispatch_choose_maybe_dated`, `run_menu_selection`) and their call sites/tests. → Mitigation: the change is mechanical (add a variant, thread it through existing `Ok(())` returns), and existing tests around `run_menu_selection`/`dispatch_choose_maybe_dated` (per `CLAUDE.md`) give a harness to verify direct-dispatch behavior is provably unchanged.
- **[Risk]** The Ctrl-C handler restructuring changes ownership/lifetime of `CancelFlag`/`fetch_active` outside `run_choose`, which is more invasive than the navigation change itself. → Mitigation: scope it tightly — only the registration mechanism changes, not `run_choose`'s internal cancellation logic (`CancelFlag::clear`/`set`, the `fetch_active` `AtomicBool` checks stay as-is).
- **[Trade-off]** `ChooseOutcome::Done` collapses several genuinely different terminal states (applied a wallpaper, hit "No wallpapers available" and returned early via `?`, an inquire error other than cancellation) into one variant, same as today's `Ok(())` does. This is intentional — the proposal explicitly keeps error handling out of scope, so preserving today's coarse-grained "anything else is Done" behavior avoids scope creep.

## Migration Plan

No data migration. This is a pure control-flow change in `src/lib.rs`, gated by existing TTY detection that's already in place. Rollback is a plain revert; no persisted state format changes.

## Open Questions

None outstanding — the two ambiguities identified during exploration (Quit-chooser semantics, error-propagation scope) were resolved in the proposal.
