use assert_cmd::Command;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn init_creates_minimal_layout_and_skill() {
    let temp = tempfile::tempdir().unwrap();
    let output = minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["ok"], true);
    assert!(temp.path().join(".minimap/config.json").exists());
    assert!(temp.path().join(".minimap/graph/places").is_dir());
    assert!(temp.path().join(".minimap/graph/edges").is_dir());
    assert!(!temp.path().join(".minimap/journal.jsonl").exists());
    assert!(!temp.path().join(".minimap/proposals").exists());
    assert!(temp
        .path()
        .join(".agents/skills/minimap-app-navigation/SKILL.md")
        .exists());
}

#[test]
fn init_refuses_legacy_layout_without_force() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".minimap/proposals")).unwrap();
    let assertion = minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .failure();
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "config_error");
    assert!(payload["summary"]
        .as_str()
        .unwrap()
        .contains("incompatible pre-lean-v1"));
}

#[test]
fn init_force_replaces_legacy_layout() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".minimap/proposals")).unwrap();
    fs::write(temp.path().join(".minimap/stale.txt"), "stale").unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex", "--force"])
        .assert()
        .success();
    assert!(!temp.path().join(".minimap/stale.txt").exists());
    assert!(temp.path().join(".minimap/graph/places").is_dir());
}

#[test]
fn help_exposes_only_lean_commands() {
    let temp = tempfile::tempdir().unwrap();
    let output = minimap(temp.path())
        .args(["--help"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8_lossy(&output);
    for command in ["whereami", "go", "tap", "scroll", "back", "layout"] {
        assert!(help.contains(command), "help should contain {command}");
    }
    for removed in [
        "  accept",
        "  route",
        "  screen",
        "  observe",
        "  learn",
        "  undo",
        "  validate",
    ] {
        assert!(
            !help.contains(removed),
            "help must not contain removed command {removed}"
        );
    }
}

#[test]
fn whereami_label_creates_place() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home"]);
    write_adb_script(&bin);
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "Home"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["place"]["slug"], "home");
    assert_eq!(payload["changed_graph"], true);
    assert!(temp
        .path()
        .join(".minimap/graph/places/place_home.json")
        .exists());
}

#[test]
fn whereami_existing_label_on_unknown_layout_does_not_claim_place() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "blank"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .code(2);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "label_mismatch");
    assert_eq!(payload["place"], Value::Null);
}

#[test]
fn tap_selector_with_label_creates_destination_and_edge() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["from"], "home");
    assert_eq!(payload["data"]["to"], "search");
    assert!(temp
        .path()
        .join(".minimap/graph/places/place_search.json")
        .exists());
    let edges: Vec<_> = fs::read_dir(temp.path().join(".minimap/graph/edges"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .collect();
    assert_eq!(edges.len(), 1);
    let edge = read_json_path(&edges[0].path());
    assert_eq!(edge["from"]["slug"], "home");
    assert_eq!(edge["to"]["slug"], "search");
    assert_eq!(
        edge["recipe"][0]["selector"],
        json!({"kind": "text", "value": "SEARCH"})
    );
}

#[test]
fn tap_retries_blank_post_action_layout() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "blank", "search"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success();

    let search = read_json_path(&temp.path().join(".minimap/graph/places/place_search.json"));
    assert!(serde_json::to_string(&search["baseline"])
        .unwrap()
        .contains("Categories"));
}

#[test]
fn tap_existing_label_adds_variant_without_replacing_baseline() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search", "search", "home_changed"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=HOME",
            "--label",
            "home",
            "--reason",
            "open home",
        ])
        .assert()
        .success();

    let home = read_json_path(&temp.path().join(".minimap/graph/places/place_home.json"));
    let baseline = serde_json::to_string(&home["baseline"]).unwrap();
    let variants = serde_json::to_string(&home["variants"]).unwrap();
    assert!(!baseline.contains("Featured today"));
    assert!(variants.contains("Featured today"));
}

#[test]
fn tap_unknown_destination_without_label_does_not_write_graph() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search"]);
    write_adb_script(&bin);
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--reason",
            "open search",
        ])
        .assert()
        .code(5);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "needs_label");
    assert!(!temp
        .path()
        .join(".minimap/graph/places/place_search.json")
        .exists());
    let edge_count = fs::read_dir(temp.path().join(".minimap/graph/edges"))
        .unwrap()
        .count();
    assert_eq!(edge_count, 0);
}

