use minimap_schemas::{canonical_json, ScreenNode};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const TEXT_KEYS: &[&str] = &[
    "text",
    "label",
    "contentDescription",
    "content_description",
    "hint",
];
const SENSITIVE_KEYS: &[&str] = &[
    "password", "passwd", "secret", "auth", "token", "jwt", "session", "email", "phone", "credit",
];
const VOLATILE_KEYS: &[&str] = &[
    "timestamp",
    "time",
    "elapsedRealtime",
    "frame",
    "counter",
    "index",
    "focused",
    "selected",
    "pressed",
    "scrollX",
    "scrollY",
    "animation",
    "bounds",
    "raw_bounds",
];

#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult {
    pub status: String,
    pub matched_screen: Option<String>,
    pub match_confidence: f64,
    pub hash_matched: bool,
}

pub fn redact_layout(layout: &Value) -> Value {
    redact_value(layout, None)
}

fn redact_value(value: &Value, key: Option<&str>) -> Value {
    if key.map(is_sensitive_key).unwrap_or(false) {
        return Value::String("<redacted>".to_string());
    }
    match value {
        Value::Object(map) => {
            let mut redacted = Map::new();
            for (key, value) in map {
                redacted.insert(key.clone(), redact_value(value, Some(key)));
            }
            Value::Object(redacted)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| redact_value(value, None))
                .collect(),
        ),
        Value::String(text) if key.map(|key| TEXT_KEYS.contains(&key)).unwrap_or(false) => {
            json!({
                "redacted": true,
                "reason": sensitive_text_reason(text).unwrap_or("verbatim_text_excluded"),
                "text_class": text_class(text)
            })
        }
        Value::String(text) if sensitive_text_reason(text).is_some() => {
            json!({"redacted": true, "reason": sensitive_text_reason(text).unwrap()})
        }
        _ => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_lowercase();
    SENSITIVE_KEYS
        .iter()
        .any(|pattern| lowered.contains(pattern))
}

fn sensitive_text_reason(text: &str) -> Option<&'static str> {
    let lowered = text.to_lowercase();
    if text.contains('@') && text.contains('.') {
        Some("email")
    } else if lowered.contains("token") || lowered.contains("bearer") || lowered.starts_with("eyj")
    {
        Some("token")
    } else if text.chars().filter(|ch| ch.is_ascii_digit()).count() >= 10 {
        Some("numeric_sensitive")
    } else {
        None
    }
}

fn text_class(text: &str) -> &'static str {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "empty"
    } else if trimmed.chars().all(|ch| ch.is_ascii_digit()) {
        "numeric"
    } else if trimmed.len() <= 3 {
        "short"
    } else if trimmed.len() <= 24 {
        "medium"
    } else {
        "long"
    }
}

pub fn normalize_layout(layout: &Value) -> Value {
    let redacted = redact_layout(layout);
    let stripped = strip_volatile(&redacted);
    let mut elements = Vec::new();
    walk(&stripped, "0".to_string(), 0, &mut elements);
    let mut role_distribution = BTreeMap::<String, usize>::new();
    for element in &elements {
        if let Some(role) = element.get("role").and_then(Value::as_str) {
            *role_distribution.entry(role.to_string()).or_default() += 1;
        }
    }
    json!({
        "schema_version": "minimap.normalized_layout.v1",
        "elements": elements,
        "role_distribution": role_distribution,
        "element_count": elements.len()
    })
}

fn strip_volatile(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut stripped = Map::new();
            for (key, value) in map {
                if !VOLATILE_KEYS.contains(&key.as_str()) {
                    stripped.insert(key.clone(), strip_volatile(value));
                }
            }
            Value::Object(stripped)
        }
        Value::Array(values) => Value::Array(values.iter().map(strip_volatile).collect()),
        _ => value.clone(),
    }
}

fn walk(value: &Value, path: String, sibling_index: usize, elements: &mut Vec<Value>) {
    let Value::Object(map) = value else {
        return;
    };
    elements.push(signature_for_node(map, &path, sibling_index));
    for child_key in ["children", "nodes", "elements"] {
        if let Some(Value::Array(children)) = map.get(child_key) {
            for (index, child) in children.iter().enumerate() {
                walk(child, format!("{path}/{index}"), index, elements);
            }
            break;
        }
    }
}

fn signature_for_node(map: &Map<String, Value>, path: &str, sibling_index: usize) -> Value {
    let role = first_string(map, &["role", "class", "className", "type"]).unwrap_or("unknown");
    let mut signature = Map::new();
    signature.insert("role".to_string(), Value::String(role.to_string()));
    signature.insert(
        "clickable".to_string(),
        Value::Bool(
            map.get("clickable")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
    );
    signature.insert(
        "enabled".to_string(),
        Value::Bool(map.get("enabled").and_then(Value::as_bool).unwrap_or(true)),
    );
    signature.insert("path".to_string(), Value::String(path.to_string()));
    signature.insert(
        "sibling_bucket".to_string(),
        Value::Number((sibling_index.min(9) as u64).into()),
    );
    if let Some(resource_id) = first_string(map, &["resource_id", "resourceId", "id", "testTag"]) {
        if !is_dynamic_id(resource_id) {
            signature.insert(
                "resource_id".to_string(),
                Value::String(resource_id.to_string()),
            );
        }
    }
    if let Some(class) = text_class_value(map) {
        signature.insert("text_class".to_string(), Value::String(class));
    }
    Value::Object(signature)
}

fn first_string<'a>(map: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str))
}

