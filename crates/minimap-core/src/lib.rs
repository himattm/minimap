use minimap_schemas::{canonical_json, Fingerprint, Place, PlaceBaseline, Selector, StaticText};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const TEXT_KEYS: &[&str] = &[
    "text",
    "label",
    "title",
    "contentDescription",
    "content_description",
    "content-desc",
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
    "center",
];

#[derive(Debug, Clone, PartialEq)]
pub struct PlaceMatch {
    pub status: String,
    pub place_id: Option<String>,
    pub slug: Option<String>,
    pub confidence: f64,
    pub hash_matched: bool,
}

pub fn normalize_label(label: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in label.trim().chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

pub fn place_id_for_slug(slug: &str) -> String {
    format!("place_{slug}")
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
        Value::String(text) if sensitive_text_reason(text).is_some() => {
            json!({"redacted": true, "reason": sensitive_text_reason(text).unwrap()})
        }
        Value::String(text) if key.map(|key| TEXT_KEYS.contains(&key)).unwrap_or(false) => {
            if is_safe_static_text(text) {
                Value::String(text.trim().to_string())
            } else {
                json!({"redacted": true, "reason": "dynamic_or_unsafe_text"})
            }
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

pub fn is_safe_static_text(text: &str) -> bool {
    let trimmed = text.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 48
        && sensitive_text_reason(trimmed).is_none()
        && !trimmed
            .chars()
            .all(|ch| ch.is_ascii_digit() || ch.is_whitespace())
}

pub fn fingerprint_layout(layout: &Value) -> PlaceBaseline {
    let redacted = redact_layout(layout);
    let stripped = strip_volatile(&redacted);
    let mut selectors = BTreeSet::<Selector>::new();
    let mut static_text = BTreeSet::<StaticText>::new();
    let mut roles = BTreeMap::<String, usize>::new();
    walk_fingerprint(&stripped, &mut selectors, &mut static_text, &mut roles);
    let fingerprint = Fingerprint {
        selectors: selectors.into_iter().collect(),
        static_text: static_text.into_iter().collect(),
        roles,
    };
    let identity_hash = identity_hash(&fingerprint);
    PlaceBaseline {
        identity_hash,
        fingerprint,
    }
}

pub fn identity_hash(fingerprint: &Fingerprint) -> String {
    let value = serde_json::to_value(fingerprint).expect("fingerprint json");
    let digest = Sha256::digest(canonical_json(&value).as_bytes());
    format!("sha256:{digest:x}")
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

fn walk_fingerprint(
    value: &Value,
    selectors: &mut BTreeSet<Selector>,
    static_text: &mut BTreeSet<StaticText>,
    roles: &mut BTreeMap<String, usize>,
) {
    match value {
        Value::Object(map) => {
            if let Some(role) = first_string(map, &["role", "class", "className", "type"]) {
                *roles.entry(role.to_string()).or_default() += 1;
            }
            collect_selector(map, selectors);
            collect_static_text(map, static_text);
            for child_key in ["children", "nodes", "elements"] {
                if let Some(Value::Array(children)) = map.get(child_key) {
                    for child in children {
                        walk_fingerprint(child, selectors, static_text, roles);
                    }
                    return;
                }
            }
            for value in map.values() {
                if matches!(value, Value::Object(_) | Value::Array(_)) {
                    walk_fingerprint(value, selectors, static_text, roles);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                walk_fingerprint(value, selectors, static_text, roles);
            }
        }
        _ => {}
    }
}

fn collect_selector(map: &Map<String, Value>, selectors: &mut BTreeSet<Selector>) {
    for (kind, keys) in [
        ("test_tag", &["testTag", "test-tag"][..]),
        (
            "resource_id",
            &["resource-id", "resource_id", "resourceId", "id"][..],
        ),
        (
            "content_desc",
            &["content-desc", "contentDescription", "content_description"][..],
        ),
    ] {
        if let Some(value) = first_string(map, keys) {
            let trimmed = value.trim();
            if !trimmed.is_empty()
                && !is_dynamic_id(trimmed)
                && sensitive_text_reason(trimmed).is_none()
            {
                selectors.insert(Selector {
                    kind: kind.to_string(),
                    value: trimmed.to_string(),
                });
            }
        }
    }
}

fn collect_static_text(map: &Map<String, Value>, static_text: &mut BTreeSet<StaticText>) {
    for key in TEXT_KEYS {
        if let Some(Value::String(value)) = map.get(*key) {
            if is_safe_static_text(value) {
                static_text.insert(StaticText {
                    value: value.trim().to_string(),
                });
            }
        }
    }
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

pub fn fingerprint_usable(baseline: &PlaceBaseline) -> bool {
    !baseline.fingerprint.selectors.is_empty()
        || !baseline.fingerprint.static_text.is_empty()
        || !baseline.fingerprint.roles.is_empty()
}

pub fn match_place(baseline: &PlaceBaseline, places: impl Iterator<Item = Place>) -> PlaceMatch {
    let mut best: Option<(Place, f64, bool)> = None;
    for place in places {
        if place.baseline.identity_hash == baseline.identity_hash
            || place
                .variants
                .iter()
                .any(|variant| variant.identity_hash == baseline.identity_hash)
        {
            return PlaceMatch {
                status: "known".to_string(),
                place_id: Some(place.id),
                slug: Some(place.slug),
                confidence: 1.0,
                hash_matched: true,
            };
        }
        let mut score = similarity(&baseline.fingerprint, &place.baseline.fingerprint);
        let mut variant_match = false;
        for variant in &place.variants {
            let variant_score = similarity(&baseline.fingerprint, &variant.fingerprint);
            if variant_score > score {
                score = variant_score;
                variant_match = true;
            }
        }
        if best
            .as_ref()
            .map(|(_, best_score, _)| score > *best_score)
            .unwrap_or(true)
        {
            best = Some((place, score, variant_match));
        }
    }
    match best {
        Some((place, score, _)) if score >= 0.84 => PlaceMatch {
            status: "known_changed".to_string(),
            place_id: Some(place.id),
            slug: Some(place.slug),
            confidence: score,
            hash_matched: false,
        },
        Some((place, score, _)) => PlaceMatch {
            status: "unknown".to_string(),
            place_id: Some(place.id),
            slug: Some(place.slug),
            confidence: score,
            hash_matched: false,
        },
        None => PlaceMatch {
            status: "unknown".to_string(),
            place_id: None,
            slug: None,
            confidence: 0.0,
            hash_matched: false,
        },
    }
}

fn similarity(left: &Fingerprint, right: &Fingerprint) -> f64 {
    let selector_score = jaccard(
        left.selectors
            .iter()
            .map(|selector| format!("{}={}", selector.kind, selector.value))
            .collect(),
        right
            .selectors
            .iter()
            .map(|selector| format!("{}={}", selector.kind, selector.value))
            .collect(),
    );
    let text_score = jaccard(
        left.static_text
            .iter()
            .map(|text| normalize_label(&text.value))
            .filter(|value| !value.is_empty())
            .collect(),
        right
            .static_text
            .iter()
            .map(|text| normalize_label(&text.value))
            .filter(|value| !value.is_empty())
            .collect(),
    );
    let role_score = jaccard(
        left.roles
            .iter()
            .map(|(role, count)| format!("{role}:{count}"))
            .collect(),
        right
            .roles
            .iter()
            .map(|(role, count)| format!("{role}:{count}"))
            .collect(),
    );
    (0.55 * selector_score) + (0.30 * text_score) + (0.15 * role_score)
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
    fn label_normalization_uses_kebab_case() {
        assert_eq!(normalize_label("Account Settings"), "account-settings");
        assert_eq!(
            normalize_label("  Developer options! "),
            "developer-options"
        );
    }

    #[test]
    fn redacts_sensitive_text_before_fingerprinting() {
        let first =
            json!({"class":"Column","children":[{"class":"Text","text":"alice@example.com"}]});
        let second =
            json!({"class":"Column","children":[{"class":"Text","text":"bob@example.com"}]});
        assert_eq!(
            fingerprint_layout(&first).identity_hash,
            fingerprint_layout(&second).identity_hash
        );
        let redacted = serde_json::to_string(&redact_layout(&first)).unwrap();
        assert!(!redacted.contains("alice@example.com"));
    }

    #[test]
    fn extracts_stable_selectors_and_static_text() {
        let baseline = fingerprint_layout(&json!({
            "class": "Column",
            "children": [
                {"class":"Button","testTag":"settings_button","text":"Settings"},
                {"class":"Text","text":"123456789012"}
            ]
        }));
        assert!(baseline
            .fingerprint
            .selectors
            .iter()
            .any(|selector| selector.kind == "test_tag" && selector.value == "settings_button"));
        assert!(baseline
            .fingerprint
            .static_text
            .iter()
            .any(|text| text.value == "Settings"));
        assert!(!baseline
            .fingerprint
            .static_text
            .iter()
            .any(|text| text.value == "123456789012"));
    }
}