// FIX 5: a malformed --point value must be a guaranteed no-op. Before the fix,
// the value was parsed only after observe/orient (which can self-heal-write the
// drifted place), so a bad coordinate could still mutate the graph. The drifted
// `home_changed` layout below would self-heal place_home into a variant if the
// orientation ran before the parse error.
#[test]
fn tap_malformed_point_is_config_error_and_leaves_graph_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home_changed"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();

    let place_path = temp.path().join(".minimap/graph/places/place_home.json");
    let before = fs::read_to_string(&place_path).unwrap();

    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["tap", "--point", "bogus", "--label", "search"])
        .assert()
        // Malformed value -> bail!() -> main() catch-all -> config_error -> 7.
        .code(7);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "config_error");

    let after = fs::read_to_string(&place_path).unwrap();
    assert_eq!(before, after, "malformed --point must not mutate the graph");
    assert!(
        !after.contains("Featured today"),
        "drifted variant must not have been self-healed in: {after}"
    );
    assert_eq!(edge_count(temp.path()), 0);
}

#[test]
fn go_replays_known_selector_edge() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search", "home", "search"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();

    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["go", "search"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["target"], "search");
    assert_eq!(payload["data"]["start_source"], "session");
    assert_eq!(payload["data"]["executed_steps"][0]["to"], "search");
}

// REDACTION/GEOMETRY REGRESSION: real `android layout` output encodes geometry
// as STRINGS ("center":"[540,2200]", "bounds":"[480,2100][600,2300]") whose
// digit count trips the numeric-sensitive redaction screen. `go` seeds selector
// resolution from the REDACTED session-cache layout, so if redaction destroys
// the geometry strings, replay fails live with "matched node has no tap bounds"
// (action_failed) even though the object-geometry fakes above stay green.
#[test]
fn go_replays_selector_edge_from_string_geometry_session_cache() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(
        &bin,
        &[
            "home_geo",
            "home_geo",
            "search_geo",
            "home_geo",
            "search_geo",
        ],
    );
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success();
    // Re-orient onto home so the session cache holds the (redacted) home layout
    // with its string geometry; `go` must resolve the tap point from it.
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();

    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["go", "search"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["target"], "search");
    assert_eq!(payload["data"]["start_source"], "session");
    assert_eq!(payload["data"]["executed_steps"][0]["to"], "search");
}

// KNOWN-EXITS REGRESSION: whereami hard-coded include_exits=false on both the
// fresh and the session-cache paths, so `known_exits` was always [] even when
// the oriented place had outgoing edges.
#[test]
fn whereami_reports_known_exits_on_fresh_and_cached_paths() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search", "home"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    // Establish one outgoing edge home -> search.
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success();

    // Fresh path: a label bypasses the session cache, so this re-observes the
    // home layout and must report the recorded exit.
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "known");
    assert_eq!(payload["place"]["slug"], "home");
    assert_eq!(payload.get("cache"), None);
    let exits = payload["known_exits"].as_array().unwrap();
    assert_eq!(exits.len(), 1, "fresh whereami must list the known exit");
    assert_eq!(exits[0]["to"], "search");
    assert_eq!(exits[0]["intent"], "open search");
    assert!(exits[0]["edge"].as_str().is_some());

    // Cache-hit path: the label-less rerun reuses the fresh session place and
    // must carry the same exits.
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "known");
    assert_eq!(payload["cache"]["hit"], true);
    let exits = payload["known_exits"].as_array().unwrap();
    assert_eq!(exits.len(), 1, "cached whereami must list the known exit");
    assert_eq!(exits[0]["to"], "search");
}

#[test]
fn whereami_reuses_fresh_verified_session_place() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(
        &bin,
        &["home", "home", "search", "home", "search", "home_changed"],
    );
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["go", "search"])
        .assert()
        .success();

    let count_after_go = fs::read_to_string(bin.join("android-count")).unwrap();
    assert_eq!(count_after_go, "5");

    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "known");
    assert_eq!(payload["place"]["slug"], "search");
    assert_eq!(payload["cache"]["hit"], true);
    assert_eq!(payload["metrics"]["layout_calls_total"], 0);
    let count_after_whereami = fs::read_to_string(bin.join("android-count")).unwrap();
    assert_eq!(count_after_whereami, "5");
}

#[test]
fn layout_reuses_fresh_verified_session_layout() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(
        &bin,
        &["home", "home", "search", "home", "search", "home_changed"],
    );
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["go", "search"])
        .assert()
        .success();

    let count_after_go = fs::read_to_string(bin.join("android-count")).unwrap();
    assert_eq!(count_after_go, "5");

    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["layout"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["cache"]["hit"], true);
    assert_eq!(payload["metrics"]["layout_calls_total"], 0);
    assert!(serde_json::to_string(&payload["layout"])
        .unwrap()
        .contains("Categories"));
    let count_after_layout = fs::read_to_string(bin.join("android-count")).unwrap();
    assert_eq!(count_after_layout, "5");
}

