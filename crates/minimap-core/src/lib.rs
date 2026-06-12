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

/// Tokenize free text into a lowercase ASCII-kebab form for the similarity text
/// dimension. This preserves the original (pre-transliteration) normalization so
/// the slug transliteration in `normalize_label` does NOT perturb similarity
/// scores: non-ASCII characters are dropped to word boundaries, never folded.
pub fn tokenize_text(text: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in text.trim().chars().flat_map(char::to_lowercase) {
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

/// Normalize a human label into a stable, pure-ASCII kebab slug. Non-ASCII text
/// is transliterated first (e.g. `Über` -> `uber`, `Москва` -> `moskva`) so the
/// slug never loses script-only labels; an empty result (e.g. emoji-only) falls
/// back to a deterministic `u-<hex8>` derived from the trimmed label. The output
/// is pure ASCII, so `slugify(normalize_label(x)) == normalize_label(x)` stays
/// idempotent (preserves the filename == slugify(id) contract).
pub fn normalize_label(label: &str) -> String {
    let trimmed = label.trim();
    let slug = tokenize_text(&deunicode::deunicode(trimmed));
    if slug.is_empty() {
        let digest = format!("{:x}", Sha256::digest(trimmed.as_bytes()));
        format!("u-{}", &digest[..8])
    } else {
        slug
    }
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
        // Real `android layout` output encodes geometry as STRINGS (e.g.
        // "center":"[1006,147]", "bounds":"[0,66][1080,2337]"). Those routinely
        // carry >= 7 digits, so the numeric screen below would destroy them and
        // break selector replay from cached layouts. Pass them through verbatim
        // ONLY when the value matches the strict per-key geometry grammar;
        // anything else under these keys keeps the default-deny screening.
        Value::String(text)
            if key
                .map(|key| is_geometry_string(key, text))
                .unwrap_or(false) =>
        {
            value.clone()
        }
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
        // Default-deny: any remaining free-text string (under a key not in
        // TEXT_KEYS, or with no key) is still screened for sensitive content so
        // PII appearing under an unexpected key cannot leak verbatim.
        Value::String(text) => match sensitive_text_reason(text) {
            Some(reason) => json!({"redacted": true, "reason": reason}),
            None => value.clone(),
        },
        _ => value.clone(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let lowered = key.to_lowercase();
    SENSITIVE_KEYS
        .iter()
        .any(|pattern| lowered.contains(pattern))
}

/// True when `text` is EXACTLY the string geometry real `android layout` emits
/// for `key`: `[<digits>,<digits>]` under `center`, or
/// `[<digits>,<digits>][<digits>,<digits>]` under `bounds`/`raw_bounds`.
/// Whitespace between tokens is tolerated; signs, decimals, and any other
/// characters are not, so non-geometry content under these keys still falls
/// through to the default-deny screening.
fn is_geometry_string(key: &str, text: &str) -> bool {
    let pairs = match key {
        "center" => 1,
        "bounds" | "raw_bounds" => 2,
        _ => return false,
    };
    let mut rest = text;
    for _ in 0..pairs {
        match consume_bracketed_pair(rest) {
            Some(remaining) => rest = remaining,
            None => return false,
        }
    }
    rest.trim_start().is_empty()
}

/// Consume one `[<digits>,<digits>]` group (leading whitespace tolerated) and
/// return the remainder.
fn consume_bracketed_pair(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('[')?;
    let rest = consume_digits(rest)?;
    let rest = rest.trim_start().strip_prefix(',')?;
    let rest = consume_digits(rest)?;
    rest.trim_start().strip_prefix(']')
}

/// Consume one non-empty ASCII digit run (leading whitespace tolerated).
fn consume_digits(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    let rest = trimmed.trim_start_matches(|ch: char| ch.is_ascii_digit());
    if rest.len() == trimmed.len() {
        None
    } else {
        Some(rest)
    }
}

fn sensitive_text_reason(text: &str) -> Option<&'static str> {
    let lowered = text.to_lowercase();
    if has_email_token(text) {
        Some("email")
    } else if lowered.contains("token") || lowered.contains("bearer") || lowered.starts_with("eyj")
    {
        Some("token")
    } else if has_grouped_pii(text) || text.chars().filter(|ch| ch.is_ascii_digit()).count() >= 7 {
        Some("numeric_sensitive")
    } else {
        None
    }
}

/// Detects an email-shaped, whitespace-free token: non-empty local part, a
/// single `@`, and a domain that contains a dot with non-empty labels. Avoids
/// firing on prose like "Rate 4.5 @ store" where `@` and `.` merely co-occur.
fn has_email_token(text: &str) -> bool {
    text.split_whitespace().any(|token| {
        let mut parts = token.splitn(2, '@');
        let local = parts.next().unwrap_or("");
        let domain = match parts.next() {
            Some(domain) => domain,
            None => return false,
        };
        if local.is_empty() || domain.contains('@') {
            return false;
        }
        match domain.rsplit_once('.') {
            Some((host, tld)) => !host.is_empty() && !tld.is_empty(),
            None => false,
        }
    })
}

/// Detects grouped digit runs that look like PII (e.g. SSNs or phone numbers):
/// `\d{3}-\d{2}-\d{4}` or `\d{3}[ -.]\d{3,4}`.
fn has_grouped_pii(text: &str) -> bool {
    let bytes = text.as_bytes();
    let digit_run = |start: usize| {
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        end - start
    };
    let is_separator = |b: u8| matches!(b, b' ' | b'-' | b'.');
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let first = digit_run(i);
            let mut j = i + first;
            if j < bytes.len() && is_separator(bytes[j]) {
                let sep = bytes[j];
                j += 1;
                let second = digit_run(j);
                // SSN: 3-2-4
                if first == 3 && second == 2 {
                    let mut k = j + second;
                    if k < bytes.len() && bytes[k] == sep {
                        k += 1;
                        if digit_run(k) == 4 {
                            return true;
                        }
                    }
                }
                // Phone-ish group: 3 then 3-4 digits.
                if first == 3 && (3..=4).contains(&second) {
                    return true;
                }
            }
            i += first.max(1);
        } else {
            i += 1;
        }
    }
    false
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

/// Keys whose string values may carry a package/id marker for a blocking overlay.
const OVERLAY_ID_KEYS: &[&str] = &[
    "id",
    "resource-id",
    "resource_id",
    "resourceId",
    "package",
    "test_tag",
    "testTag",
    "test-tag",
];

/// Conservatively detect a blocking system/permission overlay by walking the
/// layout JSON and looking for unambiguous markers under id/resource-id/package
/// keys. Only fires on strong signals (the permission-controller package or the
/// stock allow/deny dialog buttons), never on generic UI text like "Allow", so
/// callers can trust a `Some` to mean a real blocking dialog is present.
pub fn detect_overlay(layout: &Value) -> Option<String> {
    fn walk(value: &Value) -> Option<String> {
        match value {
            Value::Object(map) => {
                for key in OVERLAY_ID_KEYS {
                    if let Some(text) = map.get(*key).and_then(Value::as_str) {
                        if let Some(reason) = overlay_marker_reason(text) {
                            return Some(reason.to_string());
                        }
                    }
                }
                for value in map.values() {
                    if let Some(reason) = walk(value) {
                        return Some(reason);
                    }
                }
                None
            }
            Value::Array(values) => values.iter().find_map(walk),
            _ => None,
        }
    }
    walk(layout)
}

fn overlay_marker_reason(value: &str) -> Option<&'static str> {
    if value.contains("permission_allow_button")
        || value.contains("permission_deny_button")
        || value.contains("com.android.permissioncontroller")
    {
        Some("permission_dialog")
    } else if value.contains("com.android.packageinstaller") {
        Some("package_installer")
    } else {
        None
    }
}

