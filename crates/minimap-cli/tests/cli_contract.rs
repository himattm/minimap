use assert_cmd::Command;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn init_dry_run_reports_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let output = minimap(temp.path())
        .args(["init", "--dry-run", "--agents", "all"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["ok"], true);
    assert_eq!(payload["dry_run"], true);
    assert!(!temp.path().join(".minimap").exists());
    assert!(payload["skill_paths"]
        .as_array()
        .unwrap()
        .contains(&json!(".agents/skills/minimap-app-navigation/SKILL.md")));
    assert!(payload["skill_paths"]
        .as_array()
        .unwrap()
        .contains(&json!(".agents/skills/minimap-first-run-mapping/SKILL.md")));
}

#[test]
fn init_does_not_create_legacy_directories() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    assert!(!temp.path().join(".minimap/runs").exists());
    assert!(!temp.path().join(".minimap/state").exists());
    assert!(!temp.path().join(".minimap/checks").exists());
    assert!(!temp.path().join(".minimap/current.json").exists());
    assert!(temp.path().join(".minimap/graph/screens").is_dir());
    assert!(temp.path().join(".minimap/graph/edges").is_dir());
    assert!(temp.path().join(".minimap/routes").is_dir());
    assert!(temp.path().join(".minimap/proposals").is_dir());
}

#[test]
fn init_creates_empty_journal_jsonl() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let journal = temp.path().join(".minimap/journal.jsonl");
    assert!(journal.exists(), "journal.jsonl should exist after init");
    let bytes = fs::read(&journal).unwrap();
    assert!(bytes.is_empty(), "journal.jsonl should be empty after init");
}

#[test]
fn init_refuses_legacy_minimap_tree() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".minimap/runs")).unwrap();
    let assertion = minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .code(2);
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    let payload: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(payload["status"], "config_error");
    let summary = payload["summary"].as_str().unwrap_or_default();
    assert!(
        summary.contains("minimap 0.1.x"),
        "legacy summary should mention 0.1.x, got: {summary}"
    );
    assert!(
        summary.contains("--force"),
        "legacy summary should mention --force, got: {summary}"
    );
    let legacy_paths = payload["legacy_paths"].as_array().unwrap();
    assert!(legacy_paths.iter().any(|value| value == ".minimap/runs"));
    // init must not have created the new tree on top of the legacy refusal.
    assert!(!temp.path().join(".minimap/graph/screens").exists());
}

#[test]
fn init_force_overwrites_legacy_tree() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".minimap/runs")).unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex", "--force"])
        .assert()
        .success();
    assert!(temp.path().join(".minimap/graph/screens").is_dir());
    assert!(temp.path().join(".minimap/journal.jsonl").exists());
}

#[test]
fn doctor_passes_on_fresh_init_with_journal_writable() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    let output = minimap(temp.path())
        .args(["doctor"])
        .assert()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    let checks = payload["checks"].as_array().unwrap();
    let journal_check = checks
        .iter()
        .find(|check| check["name"] == "journal_writable")
        .expect("doctor should emit a journal_writable check");
    assert_eq!(journal_check["status"], "ok");
    let graph_dirs = checks
        .iter()
        .find(|check| check["name"] == "graph_dirs")
        .expect("doctor should emit a graph_dirs check");
    assert_eq!(graph_dirs["status"], "pass");
}

#[test]
fn doctor_warns_when_graph_not_git_tracked() {
    let temp = tempfile::tempdir().unwrap();
    minimap(temp.path())
        .args(["init", "--agents", "codex"])
        .assert()
        .success();
    // Git doesn't track empty directories, and `init` produces empty graph/routes dirs.
    // So an init'd repo with only a baseline commit will not have any graph files tracked.
    init_git_repo_with_baseline(temp.path());
    let output = minimap(temp.path())
        .args(["doctor"])
        .assert()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    let checks = payload["checks"].as_array().unwrap();
    let graph_tracked = checks
        .iter()
        .find(|check| check["name"] == "graph_tracked")
        .expect("doctor should emit a graph_tracked check");
    assert_eq!(graph_tracked["status"], "warn");
    assert!(
        payload["hint"]
            .as_str()
            .unwrap_or_default()
            .contains("git add .minimap/graph .minimap/routes"),
        "doctor should surface the git add hint when graph is untracked, got: {payload}"
    );
}

