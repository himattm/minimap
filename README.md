# Minimap

Give AI agents a map of your Android app.

Minimap is shared navigation memory and soft validation for AI agents working in
Android codebases. It wraps documented [`android` CLI](https://developer.android.com/tools/agents/android-cli) and [`adb`](https://developer.android.com/tools/adb) primitives, records
navigation runs, stores distilled graph artifacts under `.minimap/`, and lets later
agents reuse known routes instead of rediscovering the UI.

## Install

With Homebrew, after the tap is published:

```bash
brew install himattm/minimap/minimap
```

With Cargo, after the crates are published:

```bash
cargo install minimap-cli
```

From source:

```bash
cargo install --git https://github.com/himattm/minimap minimap-cli
```

From a checkout:

```bash
cargo build -p minimap-cli --bin minimap
```

Release binaries are published from GitHub releases for macOS, Linux, and
Windows. See [docs/RELEASING.md](docs/RELEASING.md) for maintainer release
steps.

## Basic Workflow

Initialize a repo:

```bash
minimap init --agents all
minimap doctor
```

`minimap init` creates an empty graph under `.minimap/`. That is the expected
starting state — Minimap is useful immediately, and the graph fills in as you
naturally use the app. There is no required baseline survey.

### Grow the graph one screen at a time

While you are using the app, just tap. The graph commits inline:

```bash
minimap layout
minimap tap --selector "text=Open" --reason "open article detail"
```

Each `tap` either matches an existing screen and commits an edge, creates a new
screen and commits an edge, or stages a drift proposal for ambiguous cases. The
ambiguous case is rare — only then is `minimap accept <proposal-id>` involved.

Name a route after walking it:

```bash
minimap route define article-detail --to screen_article_detail --from screen_home
```

### Reuse and validate

Once a route is in the graph, reuse and validate it:

```bash
minimap route resolve article-detail
minimap go article-detail
minimap validate --screen current
minimap drift
```

## Bulk Mapping (optional)

`minimap init --agents all` installs the `minimap-app-navigation` skill, which
covers both everyday navigation and bulk mapping.

Most users will not need a bulk pass. The incremental flow above grows the
graph naturally over time as the app is used.

If you want a bulk pass over many flows at once — for example, seeding a
brand-new repo with coverage of the settings, profile, and article-detail flows
in one sitting — ask the skill explicitly. It is token-intensive: the agent has
to inspect Android layout JSON, decide what to tap, and walk many routes in a
single session. The skill will warn you and propose a scoped list of named
flows before starting.

Example prompt:

```text
Bulk-map this Android app. Start with the settings, profile, and article-detail
flows. Warn me before exploring outside that list.
```

## Claude Code Plugin

Claude Code users can install the same Minimap skills from this repo's plugin
marketplace.

From Claude Code, add the marketplace:

```text
/plugin marketplace add himattm/minimap
```

Then install the plugin:

```text
/plugin install minimap@minimap
```

For local development from a checkout:

```text
/plugin marketplace add .
/plugin install minimap@minimap
```

The plugin includes:

- `minimap-app-navigation` for everyday Minimap navigation, incremental graph
  growth one screen at a time, route reuse, validation, and bulk mapping when
  the user explicitly asks to map many flows at once.

## Product Rules

- Raw Android layout JSON is not committed by default.
- Redaction runs before hashing, normalization, or graph proposal generation.
- Append-only `.minimap/journal.jsonl` is gitignored by `minimap init`.
- `minimap tap` auto-commits Screen and NavigationEdge JSON inline. Only
  ambiguous drift lands in `.minimap/proposals/` and requires `minimap accept`.
- `android layout --diff` remains an Android in-session diff. Minimap graph drift
  is reported by `minimap drift` and `minimap validate`.

## Live Device Smoke

With a built and launched Android app plus `android` and `adb` on `PATH`:

```bash
minimap doctor
minimap layout
minimap tap --selector "text=Settings" --reason "open settings"
minimap validate --screen current
```

For a known route already committed under `.minimap/`:

```bash
minimap go <route-or-screen>
minimap drift
```

Live device tests are intentionally separate from CI. CI uses fake `android` and
`adb` executables for deterministic command-contract coverage.