#[test]
fn layout_returns_redacted_layout_and_minimap_metadata() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["email"]);
    write_adb_script(&bin);
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["layout"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert!(payload["minimap"]["status"].as_str().is_some());
    let serialized = serde_json::to_string(&payload["layout"]).unwrap();
    assert!(!serialized.contains("alice@example.com"));
}

#[test]
fn doctor_reports_healthy_environment() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home"]);
    write_adb_script(&bin);
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["doctor"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["repo_ok"], true);
    assert_eq!(payload["device_ok"], true);
}

#[test]
fn doctor_unhealthy_when_config_invalid_exits_one() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    // Corrupt the config so the repo health check fails.
    fs::write(temp.path().join(".minimap/config.json"), "{ not json").unwrap();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home"]);
    write_adb_script(&bin);
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["doctor"])
        .assert()
        // config_error routes through exit_code_for_status -> 7, matching whereami/go.
        .code(7);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["repo_ok"], false);
    assert_eq!(payload["status"], "config_error");
}

#[test]
fn doctor_unhealthy_when_device_not_ready_exits_one() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home"]);
    write_adb_script_offline(&bin);
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["doctor"])
        .assert()
        // config_error routes through exit_code_for_status -> 7, matching whereami/go.
        .code(7);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["device_ok"], false);
    // The repo itself is fine; only the device readiness check fails.
    assert_eq!(payload["repo_ok"], true);
}

#[test]
fn scroll_on_same_known_place_records_no_edge() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "home", "home"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["scroll", "--direction", "down"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["changed_graph"], false);
    // Staying on the same known place must not commit a navigation edge.
    assert_eq!(edge_count(temp.path()), 0);
}

#[test]
fn scroll_between_known_places_records_direction_edge() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "search", "home", "search"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "search"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["scroll", "--direction", "down"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["from"], "home");
    assert_eq!(payload["data"]["to"], "search");

    let edges = edge_files(temp.path());
    assert_eq!(edges.len(), 1);
    let edge = read_json_path(&edges[0]);
    assert_eq!(edge["from"]["slug"], "home");
    assert_eq!(edge["to"]["slug"], "search");
    let step = &edge["recipe"][0];
    assert_eq!(step["kind"], "scroll");
    // The navigation identity is the direction, not any swipe coordinates.
    assert_eq!(step["direction"], "down");
    assert_eq!(step["point"], Value::Null);
    assert_eq!(step["selector"], Value::Null);
}

#[test]
fn back_between_known_places_records_press_back_edge() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "search", "search", "home"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "search"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["back"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["from"], "search");
    assert_eq!(payload["data"]["to"], "home");

    let edges = edge_files(temp.path());
    assert_eq!(edges.len(), 1);
    let edge = read_json_path(&edges[0]);
    assert_eq!(edge["from"]["slug"], "search");
    assert_eq!(edge["to"]["slug"], "home");
    assert_eq!(edge["recipe"][0]["kind"], "press_back");
}

#[test]
fn tap_point_records_geometry_edge_with_point_and_viewport() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--point",
            "150,300",
            "--label",
            "search",
            "--reason",
            "open search",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["to"], "search");

    let edges = edge_files(temp.path());
    assert_eq!(edges.len(), 1);
    let step = &read_json_path(&edges[0])["recipe"][0];
    assert_eq!(step["kind"], "tap");
    assert_eq!(step["point"], json!({"x": 150, "y": 300}));
    assert_eq!(step["viewport"], json!({"width": 1080, "height": 2400}));
}

#[test]
fn go_refuses_geometry_edge_at_mismatched_viewport() {
    // A geometry tap is recorded at one viewport; replaying it at a different
    // viewport must NOT replay raw pixels. The path planner pre-filters the
    // viewport-incompatible geometry edge, so `go` reports no_compatible_path
    // (the defence-in-depth `action_failed` guard inside execute_recipe is
    // unreachable from the CLI because plan and replay share one adb/viewport).
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search", "home"]);
    write_adb_script_with_size(&bin, "1080x2400");

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["tap", "--point", "150,300", "--label", "search"])
        .assert()
        .success();
    // Re-orient to home so the session start point is home (not the search
    // destination), forcing `go` to plan the recorded geometry edge.
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();

    let edges_before = edge_count(temp.path());
    // Now the device reports a different viewport.
    write_adb_script_with_size(&bin, "720x1600");
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["go", "search"])
        .assert()
        .code(5);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "no_compatible_path");
    assert_eq!(payload["data"]["changed_graph"], false);
    assert_eq!(edge_count(temp.path()), edges_before);
}