#[test]
fn claude_plugin_marketplace_declares_minimap_skills() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let marketplace = read_json_path(&repo.join(".claude-plugin/marketplace.json"));
    assert_eq!(marketplace["name"], "minimap");
    assert_eq!(marketplace["owner"]["name"], "Matt McKenna");
    assert_eq!(marketplace["plugins"][0]["name"], "minimap");
    assert_eq!(marketplace["plugins"][0]["author"]["name"], "Matt McKenna");
    assert_eq!(
        marketplace["plugins"][0]["source"],
        "./plugins/minimap-claude-code"
    );

    let plugin =
        read_json_path(&repo.join("plugins/minimap-claude-code/.claude-plugin/plugin.json"));
    assert_eq!(plugin["name"], "minimap");
    assert_eq!(plugin["author"]["name"], "Matt McKenna");

    let app_nav_skill = fs::read_to_string(
        repo.join("plugins/minimap-claude-code/skills/minimap-app-navigation/SKILL.md"),
    )
    .unwrap();
    assert!(app_nav_skill.contains("map the whole app"));
    assert!(app_nav_skill.contains("first-run mapping"));
    assert!(app_nav_skill.contains("token-intensive"));

    assert!(!repo
        .join("plugins/minimap-claude-code/skills/minimap-first-run-mapping")
        .exists());
}

#[test]
#[ignore = "TODO(phase-2): route-level context_guard was removed from the Route schema; this test asserts the old behavior."]
fn route_reports_context_mismatch_exit_code() {
    let temp = tempfile::tempdir().unwrap();
    write_json(
        &temp.path().join(".minimap/config.json"),
        &json!({
            "schema_version": "minimap.config.v1",
            "context": {"auth_state": "logged_out"}
        }),
    );
    write_json(
        &temp
            .path()
            .join(".minimap/routes/open-account.minimap.json"),
        &json!({
            "schema_version": "minimap.route.v1",
            "name": "open-account",
            "start": {"screen": "home", "context_guard": {"auth_state": "logged_in"}},
            "target": {"screen": "account"}
        }),
    );
    let output = minimap(temp.path())
        .args(["route", "open-account", "--current-screen", "home"])
        .assert()
        .code(8)
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "context_mismatch");
}

#[test]
fn layout_diff_uses_fake_android_cli() {
    let temp = tempfile::tempdir().unwrap();
    let bin = fake_bin(temp.path());
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
if [ "$1" = "layout" ] && [ "$2" = "--diff" ]; then
  printf '{"changed":[{"text":"Settings"}]}'
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["layout", "--diff"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["kind"], "android_layout_diff");
    assert_eq!(payload["diff_scope"], "android_in_session");
}

