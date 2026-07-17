# Minimap Lean V1 Design

Minimap v1 is a narrow Android navigation-memory tool for AI agents. It helps an
agent remember proven ways through a running app so later agents can navigate and
verify faster without rediscovering the same Android layout state every time.

The persisted product is the graph. Minimap should not become a crawler, test
assertion framework, source-code analyzer, telemetry system, or graph admin UI.

## Product Boundary

Persist only verified navigation memory:

- Semantic places in the app.
- Verified transitions between places.
- Ordered action recipes that produced those transitions.
- Compact matching fingerprints for places.
- Variants only for simultaneously valid states.

Do not persist:

- Raw Android layout JSON.
- Unexplored exits or crawler inventories.
- Business assertions.
- Reliability counters, timestamps, or usage telemetry.
- Proposals, accept flows, always-on journals, or undo state.
- Source-code analysis output.

Git diff and PR review are the review surface for graph changes.

## Command Surface

V1 exposes only these top-level commands:

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

`init` creates `.minimap/` and installs agent skills by default. It supports
`--agents all`, `--refresh-skills`, and `--no-skills`. Skill text installed by
`init` and the Claude plugin must come from the same source/template.

`doctor` is read-only. It checks repo health and device readiness separately:
config exists and parses, graph JSON is valid, labels are unique, edges do not
dangle, `android` is available, `adb` is available, and a device is reachable.

`whereami` calls `android layout` once, matches the current layout to the graph,
and returns compact orientation only. When Minimap has a very fresh verified
session and no `--label` was provided, it may return that session place without
another layout call. When a known place is observed with a new high-confidence
fingerprint, Minimap preserves the original baseline and records the new
fingerprint as a variant. It returns fingerprint summaries only for non-plain
known states such as unknown, changed, or label mismatch. It does not return the
full layout.

`whereami --label <label>` attaches or changes semantic identity:

- If the current place is unknown and the label is unused, create the place.
- If the current place is known and the label is unused, relabel the place and
  rewrite edge references.
- If the label already belongs to a different known place, return
  `label_mismatch`.
- Trust the explicit agent label unless there is a mechanical conflict.

`go <target>` starts from the last verified session place when Minimap was the
last actor and that session is still fresh. Otherwise it performs a cold
`whereami` layout observation. It resolves the target by exact normalized
label/slug, chooses a known UI path, executes each recipe, and verifies each
semantic transition. It does not create new places or explore unknown actions.
If one known compatible path fails, it may try another known compatible path.

`tap` calls layout before and after the action. It supports:

- `--selector <kind=value>`
- `--point <x,y>`
- `--screenshot-label <n> --screenshot <path>`

Destination identity is `--label <place>`. `--reason` records action intent.
`tap --label` names the post-tap destination. A new destination place is written
only when `--label` is present. Without a label, unknown destinations do not
enter the committed graph; Minimap may keep short-lived temp state outside the
repo so a follow-up `whereami --label` can complete the transition.

`scroll` calls layout before and after. Same-place scrolls are not graph edges.
Scroll actions can accumulate in temp state as part of the next transition
recipe.

`back` calls layout before and after. If Back moves from one known place to a
different known place, record a `press_back` edge. Back never creates a new
place.

`layout` wraps `android layout` and returns redacted Android layout plus
read-only Minimap orientation metadata. When it follows a fresh verified
observation such as `go`, `whereami`, `tap`, or `back`, it may return the
cached redacted session layout instead of calling `android layout` again. It is
the raw escape hatch for agents.

## Repository State

`.minimap` contains only committed config and graph files:

```text
.minimap/
  config.json
  graph/
    places/
    edges/
```

No `.minimap/proposals`, `.minimap/journal.jsonl`, `.minimap/runs`,
`.minimap/state`, or `.minimap/checks`.

Session/temp state lives outside the repo and is scoped by repo, ADB device
serial, active Android package, and short TTLs:

```text
<system-temp>/minimap/<repo-hash>/<adb-serial>/<package>/pending-transition.json
<system-temp>/minimap/<repo-hash>/<adb-serial>/<package>/session-place.json
```

