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

`init` infers the debug application package from a standard Android application
Gradle module when possible. Before any device-backed navigation or capture,
Minimap verifies that package against ADB's foreground activity and refuses to
record another app. Use `--allow-package-mismatch` only for an intentional
cross-app capture; `doctor` reports missing or ambiguous package configuration.

If Android CLI cannot initialize its analytics spool in a restricted filesystem,
Minimap returns `android_cli_analytics_spool_unwritable` with the blocked path
and a short permission remedy. Pass `--verbose` to include the raw subprocess
exception when debugging; normal agent output omits the Android CLI stack trace.

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

## Claude Code Plugin

Claude Code users can install the Minimap skill from this repo's plugin
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

The plugin ships `minimap-app-navigation`, the same skill `minimap init`
installs for everyday Minimap navigation and incremental graph growth.

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

If more than one device or emulator is attached, pass `--serial <SERIAL>` on
any command (or set `ANDROID_SERIAL`) so every `adb` and `android` call targets
a single device; `minimap doctor` flags ambiguous multi-device setups.

For broader manual validation, clone the public
[Android Compose samples](https://github.com/android/compose-samples) and build
one of its apps (Jetsnack, JetNews, or Jetchat):

```bash
git clone https://github.com/android/compose-samples
```

Then build and launch a sample (for example Jetsnack) from the cloned
`compose-samples/` checkout and run the smoke commands above against it; see
[docs/MINIMAP_V1_LEAN_DESIGN.md](docs/MINIMAP_V1_LEAN_DESIGN.md).