#[test]
fn tap_coordinate_journals_without_growing_graph() {
    let temp = tempfile::tempdir().unwrap();
    let bin = fake_bin(temp.path());
    // The atomic tap fetches a pre-layout for from-screen classification before executing.
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
if [ "$1" = "layout" ]; then
  printf '{"class":"Column","children":[]}'
  exit 0
fi
exit 2
"#,
    );
    write_executable(
        &bin.join("adb"),
        r#"#!/bin/sh
if [ "$1" = "shell" ] && [ "$2" = "input" ] && [ "$3" = "tap" ]; then
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["tap", "--point", "540,1200", "--reason", "header"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["outcome"], "coord_journal_only");
    assert!(!temp.path().join(".minimap/graph/edges").exists()
        || fs::read_dir(temp.path().join(".minimap/graph/edges"))
            .map(|entries| entries.count())
            .unwrap_or(0)
            == 0);
    let journal = fs::read_to_string(temp.path().join(".minimap/journal.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 1);
    assert!(journal.contains("coord_journal_only"));
}

#[test]
fn go_executes_route_edges_with_fake_android_and_adb() {
    let temp = tempfile::tempdir().unwrap();
    write_json(
        &temp
            .path()
            .join(".minimap/graph/screens/screen_article_detail.json"),
        &json!({
            "schema_version": "minimap.screen.v1",
            "id": "screen_article_detail",
            "name": "article-detail",
            "identity_hash": "sha256:not-fast-path",
            "normalized": {
                "schema_version": "minimap.normalized_layout.v1",
                "elements": [
                    {"role": "Column", "clickable": false, "enabled": true, "path": "0", "sibling_bucket": 0},
                    {"role": "Text", "clickable": false, "enabled": true, "path": "0/0", "sibling_bucket": 0, "text_class": "medium"}
                ],
                "role_distribution": {"Column": 1, "Text": 1},
                "element_count": 2
            }
        }),
    );
    write_json(
        &temp
            .path()
            .join(".minimap/graph/edges/edge_home_article.json"),
        &json!({
            "schema_version": "minimap.edge.v1",
            "id": "edge_home_article",
            "from_screen": "screen_home",
            "to_screen": "screen_article_detail",
            "action": {
                "kind": "tap",
                "selector_candidates": [
                    {"kind": "test_tag", "value": "read_article", "score": 0.92}
                ]
            }
        }),
    );
    write_json(
        &temp
            .path()
            .join(".minimap/routes/read-article.minimap.json"),
        &json!({
            "schema_version": "minimap.route.v1",
            "name": "read-article",
            "start": {"screen": "screen_home"},
            "target": {"screen": "screen_article_detail"},
            "preferred_edge_ids": ["edge_home_article"]
        }),
    );
    let bin = fake_bin(temp.path());
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
COUNT_FILE="$(dirname "$0")/android-count"
COUNT=0
if [ -f "$COUNT_FILE" ]; then COUNT=$(cat "$COUNT_FILE"); fi
COUNT=$((COUNT + 1))
printf "%s" "$COUNT" > "$COUNT_FILE"
if [ "$1" = "layout" ]; then
  if [ "$COUNT" = "1" ]; then
    printf '{"class":"Column","children":[{"testTag":"read_article","bounds":{"left":100,"top":200,"right":300,"bottom":400}}]}'
  else
    printf '{"class":"Column","children":[{"class":"Text","text":"Article body"}]}'
  fi
  exit 0
fi
exit 2
"#,
    );
    write_executable(
        &bin.join("adb"),
        r#"#!/bin/sh
if [ "$1" = "shell" ] && [ "$2" = "input" ] && [ "$3" = "tap" ] && [ "$4" = "200" ] && [ "$5" = "300" ]; then
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["go", "read-article", "--current-screen", "screen_home"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["summary"], "route executed");
    assert_eq!(payload["executed"][0]["edge"], "edge_home_article");
    assert_eq!(
        payload["executed"][0]["verification"]["matched_screen"],
        "screen_article_detail"
    );
    assert_eq!(payload["metrics"]["layout_calls_total"], 2);
    assert_eq!(payload["metrics"]["layout_json_returned_to_agent"], false);
    assert_eq!(payload["metrics"]["adb_taps_total"], 1);
}

#[test]
fn drift_passes_for_known_current_screen() {
    let temp = tempfile::tempdir().unwrap();
    write_settings_screen(temp.path());
    let bin = fake_bin(temp.path());
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
if [ "$1" = "layout" ]; then
  printf '{"class":"Column","children":[{"class":"Button","testTag":"settings","clickable":true}]}'
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["drift"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "passed");
    assert_eq!(
        payload["current_screen"]["matched_screen"],
        "screen_settings"
    );
}

#[test]
fn validate_reports_screen_unknown_and_stages_proposal() {
    let temp = tempfile::tempdir().unwrap();
    let bin = fake_bin(temp.path());
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
if [ "$1" = "layout" ]; then
  printf '{"class":"Column","children":[{"class":"Button","testTag":"unknown","clickable":true}]}'
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["validate"])
        .assert()
        .code(5)
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "screen_unknown");
    assert!(payload["drift"]["proposal_path"].as_str().is_some());
}