`pending-transition.json` is recovery glue for multi-action recipes such as
scroll plus tap. `session-place.json` is a verified navigation fast path: it
stores the current known place and a redacted layout snapshot so the next `go`
can skip rediscovering the current place and can resolve the first selector
without another layout call. The same snapshot can serve one very fresh
agent-facing `layout` call so `go` plus layout-based verification does not pay
for duplicate Android layout capture. The orientation session lasts longer than
the layout reuse window; cached agent-facing layout reuse is intentionally
short-lived and sized for normal agent decision latency.

## Config

`config.json` is required and intentionally small:

```json
{
  "schema_version": "minimap.config.v2",
  "active_app_profile": "default",
  "app_profiles": {
    "default": {
      "android_package": ""
    }
  }
}
```

V1 uses one active app profile. The profile fields reserve a mechanical migration
path for multiple app maps later without adding complexity now.

When the active profile is blank, `init` and runtime validation scan standard
Android application Gradle modules for a literal `applicationId` and the debug
build type's `applicationIdSuffix`. One unambiguous result becomes the expected
debug package; zero or multiple results fail the `app_package` doctor check.
Every device-backed command compares that expected package with ADB's top
resumed activity before capture, preventing another foreground app from
polluting the graph. `--allow-package-mismatch` is the explicit one-command
override.

Known Android CLI analytics-spool permission failures are classified at the
subprocess boundary. Normal structured output reports the blocked path and one
remediation without embedding Java or Rust stack traces. The global `--verbose`
flag adds the untouched command status, stdout, and stderr for diagnostics.

## Graph Schema

Places use product language instead of screen language.

```json
{
  "schema_version": "minimap.place.v1",
  "id": "place_settings",
  "slug": "settings",
  "label": "Settings",
  "baseline": {
    "identity_hash": "sha256:...",
    "fingerprint": {
      "selectors": [
        {"kind": "test_tag", "value": "settings_title"},
        {"kind": "resource_id", "value": "com.example:id/settings_list"}
      ],
      "static_text": [
        {"value": "Settings"}
      ],
      "roles": {"Button": 5, "Text": 12}
    }
  },
  "variants": []
}
```

Labels normalize to globally unique lowercase kebab-case slugs. There are no
aliases in v1. Safe static UI copy may be stored when useful. Dynamic text,
emails, tokens, long user content, numeric sensitive values, and input values are
excluded or redacted before hashing or persistence.

Readable place IDs are slug-derived, for example `place_settings`. If a place is
relabelled, Minimap rewrites the place ID and all edge references. This is rare
and visible in git.

Edges are verified semantic transitions with ordered recipes:

```json
{
  "schema_version": "minimap.edge.v1",
  "id": "edge_home__settings__tap_test_tag_settings_button",
  "from": {"id": "place_home", "slug": "home"},
  "to": {"id": "place_settings", "slug": "settings"},
  "intent": "open settings",
  "recipe": [
    {
      "kind": "tap",
      "selector": {"kind": "test_tag", "value": "settings_button"}
    }
  ]
}
```

Coordinate and screenshot-label actions are geometry actions. They require exact
viewport guards:

```json
{
  "kind": "tap",
  "point": {"x": 540, "y": 1200},
  "viewport": {"width": 1080, "height": 2400}
}
```

Do not store ratios. If the viewport differs, the edge is incompatible.

Edge IDs are deterministic and agent-readable where practical: source slug,
destination slug, and primary action fingerprint, with a hash fallback for long
or colliding IDs. Equivalent edges are idempotent. Different recipes between the
same places are separate edges. Same-place taps do not create navigation edges.

## Matching And Learning Rules

The graph records only actions Minimap executed and verified.

- Action lands on known destination: write or dedupe the edge.
- Action lands on unknown destination with `--label`: create labelled place and
  write the edge.
- Action lands on unknown destination without `--label`: no graph write; return
  `needs_label`.
- Action lands on a known place different from the requested label: no graph
  write; return `label_mismatch`.