#[test]
fn tap_point_without_display_size_is_environment_error() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home"]);
    write_adb_script_no_display(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["tap", "--point", "150,300", "--label", "search"])
        .assert()
        .code(6);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "environment_error");
    assert!(!temp
        .path()
        .join(".minimap/graph/places/place_search.json")
        .exists());
    assert_eq!(edge_count(temp.path()), 0);
}

#[test]
fn whereami_relabel_preserves_baseline_and_repoints_edges() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    // alpha establishes the baseline; alpha_drift is a still-matching variant
    // observed during the relabel; beta is an edge destination off of alpha.
    write_android_layout_script(&bin, &["alpha", "alpha", "beta", "alpha_drift"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "alpha"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "testTag=alpha_btn",
            "--label",
            "beta",
            "--reason",
            "open beta",
        ])
        .assert()
        .success();

    // Capture the original baseline hash before relabel.
    let alpha_before = read_json_path(&temp.path().join(".minimap/graph/places/place_alpha.json"));
    let original_hash = alpha_before["baseline"]["identity_hash"]
        .as_str()
        .unwrap()
        .to_string();

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "homescreen"])
        .assert()
        .success();

    // Old place id removed; new slug present.
    assert!(!temp
        .path()
        .join(".minimap/graph/places/place_alpha.json")
        .exists());
    let relabeled = read_json_path(
        &temp
            .path()
            .join(".minimap/graph/places/place_homescreen.json"),
    );
    // KEY: baseline preserved, drift recorded as a variant (NOT overwriting it).
    assert_eq!(relabeled["baseline"]["identity_hash"], original_hash);
    let variants = relabeled["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 1);
    assert_ne!(variants[0]["identity_hash"], json!(original_hash));
    let baseline_text = serde_json::to_string(&relabeled["baseline"]).unwrap();
    let variants_text = serde_json::to_string(&relabeled["variants"]).unwrap();
    assert!(!baseline_text.contains("Status updated"));
    assert!(variants_text.contains("Status updated"));

    // The edge was repointed from the old slug to the new one.
    let edges = edge_files(temp.path());
    assert_eq!(edges.len(), 1);
    let edge = read_json_path(&edges[0]);
    assert_eq!(edge["from"]["slug"], "homescreen");
    assert_eq!(edge["from"]["id"], "place_homescreen");
    assert_eq!(edge["to"]["slug"], "beta");
}

#[test]
fn tap_label_reaching_a_different_known_place_is_label_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "settings", "home", "settings"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "settings"])
        .assert()
        .success();
    // Tap from home asking for "home" but the post layout is the known settings.
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["tap", "--selector", "text=SEARCH", "--label", "home"])
        .assert()
        .code(2);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "label_mismatch");
    assert_eq!(payload["data"]["observed"], "settings");
    assert_eq!(payload["data"]["requested_label"], "home");
    assert_eq!(payload["data"]["changed_graph"], false);
    assert_eq!(edge_count(temp.path()), 0);
}

#[test]
fn whereami_label_collision_is_mismatch_but_allow_duplicate_suffixes_a_new_place() {
    // A NEW, distinct fingerprint (`settings`) whose requested label slug
    // ("home") is already owned by a DIFFERENT place. By default this is a
    // label_mismatch with no write; under --allow-duplicate-label the new place
    // is committed with the smallest free numeric suffix (place_home-2). This
    // locks the Tranche C collision policy: slugs never silently merge two
    // distinct screens.
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    // home (label home) -> settings (label home, collision) x2 for the retry.
    write_android_layout_script(&bin, &["home", "settings", "settings"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();

    // Default: the distinct settings screen labeled "home" is a label_mismatch.
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .code(2);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "label_mismatch");
    assert!(
        !temp
            .path()
            .join(".minimap/graph/places/place_home-2.json")
            .exists(),
        "no suffixed place should be written without the flag"
    );

    // With --allow-duplicate-label: commit a fresh suffixed place_home-2.
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home", "--allow-duplicate-label"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["place"]["slug"], "home-2");
    assert_eq!(payload["changed_graph"], true);
    assert!(temp
        .path()
        .join(".minimap/graph/places/place_home.json")
        .exists());
    assert!(temp
        .path()
        .join(".minimap/graph/places/place_home-2.json")
        .exists());
}

#[test]
fn tap_unknown_destination_keeps_pending_out_of_minimap_tree() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "search"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--reason",
            "open search",
        ])
        .assert()
        .code(5);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "needs_label");
    assert!(!temp
        .path()
        .join(".minimap/graph/places/place_search.json")
        .exists());
    assert_eq!(edge_count(temp.path()), 0);
    // The unconfirmed pending transition must never be written under .minimap.
    let stray = find_files_containing(&temp.path().join(".minimap"), "pending");
    assert!(
        stray.is_empty(),
        "found pending state in .minimap: {stray:?}"
    );
}