#[test]
fn validate_all_is_dry_by_default() {
    let temp = tempfile::tempdir().unwrap();
    write_home_to_article_graph(temp.path());
    let bin = fake_bin(temp.path());
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
if [ "$1" = "layout" ]; then
  printf '{"class":"Column","children":[{"class":"Button","testTag":"read_article","clickable":true,"bounds":{"left":100,"top":200,"right":300,"bottom":400}}]}'
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["validate", "--all", "--current-screen", "screen_home"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "passed");
    assert_eq!(payload["impact_analysis"]["precise"], false);
    assert!(payload["route_results"].is_null());
}

#[test]
fn validate_all_execute_runs_matching_route() {
    let temp = tempfile::tempdir().unwrap();
    write_home_to_article_graph(temp.path());
    let bin = fake_bin(temp.path());
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
COUNT_FILE="$(dirname "$0")/validate-count"
COUNT=0
if [ -f "$COUNT_FILE" ]; then COUNT=$(cat "$COUNT_FILE"); fi
COUNT=$((COUNT + 1))
printf "%s" "$COUNT" > "$COUNT_FILE"
if [ "$1" = "layout" ]; then
  if [ "$COUNT" = "1" ] || [ "$COUNT" = "2" ]; then
    printf '{"class":"Column","children":[{"class":"Button","testTag":"read_article","clickable":true,"bounds":{"left":100,"top":200,"right":300,"bottom":400}}]}'
  else
    printf '{"class":"Column","children":[{"class":"Text","text":"Article body"}]}'
  fi
  exit 0
fi
exit 2
"#,
    );
    write_executable(
        &bin.join("adb"),
        r#"#!/bin/sh
if [ "$1" = "shell" ] && [ "$2" = "input" ] && [ "$3" = "tap" ] && [ "$4" = "200" ] && [ "$5" = "300" ]; then
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args([
            "validate",
            "--all",
            "--execute",
            "--current-screen",
            "screen_home",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "passed");
    assert_eq!(payload["impact_analysis"]["precise"], true);
    assert_eq!(payload["route_results"][0]["route"], "read-article");
    assert_eq!(
        payload["route_results"][0]["result"]["final_screen"]["matched_screen"],
        "screen_article_detail"
    );
}

#[test]
fn route_define_writes_slim_route_json() {
    let temp = tempfile::tempdir().unwrap();
    write_settings_screen(temp.path());
    minimap(temp.path())
        .args([
            "route",
            "define",
            "open-settings",
            "--to",
            "screen_settings",
            "--triggers",
            "Settings*.kt,*Preferences*",
        ])
        .assert()
        .success();
    let route_path = temp
        .path()
        .join(".minimap/routes/open-settings.minimap.json");
    assert!(route_path.exists(), "route file should be written");
    let route: Value = serde_json::from_str(&fs::read_to_string(&route_path).unwrap()).unwrap();
    assert_eq!(route["schema_version"], "minimap.route.v1");
    assert_eq!(route["name"], "open-settings");
    assert_eq!(route["target"]["screen"], "screen_settings");
    assert!(route.get("from").is_none() || route["from"].is_null());
    let triggers = route["triggers"].as_array().unwrap();
    assert_eq!(triggers.len(), 2);
    assert_eq!(triggers[0], "Settings*.kt");
    assert_eq!(triggers[1], "*Preferences*");
    assert!(route["aliases"].as_array().unwrap().is_empty());
    // Phase 1 schema must not include legacy fields.
    assert!(route.get("preferred_edge_ids").is_none());
    assert!(route.get("allow_graph_fallback").is_none());
    assert!(route.get("path_constraints").is_none());
    assert!(route.get("checks").is_none());
}

#[test]
fn route_define_errors_when_target_screen_missing() {
    let temp = tempfile::tempdir().unwrap();
    write_settings_screen(temp.path());
    let assertion = minimap(temp.path())
        .args([
            "route",
            "define",
            "missing-target",
            "--to",
            "screen_does_not_exist",
        ])
        .assert()
        .failure();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    let payload: Value = serde_json::from_str(&stdout).unwrap();
    let error_msg = payload["data"]["error"]["message"]
        .as_str()
        .or_else(|| payload["summary"].as_str())
        .unwrap_or_default();
    assert!(
        error_msg.contains("screen_does_not_exist"),
        "expected error message to name the missing screen, got: {error_msg}"
    );
    assert!(!temp
        .path()
        .join(".minimap/routes/missing-target.minimap.json")
        .exists());
}

#[test]
fn screen_rename_updates_name_field() {
    let temp = tempfile::tempdir().unwrap();
    write_settings_screen(temp.path());
    minimap(temp.path())
        .args(["screen", "rename", "screen_settings", "Preferences"])
        .assert()
        .success();
    let screen: Value = serde_json::from_str(
        &fs::read_to_string(
            temp.path()
                .join(".minimap/graph/screens/screen_settings.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(screen["id"], "screen_settings");
    assert_eq!(screen["name"], "Preferences");
}

#[test]
fn screen_rename_does_not_touch_edges() {
    let temp = tempfile::tempdir().unwrap();
    write_home_to_article_graph(temp.path());
    let edge_path = temp
        .path()
        .join(".minimap/graph/edges/edge_home_article.json");
    let original_bytes = fs::read(&edge_path).unwrap();
    minimap(temp.path())
        .args(["screen", "rename", "screen_home", "landing"])
        .assert()
        .success();
    let after_bytes = fs::read(&edge_path).unwrap();
    assert_eq!(
        original_bytes, after_bytes,
        "rename should not modify edge files"
    );
}

#[test]
fn undo_drops_uncommitted_graph_changes() {
    let temp = tempfile::tempdir().unwrap();
    write_settings_screen(temp.path());
    init_git_repo_with_baseline(temp.path());
    let screen_path = temp
        .path()
        .join(".minimap/graph/screens/screen_settings.json");
    let original = fs::read_to_string(&screen_path).unwrap();
    fs::write(&screen_path, "{\"tampered\":true}\n").unwrap();
    let output = minimap(temp.path())
        .args(["undo"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert!(
        payload["summary"]
            .as_str()
            .unwrap_or_default()
            .contains("dropped"),
        "summary should mention dropped changes: {payload}"
    );
    let restored = fs::read_to_string(&screen_path).unwrap();
    assert_eq!(restored, original);
}

#[test]
fn undo_errors_outside_git_repo() {
    let temp = tempfile::tempdir().unwrap();
    let assertion = minimap(temp.path()).args(["undo"]).assert().failure();
    let stdout = String::from_utf8_lossy(&assertion.get_output().stdout).to_string();
    let payload: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(payload["status"], "config_error");
    assert!(payload["summary"]
        .as_str()
        .unwrap_or_default()
        .contains("not a git repo"));
}

#[test]
fn undo_reports_nothing_to_undo_when_clean() {
    let temp = tempfile::tempdir().unwrap();
    write_settings_screen(temp.path());
    init_git_repo_with_baseline(temp.path());
    let output = minimap(temp.path())
        .args(["undo"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["summary"], "nothing to undo");
    assert_eq!(payload["dropped"], 0);
}

#[test]
fn validate_screen_current_reports_matched_for_known_screen() {
    let temp = tempfile::tempdir().unwrap();
    write_settings_screen(temp.path());
    let bin = fake_bin(temp.path());
    write_executable(
        &bin.join("android"),
        r#"#!/bin/sh
if [ "$1" = "layout" ]; then
  printf '{"class":"Column","children":[{"class":"Button","testTag":"settings","clickable":true}]}'
  exit 0
fi
exit 2
"#,
    );
    let output = minimap(temp.path())
        .env("PATH", prepend_path(&bin))
        .args(["validate", "--screen", "current"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let payload: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(payload["status"], "matched");
    assert_eq!(
        payload["current_screen"]["matched_screen"],
        "screen_settings"
    );
}

fn minimap(dir: &Path) -> Command {
    let mut command = Command::cargo_bin("minimap").unwrap();
    command.current_dir(dir);
    command
}

fn init_git_repo_with_baseline(dir: &Path) {
    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("git available");
        assert!(
            status.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }
    git(dir, &["init", "--quiet", "--initial-branch=main"]);
    git(dir, &["add", "."]);
    git(dir, &["commit", "--quiet", "-m", "baseline"]);
}

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_string_pretty(value).unwrap()).unwrap();
}

fn read_json_path(path: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn write_settings_screen(root: &Path) {
    write_json(
        &root.join(".minimap/graph/screens/screen_settings.json"),
        &json!({
            "schema_version": "minimap.screen.v1",
            "id": "screen_settings",
            "name": "settings",
            "identity_hash": "sha256:not-fast-path",
            "normalized": {
                "schema_version": "minimap.normalized_layout.v1",
                "elements": [
                    {"role": "Column", "clickable": false, "enabled": true, "path": "0", "sibling_bucket": 0},
                    {"role": "Button", "clickable": true, "enabled": true, "path": "0/0", "sibling_bucket": 0, "resource_id": "settings"}
                ],
                "role_distribution": {"Button": 1, "Column": 1},
                "element_count": 2
            }
        }),
    );
}

fn write_home_to_article_graph(root: &Path) {
    write_json(
        &root.join(".minimap/graph/screens/screen_home.json"),
        &json!({
            "schema_version": "minimap.screen.v1",
            "id": "screen_home",
            "name": "home",
            "identity_hash": "sha256:not-fast-path",
            "normalized": {
                "schema_version": "minimap.normalized_layout.v1",
                "elements": [
                    {"role": "Column", "clickable": false, "enabled": true, "path": "0", "sibling_bucket": 0},
                    {"role": "Button", "clickable": true, "enabled": true, "path": "0/0", "sibling_bucket": 0, "resource_id": "read_article"}
                ],
                "role_distribution": {"Button": 1, "Column": 1},
                "element_count": 2
            }
        }),
    );
    write_json(
        &root.join(".minimap/graph/screens/screen_article_detail.json"),
        &json!({
            "schema_version": "minimap.screen.v1",
            "id": "screen_article_detail",
            "name": "article-detail",
            "identity_hash": "sha256:not-fast-path",
            "normalized": {
                "schema_version": "minimap.normalized_layout.v1",
                "elements": [
                    {"role": "Column", "clickable": false, "enabled": true, "path": "0", "sibling_bucket": 0},
                    {"role": "Text", "clickable": false, "enabled": true, "path": "0/0", "sibling_bucket": 0, "text_class": "medium"}
                ],
                "role_distribution": {"Column": 1, "Text": 1},
                "element_count": 2
            }
        }),
    );
    write_json(
        &root.join(".minimap/graph/edges/edge_home_article.json"),
        &json!({
            "schema_version": "minimap.edge.v1",
            "id": "edge_home_article",
            "from_screen": "screen_home",
            "to_screen": "screen_article_detail",
            "action": {
                "kind": "tap",
                "selector_candidates": [
                    {"kind": "test_tag", "value": "read_article", "score": 0.92}
                ]
            }
        }),
    );
    write_json(
        &root.join(".minimap/routes/read-article.minimap.json"),
        &json!({
            "schema_version": "minimap.route.v1",
            "name": "read-article",
            "from": {"screen": "screen_home"},
            "target": {"screen": "screen_article_detail"}
        }),
    );
}

fn fake_bin(root: &Path) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    bin
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }
}

fn prepend_path(bin: &Path) -> String {
    let current = std::env::var("PATH").unwrap_or_default();
    format!("{}:{current}", bin.display())
}
