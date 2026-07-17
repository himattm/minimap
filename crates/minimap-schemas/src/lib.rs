use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub const CONFIG_SCHEMA_VERSION: &str = "minimap.config.v2";
pub const PLACE_SCHEMA_VERSION: &str = "minimap.place.v1";
pub const EDGE_SCHEMA_VERSION: &str = "minimap.edge.v1";
pub const RESULT_SCHEMA_VERSION: &str = "minimap.result.v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Viewport {
    pub width: i64,
    pub height: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct Point {
    pub x: i64,
    pub y: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppProfile {
    #[serde(default)]
    pub android_package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android_device: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MinimapConfig {
    pub schema_version: String,
    pub active_app_profile: String,
    pub app_profiles: BTreeMap<String, AppProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct Selector {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct StaticText {
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fingerprint {
    #[serde(default)]
    pub selectors: Vec<Selector>,
    #[serde(default)]
    pub static_text: Vec<StaticText>,
    #[serde(default)]
    pub roles: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaceBaseline {
    pub identity_hash: String,
    pub fingerprint: Fingerprint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Place {
    pub schema_version: String,
    pub id: String,
    pub slug: String,
    pub label: String,
    pub baseline: PlaceBaseline,
    #[serde(default)]
    pub variants: Vec<PlaceBaseline>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EdgeEndpoint {
    pub id: String,
    pub slug: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActionStep {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<Selector>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<Point>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

impl ActionStep {
    pub fn is_geometry(&self) -> bool {
        self.point.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Edge {
    pub schema_version: String,
    pub id: String,
    pub from: EdgeEndpoint,
    pub to: EdgeEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    pub recipe: Vec<ActionStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MinimapResult {
    pub schema_version: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_action: Option<String>,
}

impl MinimapResult {
    pub fn new(status: impl Into<String>, summary: impl Into<String>, data: Value) -> Self {
        Self {
            schema_version: RESULT_SCHEMA_VERSION.to_string(),
            status: status.into(),
            summary: Some(summary.into()),
            data,
            recommended_action: None,
        }
    }

    pub fn with_recommendation(mut self, recommendation: impl Into<String>) -> Self {
        self.recommended_action = Some(recommendation.into());
        self
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
    fn canonical_json_sorts_keys() {
        let value = serde_json::json!({"z": 1, "a": {"b": 2, "a": 1}});
        assert_eq!(
            canonical_json(&value),
            "{\n  \"a\": {\n    \"a\": 1,\n    \"b\": 2\n  },\n  \"z\": 1\n}\n"
        );
    }
}
