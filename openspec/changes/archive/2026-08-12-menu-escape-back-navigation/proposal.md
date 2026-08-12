## Why

When the top-level parent menu (bare `daily-wallpaper` invocation) routes into Choose or Browse cache, pressing Escape at the first prompt of that flow exits the whole program back to the terminal instead of returning to the parent menu — even though Escape at every *other* level of the same flow (the Action menu, the Favorites menu) already goes back one level. This is inconsistent and surprising: users have to fully re-launch `daily-wallpaper` just to pick a different top-level action. Meanwhile, running `daily-wallpaper choose` (or `choose --date pick`) directly, with no parent menu involved, should keep exiting to the terminal on Escape exactly as it does today, since there's no "back" to go to.

## What Changes

- The outermost prompt of a menu-reachable flow — the wallpaper-selection list in Choose (`run_choose`) and the cached-date list in Browse cache (`pick_cached_date`) — becomes context-aware: canceling it (Escape, or the explicit "Quit chooser" action) returns to the top-level parent menu when that flow was entered from the parent menu, and exits to the terminal when entered directly via `daily-wallpaper choose` / `daily-wallpaper choose --date pick`.
- The top-level parent menu (bare invocation) becomes a loop: after a menu-driven flow reports it was canceled at its outermost prompt, the parent menu is shown again instead of the process exiting. Escape at the parent menu itself continues to exit to the terminal (unchanged).
- The explicit "Quit chooser" action inside Choose is folded into the same canceled outcome as Escape — it also returns to the parent menu when reached that way, and exits when Choose was entered directly.
- Everything already nested *inside* Choose (the per-wallpaper Action menu, the Favorites list and its Action menu) keeps its current "Escape backs out one level" behavior unchanged — only the outermost prompt of each flow changes.
- Reapply and Info are one-shot actions with no prompts of their own; their error propagation is unchanged and out of scope for this change.
- Fixes a latent re-entrancy issue in `run_choose`'s `ctrlc::set_handler` call: today it can only ever run once per process, so a second `Ctrl-C` handler installation attempt (which silently fails today) was never reachable. Once the parent menu can loop back into Choose more than once per process, this must be handled so Ctrl-C still cancels an in-progress fetch on the second (and later) entries.

## Capabilities

### New Capabilities

(none)

### Modified Capabilities

- `parent-menu`: Selecting Choose or Browse cache from the parent menu no longer guarantees byte-for-byte identical behavior to the direct subcommand — canceling the outermost prompt of that flow (Escape or "Quit chooser") now returns to the parent menu instead of exiting the process. The parent menu itself becomes a loop rather than a single prompt-then-dispatch.
- `browse-cache`: The "Canceling the picker" requirement is refined to distinguish entry context — canceling `--date pick`'s date list exits quietly (unchanged) when reached via `daily-wallpaper choose --date pick` directly, but returns to the parent menu when reached via the parent menu's Browse cache item.

## Impact

- `src/lib.rs`: `run()`'s bare-invocation branch, `run_menu_selection`, `dispatch_choose_maybe_dated`, `dispatch_choose`, `run_choose`, `pick_cached_date` — return types along this call chain change from `Result<()>` to something that distinguishes "done" from "canceled at the outermost prompt," and the direct-subcommand call sites (`CommandArg::Choose`) collapse that distinction back to today's behavior.
- `run_choose`'s `ctrlc::set_handler` setup needs to tolerate (or be restructured to survive) being entered more than once per process.
- No config, cache format, CLI flags, or launchd integration changes.
- Existing tests referenced in `CLAUDE.md`/`src/lib.rs` around `run_menu_selection` and `dispatch_choose_maybe_dated` will need new cases for the loop-back behavior; no existing test behavior for direct subcommand invocation should change.