#[test]
fn go_with_broken_selector_edge_fails_without_writing_graph() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    // nav -> other via a tap on testTag=go_search. nav_drift still matches nav
    // (known_changed) but the go_search node is gone, so the selector cannot
    // resolve on replay.
    write_android_layout_script(&bin, &["nav", "nav", "other", "nav_drift"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "nav"])
        .assert()
        .success();
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "testTag=go_search",
            "--label",
            "other",
            "--reason",
            "open other",
        ])
        .assert()
        .success();
    // Re-orient onto the drifted nav layout (records it as a nav variant and
    // points the session at nav with the drifted layout cached).
    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "nav"])
        .assert()
        .success();

    let edges_before = edge_count(temp.path());
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["go", "other"])
        .assert()
        .code(2);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "action_failed");
    assert_eq!(payload["data"]["changed_graph"], false);
    assert_eq!(edge_count(temp.path()), edges_before);
}

#[test]
fn tap_into_permission_overlay_is_blocked_without_writing_graph() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home", "permission"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--selector",
            "text=SEARCH",
            "--reason",
            "open search",
        ])
        .assert()
        .code(2);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "blocked_by_overlay");
    assert_eq!(payload["data"]["reason"], "permission_dialog");
    assert_eq!(payload["data"]["changed_graph"], false);
    assert_eq!(edge_count(temp.path()), 0);
    // Only the home place exists; the overlay must not become a place.
    assert_eq!(place_count(temp.path()), 1);
    let stray = find_files_containing(&temp.path().join(".minimap"), "pending");
    assert!(
        stray.is_empty(),
        "overlay must not leave pending state: {stray:?}"
    );
}

#[test]
fn layout_diff_and_plain_layout_report_distinct_kinds() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home"]);
    write_adb_script(&bin);

    let diff_output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["layout", "--diff"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let diff: Value = serde_json::from_slice(&diff_output).unwrap();
    assert_eq!(diff["kind"], "android_layout_diff");
    // Diffs carry no orientation/cache state.
    assert_eq!(diff["minimap"]["orientation"], "unavailable_for_diff");
    assert_eq!(diff["minimap"]["place"], Value::Null);

    let plain_output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["layout"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let plain: Value = serde_json::from_slice(&plain_output).unwrap();
    assert_eq!(plain["kind"], "android_layout");
    // Plain layout carries orientation metadata (a fingerprint identity hash).
    assert!(plain["minimap"]["match"]["identity_hash"]
        .as_str()
        .is_some());
    assert!(plain["minimap"]["status"].as_str().is_some());
}

#[test]
fn whereami_clears_stale_session_pointing_at_missing_place() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home", "home"]);
    write_adb_script(&bin);

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    // Remove the place the fresh session points at, leaving a dangling session.
    fs::remove_file(temp.path().join(".minimap/graph/places/place_home.json")).unwrap();

    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami"])
        .assert()
        .code(5)
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    // The stale session is dropped and a fresh observation finds no place.
    assert_eq!(payload["status"], "unknown");
    assert_eq!(payload.get("cache"), None);
    // A real layout call was made (count advanced past the first whereami).
    let count = fs::read_to_string(bin.join("android-count")).unwrap();
    assert_eq!(count, "2");
}

#[test]
fn serial_flag_threads_serial_through_every_adb_and_android_call() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    // Both fakes exit 1 unless the serial arrives: adb requires `-s` before
    // every subcommand and android requires `--device=` on layout, so any
    // serial-less call fails the whole flow.
    write_android_layout_script_expect_serial(&bin, &["home", "home", "search"], "emulator-5554");
    write_adb_script_expect_serial(&bin, "emulator-5554");

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home", "--serial", "emulator-5554"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "tap",
            "--point",
            "150,300",
            "--label",
            "search",
            "--serial",
            "emulator-5554",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["from"], "home");
    assert_eq!(payload["data"]["to"], "search");
}

