---
name: minimap-app-navigation
description: Use in an Android codebase for app navigation with Minimap. Minimap records proven navigation paths as a repo graph so agents can reuse them. Prefer minimap whereami/go/tap/scroll/back before raw android layout or adb commands.
metadata:
  author: minimap
  version: "2.0"
---

# Minimap App Navigation

Minimap is this repo's Android navigation memory for agents. It stores only
verified places and transitions under `.minimap/graph`.

Prerequisites: this skill needs the `minimap`, `android`, and `adb` binaries on
`PATH`. Claude Code plugins cannot install binaries, so if `minimap --version`
fails, ask the user to install minimap before continuing. If more than one
device or emulator is attached, pass `--device <SERIAL>` (or set
`ANDROID_SERIAL`) so Minimap targets a single device.

Use this command loop:

```bash
minimap whereami
minimap go <label>
minimap tap --selector "<kind>=<value>" --label <destination> --reason "<intent>"
minimap scroll --direction down
minimap back
```

Rules:

- `go <label>` follows known UI paths and verifies each transition.
- Unlabeled `whereami` may reuse very fresh verified session state for cheap orientation.
- `tap --label <destination>` labels the post-tap destination.
- Unknown destinations without `--label` are not committed.
- If a `--label` slug collides with a different existing place, `whereami`/`tap` return `label_mismatch` and write nothing. Sibling screens that should share one item-agnostic place will match by fingerprint and merge automatically; only pass `--allow-duplicate-label` to deliberately keep two places with the same label, which appends the smallest free numeric suffix (e.g. `account-settings-2`).
- `layout` is the raw Android layout escape hatch for business verification or finding selectors. Immediately after a verified Minimap observation, it may reuse the fresh session layout instead of calling Android layout again.
- Do not use removed workflows: observe, learn, map, route, screen, accept, repair, validate, undo.
- Review graph changes through normal git diff/PR review.

Selector preference: test tag, resource id, content description, stable visible text. Use points or screenshot labels only when selectors are not available; those edges are viewport-guarded and fragile.