- Geometry action without viewport capture: action may execute, but no edge is
  committed.

Normal UI evolution adds a place variant when the new fingerprint still matches
the same semantic place. The original baseline is not overwritten during normal
navigation, which keeps diffs reviewable and avoids erasing the first proven
identity. Git history handles old app versions.

`whereami` may self-heal a known place baseline on high-confidence match. The
threshold is not configurable in v1.

Place baseline updates do not rewrite edge recipes. A stale edge is repaired
only by observing a new successful action to the same destination.

Blocking overlays are reported, not managed. If an overlay prevents destination
verification, return `blocked_by_overlay` and do not record the edge. Agents
decide how to handle permissions, sign-in prompts, dialogs, keyboard state, and
other transient UI.

## Result Contract

All command output is JSON by default and includes `schema_version`.

Use a small shared status vocabulary:

```text
ok
known
known_changed
unknown
needs_label
label_mismatch
blocked_by_overlay
no_known_path
no_compatible_path
action_failed
environment_error
config_error
```

Graph changes are successful command execution with `changed_graph: true` and
`changed_files`. Nonzero exit codes are reserved for failures and blockers such
as `needs_label`, `blocked_by_overlay`, `label_mismatch`, incompatible paths,
environment errors, and config errors.

## Validation Strategy

Validation has three layers.

1. Rust unit tests for redaction, fingerprinting, label normalization, graph
   loading, graph consistency, matching thresholds, recipe IDs, viewport guards,
   and path ranking.
2. CLI contract tests with fake `android` and `adb` executables. These cover the
   stable JSON contract and file writes without requiring a device.
3. Live Android smoke tests against real Compose sample apps in
   `/Users/mmckenna/Dev/compose-samples`.

Live smoke is intentionally outside CI by default. It verifies that Minimap
works against real Compose semantics and Android CLI output while still assuming
the agent owns build/install/launch.

Recommended sample targets:

- `Jetsnack` (`com.example.jetsnack`): bottom navigation and detail navigation.
  Existing sample tests cover `HOME`, `SEARCH`, `MY CART`, `PROFILE`, and the
  `Chips` detail page. Use this to validate `whereami --label`, selector/text
  taps, known path replay, and edge dedupe.
- `JetNews` (`com.example.jetnews`): drawer navigation and scroll-to-post.
  Existing sample tests open the navigation drawer, go to `Interests`, and
  scroll/click a post. Use this to validate multi-action recipes
  (`scroll` + `tap`) and Back/up behavior.
- `Jetchat` (`com.example.compose.jetchat`): drawer profile navigation and Back.
  Existing sample tests open the drawer, navigate to a profile, and press Back.
  Use this to validate content-description selectors and `back` edge recording.

Manual smoke outline for a sample:

```bash
cd /Users/mmckenna/Dev/compose-samples/Jetsnack
./gradlew :app:installDebug
adb shell monkey -p com.example.jetsnack 1

minimap init --force --agents codex
minimap doctor
minimap whereami --label home
minimap tap --selector "content_desc=SEARCH" --label search --reason "open search"
minimap go search
```

Expected smoke behavior:

- `.minimap/graph/places` and `.minimap/graph/edges` contain only compact graph
  JSON.
- No raw layout, journal, proposal, run, or state files are created under
  `.minimap`.
- Re-running the same learned navigation is idempotent.
- `go` uses `whereami`, selects known UI paths, verifies each transition, and
  reports `changed_graph` only when graph files changed.
- Unknown destinations without `--label` return `needs_label` and do not write
  graph files.

## Implementation Direction

This is a breaking pre-1.0 refactor. Plain `init` should refuse incompatible old
`.minimap` layouts with a clear message. `init --force` should replace old
Minimap state with the minimal v1 layout.

Remove or hide old command concepts from the CLI and skills:

- `accept`
- `proposals`
- `journal`
- `undo`
- `route`
- `screen`
- `observe`
- `learn`
- `map`
- `repair`
- heavyweight `validate`

Rewrite README, skills, changelog, and tests around the lean command surface.
