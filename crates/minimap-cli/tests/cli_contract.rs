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

fn minimap(cwd: &Path) -> Command {
    let mut command = Command::cargo_bin("minimap").unwrap();
    command.current_dir(cwd);
    command.env("MINIMAP_ACTION_SETTLE_MS", "0");
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
    let sequence = sequence.join(" ");
    let body = format!(
        r#"#!/bin/sh
COUNT_FILE="$(dirname "$0")/android-count"
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
      printf '{{"class":"Column","children":[{{"class":"Button","text":"HOME","bounds":{{"left":10,"top":20,"right":100,"bottom":120}}}},{{"class":"Text","text":"SEARCH"}},{{"class":"Text","text":"Categories"}}]}}'
      ;;
    home_changed)
      printf '{{"class":"Column","children":[{{"class":"Text","text":"HOME"}},{{"class":"Text","text":"SEARCH"}},{{"class":"Text","text":"Featured today"}}]}}'
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
  printf 'Physical size: 1080x2400\n'
  exit 0
fi
if [ "$1" = "shell" ] && [ "$2" = "input" ]; then
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