#[test]
fn android_serial_env_threads_serial_like_the_flag() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script_expect_serial(&bin, &["home", "home", "search"], "emulator-5554");
    write_adb_script_expect_serial(&bin, "emulator-5554");

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .env("ANDROID_SERIAL", "emulator-5554")
        .args(["whereami", "--label", "home"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .env("ANDROID_SERIAL", "emulator-5554")
        .args(["tap", "--point", "150,300", "--label", "search"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["data"]["to"], "search");
}

#[test]
fn configured_serial_short_circuits_get_serialno_for_session_cache() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script_expect_serial(&bin, &["home", "home"], "emulator-5554");
    // This fake's `get-serialno` always exits 1: the session cache can only
    // work if the configured serial is returned without shelling out.
    write_adb_script_expect_serial(&bin, "emulator-5554");

    minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--label", "home", "--serial", "emulator-5554"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["whereami", "--serial", "emulator-5554"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "known");
    assert_eq!(payload["place"]["slug"], "home");
    assert_eq!(payload["cache"]["hit"], true);
    assert_eq!(payload["metrics"]["layout_calls_total"], 0);
    // No second Android layout capture happened.
    let count = fs::read_to_string(bin.join("android-count")).unwrap();
    assert_eq!(count, "1");
}

#[test]
fn doctor_flags_multiple_devices_without_serial_and_targets_one_with_serial() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let bin = fake_bin(temp.path());
    write_android_layout_script(&bin, &["home"]);
    write_adb_script_two_devices(&bin);

    // No serial resolved: two attached devices are a config error with a hint.
    let assertion = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["doctor"])
        .assert()
        // config_error routes through exit_code_for_status -> 7, matching whereami/go.
        .code(7);
    let payload: Value = serde_json::from_slice(&assertion.get_output().stdout).unwrap();
    assert_eq!(payload["status"], "config_error");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["device_ok"], false);
    assert_eq!(payload["repo_ok"], true);
    let device = &payload["checks"]["environment"][2];
    assert_eq!(device["name"], "device");
    assert_eq!(device["status"], "fail");
    assert_eq!(
        device["hint"],
        "multiple devices attached; pass --serial or set ANDROID_SERIAL"
    );

    // Targeting one device by serial restores a healthy doctor.
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["doctor", "--serial", "emulator-5554"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["device_ok"], true);
    let device = &payload["checks"]["environment"][2];
    assert_eq!(device["status"], "pass");
    assert_eq!(device["serial"], "emulator-5554");
    assert!(device.get("hint").is_none());
}

fn minimap(cwd: &Path) -> Command {
    let mut command = Command::cargo_bin("minimap").unwrap();
    command.current_dir(cwd);
    command.env("MINIMAP_ACTION_SETTLE_MS", "0");
    // Keep an ambient ANDROID_SERIAL on the host from leaking into tests that
    // exercise the serial-less default behavior.
    command.env_remove("ANDROID_SERIAL");
    command
}

fn fake_bin(root: &Path) -> PathBuf {
    let bin = root.join("fake-bin");
    fs::create_dir_all(&bin).unwrap();
    bin
}

fn prepend_path(bin: &Path) -> String {
    let old = std::env::var("PATH").unwrap_or_default();
    format!("{}:{old}", bin.display())
}

fn write_android_layout_script(bin: &Path, sequence: &[&str]) {
    write_android_layout_script_with_guard(bin, sequence, "");
}

/// Fake `android` that exits 1 unless `layout` invocations carry
/// `--device=<serial>`, proving the configured serial reaches the layout CLI.
fn write_android_layout_script_expect_serial(bin: &Path, sequence: &[&str], serial: &str) {
    let guard = format!(
        r#"if [ "$1" = "layout" ]; then
  FOUND=no
  for ARG in "$@"; do
    if [ "$ARG" = "--device={serial}" ]; then FOUND=yes; fi
  done
  if [ "$FOUND" != "yes" ]; then
    echo "expected --device={serial} on android layout, got: $*" >&2
    exit 1
  fi
fi
"#
    );
    write_android_layout_script_with_guard(bin, sequence, &guard);
}