fn is_dynamic_id(value: &str) -> bool {
    let lowered = value.to_lowercase();
    ["generated", "uuid", "random", "timestamp", "session"]
        .iter()
        .any(|marker| lowered.contains(marker))
}

fn text_class_value(map: &Map<String, Value>) -> Option<String> {
    for key in TEXT_KEYS {
        match map.get(*key) {
            Some(Value::String(text)) => return Some(text_class(text).to_string()),
            Some(Value::Object(text))
                if text
                    .get("redacted")
                    .and_then(Value::as_bool)
                    .unwrap_or(false) =>
            {
                return Some(
                    text.get("text_class")
                        .and_then(Value::as_str)
                        .unwrap_or("redacted")
                        .to_string(),
                );
            }
            _ => {}
        }
    }
    None
}

pub fn identity_hash(normalized: &Value) -> String {
    let digest = Sha256::digest(canonical_json(normalized).as_bytes());
    format!("sha256:{digest:x}")
}

pub fn match_screen(normalized: &Value, screens: impl Iterator<Item = ScreenNode>) -> MatchResult {
    let current_hash = identity_hash(normalized);
    let mut best: Option<(String, f64)> = None;
    for screen in screens {
        if screen.identity_hash == current_hash {
            return MatchResult {
                status: "matched".to_string(),
                matched_screen: Some(screen.id),
                match_confidence: 1.0,
                hash_matched: true,
            };
        }
        if let Some(baseline) = screen.normalized.as_ref() {
            let score = similarity(normalized, baseline);
            if best
                .as_ref()
                .map(|(_, best_score)| score > *best_score)
                .unwrap_or(true)
            {
                best = Some((screen.id, score));
            }
        }
    }
    match best {
        Some((screen, score)) if score >= 0.78 => MatchResult {
            status: "matched".to_string(),
            matched_screen: Some(screen),
            match_confidence: score,
            hash_matched: false,
        },
        Some((screen, score)) if score >= 0.65 => MatchResult {
            status: "repair_candidate".to_string(),
            matched_screen: Some(screen),
            match_confidence: score,
            hash_matched: false,
        },
        Some((screen, score)) => MatchResult {
            status: "screen_unknown".to_string(),
            matched_screen: Some(screen),
            match_confidence: score,
            hash_matched: false,
        },
        None => MatchResult {
            status: "screen_unknown".to_string(),
            matched_screen: None,
            match_confidence: 0.0,
            hash_matched: false,
        },
    }
}

fn similarity(left: &Value, right: &Value) -> f64 {
    let element_score = jaccard(element_keys(left), element_keys(right));
    let role_score = jaccard(role_keys(left), role_keys(right));
    0.75 * element_score + 0.25 * role_score
}

fn element_keys(value: &Value) -> BTreeSet<String> {
    value
        .get("elements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(|element| {
            format!(
                "{}|{}|{}|{}|{}",
                element.get("role").and_then(Value::as_str).unwrap_or(""),
                element
                    .get("resource_id")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                element
                    .get("text_class")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                element
                    .get("clickable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                element
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true)
            )
        })
        .collect()
}

fn role_keys(value: &Value) -> BTreeSet<String> {
    value
        .get("role_distribution")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|roles| roles.iter())
        .map(|(role, count)| format!("{role}:{count}"))
        .collect()
}

fn jaccard(left: BTreeSet<String>, right: BTreeSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let intersection = left.intersection(&right).count() as f64;
    let union = left.union(&right).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_before_normalizing_and_hashing() {
        let first =
            json!({"class":"Column","children":[{"class":"Text","text":"alice@example.com"}]});
        let second =
            json!({"class":"Column","children":[{"class":"Text","text":"bob@example.com"}]});
        let first_normalized = normalize_layout(&first);
        let second_normalized = normalize_layout(&second);
        assert_eq!(
            identity_hash(&first_normalized),
            identity_hash(&second_normalized)
        );
        assert!(!canonical_json(&first_normalized).contains("alice@example.com"));
    }

    #[test]
    fn matches_similar_screen() {
        let normalized = normalize_layout(&json!({
            "class": "Column",
            "children": [{"class":"Button","testTag":"settings","clickable":true}]
        }));
        let screen = ScreenNode {
            schema_version: minimap_schemas::SCREEN_SCHEMA_VERSION.to_string(),
            id: "screen_settings".to_string(),
            name: "settings".to_string(),
            identity_hash: "sha256:not-fast-path".to_string(),
            context_guard: Default::default(),
            aliases: vec![],
            match_profile: Value::Null,
            checks: vec![],
            source: Value::Null,
            normalized: Some(normalized.clone()),
        };
        let result = match_screen(&normalized, vec![screen].into_iter());
        assert_eq!(result.status, "matched");
        assert_eq!(result.matched_screen.as_deref(), Some("screen_settings"));
    }
}
