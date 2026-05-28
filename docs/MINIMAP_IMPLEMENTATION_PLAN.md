# Minimap Implementation Plan

The active implementation target is the lean v1 design in
[MINIMAP_V1_LEAN_DESIGN.md](MINIMAP_V1_LEAN_DESIGN.md).

Minimap is now implemented as a breaking pre-1.0 Rust refactor around one narrow
goal: Android navigation memory for agents. The committed graph stores semantic
places and verified transition recipes only.

## Active Command Surface

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

Removed concepts from earlier plans are intentionally out of scope:

- observe/learn/map
- route/screen admin commands
- proposals/accept
- repair
- undo
- heavyweight validate
- always-on journal

## Implementation Priorities

1. Keep `.minimap/` minimal: config plus graph.
2. Keep JSON output stable and agent-first.
3. Persist only verified navigation facts.
4. Reject old schemas/layouts instead of carrying compatibility code.
5. Validate with Rust tests, fake Android/ADB CLI contract tests, and manual
   live smoke against `/Users/mmckenna/Dev/compose-samples`.
