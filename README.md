# Minimap

Android navigation memory for AI agents.

Minimap records proven paths through a running Android app as a small repo graph
under `.minimap/`. Later agents can ask where they are, go to known places, and
extend the graph as they navigate instead of rediscovering the same layout state
from scratch.

Minimap is deliberately narrow. It is not a crawler, assertion framework, source
analyzer, telemetry system, or app launcher. Agents still build, install, launch,
and verify business behavior. Minimap remembers navigation.

## Install

From a checkout:

```bash
cargo build -p minimap-cli --bin minimap
```

From source after publication:

```bash
cargo install --git https://github.com/himattm/minimap minimap-cli
```

## Basic Workflow

Initialize the repo and install agent skills:

```bash
minimap init --agents all
minimap doctor
```

Label the current place:

```bash
minimap whereami --label home
```

Navigate and learn a verified transition:

```bash
minimap tap --selector "text=SEARCH" --label search --reason "open search"
```

Reuse the graph:

```bash
minimap go search
minimap layout
```

Use raw layout only when the agent needs details Minimap does not model:

```bash
minimap layout
```

`layout` returns redacted Android layout plus Minimap orientation metadata.
Unlabeled `whereami` returns compact orientation. If either immediately follows a
fresh verified observation, Minimap can serve the cached session state instead
of paying for another Android layout capture.

## Commands

The v1 command surface is intentionally small:

```text
minimap init
minimap doctor
minimap whereami
minimap go
minimap tap
minimap scroll
minimap back
minimap layout
```

All commands return JSON by default. Graph changes are reported with
`changed_graph: true` and `changed_files`.

## Graph State

`.minimap/` contains only committed graph/config files:

```text
.minimap/
  config.json
  graph/
    places/
    edges/
```

There are no proposals, journals, run directories, or hidden repo-local runtime
state. Review graph changes through normal git diffs and PRs.

## Live Device Smoke

With an Android app already built, installed, and launched plus `android` and
`adb` on `PATH`:

```bash
minimap doctor
minimap whereami --label home
minimap tap --selector "text=SEARCH" --label search --reason "open search"
minimap back
minimap go search
```

For broader manual validation, use the Compose sample apps in
`/Users/mmckenna/Dev/compose-samples`; see
[docs/MINIMAP_V1_LEAN_DESIGN.md](docs/MINIMAP_V1_LEAN_DESIGN.md).
