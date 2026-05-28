# Minimap Change Benchmark Protocol

This protocol measures how Minimap behaves when the app is actively changing:
new options, new screens, changed destination layouts, and broken known routes.
It is separate from known-path replay benchmarks.

Known-path replay asks: can the agent reuse a route it already knows?

Change benchmarking asks: can the agent extend or heal the map with minimal
rediscovery, minimal graph churn, and clear failure behavior?

## Benchmark Arms

Run the same task with two independent agents:

1. Raw Android agent
   - May use `android layout`, screenshots, and `adb shell input`.
   - Must not use Minimap.
   - Measures what the agent has to rediscover manually.

2. Minimap change agent
   - Must use Minimap first: `whereami`, `layout`, `tap`, `scroll`, `back`,
     and `go`.
   - May use raw Android only when Minimap cannot proceed.
   - Measures how much Minimap helps during map growth or repair.

Do not let the agents see each other's transcript during the run.

## Controls

Each case uses two app builds or states:

- Baseline: graph was seeded from this version.
- Candidate: changed app under test.

For every trial:

- Same emulator/device, viewport, locale, font scale, orientation, and app data.
- Same start place.
- Same graph snapshot before each candidate trial.
- Clear temp Minimap session for cold-change cases.
- Seed a verified session for hot-change cases only when the case explicitly
  says so.
- Record command logs with the benchmark wrapper or equivalent JSONL command
  telemetry outside `.minimap`.

## Metrics

Collect these for each trial:

- Success: target reached or change classified correctly.
- Agent-visible commands.
- Agent-visible raw Android layout dumps.
- Agent-visible stdout/stderr bytes.
- Tool wall-clock.
- ADB input actions.
- Minimap statuses: `known`, `known_changed`, `unknown`, `needs_label`,
  `label_mismatch`, `no_known_path`, `no_compatible_path`, `config_error`.
- `changed_graph` and `changed_files`.
- Git diff footprint: changed place files, changed edge files, deleted files.
- Whether the agent needed user confirmation.
- Whether an old baseline or edge was overwritten unexpectedly.

## Success Rules

Minimap succeeds when it helps the agent do the right graph operation with less
rediscovery and without hiding uncertainty.

Expected good outcomes:

- Existing place grew: one place file gains a variant; no edge churn.
- New destination learned: one new place file and one new edge file.
- Unknown destination without label: no graph write, `needs_label`.
- Known route reaches changed destination: destination place gains a variant.
- Broken route: no silent graph mutation; agent gets enough context to repair by
  executing and labeling a new successful action.

Expected bad outcomes:

- Existing baseline overwritten instead of adding a variant.
- Unknown screen mislabeled as an existing place.
- Edge rewritten or deleted without an observed successful replacement.
- Graph changes occur when command status is a blocker.
- Large unrelated diffs under `.minimap`.

## Case C1: Existing Place Grew

Purpose: test a screen or place that has a new option but remains the same
semantic place.

Example:

- Baseline Home has Search, Cart, Profile.
- Candidate Home adds a new bottom-nav option or a new visible card.

Start:

- Candidate app on the changed known place.
- Minimap graph from the baseline app.
- No fresh Minimap session.

Raw-agent task:

```text
Identify the current screen and report the new visible option.
Use raw Android only.
```

Minimap-agent task:

```text
Run `minimap whereami`. Report whether Minimap recognized the place, whether it
changed the graph, and which graph files changed. Then run `minimap layout` only
if needed to inspect the new option.
```

Expected Minimap outcome:

- Best case: `status: known_changed`, `changed_graph: true`.
- `changed_files`: exactly one existing place file.
- Place baseline remains; new fingerprint appears under `variants`.
- No edge files change.

If the changed layout falls below the matching threshold:

- Acceptable: `status: unknown`, no graph write.
- The agent should not force `whereami --label <existing>` without explicit
  confirmation, because that can conflate a real new place with an evolved old
  place.

Primary score:

- Did Minimap classify the changed place safely?
- Was the graph diff limited to one place variant?
- How many raw layouts did the agent need before knowing what changed?

## Case C2: New Option Opens New Screen

Purpose: test active development where a new control opens a new destination.

Example:

- Candidate Home or Settings has a new option, `Beta settings`.
- Baseline graph has no destination for it.

Start:

- Candidate app on the source place.
- Minimap graph from baseline app.

Raw-agent task:

```text
Find the new option, open it, and verify the destination screen.
Use raw Android only.
```

Minimap-agent task:

```text
Use Minimap to orient, inspect the layout if needed, tap the new option with
`--label <new-place>`, and report graph changes.
```

Expected Minimap command shape:

```text
minimap whereami
minimap layout
minimap tap --selector "<stable-selector>" --label <new-place> --reason "<intent>"
```

Expected Minimap outcome:

- `tap` returns `status: ok`.
- `changed_graph: true`.
- `changed_files`: exactly one new place file and one new edge file.
- Existing source place may also gain one variant if it changed; that is
  acceptable only when the source layout actually changed.

Safety subcase:

```text
minimap tap --selector "<stable-selector>" --reason "<intent>"
```

Expected:

- If destination is unknown and no label is provided, return `needs_label`.
- No graph write.

Primary score:

- Commands to learn the new route.
- Graph diff size.
- Whether the agent needed raw Android after Minimap orientation.
- Whether replay works immediately after learning:
  `minimap go <new-place>` from the source place.

## Case C3: Known Route, Destination Changed

Purpose: test a route that still works, but the destination screen grew or
changed.

Example:

- Existing `home -> cart` route still opens Cart.
- Candidate Cart adds a discount code field.

Start:

- Candidate app on the known source place.
- Baseline graph has the old route and old destination fingerprint.

Raw-agent task:

```text
Navigate to the target and verify the new destination content.
Use raw Android only.
```

Minimap-agent task:

```text
Run `minimap go <target>`, then inspect the result. Report whether Minimap
reused the route, whether the destination place changed, and which graph files
changed.
```

Expected Minimap outcome:

- `go` returns `status: ok`.
- `planned_path` is the known route.
- If the new destination still matches the known place:
  `changed_graph: true` with exactly one destination place file changed.
- No edge files change.
- Follow-up `minimap layout` can verify the new content.

Primary score:

- Route reuse succeeds despite destination evolution.
- Diff is one place variant, not an edge rewrite.
- Agent reaches business verification faster than raw.

## Case C4: Known Route Broken By Selector Change

Purpose: test repair behavior when the old edge recipe no longer executes.

Example:

- Existing edge uses `text=Chips`.
- Candidate renames the item to `Potato chips` or changes its content
  description.

Start:

- Candidate app on known source place.
- Baseline graph has the old edge.

Raw-agent task:

```text
Find the renamed option, open the intended destination, and verify it.
Use raw Android only.
```

Minimap-agent task:

```text
Try `minimap go <target>`. If it cannot execute the old edge, inspect with
`minimap layout`, find the new selector, and record a new successful action to
the same destination label.
```

Expected Minimap outcome:

- Initial `go` should not mutate the graph if the old action cannot be verified.
- Repair path should use:
  `minimap tap --selector "<new-selector>" --label <target> --reason "<intent>"`.
- Graph diff should add a new edge or update only the necessary place variant.
- Old edge should remain unless the product later defines pruning.

Primary score:

- Clarity of the initial failure.
- Commands needed to repair.
- Whether the final graph has a deterministic new edge and no unrelated churn.

## Case C5: Known Option Removed

Purpose: test when a route is no longer available and no replacement is obvious.

Start:

- Candidate app on known source place.
- Baseline graph has an edge for an option that no longer appears.

Raw-agent task:

```text
Try to reach the target. If unavailable, report that the option is missing.
Use raw Android only.
```

Minimap-agent task:

```text
Try `minimap go <target>`. If it fails, inspect with `minimap layout` and report
whether the option appears to be absent. Do not invent a replacement path.
```

Expected Minimap outcome:

- No graph mutation.
- Clear failure or blocker status.
- Agent reports the missing option with layout evidence.

Primary score:

- No false repair.
- No graph write.
- Agent-visible evidence is concise enough for a user or test to act on.

## Standard Trial Labels

Use these trial IDs so results aggregate cleanly:

```text
change_c1_place_grew_raw_001
change_c1_place_grew_minimap_001
change_c2_new_option_raw_001
change_c2_new_option_minimap_001
change_c3_destination_changed_raw_001
change_c3_destination_changed_minimap_001
change_c4_selector_changed_raw_001
change_c4_selector_changed_minimap_001
change_c5_option_removed_raw_001
change_c5_option_removed_minimap_001
```

## Sub-Agent Prompt Template

Raw agent:

```text
LIVE CHANGE BENCHMARK TRIAL: <trial-id>.
Starting state: <state>.
Goal: <goal>.
Tool policy: raw Android only. Do not use Minimap.
Use `android layout` and `adb shell input ...`.
Keep the run bounded: max <N> device commands.
Final response: success/failure, command sequence, how you decided what to tap,
final verification or missing-option evidence, and any uncertainty.
```

Minimap agent:

```text
LIVE CHANGE BENCHMARK TRIAL: <trial-id>.
Starting state: <state>.
Goal: <goal>.
Tool policy: Minimap-first. Use `whereami`, `layout`, `go`, `tap`, `scroll`,
and `back` before raw Android.
Report Minimap statuses, `changed_graph`, `changed_files`, cache/session fields,
and final verification or missing-option evidence.
Keep the run bounded: max <N> commands.
Do not force labels for unknown screens unless the trial explicitly says to
learn a new destination.
```

## Reporting Table

Use this table for each case:

| Metric | Raw | Minimap |
| --- | ---: | ---: |
| Success |  |  |
| Agent-visible commands |  |  |
| Agent-visible raw layout dumps |  |  |
| Tool wall-clock |  |  |
| Agent-visible bytes |  |  |
| ADB actions |  |  |
| Graph files changed | n/a |  |
| Place files changed | n/a |  |
| Edge files changed | n/a |  |
| Unexpected graph churn | n/a |  |
| User confirmation needed |  |  |

Add a short narrative explaining what the agent had to reason about. For active
development, the reasoning burden is often the most important signal.
