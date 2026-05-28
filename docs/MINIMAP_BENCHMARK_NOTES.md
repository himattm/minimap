# Minimap Benchmark Notes

Working notes for validating whether Minimap materially accelerates AI-agent
navigation on real Android apps. These notes are intended to become source
material for a blog post or conference talk.

## Hypothesis

Agents spend repeated tool calls and tokens rediscovering app navigation from
raw Android layout output. Minimap should make the second pass cheaper by
turning the first successful navigation into a repo graph of places and verified
transition recipes.

The benchmark should measure whether an agent can navigate to previously learned
places with fewer full layout dumps and less reasoning than the first pass.

## Current Local Build

- Local `minimap` installed from this checkout with `cargo install --path
  crates/minimap-cli --force`.
- Version under test: `0.2.0` lean v1 refactor.
- Command surface under test: `init`, `doctor`, `whereami`, `go`, `tap`,
  `scroll`, `back`, `layout`.
- Shell `minimap` currently resolves to an older Homebrew binary at
  `/opt/homebrew/bin/minimap`; benchmark commands must use
  `/Users/mmckenna/.cargo/bin/minimap` or a `PATH` with `~/.cargo/bin` first.

## Environment Notes

- Android CLI is available: `android info` reports SDK
  `/Users/mmckenna/Library/Android/sdk`, version `0.7.15232955`.
- ADB device is available: `emulator-5554`.
- Compose sample packages were not already installed on the emulator at the
  start of benchmarking, so samples need to be installed before live smoke.

## Benchmark Shape

Compare two paths on the same sample app and device:

1. Raw agent navigation: use `android layout` plus manual reasoning/taps until
   the target screen is reached.
2. Minimap-assisted navigation:
   - First pass learns places and edges with `whereami --label` and
     `tap --label`.
   - Second pass uses `go <label>`.

Primary metrics:

- Count of full Android layout JSON calls exposed to the agent.
- Count of Android layout nodes exposed to the agent.
- Agent-visible output bytes.
- Count of `adb` actions.
- Count of agent reasoning steps/decisions.
- Whether the second pass avoids re-reading full layout JSON.
- Whether `.minimap/graph` diffs are minimal and reviewable.

Secondary metrics:

- Wall-clock time.
- Command count.
- Failure/mismatch cases and how recoverable the JSON output is.

## Measurement Protocol

Measure raw rediscovery and Minimap replay separately.

Raw rediscovery path:

1. Start the app at the same known entry place.
2. Run `android layout -o <pre>.json`.
3. Inspect the layout to find the target control.
4. Execute the tap with `adb shell input tap <x> <y>`.
5. Run `android layout -o <post>.json` and verify the destination.

Minimap replay path:

1. Start the app at the same known entry place.
2. Run `/Users/mmckenna/.cargo/bin/minimap go <label>`.
3. Record the JSON result, status, `changed_graph`, and output size.

Derived metrics:

```text
layout_dump_reduction = 1 - minimap_agent_visible_layout_dumps / raw_layout_dumps
node_exposure_reduction = 1 - minimap_agent_visible_layout_nodes / raw_layout_nodes
output_reduction = 1 - minimap_result_bytes / raw_layout_bytes
command_reduction = 1 - minimap_agent_commands / raw_agent_commands
```

For known-path replay, Minimap should normally have:

- `minimap_agent_visible_layout_dumps = 0`
- `minimap_agent_visible_layout_nodes = 0`
- `changed_graph = false`

Wall-clock is useful but secondary. On a cold start, Minimap still calls layout
internally before and after navigation to verify that the graph is not lying.
The first expected win is less agent rediscovery; the hot-path win comes from
verified session reuse.

Wall-clock interpretation:

- Raw single-step navigation usually performs two Android layout calls: one to
  discover the control, one to verify the destination.
- Optimized Minimap single-step replay also performs two Android layout calls:
  one to identify the current place, one to verify the destination. It reuses the
  first layout to resolve the selector instead of calling layout a third time.
- Session-fast Minimap replay performs one Android layout call for common
  one-step selector paths: it uses the last verified session place plus its
  cached redacted layout to execute the first action, then verifies the
  destination with one fresh layout.
- If the agent asks `minimap layout` immediately after a verified observation
  such as `go`, `whereami`, `tap`, or `back`, Minimap can return the fresh
  redacted session layout without a second Android layout call. This preserves
  the agent-facing verification workflow while avoiding duplicate device I/O.
- If the agent asks unlabeled `minimap whereami` immediately after a verified
  observation, Minimap can return the fresh session place without another layout
  call. Labeled `whereami --label ...` still observes live layout because it can
  mutate the graph.