fn write_android_layout_script_with_guard(bin: &Path, sequence: &[&str], guard: &str) {
    let sequence = sequence.join(" ");
    let body = format!(
        r#"#!/bin/sh
{guard}COUNT_FILE="$(dirname "$0")/android-count"
COUNT=0
if [ -f "$COUNT_FILE" ]; then COUNT=$(cat "$COUNT_FILE"); fi
COUNT=$((COUNT + 1))
printf "%s" "$COUNT" > "$COUNT_FILE"
ITEM="$(printf '{sequence}' | cut -d ' ' -f "$COUNT")"
if [ -z "$ITEM" ]; then ITEM="$(printf '{sequence}' | awk '{{print $NF}}')"; fi
if [ "$1" = "layout" ]; then
  case "$ITEM" in
    home)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"HOME"}},{{"class":"Button","text":"SEARCH","bounds":{{"left":100,"top":200,"right":300,"bottom":400}}}}]}}'
      ;;
    search)
      printf '{{"class":"Column","children":[{{"class":"Button","text":"HOME","bounds":{{"left":10,"top":20,"right":100,"bottom":120}}}},{{"class":"Text","text":"Categories"}},{{"class":"Text","text":"Lifestyles"}}]}}'
      ;;
    home_geo)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"HOME"}},{{"class":"Button","text":"SEARCH","center":"[540,2200]","bounds":"[480,2100][600,2300]"}}]}}'
      ;;
    search_geo)
      printf '{{"class":"Column","children":[{{"class":"Button","text":"HOME","center":"[100,8000]","bounds":"[0,7900][200,8100]"}},{{"class":"Text","text":"Categories"}},{{"class":"Text","text":"Lifestyles"}}]}}'
      ;;
    home_changed)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"HOME"}},{{"class":"Text","text":"SEARCH"}},{{"class":"Text","text":"Featured today"}}]}}'
      ;;
    settings)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"Settings"}},{{"class":"Text","text":"Account"}},{{"class":"Text","text":"Notifications"}}]}}'
      ;;
    permission)
      printf '{{"class":"FrameLayout","children":[{{"class":"Button","resource-id":"com.android.permissioncontroller:id/permission_allow_button","text":"Allow"}},{{"class":"Button","resource-id":"com.android.permissioncontroller:id/permission_deny_button","text":"Deny"}}]}}'
      ;;
    alpha)
      printf '{{"class":"Column","children":[{{"class":"Button","testTag":"alpha_btn","text":"Alpha","bounds":{{"left":100,"top":200,"right":300,"bottom":400}}}},{{"class":"Text","text":"Welcome"}},{{"class":"Text","text":"Status"}}]}}'
      ;;
    alpha_drift)
      printf '{{"class":"Column","children":[{{"class":"Button","testTag":"alpha_btn","text":"Alpha","bounds":{{"left":100,"top":200,"right":300,"bottom":400}}}},{{"class":"Text","text":"Welcome"}},{{"class":"Text","text":"Status updated"}}]}}'
      ;;
    beta)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"Beta page"}},{{"class":"Text","text":"Detail"}}]}}'
      ;;
    nav)
      printf '{{"class":"Column","children":[{{"class":"Button","testTag":"s1"}},{{"class":"Button","testTag":"s2"}},{{"class":"Button","testTag":"s3"}},{{"class":"Button","testTag":"s4"}},{{"class":"Button","testTag":"s5"}},{{"class":"Button","testTag":"go_search","bounds":{{"left":100,"top":200,"right":300,"bottom":400}}}},{{"class":"Text","text":"Nav"}}]}}'
      ;;
    nav_drift)
      printf '{{"class":"Column","children":[{{"class":"Button","testTag":"s1"}},{{"class":"Button","testTag":"s2"}},{{"class":"Button","testTag":"s3"}},{{"class":"Button","testTag":"s4"}},{{"class":"Button","testTag":"s5"}},{{"class":"Text","text":"Nav"}}]}}'
      ;;
    other)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"Other page"}},{{"class":"Text","text":"Body"}}]}}'
      ;;
    blank)
      printf '[]'
      ;;
    email)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"alice@example.com"}}]}}'
      ;;
    *)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"HOME"}},{{"class":"Button","text":"SEARCH","bounds":{{"left":100,"top":200,"right":300,"bottom":400}}}}]}}'
      ;;
  esac
  exit 0
fi
if [ "$1" = "screen" ] && [ "$2" = "capture" ]; then
  exit 0
fi
if [ "$1" = "screen" ] && [ "$2" = "resolve" ]; then
  printf 'input tap 200 300'
  exit 0
fi
exit 2
"#
    );
    write_executable(&bin.join("android"), &body);
}

fn write_adb_script(bin: &Path) {
    write_adb_script_with_size(bin, "1080x2400");
}