pub fn fingerprint_usable(baseline: &PlaceBaseline) -> bool {
    !baseline.fingerprint.selectors.is_empty()
        || !baseline.fingerprint.static_text.is_empty()
        || !baseline.fingerprint.roles.is_empty()
}

/// Minimum evidence-based similarity for a non-exact match to be reported as
/// "known_changed" (a self-healing variant). "Balanced" / band-center value:
/// high enough that blank/disjoint screens stay "unknown", low enough that a
/// screen sharing most of its selectors with one-sided scroll drift still
/// self-heals (the Jaccard+containment blend lifts the smaller, scrolled side
/// toward its larger counterpart), while keeping genuinely-different sibling
/// screens out.
//
// LOCKED by live re-measure on emulator-5554 (Jetsnack, tranche C). The blended
// scorer cleanly separates same-place (incl. snack-detail siblings) from
// different-tab screens:
//   same_place  : drift 0.902, state 0.956, siblings 0.943/0.958  (min 0.902)
//   different   : cart 0.542, search 0.641, profile 0.689         (max 0.689)
// Gap [0.689, 0.902]; threshold = band center, rounded to the dead-center value.
const KNOWN_CHANGED_THRESHOLD: f64 = 0.80;

pub fn match_place(baseline: &PlaceBaseline, places: impl Iterator<Item = Place>) -> PlaceMatch {
    // Never score a blank/unusable incoming observation against known places:
    // an empty fingerprint carries no evidence and must report "unknown".
    if !fingerprint_usable(baseline) {
        return PlaceMatch {
            status: "unknown".to_string(),
            place_id: None,
            slug: None,
            confidence: 0.0,
            hash_matched: false,
        };
    }
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
        Some((place, score, _)) if score >= KNOWN_CHANGED_THRESHOLD => PlaceMatch {
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
    let selector_left: BTreeSet<String> = left
        .selectors
        .iter()
        .map(|selector| format!("{}={}", selector.kind, selector.value))
        .collect();
    let selector_right: BTreeSet<String> = right
        .selectors
        .iter()
        .map(|selector| format!("{}={}", selector.kind, selector.value))
        .collect();
    let text_left: BTreeSet<String> = left
        .static_text
        .iter()
        .map(|text| tokenize_text(&text.value))
        .filter(|value| !value.is_empty())
        .collect();
    let text_right: BTreeSet<String> = right
        .static_text
        .iter()
        .map(|text| tokenize_text(&text.value))
        .filter(|value| !value.is_empty())
        .collect();
    // Roles ride the blended dimension as a presence SET of role names (count is
    // factored out into a separate count-similarity term below), so a role that
    // merely repeats more or fewer times does not split an otherwise-matching
    // screen.
    let role_left: BTreeSet<String> = left.roles.keys().cloned().collect();
    let role_right: BTreeSet<String> = right.roles.keys().cloned().collect();

    // Evidence-based weighted average: a dimension that is empty on both sides
    // carries no evidence, so it neither contributes a (spurious) perfect score
    // nor consumes weight. Only dimensions present on at least one side count,
    // and the weights are renormalized over the present dimensions. Each
    // dimension blends symmetric Jaccard with containment so a one-sided scroll
    // loss (the smaller side is a near-subset of the larger) still scores high.
    let mut weighted = 0.0;
    let mut total_weight = 0.0;
    for (weight, left_set, right_set) in [
        (0.55, &selector_left, &selector_right),
        (0.30, &text_left, &text_right),
        // Role presence rides at the base 0.10; the remaining ~0.05 of the role
        // budget is the count-similarity term folded in below.
        (0.10, &role_left, &role_right),
    ] {
        if left_set.is_empty() && right_set.is_empty() {
            continue;
        }
        weighted += weight * blended_overlap(left_set, right_set);
        total_weight += weight;
    }

    // Count-similarity: mean over roles present on BOTH sides of min(c)/max(c),
    // folded in at base weight 0.05. Skipped entirely when no roles are shared so
    // it can never lift a genuinely-different screen.
    if let Some(count_similarity) = role_count_similarity(&left.roles, &right.roles) {
        weighted += 0.05 * count_similarity;
        total_weight += 0.05;
    }

    if total_weight == 0.0 {
        0.0
    } else {
        weighted / total_weight
    }
}

/// Mean of symmetric Jaccard (|A∩B|/|A∪B|) and containment (|A∩B|/min(|A|,|B|)).
/// Containment heals one-sided drift: the smaller, scrolled side is a near-subset
/// of the larger, so containment approaches 1.0 while Jaccard stays low when the
/// sizes differ a lot. Blending the two tempers containment's subset-merge, so a
/// sparse splash/loading screen that is a strict subset of a big screen does NOT
/// merge on containment alone.
fn blended_overlap(left: &BTreeSet<String>, right: &BTreeSet<String>) -> f64 {
    if left.is_empty() && right.is_empty() {
        return 1.0;
    }
    let intersection = left.intersection(right).count() as f64;
    let union = left.union(right).count() as f64;
    let jaccard = if union == 0.0 {
        0.0
    } else {
        intersection / union
    };
    let smaller = left.len().min(right.len()) as f64;
    let containment = if smaller == 0.0 {
        0.0
    } else {
        intersection / smaller
    };
    (jaccard + containment) / 2.0
}

/// Count-similarity across roles present on BOTH sides: mean of min(c)/max(c)
/// over the shared role names. Returns `None` when no roles are shared (so the
/// caller skips the term rather than scoring it 0 and dragging the blend down).
fn role_count_similarity(
    left: &BTreeMap<String, usize>,
    right: &BTreeMap<String, usize>,
) -> Option<f64> {
    let mut total = 0.0;
    let mut shared = 0usize;
    for (role, left_count) in left {
        if let Some(right_count) = right.get(role) {
            let min = (*left_count).min(*right_count) as f64;
            let max = (*left_count).max(*right_count) as f64;
            total += if max == 0.0 { 1.0 } else { min / max };
            shared += 1;
        }
    }
    if shared == 0 {
        None
    } else {
        Some(total / shared as f64)
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
    fn normalize_label_transliterates_non_ascii() {
        assert_eq!(normalize_label("Über"), "uber");
        assert_eq!(normalize_label("Café"), "cafe");
        assert_eq!(normalize_label("Москва"), "moskva");
        // CJK transliterates to a non-empty ASCII slug (exact form is
        // deunicode-defined; assert only that it is non-empty and pure ASCII).
        let cjk = normalize_label("日本語");
        assert!(!cjk.is_empty());
        assert!(cjk.is_ascii());
    }

    #[test]
    fn normalize_label_falls_back_to_hash_when_empty() {
        // A zero-width space transliterates to a single space (no ASCII
        // alphanumerics survive tokenization), so the deterministic `u-<hex8>`
        // fallback fires. (Note: many emoji such as 🎉 DO have a deunicode short
        // name like "tada" and therefore produce a real kebab slug, not the
        // fallback.)
        let input = "\u{200B}";
        let slug = normalize_label(input);
        assert!(slug.starts_with("u-"), "expected u- fallback, got {slug}");
        assert_eq!(slug.len(), 10); // "u-" + 8 hex chars
                                    // Deterministic for the same input.
        assert_eq!(slug, normalize_label(input));
    }

    #[test]
    fn normalize_label_output_is_slugify_idempotent() {
        // The slug is already pure ASCII-kebab, so re-tokenizing it is a no-op.
        // This preserves Tranche A's filename == slugify(id) contract.
        for label in ["Über", "Café", "Москва", "日本語", "🎉", "Account Settings"] {
            let slug = normalize_label(label);
            assert_eq!(tokenize_text(&slug), slug, "not idempotent for {label}");
            assert_eq!(normalize_label(&slug), slug, "not idempotent for {label}");
        }
    }

    #[test]
    fn role_count_similarity_uses_min_over_max() {
        let mut left = BTreeMap::new();
        left.insert("Button".to_string(), 5);
        let mut right = BTreeMap::new();
        right.insert("Button".to_string(), 6);
        let similarity = role_count_similarity(&left, &right).unwrap();
        assert!(
            (similarity - 0.8333).abs() < 0.001,
            "5 vs 6 should be ~0.83, got {similarity}"
        );
    }

    #[test]
    fn role_count_similarity_skipped_when_disjoint() {
        let mut left = BTreeMap::new();
        left.insert("Button".to_string(), 2);
        let mut right = BTreeMap::new();
        right.insert("Switch".to_string(), 2);
        assert_eq!(role_count_similarity(&left, &right), None);
    }

    #[test]
    fn no_roles_pair_scores_identically_after_role_change() {
        // Jetsnack-style fingerprints expose no roles. The role changes in
        // Defect 2 must be a byte-identical no-op for a no-roles pair: the score
        // depends only on selectors and text. This locks the value so the test
        // phase can compare against it.
        let left = fingerprint(
            &[("test_tag", "a"), ("resource_id", "b")],
            &["Home", "Discover"],
            &[],
        );
        let right = fingerprint(
            &[("test_tag", "a"), ("resource_id", "b")],
            &["Home", "Search"],
            &[],
        );
        let score = similarity(&left, &right);
        // selectors: identical -> blended_overlap == 1.0 (weight 0.55)
        // text: {home, discover} vs {home, search} -> intersection 1, union 3,
        //   jaccard 1/3; min size 2 -> containment 1/2; blend = (1/3 + 1/2)/2.
        let text_blend = (1.0 / 3.0 + 0.5) / 2.0;
        let expected = (0.55 * 1.0 + 0.30 * text_blend) / (0.55 + 0.30);
        assert!(
            (score - expected).abs() < 1e-12,
            "no-roles score {score} should equal {expected}"
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

    fn selector(kind: &str, value: &str) -> Selector {
        Selector {
            kind: kind.to_string(),
            value: value.to_string(),
        }
    }

    fn fingerprint(
        selectors: &[(&str, &str)],
        static_text: &[&str],
        roles: &[(&str, usize)],
    ) -> Fingerprint {
        Fingerprint {
            selectors: selectors
                .iter()
                .map(|(kind, value)| selector(kind, value))
                .collect(),
            static_text: static_text
                .iter()
                .map(|value| StaticText {
                    value: value.to_string(),
                })
                .collect(),
            roles: roles
                .iter()
                .map(|(role, count)| (role.to_string(), *count))
                .collect(),
        }
    }

    fn baseline(fingerprint: Fingerprint) -> PlaceBaseline {
        PlaceBaseline {
            identity_hash: identity_hash(&fingerprint),
            fingerprint,
        }
    }

    fn place(slug: &str, baseline: PlaceBaseline, variants: Vec<PlaceBaseline>) -> Place {
        Place {
            schema_version: "1".to_string(),
            id: place_id_for_slug(slug),
            slug: slug.to_string(),
            label: slug.to_string(),
            baseline,
            variants,
        }
    }

    #[test]
    fn match_blank_layout_is_unknown() {
        let incoming = fingerprint_layout(&json!([]));
        assert!(!fingerprint_usable(&incoming));
        let candidate = place(
            "roles-only",
            baseline(fingerprint(&[], &[], &[("Button", 2), ("Text", 3)])),
            vec![],
        );
        let result = match_place(&incoming, vec![candidate].into_iter());
        assert_eq!(result.status, "unknown");
        assert_eq!(result.confidence, 0.0);
        assert!(!result.hash_matched);
    }

    #[test]
    fn disjoint_roles_only_screens_not_merged() {
        let incoming = baseline(fingerprint(&[], &[], &[("Button", 2), ("Image", 1)]));
        let candidate = place(
            "other-roles",
            baseline(fingerprint(&[], &[], &[("Switch", 4), ("Slider", 2)])),
            vec![],
        );
        let result = match_place(&incoming, vec![candidate].into_iter());
        assert_eq!(result.status, "unknown");
    }

    #[test]
    fn exact_hash_is_known() {
        let fp = fingerprint(
            &[("test_tag", "home_tab"), ("resource_id", "root")],
            &["Home"],
            &[("Button", 1)],
        );
        let incoming = baseline(fp.clone());
        let candidate = place("home", baseline(fp), vec![]);
        let result = match_place(&incoming, vec![candidate].into_iter());
        assert_eq!(result.status, "known");
        assert_eq!(result.confidence, 1.0);
        assert!(result.hash_matched);
    }

    #[test]
    fn variant_hash_is_known() {
        let base_fp = fingerprint(&[("test_tag", "home_tab")], &["Home"], &[("Button", 1)]);
        let variant_fp = fingerprint(
            &[("test_tag", "home_tab")],
            &["Home", "Updated"],
            &[("Button", 1)],
        );
        let incoming = baseline(variant_fp.clone());
        let candidate = place("home", baseline(base_fp), vec![baseline(variant_fp)]);
        let result = match_place(&incoming, vec![candidate].into_iter());
        assert_eq!(result.status, "known");
        assert_eq!(result.confidence, 1.0);
        assert!(result.hash_matched);
    }

    #[test]
    fn evolved_screen_is_known_changed() {
        // Shares all selectors; text drifts ("Home" -> "Home" + "New"); roles drift slightly.
        let baseline_fp = fingerprint(
            &[("test_tag", "a"), ("resource_id", "b")],
            &["Home"],
            &[("Button", 2)],
        );
        let incoming_fp = fingerprint(
            &[("test_tag", "a"), ("resource_id", "b")],
            &["Home", "New"],
            &[("Button", 2)],
        );
        let incoming = baseline(incoming_fp);
        let candidate = place("home", baseline(baseline_fp), vec![]);
        let result = match_place(&incoming, vec![candidate].into_iter());
        assert!(
            result.confidence >= KNOWN_CHANGED_THRESHOLD,
            "evolved score {} should reach threshold {}",
            result.confidence,
            KNOWN_CHANGED_THRESHOLD
        );
        assert_eq!(result.status, "known_changed");
        assert!(!result.hash_matched);
    }

    // ----------------------------------------------------------------------
    // Matching-band regression fixtures (tranche C).
    //
    // These are grounded in the REAL Jetsnack emulator captures measured during
    // the live-tune phase on emulator-5554. Jetsnack screens expose NO roles and
    // their only shared selectors are the four bottom-nav content-desc tabs; the
    // body is captured as static_text. So scoring runs on the selector + text
    // dimensions exactly as it does on-device, and these synthetic fingerprints
    // reproduce the live-tune score bands (ground truth in parentheses):
    //   same_place : drift 0.91 (0.902 floor), state 0.90 (0.956),
    //                siblings 0.94 (0.943/0.958)
    //   different  : search 0.54 (0.641), cart 0.49 (0.542), profile 0.65 (0.689)
    // The locked KNOWN_CHANGED_THRESHOLD (0.80) sits strictly inside the gap.

    /// The four Jetsnack bottom-nav tabs, present on every screen. Modeled as
    /// content-desc selectors because that is what `android layout` exposes for
    /// the nav and what the real fingerprints share across tabs.
    fn jetsnack_nav() -> Vec<(&'static str, &'static str)> {
        vec![
            ("content_desc", "Home"),
            ("content_desc", "Search"),
            ("content_desc", "Cart"),
            ("content_desc", "Profile"),
        ]
    }

    /// Nav + the snack-detail action affordances shared by every product page.
    fn jetsnack_detail_chrome() -> Vec<(&'static str, &'static str)> {
        let mut sel = jetsnack_nav();
        sel.extend([
            ("content_desc", "Add to cart"),
            ("content_desc", "Increase"),
            ("content_desc", "Decrease"),
            ("content_desc", "Back"),
        ]);
        sel
    }

    /// Home body: six collection headers captured as static text. The
    /// no-roles, text-heavy Jetsnack home screen.
    const HOME_BODY: &[&str] = &[
        "Popular on Jetsnack",
        "Cant resist these",
        "New arrivals",
        "Chips and crackers",
        "Bakery",
        "Fruit",
    ];

    #[test]
    fn drift_heals_home_vs_home_scrolled() {
        // Home vs home after a scroll that drops the bottom collections off
        // screen: the scrolled side is a strict near-subset of the full home
        // body. The Jaccard+containment blend lifts the smaller side, so the
        // false split is healed -> known_changed (same place). Live floor 0.902.
        let home = fingerprint(&jetsnack_nav(), HOME_BODY, &[]);
        let scrolled = fingerprint(
            &jetsnack_nav(),
            &["Popular on Jetsnack", "Cant resist these", "New arrivals"],
            &[],
        );
        let result = match_place(
            &baseline(scrolled),
            vec![place("home", baseline(home), vec![])].into_iter(),
        );
        assert_eq!(
            result.status, "known_changed",
            "drift must heal, score {}",
            result.confidence
        );
        assert!(result.confidence >= KNOWN_CHANGED_THRESHOLD);
        assert!(!result.hash_matched);
    }

    #[test]
    fn siblings_merge_donut_vs_cupcake_detail() {
        // Two snack-detail pages share the identical detail template (chrome +
        // section headers); only the product-name token differs. They are the
        // same PLACE (the detail route), reported as known_changed. Live 0.958.
        let detail_body = [
            "Ingredients",
            "Nutrition",
            "Reviews",
            "Related",
            "Add quantity",
            "Price",
            "Tagline",
        ];
        let mut donut: Vec<&str> = vec!["Donut detail"];
        donut.extend(detail_body);
        let mut cupcake: Vec<&str> = vec!["Cupcake detail"];
        cupcake.extend(detail_body);
        let donut_fp = fingerprint(&jetsnack_detail_chrome(), &donut, &[]);
        let cupcake_fp = fingerprint(&jetsnack_detail_chrome(), &cupcake, &[]);
        let result = match_place(
            &baseline(donut_fp),
            vec![place("snack-detail", baseline(cupcake_fp), vec![])].into_iter(),
        );
        assert_eq!(
            result.status, "known_changed",
            "siblings must merge, score {}",
            result.confidence
        );
        assert!(result.confidence >= KNOWN_CHANGED_THRESHOLD);
    }

    #[test]
    fn siblings_merge_chips_vs_donut_detail() {
        // Same as above for a third sibling pairing. Live 0.943.
        let detail_body = [
            "Ingredients",
            "Nutrition",
            "Reviews",
            "Related",
            "Add quantity",
            "Price",
            "Tagline",
        ];
        let mut chips: Vec<&str> = vec!["Chips detail"];
        chips.extend(detail_body);
        let mut donut: Vec<&str> = vec!["Donut detail"];
        donut.extend(detail_body);
        let chips_fp = fingerprint(&jetsnack_detail_chrome(), &chips, &[]);
        let donut_fp = fingerprint(&jetsnack_detail_chrome(), &donut, &[]);
        let result = match_place(
            &baseline(chips_fp),
            vec![place("snack-detail", baseline(donut_fp), vec![])].into_iter(),
        );
        assert_eq!(
            result.status, "known_changed",
            "siblings must merge, score {}",
            result.confidence
        );
        assert!(result.confidence >= KNOWN_CHANGED_THRESHOLD);
    }

    #[test]
    fn state_change_collapsed_vs_expanded_is_known_changed() {
        // The SAME detail screen, "see more" collapsed vs expanded: the
        // expanded variant swaps the "see more" affordance for the full
        // description paragraph and adds one extra block. Same place, evolved
        // state -> known_changed. Live 0.956.
        let collapsed = fingerprint(
            &jetsnack_detail_chrome(),
            &[
                "Cupcake",
                "Chocolate",
                "Ingredients",
                "See more",
                "Reviews",
                "Related",
            ],
            &[],
        );
        let expanded = fingerprint(
            &jetsnack_detail_chrome(),
            &[
                "Cupcake",
                "Chocolate",
                "Ingredients",
                "Full description",
                "Reviews",
                "Related",
                "More info",
            ],
            &[],
        );
        let result = match_place(
            &baseline(expanded),
            vec![place("snack-detail", baseline(collapsed), vec![])].into_iter(),
        );
        assert_eq!(
            result.status, "known_changed",
            "state change must stay same place, score {}",
            result.confidence
        );
        assert!(result.confidence >= KNOWN_CHANGED_THRESHOLD);
    }

    #[test]
    fn different_tabs_stay_unknown_vs_home() {
        // search / cart / profile each share only the bottom nav with home and
        // have entirely distinct bodies. They must NOT merge into home: every
        // score sits below the locked threshold -> unknown. Live: search 0.641,
        // cart 0.542, profile 0.689 (profile, empty-ish body, is the ceiling).
        let home = || {
            place(
                "home",
                baseline(fingerprint(&jetsnack_nav(), HOME_BODY, &[])),
                vec![],
            )
        };

        let mut search_sel = jetsnack_nav();
        search_sel.extend([("content_desc", "Search field"), ("content_desc", "Filter")]);
        let search = fingerprint(
            &search_sel,
            &["Search Jetsnack", "Categories", "Lifestyles", "Desserts"],
            &[],
        );
        let r = match_place(&baseline(search), vec![home()].into_iter());
        assert_eq!(
            r.status, "unknown",
            "search must stay out, score {}",
            r.confidence
        );
        assert!(r.confidence < KNOWN_CHANGED_THRESHOLD);

        let mut cart_sel = jetsnack_nav();
        cart_sel.extend([
            ("content_desc", "Checkout"),
            ("content_desc", "Remove"),
            ("content_desc", "Increase"),
            ("content_desc", "Decrease"),
        ]);
        let cart = fingerprint(
            &cart_sel,
            &["Your cart", "Subtotal", "Shipping", "Checkout"],
            &[],
        );
        let r = match_place(&baseline(cart), vec![home()].into_iter());
        assert_eq!(
            r.status, "unknown",
            "cart must stay out, score {}",
            r.confidence
        );
        assert!(r.confidence < KNOWN_CHANGED_THRESHOLD);

        // Profile: nav-only selectors, sparse body. This is the WORST case (the
        // highest different score) because it shares the most with home.
        let profile = fingerprint(&jetsnack_nav(), &["My profile", "Log out"], &[]);
        let r = match_place(&baseline(profile), vec![home()].into_iter());
        assert_eq!(
            r.status, "unknown",
            "profile must stay out, score {}",
            r.confidence
        );
        assert!(r.confidence < KNOWN_CHANGED_THRESHOLD);
    }

    #[test]
    fn subset_splash_does_not_merge_into_home() {
        // SUBSET GUARD: a sparse splash/loading screen whose selectors AND text
        // are both strict subsets of home. Pure containment would score this 1.0
        // and silently swallow the splash into home; the Jaccard+containment
        // blend keeps it well under threshold -> unknown. This is the regression
        // that defeats subset-merge.
        let home = place(
            "home",
            baseline(fingerprint(&jetsnack_nav(), HOME_BODY, &[])),
            vec![],
        );
        let splash = fingerprint(&[("content_desc", "Home")], &["Popular on Jetsnack"], &[]);
        let result = match_place(&baseline(splash), vec![home].into_iter());
        assert_eq!(
            result.status, "unknown",
            "subset splash must NOT merge, score {}",
            result.confidence
        );
        assert!(result.confidence < KNOWN_CHANGED_THRESHOLD);
    }

    #[test]
    fn role_count_drift_heals_into_known_changed() {
        // A roles-bearing screen whose role COUNTS drift (e.g. a list that grew
        // a few rows) but whose selectors and text are unchanged. The
        // presence-set keeps the role dimension at full overlap and the
        // count-similarity term (min/max) only gently discounts, so the screen
        // self-heals -> known_changed.
        let before = fingerprint(
            &[("test_tag", "list")],
            &["Item"],
            &[("Button", 3), ("Text", 5)],
        );
        let after = fingerprint(
            &[("test_tag", "list")],
            &["Item"],
            &[("Button", 4), ("Text", 7)],
        );
        let result = match_place(
            &baseline(after),
            vec![place("feed", baseline(before), vec![])].into_iter(),
        );
        assert_eq!(
            result.status, "known_changed",
            "role-count drift must heal, score {}",
            result.confidence
        );
        assert!(result.confidence >= KNOWN_CHANGED_THRESHOLD);
    }

    #[test]
    fn disjoint_roles_skip_count_term_at_match_level() {
        // Identical selectors + text, but roles are fully disjoint
        // ({Button} vs {Switch}). The count-similarity term is SKIPPED (no
        // shared role), so it neither lifts nor (scoring 0) drags the blend: the
        // score is driven by the matching selectors/text plus the zero
        // role-PRESENCE overlap. Asserts the count term is skipped by comparing
        // against a same-roles control: the disjoint case must score strictly
        // LOWER (presence overlap drops) yet the count term never contributed a 0.
        let sel = [("test_tag", "panel")];
        let txt = ["Toggle"];
        let disjoint = match_place(
            &baseline(fingerprint(&sel, &txt, &[("Button", 2)])),
            vec![place(
                "p",
                baseline(fingerprint(&sel, &txt, &[("Switch", 2)])),
                vec![],
            )]
            .into_iter(),
        );
        let shared = match_place(
            &baseline(fingerprint(&sel, &txt, &[("Button", 2)])),
            vec![place(
                "p",
                baseline(fingerprint(&sel, &txt, &[("Button", 2)])),
                vec![],
            )]
            .into_iter(),
        );
        // The shared-role control hash-matches (identical fingerprint) at 1.0;
        // the disjoint case cannot hash-match and is scored. The disjoint score
        // must be strictly below the shared control, confirming role presence
        // diverged, while still being a finite blended score (count term skipped,
        // not a 0 that would have tanked it below the selector/text floor).
        assert!(shared.hash_matched);
        assert!(!disjoint.hash_matched);
        assert!(
            disjoint.confidence < shared.confidence,
            "disjoint roles ({}) should score below shared control ({})",
            disjoint.confidence,
            shared.confidence
        );
        // Floor check: with selectors+text fully matched (0.55+0.30 weight) the
        // disjoint score stays high enough that ONLY the role presence and the
        // skipped count term separate it -- i.e. it never collapsed to 0.
        assert!(
            disjoint.confidence > 0.80,
            "disjoint score {} unexpectedly low",
            disjoint.confidence
        );
    }

    #[test]
    fn separation_invariant_threshold_sits_inside_the_gap() {
        // One loud, holistic assertion: every same-place pairing scores strictly
        // ABOVE every different pairing, and the locked KNOWN_CHANGED_THRESHOLD
        // lives strictly inside that gap. If a future formula change collapses
        // the bands, this fails immediately with the offending numbers.
        let nav = jetsnack_nav();
        let detail = jetsnack_detail_chrome();
        let detail_body = [
            "Ingredients",
            "Nutrition",
            "Reviews",
            "Related",
            "Add quantity",
            "Price",
            "Tagline",
        ];

        let home_fp = fingerprint(&nav, HOME_BODY, &[]);
        let scrolled_fp = fingerprint(
            &nav,
            &["Popular on Jetsnack", "Cant resist these", "New arrivals"],
            &[],
        );
        let mut donut: Vec<&str> = vec!["Donut detail"];
        donut.extend(detail_body);
        let mut cupcake: Vec<&str> = vec!["Cupcake detail"];
        cupcake.extend(detail_body);
        let donut_fp = fingerprint(&detail, &donut, &[]);
        let cupcake_fp = fingerprint(&detail, &cupcake, &[]);
        let collapsed_fp = fingerprint(
            &detail,
            &[
                "Cupcake",
                "Chocolate",
                "Ingredients",
                "See more",
                "Reviews",
                "Related",
            ],
            &[],
        );
        let expanded_fp = fingerprint(
            &detail,
            &[
                "Cupcake",
                "Chocolate",
                "Ingredients",
                "Full description",
                "Reviews",
                "Related",
                "More info",
            ],
            &[],
        );

        let same_place_scores = [
            ("drift", similarity(&scrolled_fp, &home_fp)),
            ("sibling-donut-cupcake", similarity(&donut_fp, &cupcake_fp)),
            (
                "state-collapsed-expanded",
                similarity(&expanded_fp, &collapsed_fp),
            ),
        ];

        let mut search_sel = nav.clone();
        search_sel.extend([("content_desc", "Search field"), ("content_desc", "Filter")]);
        let search_fp = fingerprint(
            &search_sel,
            &["Search Jetsnack", "Categories", "Lifestyles", "Desserts"],
            &[],
        );
        let mut cart_sel = nav.clone();
        cart_sel.extend([
            ("content_desc", "Checkout"),
            ("content_desc", "Remove"),
            ("content_desc", "Increase"),
            ("content_desc", "Decrease"),
        ]);
        let cart_fp = fingerprint(
            &cart_sel,
            &["Your cart", "Subtotal", "Shipping", "Checkout"],
            &[],
        );
        let profile_fp = fingerprint(&nav, &["My profile", "Log out"], &[]);
        let splash_fp = fingerprint(&[("content_desc", "Home")], &["Popular on Jetsnack"], &[]);

        let different_scores = [
            ("search", similarity(&search_fp, &home_fp)),
            ("cart", similarity(&cart_fp, &home_fp)),
            ("profile", similarity(&profile_fp, &home_fp)),
            ("splash-subset", similarity(&splash_fp, &home_fp)),
        ];

        let min_same = same_place_scores
            .iter()
            .map(|(_, s)| *s)
            .fold(f64::INFINITY, f64::min);
        let max_diff = different_scores
            .iter()
            .map(|(_, s)| *s)
            .fold(f64::NEG_INFINITY, f64::max);

        assert!(
            max_diff < KNOWN_CHANGED_THRESHOLD && KNOWN_CHANGED_THRESHOLD < min_same,
            "separation invariant violated: max(different)={max_diff:.4} threshold={KNOWN_CHANGED_THRESHOLD} min(same)={min_same:.4}\n  same:      {same_place_scores:?}\n  different: {different_scores:?}"
        );
        // Strict band separation independent of the threshold value.
        assert!(
            max_diff < min_same,
            "bands overlap: max(different)={max_diff:.4} >= min(same)={min_same:.4}"
        );
    }

    #[test]
    fn detect_overlay_flags_permission_dialog() {
        let layout = json!({
            "class": "FrameLayout",
            "children": [
                {
                    "class": "Button",
                    "resource-id": "com.android.permissioncontroller:id/permission_allow_button",
                    "text": "Allow"
                }
            ]
        });
        assert_eq!(
            detect_overlay(&layout),
            Some("permission_dialog".to_string())
        );
    }

    #[test]
    fn detect_overlay_ignores_normal_layout() {
        let layout = json!({
            "class": "Column",
            "children": [
                {"class": "Button", "testTag": "allow_button", "text": "Allow"},
                {"class": "Text", "text": "Allow access to continue"}
            ]
        });
        assert_eq!(detect_overlay(&layout), None);
    }

    #[test]
    fn detect_overlay_ignores_app_alert_dialog_positive_button() {
        // A normal app AlertDialog uses the framework positive-button id
        // (android:id/button1). This must NOT be treated as a blocking overlay.
        let layout = json!({
            "class": "FrameLayout",
            "children": [
                {"class": "TextView", "text": "Delete this item?"},
                {
                    "class": "Button",
                    "resource-id": "android:id/button1",
                    "text": "OK"
                },
                {
                    "class": "Button",
                    "resource-id": "android:id/button2",
                    "text": "Cancel"
                }
            ]
        });
        assert_eq!(detect_overlay(&layout), None);
    }

    #[test]
    fn redacts_sensitive_text_under_non_allowlisted_key() {
        // Free text that is not under a TEXT_KEYS key (here "subtitle" / "note")
        // must still be screened so PII cannot leak verbatim.
        let email_layout = json!({
            "class": "Row",
            "subtitle": "alice@example.com"
        });
        let redacted = redact_value(&email_layout, None);
        assert_eq!(
            redacted["subtitle"],
            json!({"redacted": true, "reason": "email"})
        );

        let ssn_layout = json!({
            "class": "Row",
            "note": "SSN 123-45-6789 on file"
        });
        let redacted = redact_value(&ssn_layout, None);
        assert_eq!(
            redacted["note"],
            json!({"redacted": true, "reason": "numeric_sensitive"})
        );
    }

    #[test]
    fn redaction_preserves_string_geometry_under_geometry_keys() {
        // Real `android layout` output encodes geometry as strings; these must
        // survive redaction byte-for-byte or selector replay from a cached
        // layout loses all tap geometry.
        let layout = json!({
            "class": "Button",
            "text": "Checkout",
            "center": "[1006,147]",
            "bounds": "[0,66][1080,2337]",
            "raw_bounds": "[ 0 , 66 ] [ 1080 , 2337 ]"
        });
        let redacted = redact_layout(&layout);
        assert_eq!(redacted["center"], json!("[1006,147]"));
        assert_eq!(redacted["bounds"], json!("[0,66][1080,2337]"));
        assert_eq!(redacted["raw_bounds"], json!("[ 0 , 66 ] [ 1080 , 2337 ]"));
    }

    #[test]
    fn redaction_still_screens_non_geometry_strings_under_geometry_keys() {
        let layout = json!({
            "class": "Text",
            // Grouped PII shape (phone-like) under a geometry key.
            "center": "555 1234",
            // >= 7 digits but not the strict grammar (prose tail).
            "bounds": "[555],[1234] call me",
            // Wrong arity for the key (a single center-shaped pair under bounds).
            "raw_bounds": "[1006,147]"
        });
        let redacted = redact_layout(&layout);
        for key in ["center", "bounds", "raw_bounds"] {
            assert_eq!(
                redacted[key],
                json!({"redacted": true, "reason": "numeric_sensitive"}),
                "non-geometry string under {key} must stay screened"
            );
        }
    }

    #[test]
    fn geometry_grammar_is_strict() {
        assert!(is_geometry_string("center", "[1006,147]"));
        assert!(is_geometry_string("center", " [ 1006 , 147 ] "));
        assert!(is_geometry_string("bounds", "[0,66][1080,2337]"));
        assert!(is_geometry_string("raw_bounds", "[0,66] [1080,2337]"));
        // Wrong arity for the key.
        assert!(!is_geometry_string("center", "[0,66][1080,2337]"));
        assert!(!is_geometry_string("bounds", "[1006,147]"));
        // No signs, decimals, prose, or trailing garbage.
        assert!(!is_geometry_string("center", "[-1006,147]"));
        assert!(!is_geometry_string("center", "[1006.5,147]"));
        assert!(!is_geometry_string("center", "[1006,147] call me"));
        assert!(!is_geometry_string("center", "555 1234"));
        // Only geometry keys participate in the bypass.
        assert!(!is_geometry_string("text", "[1006,147]"));
    }

    #[test]
    fn geometry_shaped_text_under_other_keys_keeps_default_screening() {
        // "[1006,147]" carries 7 digits, so under a non-geometry key the
        // existing numeric screen still fires (current-rule outcome).
        let layout = json!({"class": "Text", "text": "[1006,147]"});
        assert_eq!(
            redact_layout(&layout)["text"],
            json!({"redacted": true, "reason": "numeric_sensitive"})
        );
        // A short geometry-shaped string under a free-text key carries no
        // sensitive signal, so the default-deny arm keeps it (current rule).
        let layout = json!({"class": "Text", "note": "[12,34]"});
        assert_eq!(redact_layout(&layout)["note"], json!("[12,34]"));
    }

    #[test]
    fn string_geometry_passthrough_is_fingerprint_neutral() {
        // bounds/raw_bounds/center are VOLATILE_KEYS stripped before hashing,
        // so preserving them through redaction must not perturb identity hashes.
        let with_geometry = json!({
            "class": "Column",
            "children": [{
                "class": "Button",
                "text": "Checkout",
                "center": "[1006,147]",
                "bounds": "[0,66][1080,2337]"
            }]
        });
        let without_geometry = json!({
            "class": "Column",
            "children": [{"class": "Button", "text": "Checkout"}]
        });
        assert_eq!(
            fingerprint_layout(&with_geometry).identity_hash,
            fingerprint_layout(&without_geometry).identity_hash
        );
    }

    #[test]
    fn email_heuristic_requires_email_shaped_token() {
        assert_eq!(sensitive_text_reason("Rate 4.5 @ store"), None);
        assert_eq!(sensitive_text_reason("Meet me @ 5.30pm sharp"), None);
        assert_eq!(sensitive_text_reason("a@b.com"), Some("email"));
        assert_eq!(
            sensitive_text_reason("Contact alice@example.com today"),
            Some("email")
        );
    }

    #[test]
    fn numeric_heuristics_flag_grouped_pii_and_long_runs() {
        // 9-digit SSN with grouping.
        assert_eq!(
            sensitive_text_reason("123-45-6789"),
            Some("numeric_sensitive")
        );
        // Phone-shaped grouping.
        assert_eq!(sensitive_text_reason("555 1234"), Some("numeric_sensitive"));
        // Plain run of >= 7 digits.
        assert_eq!(sensitive_text_reason("1234567"), Some("numeric_sensitive"));
        // Short, ungrouped numbers stay clear.
        assert_eq!(sensitive_text_reason("Step 3 of 4"), None);
        assert_eq!(sensitive_text_reason("Page 12"), None);
    }
}