- Minimap observes immediately after an action and only waits/retries when the
  observed layout is unusable or unchanged from the pre-action place.
- For multi-edge routes, Minimap can become wall-clock faster because it uses
  one initial layout plus one verification layout per edge, while raw
  rediscovery typically needs discovery and verification layout calls at every
  step.

## Candidate Compose Samples

- `Jetsnack` (`com.example.jetsnack`): bottom navigation and snack detail.
- `JetNews` (`com.example.jetnews`): drawer navigation and scroll-to-post.
- `Jetchat` (`com.example.compose.jetchat`): drawer/profile/back navigation.

## Jetsnack Smoke: 2026-05-27

Setup:

- Installed Jetsnack with:
  `env ANDROID_HOME=/Users/mmckenna/Library/Android/sdk ANDROID_SDK_ROOT=/Users/mmckenna/Library/Android/sdk ./gradlew :app:installDebug`
- Launched with `adb shell monkey -p com.example.jetsnack 1`.
- Initialized the temporary sample graph with
  `/Users/mmckenna/.cargo/bin/minimap init --force --no-skills`.

First learning path:

- `whereami --label home` created `place_home.json`.
- `tap --selector content_desc=SEARCH --label search --reason "open bottom navigation search"`
  created `place_search.json` and
  `edge_home_search_tap_content_desc_SEARCH.json`.
- `go search` from a fresh Home launch replayed the edge and returned
  `changed_graph: false`.

Second learning path:

- `tap --selector content_desc="MY CART" --label cart --reason "open bottom navigation cart"`
  created `place_cart.json` and
  `edge_home_cart_tap_content_desc_MY_CART.json`.
- `go cart` from a fresh Home launch replayed the edge and returned
  `changed_graph: false`.

Third learning path:

- Raw baseline used `PROFILE` from the Home layout at `[971,2263]`.
- `tap --selector content_desc=PROFILE --label profile --reason "open bottom navigation profile"`
  created `place_profile.json` and
  `edge_home_profile_tap_content_desc_PROFILE.json`.
- `go profile` from a fresh Home launch replayed the edge and returned
  `changed_graph: false`.

Fourth learning path:

- Raw baseline used the visible text `Chips` from the Home layout at
  `[222,1737]`.
- `tap --selector text=Chips --label chips-detail --reason "open chips detail"`
  created `place_chips-detail.json` and
  `edge_home_chips-detail_tap_text_Chips.json`.
- `go chips-detail` from a fresh Home launch replayed the edge and returned
  `changed_graph: false`.

Return path:

- `back` from Chips detail recorded `edge_chips-detail_home_press_back.json`.
- `go home` from Chips detail replayed the Back edge and returned
  `changed_graph: false`.

Observed graph shape:

```text
.minimap/config.json
.minimap/graph/places/place_home.json
.minimap/graph/places/place_search.json
.minimap/graph/places/place_cart.json
.minimap/graph/places/place_profile.json
.minimap/graph/places/place_chips-detail.json
.minimap/graph/edges/edge_home_search_tap_content_desc_SEARCH.json
.minimap/graph/edges/edge_home_cart_tap_content_desc_MY_CART.json
.minimap/graph/edges/edge_home_profile_tap_content_desc_PROFILE.json
.minimap/graph/edges/edge_home_chips-detail_tap_text_Chips.json
.minimap/graph/edges/edge_chips-detail_home_press_back.json
```

The edge files are compact and reviewable. Example recipe:

```json
{
  "kind": "tap",
  "selector": {
    "kind": "content_desc",
    "value": "SEARCH"
  }
}
```

Benchmark signal:

- Raw `layout` output for Home was large and included dozens of nodes. The
  `go` result was a compact JSON plan/execution result.
- Wall-clock time is not yet the win because Minimap still calls layout before
  and after navigation for verification. The near-term win is less agent
  rediscovery: the agent does not need to inspect full layout JSON or reason
  through bottom navigation again.
- The initial smoke exposed two important correctness issues:
  1. A post-tap layout can be temporarily blank/sparse during Compose
     transition. Minimap now retries once when the first post-action fingerprint
     is unusable.
  2. Reaching an existing place with a changed fingerprint must not overwrite
     the place baseline. Minimap now preserves the baseline and adds a variant
     for usable changed fingerprints.

Measured bottom-nav replay comparison:

| Path | Raw agent commands | Raw layout dumps | Raw nodes exposed | Raw bytes | Minimap commands | Minimap raw layout nodes exposed | Minimap bytes | Graph changed on replay |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Home -> Search | 3 | 2 | 82 | 6,012 | 1 | 0 | 424 | no |
| Home -> Cart | 3 | 2 | 114 | 8,089 | 1 | 0 | 418 | no |
| Home -> Profile | 3 | 2 | 60 | 4,520 | 1 | 0 | 430 | no |
| **Total** | **9** | **6** | **256** | **18,621** | **3** | **0** | **1,272** | **no** |

Improvement so far:

- Agent-visible command reduction: `66.7%` (`9 -> 3`).
- Agent-visible raw layout dump reduction: `100%` (`6 -> 0`).
- Agent-visible layout node exposure reduction: `100%` (`256 -> 0`).
- Agent-visible output byte reduction: `93.2%` (`18,621 -> 1,272`).
- Output size multiplier: raw rediscovery exposed `14.6x` more bytes than
  Minimap replay.

Measured detail replay comparison:

| Path | Raw agent commands | Raw layout dumps | Raw nodes exposed | Raw bytes | Minimap commands | Minimap raw layout nodes exposed | Minimap bytes | Graph changed on replay |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Home -> Chips detail | 3 | 2 | 73 | 6,104 | 1 | 0 | 430 | no |

Measured return-path replay comparison:

| Path | Raw agent commands | Raw layout dumps | Raw nodes exposed | Raw bytes | Minimap commands | Minimap raw layout nodes exposed | Minimap bytes | Graph changed on replay |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Chips detail -> Home | 2 | 1 | 49 | 3,665 | 1 | 0 | 406 | no |

Combined Jetsnack replay comparison:

| Set | Raw agent commands | Raw layout dumps | Raw nodes exposed | Raw bytes | Minimap commands | Minimap raw layout nodes exposed | Minimap bytes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Bottom nav + detail + return | 14 | 9 | 378 | 28,390 | 5 | 0 | 2,108 |

Combined improvement:

- Agent-visible command reduction: `64.3%` (`14 -> 5`).
- Agent-visible raw layout dump reduction: `100%` (`9 -> 0`).
- Agent-visible layout node exposure reduction: `100%` (`378 -> 0`).
- Agent-visible output byte reduction: `92.6%` (`28,390 -> 2,108`).
- Output size multiplier: raw rediscovery exposed `13.5x` more bytes than
  Minimap replay.

Measured wall-clock comparison:

| Path | Raw wall-clock | Minimap before replay optimization | Minimap after replay optimization | Minimap with verified session | Current speed delta |
| --- | ---: | ---: | ---: | ---: | ---: |
| Home -> Search | 7.66s | 19.16s | 8.91s | 4.27s | 44.3% faster |
| Home -> Chips detail | 7.79s | not measured | 9.74s | not measured | not measured |

The replay optimization removed one redundant layout call from selector-based
`go`, taking Home -> Search from `19.16s` to `9.13s` in the measured run. The
adaptive settle change then brought the same path to `8.91s`. That is a `53.5%`
improvement inside Minimap replay, but not yet faster than raw
single-step navigation. The verified-session fast path then brought Home ->
Search to `4.43s`. With fresh `layout` cache reuse, the measured `go search`
followed by `layout` path is `4.27s`, making the full navigation-plus-layout
verification loop `44.3%` faster than the raw baseline.

Current wall-clock conclusion:

- Minimap already improves agent-visible navigation work.
- Minimap improves wall-clock when the current place is already known from a
  verified session.
- Fresh layout cache reuse matters because agents often navigate and then ask
  for layout to validate product state. Without cache reuse, Minimap would
  verify the edge internally and then pay the same layout cost again for the
  agent-facing assertion step.
- Cold-start `go` remains close to raw because Android layout capture dominates.
- Next speed work should target Android layout latency, because layout capture
  dominates every measured path.

Measured multi-step wall-clock comparison:

| Path | Raw agent commands | Raw layout dumps | Raw nodes exposed | Raw bytes | Raw wall-clock | Minimap commands | Minimap bytes | Minimap wall-clock | Speed delta |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Chips detail -> Home -> Cart | 5 | 3 | 138 | 10,531 | 12.12s | 2 | 8,917 | 8.84s | 27.1% faster |
| Home -> scroll -> Chips detail | 5 | 3 | 117 | 9,305 | 11.96s | 1 | 424 | 13.77s | 15.1% slower |

The two-edge route is now faster when it starts from verified session state.
The newer measurement includes the common verification pattern `go cart` then
`layout`; `layout` is served from the fresh session cache. One raw run in the
same pass failed to reach Cart reliably, while both Minimap runs succeeded,
which is an important part of the value proposition: graph replay removes
coordinate timing guesses from the agent loop. The scroll-plus-tap route remains
slower because it still performs a relatively expensive swipe plus verification
layout and uses a geometry tap.