/// Fake `adb` whose `wm size` reports a caller-chosen viewport. Used to record a
/// geometry edge at one viewport and replay it at another (see the viewport
/// mismatch test).
fn write_adb_script_with_size(bin: &Path, size: &str) {
    let body = format!(
        r#"#!/bin/sh
if [ "$1" = "get-state" ]; then
  printf 'device\n'
  exit 0
fi
if [ "$1" = "get-serialno" ]; then
  printf 'fake-serial\n'
  exit 0
fi
if [ "$1" = "shell" ] && [ "$2" = "wm" ] && [ "$3" = "size" ]; then
  printf 'Physical size: {size}\n'
  exit 0
fi
if [ "$1" = "shell" ] && [ "$2" = "input" ]; then
  exit 0
fi
exit 2
"#
    );
    write_executable(&bin.join("adb"), &body);
}

/// Fake `adb` that cannot report a display size (so `display_size()` fails). Taps
/// still succeed; only `wm size` is unavailable. Used to exercise the geometry
/// tap `environment_error` path.
fn write_adb_script_no_display(bin: &Path) {
    write_executable(
        &bin.join("adb"),
        r#"#!/bin/sh
if [ "$1" = "get-state" ]; then
  printf 'device\n'
  exit 0
fi
if [ "$1" = "get-serialno" ]; then
  printf 'fake-serial\n'
  exit 0
fi
if [ "$1" = "shell" ] && [ "$2" = "wm" ] && [ "$3" = "size" ]; then
  printf 'wm size unavailable\n'
  exit 1
fi
if [ "$1" = "shell" ] && [ "$2" = "input" ]; then
  exit 0
fi
exit 2
"#,
    );
}

/// Fake `adb` that exits 1 unless every invocation starts with `-s <serial>`,
/// proving the serial is threaded into each adb call. `get-serialno` always
/// fails so a configured serial must short-circuit it (cache paths included).
fn write_adb_script_expect_serial(bin: &Path, serial: &str) {
    let body = format!(
        r#"#!/bin/sh
if [ "$1" != "-s" ] || [ "$2" != "{serial}" ]; then
  echo "expected -s {serial}, got: $*" >&2
  exit 1
fi
shift 2
if [ "$1" = "get-state" ]; then
  printf 'device\n'
  exit 0
fi
if [ "$1" = "get-serialno" ]; then
  echo "get-serialno must never run when a serial is configured" >&2
  exit 1
fi
if [ "$1" = "shell" ] && [ "$2" = "wm" ] && [ "$3" = "size" ]; then
  printf 'Physical size: 1080x2400\n'
  exit 0
fi
if [ "$1" = "shell" ] && [ "$2" = "input" ]; then
  exit 0
fi
exit 2
"#
    );
    write_executable(&bin.join("adb"), &body);
}

/// Fake `adb` with two attached devices: bare `get-state` fails the way real
/// adb does with multiple devices, while `-s emulator-5554 get-state` succeeds.
fn write_adb_script_two_devices(bin: &Path) {
    write_executable(
        &bin.join("adb"),
        r#"#!/bin/sh
if [ "$1" = "devices" ]; then
  printf 'List of devices attached\nemulator-5554\tdevice\nemulator-5556\tdevice\n'
  exit 0
fi
if [ "$1" = "-s" ] && [ "$2" = "emulator-5554" ] && [ "$3" = "get-state" ]; then
  printf 'device\n'
  exit 0
fi
if [ "$1" = "get-state" ]; then
  echo 'adb: more than one device/emulator' >&2
  exit 1
fi
exit 2
"#,
    );
}

/// Fake `adb` whose device is not in the `device` state (e.g. unauthorized).
/// Used to drive the unhealthy `doctor` path.
fn write_adb_script_offline(bin: &Path) {
    write_executable(
        &bin.join("adb"),
        r#"#!/bin/sh
if [ "$1" = "get-state" ]; then
  printf 'unauthorized\n'
  exit 1
fi
if [ "$1" = "get-serialno" ]; then
  printf 'fake-serial\n'
  exit 0
fi
exit 2
"#,
    );
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn read_json_path(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn edge_files(root: &Path) -> Vec<PathBuf> {
    json_files_in(&root.join(".minimap/graph/edges"))
}

fn edge_count(root: &Path) -> usize {
    edge_files(root).len()
}

fn place_count(root: &Path) -> usize {
    json_files_in(&root.join(".minimap/graph/places")).len()
}

fn json_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect()
}

/// Recursively collect any files under `dir` whose name contains `needle`.
fn find_files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(find_files_containing(&path, needle));
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.contains(needle))
            .unwrap_or(false)
        {
            found.push(path);
        }
    }
    found
}
