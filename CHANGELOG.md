# Changelog

All notable changes to Minimap are documented here.

## 0.2.0 - unreleased

Breaking redesign: Minimap is now a lean Android navigation-memory tool. The
repo graph stores only semantic places, verified transitions, and compact
fingerprints. Agents use `whereami`, `go`, `tap`, `scroll`, `back`, and `layout`;
git diff/PR review is the review surface.

This is a clean break — existing `.minimap/` trees from 0.1.x must be re-initialized (`minimap init --force`).

### Removed

- `observe start` / `observe stop`
- `learn`
- `map --discover` / `map --finish`
- `repair`
- `check`
- `validate`
- `accept`
- `undo`
- `route`
- `screen`
- `.minimap/proposals`
- `.minimap/journal.jsonl`

### Added

- `whereami` — orient from one Android layout observation; `--label` attaches or updates semantic identity.
- `go <target>` — navigate to a known global label through verified graph edges.
- `scroll` — execute scrolls and retain them as pending recipe steps for the next transition.
- `back` — press Android Back and record a verified known transition when one occurs.
- Destination labels on `tap` via `--label`; `--reason` records action intent only.
- Screenshot-label taps via `--screenshot-label <n> --screenshot <path>`.
- `.minimap/graph/places` and `.minimap/graph/edges` as the committed graph.

### Changed

- `.minimap/` now contains only `config.json`, `graph/places`, and `graph/edges`.
- `init --force` replaces the whole `.minimap/` directory with the lean layout.
- Labels are global, unique, normalized slugs; no aliases in v1.
- Place IDs are readable and label-derived, for example `place_settings`.
- Edge IDs are deterministic and agent-readable where practical.
- Edge recipes are ordered action steps, not tap-only actions.
- Coordinate and screenshot-label edges require exact viewport guards.
- Unknown destinations without `--label` do not enter the committed graph.
- `layout` returns redacted Android layout plus read-only Minimap orientation metadata.
- Agent skills installed by `init` now teach the lean command surface.

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
