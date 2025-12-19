# Favorites Feature Plan & Questions

## Plan draft

- **Requirements alignment**
  - Clarify what “favorite” means: explicit user action vs. auto-when-applied; allow multiple favorites per day/source.
  - Decide storage for portability: e.g., `~/Pictures/daily-wallpapers/favorites/` + `favorites/index.json`; include schema/versioning.
  - Choose metadata to preserve: title, description, attribution, info URL, source, date, original URL, local filename, checksum, resolution.
  - Ensure favorites are exempt from cache pruning and remain usable offline.
  - Define duplicate handling: re-favoriting same candidate is overwrite/no-op? allow multiple favorites from same day?

- **CLI and UX**
  - Commands to add/list/apply/remove favorites (e.g., `favorite <id>`, `favorites list`, `favorites apply <id>`, `favorites remove <id>`).
  - Optional `favorites export/import` for moving between machines.
  - Support custom favorites dir via config/flag (`favorites_dir` / `--favorites-dir`).
  - Interactive chooser: add “Favorite” action and indicate already-favorited items.

- **Data model & persistence**
  - `FavoriteEntry` struct with metadata + path (copied or hardlinked) and stable id (candidate id or UUID).
  - File naming convention for portability: `<source>-<date>-<sanitized-title>.jpg` (with suffixing on collision).
  - Atomic writes for index (reuse `write_bytes_atomic`); consider checksum for integrity.

- **Operations**
  - Mark favorite: validate cached candidate, copy/hardlink image into favorites dir, upsert index entry.
  - List favorites: human-readable + optional JSON.
  - Apply favorite: reuse existing apply logic, ensure metadata (info) is available.
  - Remove favorite: drop index entry and file (unless shared); handle missing file gracefully.
  - Export/import (optional): tar/zip or documented folder copy; include index versioning.

- **Testing**
  - Unit/integration: mark/list/apply/remove across sources using temp dirs; offline behavior; collision handling; prune does not remove favorites; corruption/missing file errors are clear.

## Questions to finalize spec

1) Default location OK? `~/Pictures/daily-wallpapers/favorites` with `favorites/index.json`? Need configurable override?
2) Storage method: copy files for portability, or hardlink when possible (space-saving) with copy fallback?
3) Required metadata to keep for portability? (title, description, attribution, info URL, source, date, original URL, resolution, checksum?)
4) CLI surface: which commands do you want (add/list/apply/remove/export/import)? Any flags (e.g., `--favorites-dir`)?
5) Should the interactive chooser show favorite status and offer a “Favorite” action?
6) Naming: keep internal `candidate.id`, or allow custom label when marking? Any human-friendly display fields?
7) How to handle duplicates: re-favoriting same candidate overwrites? allow multiple favorites per day/source? collision suffixing rules?
8) Pruning/cleanup: should favorites be fully exempt from cache pruning? Any desired limit or cleanup strategy?
9) Portability: prefer explicit export/import artifact (zip/tar) or is copying the favorites folder acceptable?