Complex-path implementation improvements found during benchmarking:

- Selector replay now reuses the current `whereami` layout instead of issuing an
  extra layout call.
- Post-action verification now observes immediately and retries only when the
  result is unusable or unchanged.
- Scroll distance was increased because the original default swipe was too
  short for Jetsnack's feed.
- A pending scroll can now feed the next labelled tap even when the scrolled
  intermediate layout is not recognized as a committed place. This enables
  scroll-plus-tap recipes without committing scroll offsets as places.
- A verified session place is now stored outside the repo with a redacted layout
  snapshot, allowing the next `go` to skip current-place rediscovery and first
  selector resolution.
- `layout` now reuses a very fresh verified session layout, reducing the common
  `go` then `layout` verification loop from two Android layout calls to one.
- Unlabeled `whereami` now reuses a very fresh verified session place, reducing
  `go` then `whereami` orientation checks to zero additional Android layout
  calls. Targeted Jetsnack check after compaction: cached `whereami` after
  `go search` returned in `0.019s`, `604` bytes, and `layout_calls_total: 0`.
- Live sub-agent data showed that a `5s` cache TTL was too short for real agent
  thinking between `go` and `layout`; the first Minimap sub-agent replay missed
  the cache despite immediately following the intended workflow. The TTL is now
  `30s`, still short-lived but long enough for a normal agent decision step.
- Plain known-place `whereami` no longer returns `fingerprint_summary`, saving
  tokens on orientation checks while preserving the summary for unknown, changed,
  and label-mismatch states where the agent actually needs it.

## Sub-Agent Benchmark: 2026-05-27

Purpose: check whether independent agents actually use Minimap better, not just
whether scripted commands look better.

Instrumentation:

- Added temporary PATH shims outside the repo at
  `/private/tmp/minimap-agent-bench/bin`.
- Logs are JSONL under `/private/tmp/minimap-agent-bench/logs`.
- The shims record command argv, status, wall-clock seconds, stdout bytes, and
  stderr bytes for `android`, `adb`, and `minimap`.
- The shims intentionally live outside `.minimap`; they are benchmark tooling,
  not product state.

Sub-agent work split:

- Protocol agent produced the raw/learning/cold-replay/hot-replay benchmark
  arms and controls.
- Task-selection agent proposed Jetsnack, JetNews, and Jetchat task matrix.
- Metrics agent independently recommended the same lightweight wrapper approach.
- Live raw and Minimap agents then ran serialized trials on the same emulator.

Clean live task:

```text
App: Jetsnack
Start: chips-detail
Target: cart
Verification: Cart layout contains Order/Checkout text
```

Clean raw-agent sample: `raw_chips_to_cart_002` through
`raw_chips_to_cart_005`

| Metric | Value |
| --- | ---: |
| Success | 4/4 |
| Median agent-visible commands | 5 |
| Median agent-visible raw layout dumps | 3 |
| Median ADB input actions | 2 |
| Median visible tool wall-clock | 10.45s |
| Median agent-visible stdout bytes | 10,531 |
| Final verification | `Order (3 items)`, `Summary`, `Total`, `Checkout` |

Raw command path:

```text
android layout
adb shell input tap 90 137
android layout
adb shell input tap 755 2263
android layout
```

The raw agent had to inspect detail layout, infer Back, inspect Home layout,
find the Cart tab coordinate, tap it, then inspect Cart layout.

Clean Minimap-agent sample after TTL tuning: `minimap_chips_to_cart_002`
through `minimap_chips_to_cart_005`

| Metric | Value |
| --- | ---: |
| Success | 4/4 |
| Median agent-visible commands | 2 |
| Median agent-visible raw layout dumps | 0 |
| Median Minimap commands | 2 |
| Median visible tool wall-clock | 8.49s |
| Median agent-visible stdout bytes | 8,918 |
| `go.start_source` | `session` on all clean trials |
| `layout.cache.hit` | true on all clean trials |
| `layout.metrics.layout_calls_total` | 0 on all clean trials |
| Final verification | `Order (3 items)`, `Summary`, `Total`, `Checkout` |

Minimap command path:

```text
minimap go cart
minimap layout
```

The Minimap agent did not reason through Back or bottom-nav coordinates. It
used the known graph path `chips-detail -> home -> cart`, then reused the fresh
verified session layout for agent-facing verification.

Sub-agent comparison:

- Visible command reduction: `60%` (`5 -> 2`).
- Agent-visible raw Android layout dump reduction: `100%` (`3 -> 0`).
- Median visible tool wall-clock reduction: `18.8%` (`10.45s -> 8.49s`).
- Median agent-visible stdout byte reduction: `15.3%` (`10,531 -> 8,918`).
- Navigation reasoning reduction: raw had to infer Back plus Cart coordinate;
  Minimap consumed the graph path directly.
- Business verification still returns layout-sized output when the agent needs
  to assert screen content. That is expected: Minimap should remove navigation
  rediscovery, not hide the UI state needed for product assertions.

Important product finding:

- The first Minimap sub-agent trial used the correct `go` then `layout` loop but
  missed a `5s` layout cache because model thinking time exceeded the TTL. This
  was not visible in scripted benchmarks. The TTL was increased to `30s`, after
  which the same sub-agent pattern produced `cache.hit: true` and
  `layout_calls_total: 0`.
- A `whereami --label <existing>` call on an unknown/blank layout no longer
  claims the app is at that existing place, which prevents transition frames
  from polluting session state.

## Benchmark Plan

Use a manual, repeatable matrix before adding automation:

1. Jetsnack bottom navigation:
   Home -> Search, Home -> Cart, Home -> Profile.
2. Jetsnack detail navigation:
   Home -> Chips detail, then Back/up to Home.
3. JetNews drawer navigation:
   Home -> Interests through drawer.
4. JetNews scroll plus tap:
   Home -> visible/scroll-target post detail.
5. Jetchat drawer/profile/back:
   Conversation -> drawer -> profile -> Back.

For each path, capture:

- Learning commands used.
- Replay command used.
- Number of graph files changed.
- Whether replay returns `changed_graph: false`.
- Whether the agent needed raw `layout` output during replay.
- Any blocker status and recovery action.

Acceptance bar for this phase:

- Known selector paths replay reliably on the same emulator/device.
- Replay returns compact JSON without requiring the agent to inspect raw layout.
- Graph diffs remain limited to one place file for a new place and one edge file
  for a new transition, except when a legitimate place variant is observed.
- Unknown or unstable destinations do not corrupt existing places.

For active development scenarios where the app itself changed, use
[MINIMAP_CHANGE_BENCHMARK_PROTOCOL.md](MINIMAP_CHANGE_BENCHMARK_PROTOCOL.md).
That protocol standardizes new options, new screens, changed destination
layouts, broken selectors, and removed options separately from known-path
replay.

## Controlled Change Smoke

Run: `/private/tmp/minimap-change-bench/runs/20260528-134010`

Results file:
`/private/tmp/minimap-change-bench/runs/20260528-134010/change-smoke-results.json`

This was a deterministic smoke against the installed Minimap CLI using fake
`android layout` and fake `adb` commands. It does not replace the real changed
Compose-sample benchmark, but it verifies the change protocol against the
actual command behavior and graph writes.

| Case | Minimap result | Graph result |
| --- | --- | --- |
| Existing place grew | `whereami --label home` returned `known_changed` | `place_home.json` gained 1 variant; no edge churn |
| New option opens new screen | first tap returned `needs_label`; `whereami --label beta-settings` committed it | new `place_beta-settings.json` and `edge_home_beta-settings_tap_text_Beta.json` |
| Known route, destination changed | `go cart` succeeded | `place_cart.json` gained 1 variant; existing edge reused |
| Known selector renamed | `go chips-detail` returned `config_error` for `text=Chips`; repaired with `text=Potato Chips` | `place_home.json` gained 1 variant; new replacement edge added; old edge retained |
| Known option removed | `go chips-detail` returned `config_error` for `text=Chips` | `place_home.json` gained 1 variant; no new edge |

Product findings from the smoke:

- Cold-change cases need the temp Minimap session cleared, otherwise the fresh
  session cache can correctly optimize known replay but hide a changed starting
  layout during a benchmark window.
- Destination growth is handled well: Minimap reuses the route and records the
  destination variant.
- New paths are handled safely: unknown destination without a label produces
  `needs_label`, then the label commit creates one place and one edge.
- Broken selectors are surfaced as `config_error`. The agent can repair by
  using current layout evidence and recording a new edge, but this path should
  be measured separately from successful known-route replay.

## Talk/Post Narrative

Potential frame:

1. Humans remember app navigation; agents forget every session.
2. First pass is intentionally expensive because the agent has to inspect and
   reason over Android layout.
3. Minimap converts successful navigation into a small graph.
4. Second pass becomes graph replay plus verification.
5. The source of truth is git-reviewable `.minimap/graph`, not a hidden cache.
