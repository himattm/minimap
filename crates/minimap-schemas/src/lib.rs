use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const CONFIG_SCHEMA_VERSION: &str = "minimap.config.v1";
pub const SCREEN_SCHEMA_VERSION: &str = "minimap.screen.v1";
pub const EDGE_SCHEMA_VERSION: &str = "minimap.edge.v1";
pub const ROUTE_SCHEMA_VERSION: &str = "minimap.route.v1";
pub const PROPOSAL_SCHEMA_VERSION: &str = "minimap.proposal.v1";
pub const RESULT_SCHEMA_VERSION: &str = "minimap.result.v1";
pub const JOURNAL_ENTRY_SCHEMA_VERSION: &str = "minimap.journal_entry.v1";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct GraphContext(pub BTreeMap<String, Value>);

impl GraphContext {
    pub fn satisfies(&self, guard: &BTreeMap<String, Value>) -> bool {
        self.mismatches(guard).is_empty()
    }

    pub fn mismatches(&self, guard: &BTreeMap<String, Value>) -> Vec<String> {
        guard
            .iter()
            .filter_map(|(key, required)| {
                if value_matches(self.0.get(key), required) {
                    None
                } else {
                    Some(key.clone())
                }
            })
            .collect()
    }
}

fn value_matches(current: Option<&Value>, required: &Value) -> bool {
    if required.is_null() || required == "any" {
        return true;
    }
    match required {
        Value::Array(values) => values.iter().any(|value| value_matches(current, value)),
        Value::String(required) if required.ends_with("_or_unknown") => {
            let base = required.trim_end_matches("_or_unknown");
            current == Some(&Value::String(base.to_string()))
                || current == Some(&Value::String("unknown".to_string()))
        }
        Value::String(required) if required == "unknown" => {
            current == Some(&Value::String("unknown".to_string()))
        }
        _ => current == Some(required),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenNode {
    pub schema_version: String,
    /// Stable handle (`screen_<hash8>`); edges reference this. Never renamed.
    pub id: String,
    /// Best-effort auto-derived display name. Mutable — `screen rename` rewrites it.
    pub name: String,
    pub identity_hash: String,
    #[serde(default)]
    pub context_guard: BTreeMap<String, Value>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub match_profile: Value,
    #[serde(default)]
    pub checks: Vec<Value>,
    #[serde(default)]
    pub source: Value,
    #[serde(default)]
    pub normalized: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TapRecipe {
    #[serde(default = "tap_kind")]
    pub kind: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub selector_candidates: Vec<SelectorCandidate>,
    #[serde(default)]
    pub tap_cache: Option<Value>,
}

fn tap_kind() -> String {
    "tap".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelectorCandidate {
    pub kind: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NavigationEdge {
    pub schema_version: String,
    pub id: String,
    /// Stable `ScreenNode.id` reference (e.g. `screen_<hash8>`). Never a display name.
    pub from_screen: String,
    /// Stable `ScreenNode.id` reference (e.g. `screen_<hash8>`). Never a display name.
    pub to_screen: String,
    pub action: TapRecipe,
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub context_guard: BTreeMap<String, Value>,
    #[serde(default)]
    pub expectations: Vec<Value>,
    #[serde(default)]
    pub learned_from: Value,
    #[serde(default)]
    pub confidence_model: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Route {
    pub schema_version: String,
    pub name: String,
    pub target: Value,
    #[serde(default)]
    pub from: Option<Value>,
    #[serde(default)]
    pub triggers: Vec<Value>,
    #[serde(default)]
    pub aliases: Vec<String>,
}

impl Route {
    pub fn target_screen(&self) -> Option<&str> {
        self.target.get("screen").and_then(Value::as_str)
    }

    pub fn from_screen(&self) -> Option<&str> {
        self.from.as_ref()?.get("screen").and_then(Value::as_str)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JournalEntry {
    pub schema_version: String,
    pub ts: i64,
    #[serde(default)]
    pub from_screen_id: Option<String>,
    #[serde(default)]
    pub edge_id: Option<String>,
    #[serde(default)]
    pub to_screen_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// One of the documented outcome strings emitted by atomic tap:
    /// `matched` | `new_screen` | `drift_staged` | `coord_journal_only` | `tap_failed`
    /// | `from_screen_unknown`. `from_screen_unknown` means the pre-tap layout did
    /// not match any committed screen, so the tap was executed but no edge was
    /// grown — re-run `minimap layout` (or commit the current screen) to get oriented.
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Proposal {
    pub schema_version: String,
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub changes: Vec<Value>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MinimapResult {
    pub schema_version: String,
    pub status: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub recommended_action: Option<String>,
}

impl MinimapResult {
    pub fn ok(summary: impl Into<String>, data: Value) -> Self {
        Self {
            schema_version: RESULT_SCHEMA_VERSION.to_string(),
            status: "ok".to_string(),
            summary: Some(summary.into()),
            data,
            recommended_action: None,
        }
    }

    pub fn context_mismatch(mismatches: Vec<String>) -> Self {
        Self {
            schema_version: RESULT_SCHEMA_VERSION.to_string(),
            status: "context_mismatch".to_string(),
            summary: Some("current context does not satisfy the graph guard".to_string()),
            data: serde_json::json!({ "mismatches": mismatches }),
            recommended_action: Some(
                "establish required context or choose another route variant".to_string(),
            ),
        }
    }
}

pub fn require_schema(value: &Value, expected: &str) -> Result<()> {
    let actual = value.get("schema_version").and_then(Value::as_str);
    if actual == Some(expected) {
        Ok(())
    } else {
        bail!("unsupported schema_version: expected {expected}, got {actual:?}")
    }
}

pub fn canonical_json(value: &Value) -> String {
    let sorted = sort_json(value);
    let mut output = serde_json::to_string_pretty(&sorted).expect("canonical JSON serialization");
    output.push('\n');
    output
}

pub fn sort_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<_> = map.keys().collect();
            keys.sort();
            for key in keys {
                sorted.insert(key.clone(), sort_json(&map[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(values) => Value::Array(values.iter().map(sort_json).collect()),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_guard_accepts_or_unknown() {
        let context = GraphContext(BTreeMap::from([(
            "auth_state".to_string(),
            Value::String("unknown".to_string()),
        )]));
        let guard = BTreeMap::from([(
            "auth_state".to_string(),
            Value::String("logged_in_or_unknown".to_string()),
        )]);
        assert!(context.satisfies(&guard));
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let value = serde_json::json!({"z": 1, "a": {"b": 2, "a": 1}});
        assert_eq!(
            canonical_json(&value),
            "{\n  \"a\": {\n    \"a\": 1,\n    \"b\": 2\n  },\n  \"z\": 1\n}\n"
        );
    }
}
