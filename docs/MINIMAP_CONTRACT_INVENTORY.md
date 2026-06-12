# Minimap Lean V1 Contract Inventory

This inventory records the active lean v1 contract. Older Python/Rust 0.1
contracts are intentionally superseded by the breaking 0.2 refactor.

| Command | Status | Writes graph | Android CLI | adb |
|---|---:|---:|---:|---:|
| `minimap init` | implemented | yes, `.minimap` setup | no | no |
| `minimap doctor` | implemented | no | path check | path/device check |
| `minimap whereami` | implemented | only when labeling or self-healing known place | yes | temp-state scope only |
| `minimap layout` | implemented | no | yes | no |
| `minimap tap --selector` | implemented | when destination is known/labeled | yes | yes |
| `minimap tap --point` | implemented | when destination is known/labeled and viewport captured | yes | yes |
| `minimap tap --screenshot-label` | implemented | when destination is known/labeled and viewport captured | yes | yes |
| `minimap scroll` | implemented | only if semantic transition is known | yes | yes |
| `minimap back` | implemented | only if semantic transition is known | yes | yes |
| `minimap go` | implemented | may self-heal known destination baselines | yes | yes |

## Stable Status Vocabulary

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

Graph changes return exit code `0` with `changed_graph: true`. Blockers and
failures return nonzero.

## Removed Commands

The following commands are not part of lean v1 and should not appear in help:

```text
accept
route
screen
observe
learn
map
repair
undo
validate
```
