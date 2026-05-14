---
name: minimap-app-navigation
description: Use in an Android codebase for any Minimap work — navigating the launched app, inspecting Android layout JSON, running android layout or android layout --diff, tapping UI elements, validating screens, reusing known routes, or growing the repo's Minimap graph one screen at a time. Also fires for bulk-mapping requests like "map the whole app", "first-run mapping", "bulk-map", "do an initial pass", or "explore the app comprehensively." Before calling android layout or raw adb tap commands directly, check Minimap first.
metadata:
  author: minimap
  version: "2.0"
---

# Minimap App Navigation Skill

Minimap is this repo's shared navigation memory for AI agents working in this Android codebase. Use Minimap before raw `android` or `adb` commands.

## Mental model

Minimap maintains a graph of the app under `.minimap/graph/`. Every `minimap tap --selector ...` you make grows the graph automatically — the matched-or-new-screen and the edge are committed inline. There is no separate "learn" or "promote" step. The only thing that ever lands in `.minimap/proposals/` is an ambiguous drift band, which is rare.

## Core loop

One atomic primitive. Repeat until the user's stated goal is reached.

```bash
minimap layout                                                # see what's on screen
minimap tap --selector "<kind>=<value>" --reason "<why>"       # tap; graph grows
```

That's it. Each `tap` returns one of:

- exit 0, `outcome: matched` or `outcome: new_screen` — edge committed, keep going
- exit 1, `outcome: drift_staged` — drift proposal staged, see the drift section below
- exit 2 — selector not found or no transition; reread layout and try a different selector

## Selectors

Preference order, most stable first:

1. `test_tag=...`
2. `resource_id=...`
3. `content-desc=...` / `accessibility=...`
4. stable `visible_text=...`

Escape hatch for WebViews or layout-opaque content: take a screenshot and pass `--label N --screenshot <path>` so vision can ground the tap.

Last resort: `--point X,Y`. This taps the device and journals the event but does NOT grow the graph — the CLI prints `tap recorded; no edge added — use --selector or --label to grow the graph`. Use only when there is genuinely no selector or screenshot label option (e.g. a drag handle in a WebView with no test IDs).

## Reusing the graph

```bash
minimap route resolve <route-name>     # show the path
minimap go                             # walk it
minimap validate --screen current      # check the current screen against the graph
minimap drift                          # batch scan all committed screens for drift
```

## Naming things

New screens auto-get a hash-based id (`screen_a7f3b2c1`) and a display name derived from a11y heading / static text / content-desc. To rename:

```bash
minimap screen rename <id> <preferred-name>
```

To name a route after walking it:

```bash
minimap route define <name> --to <screen> [--from <screen>] [--triggers <glob>]
```

`route define` is the only way a Route is born.

## Drift review

If a `tap` exits 1 with `outcome: drift_staged`, a proposal lives at `.minimap/proposals/<id>.json`. Report the proposal id and stop. Do NOT run `minimap accept` without explicit user approval — this is the only remaining case where you must pause for a human. Everything else auto-commits.

## Undo

```bash
minimap undo
```

Sugar for `git checkout -- .minimap/graph .minimap/routes`. Recommend it if a recent tap clearly went wrong and the user wants a fresh start.

## Bulk mapping mode

Triggered by phrases like "map the whole app", "first-run mapping", "bulk-map", "do an initial pass".

1. Warn the user that bulk mapping is token-intensive. Propose a scoped list of named flows first, e.g. "I'll map: login, home, settings, profile-edit. Add or remove from this list?"
2. For each flow in the agreed list: walk it with the core loop above, then `minimap route define <name> --to <screen> [--from <screen>]` after completion.
3. Stop when the named list is covered. Do not crawl past the requested scope.

The capture loop is identical to everyday navigation — only the framing (scope agreement, route-name-per-flow) differs.

## What NOT to do

- Don't call `minimap observe start` / `observe stop` — those commands no longer exist.
- Don't call `minimap learn` — no longer exists.
- Don't call `minimap map --discover` — no longer exists.
- Don't call `minimap check` — use `minimap validate --screen current`.
- Don't call `minimap repair` — drift is staged inline by `tap`; rerun `minimap drift` for batch sweeps.
- Don't call `minimap accept` without explicit user approval (only used for drift proposals).
- Don't use `--point X,Y` taps unless there is genuinely no selector or screenshot label option.

## Prerequisites

The `minimap` CLI must be on `PATH`. Claude Code plugins cannot install binaries, so if `minimap --version` fails, ask the user to install it before continuing:

- Homebrew: `brew install himattm/minimap/minimap`
- Cargo: `cargo install minimap-cli`
- From source: `cargo install --git https://github.com/himattm/minimap minimap-cli`

`android` and `adb` must also be on `PATH` for any layout or tap commands.
