# Changelog

All notable changes to Minimap are documented here.

## 0.2.0 - unreleased

Breaking redesign: capture is now incremental and progressive. Every selector- or label-grounded `tap` grows the graph automatically; there is no separate `learn` or `accept` ritual for the common case.

This is a clean break — existing `.minimap/` trees from 0.1.x must be re-initialized (`minimap init --force`).

### Removed

- `observe start` / `observe stop`
- `learn`
- `map --discover` / `map --finish`
- `repair`
- `check` (use `validate --screen current` instead)

### Renamed

- `minimap route <name>` → `minimap route resolve <name>` (`route` is now a subcommand group)

### Added

- `route define <name> --to <screen> [--from <screen>] [--triggers ...]` (pass `--triggers` multiple times for multiple globs; embedded commas like `{login,signup}/**` are preserved)
- `screen rename <id> <new-name>`
- `undo` — sugar for `git checkout -- .minimap/graph .minimap/routes`
- `validate --screen current` replaces the old `check`
- `accept <id> --as-new` materializes a new screen from a drift proposal instead of merging with the candidate
- `.minimap/journal.jsonl` — append-only event log with documented outcomes (`matched | new_screen | drift_staged | coord_journal_only | tap_failed | from_screen_unknown`)
- `navigation.post_tap_settle_ms` config key controls the post-tap settle window (default 500 ms)
- Tap actions and journal entries now carry an optional viewport (`{width, height}`) captured from `adb shell wm size`, enabling cross-device reusability checks

### Changed

- `tap` is atomic: it captures the pre-tap layout, executes the tap, waits for settle, captures the post-tap layout, and either commits an edge or stages a drift proposal — all in one call
- `.minimap/runs/` is gone; replaced by `journal.jsonl`
- `Route` schema slimmed to `{name, target, from?, triggers, aliases}`
- Screen IDs are now stable `screen_<hash8>` handles; edges reference IDs, so `screen rename` does not break edges
- Drift proposals now round-trip through `accept`: the default resolution writes the staged edge from the source to the candidate, and `accept --as-new` materializes a fresh screen plus edge from the observed layout instead

## 0.1.3 - 2026-05-08

### Changed

- Reframed Minimap as incremental from the start. `minimap init` now produces a useful empty graph; the graph fills in one screen at a time as the user (or an agent) navigates the app. The bulk "first-run mapping" survey is now optional, not a prerequisite.

### Documentation

- `minimap-app-navigation` skill now owns incremental mapping. Its description advertises growing the graph "even when no graph exists yet," and its body documents the lightweight `observe → tap → layout → learn --stage` loop, selector preference, and the rule that unknown-route navigation is a chance to record the route.
- `minimap-first-run-mapping` skill description tightened to bulk-survey triggers only ("map the whole app", "do first-run mapping", etc.) and now explicitly redirects everyday triggers ("use minimap", "fresh repo", "navigate to X", "record this route") to `minimap-app-navigation`.
- README "Basic Workflow" leads with `minimap init` + a "Grow the graph one screen at a time" subsection. First-Run Agent Mapping is now labeled optional with a "most users won't need it" note.

## 0.1.2 - 2026-05-07

### Changed

- Sharpened `minimap --help` output: every subcommand (and `observe start`/`observe stop`) now ships an instructive `about` string covering required flags, side effects, and which command mutates the committed graph.

### Documentation

- `minimap-app-navigation` and `minimap-first-run-mapping` skills now spell out that Claude Code plugins cannot install binaries and document the brew/cargo/source install paths to fall back on.
- Bumped Claude Code plugin and marketplace metadata to match the release.

## 0.1.0 - 2026-05-06

### Added

- Rust `minimap` CLI for Android route recording, reuse, drift checks, and validation.
- Repo-committed `.minimap/` graph artifacts with ignored runtime state and run data.
- Bounded first-run mapping workflow for agent-driven Android UI discovery.
- Repo-local skills for normal route navigation and token-intensive first-run mapping.
- Claude Code plugin marketplace metadata for installing Minimap skills.
- GitHub release workflow for macOS, Linux, and Windows binaries.
- crates.io publishing workflow and Homebrew tap formula template.
