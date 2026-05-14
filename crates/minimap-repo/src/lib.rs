use anyhow::{Context, Result};
use minimap_schemas::{
    canonical_json, GraphContext, JournalEntry, NavigationEdge, Proposal, Route, ScreenNode,
    CONFIG_SCHEMA_VERSION, EDGE_SCHEMA_VERSION, PROPOSAL_SCHEMA_VERSION, ROUTE_SCHEMA_VERSION,
    SCREEN_SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_SKILL_NAME: &str = APP_NAVIGATION_SKILL_NAME;
pub const APP_NAVIGATION_SKILL_NAME: &str = "minimap-app-navigation";
pub const FIRST_RUN_MAPPING_SKILL_NAME: &str = "minimap-first-run-mapping";
pub const GITIGNORE_ENTRIES: &[&str] = &[".minimap/journal.jsonl"];

pub const MINIMAP_DIRS: &[&str] = &[
    ".minimap",
    ".minimap/graph",
    ".minimap/graph/screens",
    ".minimap/graph/edges",
    ".minimap/routes",
    ".minimap/proposals",
];

pub const LEGACY_MINIMAP_PATHS: &[&str] = &[
    ".minimap/runs",
    ".minimap/state",
    ".minimap/checks",
    ".minimap/current.json",
];

pub const LEGACY_MINIMAP_MESSAGE: &str = "this project was initialized under minimap 0.1.x \u{2014} the on-disk layout has changed.\nplease remove `.minimap/` and re-run `minimap init`.\n\n(use `minimap init --force` to overwrite anyway.)\n(graph and routes from the old format are not migrated. see CHANGELOG.md for details.)";

pub fn detect_legacy_minimap(root: &Path) -> Vec<String> {
    LEGACY_MINIMAP_PATHS
        .iter()
        .filter(|path| root.join(path).exists())
        .map(|path| path.to_string())
        .collect()
}

pub const APP_NAVIGATION_SKILL_BODY: &str = r#"---
name: minimap-app-navigation
description: Use in an Android codebase for any Minimap work — navigating the launched app, inspecting Android layout JSON, running android layout or android layout --diff, tapping UI elements, validating screens, learning routes, reusing known navigation, or growing the repo's Minimap graph one screen at a time even when no graph exists yet. Before calling android layout or raw adb tap commands directly, check Minimap first.
metadata:
  author: minimap
  version: "1.0"
---

# Minimap App Navigation Skill

Minimap is this repo's shared navigation memory and soft validation layer for AI agents working in this Android codebase.

Use Minimap before raw Android layout or adb tap commands. Stage learned graph updates, but do not accept or commit them without explicit user approval.

## Incremental mapping

Minimap graphs grow one screen at a time. An empty `.minimap/` after `minimap init` is normal — the graph fills in as the user navigates the app. Do not treat "no graph yet" as a reason to fall back to raw `android` or `adb` commands.

When the user asks you to navigate to a route Minimap does not yet know, treat that navigation itself as a chance to record the route. Run the lightweight loop below, stage a proposal, and surface the proposal id. Do not auto-`accept` — wait for user approval.

Lightweight loop for adding one screen:

```bash
minimap observe start <short-route-name>
minimap layout
minimap tap --selector "<kind>=<value>" --reason "<why>"
minimap layout
minimap observe stop
minimap learn --from-current-run --stage
```

Then report the proposal id and stop.

Selector preference (most stable first): test tag, resource id, accessibility/content description, stable visible text. Avoid coordinate taps unless nothing else is usable.

When the graph already has the route, reuse it: `minimap route`, `minimap go`, `minimap check`. Run `minimap drift` or `minimap validate --all` when verifying existing screens.

Always stage. Never `minimap accept` without explicit user approval.

## Prerequisites

The `minimap` CLI must be on `PATH`. Claude Code plugins cannot install binaries, so if `minimap --version` fails, ask the user to install it before continuing:

- Homebrew: `brew install himattm/minimap/minimap`
- Cargo: `cargo install minimap-cli`
- From source: `cargo install --git https://github.com/himattm/minimap minimap-cli`

`android` and `adb` must also be on `PATH` for any layout or tap commands.
"#;

pub const FIRST_RUN_MAPPING_SKILL_BODY: &str = r#"---
name: minimap-first-run-mapping
description: Use only when the user explicitly asks for a bounded bulk survey of the launched Android app — phrases like "map the whole app", "do first-run mapping", "bulk-map the app", "do an initial pass over <list of flows>", or "explore the app comprehensively." Do NOT fire on "use minimap", "this is a fresh repo", "navigate to X", "build the graph", or "record this route" — those are everyday incremental work and belong to minimap-app-navigation. For incremental mapping (one screen at a time as the user navigates), use minimap-app-navigation instead.
metadata:
  author: minimap
  version: "1.0"
---

# Minimap First-Run Mapping Skill

If you found this skill via a vague trigger like "use minimap on this app", "this repo has no .minimap yet", or "navigate to X", stop and use `minimap-app-navigation` instead — it handles incremental mapping and is the right tool for everyday Minimap work. This skill is only for bounded bulk surveys the user explicitly asked for.

Minimap first-run mapping does a deliberate bulk pass over a launched Android app to seed navigation memory across many flows at once. It is intentionally separate from everyday Minimap navigation because it is expensive: the agent must inspect Android layout JSON, decide what to tap, navigate the app, and record routes in a single sustained session.

Stage learned graph updates, but do not accept or commit them without explicit user approval.

## First-Run Mapping Mode

Use this mode only when the user has explicitly asked for a bulk survey: "map the whole app", "do first-run mapping", "bulk-map the app", "do an initial pass over <flows>", "explore the app comprehensively." Anything narrower — a single route, a fresh repo, "build the graph over time" — belongs to `minimap-app-navigation`.

Warn the user before starting: first-run mapping is token-intensive. Keep the run bounded by the user's requested scope. If no scope is given, map a small set of high-value flows first, then report what remains.

Bounded reasons to invoke this skill:
- The user explicitly requests a bulk initial app map.
- A new feature area needs broad coverage in one pass.
- A major UI redesign invalidated existing routes and a re-survey is requested.
- A separate app context (logged-out, logged-in, onboarding, permission-gated, feature-flagged) needs mapping.
- The user explicitly asks for additional route coverage across multiple flows.

Prerequisites:
- The Android app is already built, installed, launched, and on the screen where mapping should begin.
- `minimap`, `android`, and `adb` are on PATH. Claude Code plugins cannot install binaries, so if `minimap --version` fails, ask the user to install it first:
  - Homebrew: `brew install himattm/minimap/minimap`
  - Cargo: `cargo install minimap-cli`
  - From source: `cargo install --git https://github.com/himattm/minimap minimap-cli`
- Run `minimap init --agents all` if Minimap has not been initialized.
- Run `minimap doctor` and fix blocking environment issues before mapping.

Mapping workflow for each route:
1. Choose a short route name such as `settings`, `article-detail`, or `profile-edit`.
2. Run `minimap map --discover <route-name> --max-actions 5 --stage`.
3. Run `minimap layout` and inspect the current screen.
4. Choose stable selectors in this order: test tag, resource id, accessibility/content description, stable visible text. Avoid coordinate taps unless there is no usable selector.
5. Run `minimap tap --selector "<kind>=<value>" --reason "<why this moves toward the route>"`.
6. Run `minimap layout` after each meaningful transition.
7. Repeat only until the named route target is reached. Avoid unbounded crawling.
8. Run `minimap map --discover <route-name> --max-actions 5 --stage --finish`.
9. Record the proposal id/path for the user. Do not run `minimap accept` unless the user explicitly approves accepting staged graph changes.

After mapping:
- Run `minimap validate --all` when at least one route has been accepted.
- Summarize mapped routes, staged proposals, selectors used, screens reached, and any flows skipped.
- Keep raw layout observations in `.minimap/runs/`; do not commit raw layouts or runtime state.

Failure handling:
- If a selector no longer works, run `minimap drift` or `minimap repair <target> --stage`.
- If the current screen is unknown, stage the proposal and report that review is required.
- If login, onboarding, permissions, or feature flags block navigation, report the required context instead of forcing through it.
"#;

#[derive(Debug, Clone, Serialize)]
pub struct InitChange {
    pub kind: String,
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct InitResult {
    pub ok: bool,
    pub dry_run: bool,
    pub root: String,
    pub agents: Vec<String>,
    pub skill_paths: Vec<String>,
    pub changes: Vec<InitChange>,
}

pub fn default_config() -> Value {
    json!({
        "schema_version": CONFIG_SCHEMA_VERSION,
        "android": {
            "app_package": "",
            "assume_app_launched": true,
            "permissions": []
        },
        "context": {
            "auth_state": "unknown",
            "onboarding_state": "unknown",
            "locale": "unknown",
            "orientation": "unknown",
            "feature_flags": {}
        },
        "storage": {
            "commit_raw_layouts": false,
            "commit_runtime_telemetry": false,
            "generate_index_cache": true,
            "commit_index_cache": false
        },
        "navigation": {
            "default_mode": "verified",
            "safe_mode_fallback": true,
            "screen_match_confidence_min": 0.78,
            "repair_candidate_confidence_min": 0.65,
            "transition_timeout_ms": 3000
        },
        "normalization": {
            "store_normalized_bounds": true,
            "collapse_repeating_lists": true,
            "strip_dynamic_text_inputs": true
        },
        "redaction": {
            "run_before_hashing": true,
            "default_text_action": "exclude",
            "commit_verbatim_text": false,
            "allowlist_static_text": ["Home", "Settings", "Bookmarks"]
        },
        "skills": {
            "skill_name": DEFAULT_SKILL_NAME,
            "skill_names": [APP_NAVIGATION_SKILL_NAME, FIRST_RUN_MAPPING_SKILL_NAME],
            "install_strategy": "multi-write-detected",
            "install_paths": [".agents/skills", ".codex/skills", ".skills", ".agent/skills", ".claude/skills", ".gemini/skills"]
        }
    })
}

pub fn run_init(root: &Path, dry_run: bool, agents: &str) -> Result<InitResult> {
    let agents = parse_agents(agents)?;
    let skill_paths = skill_paths_for_agents(&agents);
    let changes = plan_init(root, &skill_paths);
    if !dry_run {
        apply_init(root, &changes)?;
    }
    let changes = if dry_run {
        changes
            .into_iter()
            .map(|mut change| {
                if matches!(change.status.as_str(), "create" | "append") {
                    change.status = "planned".to_string();
                }
                change
            })
            .collect()
    } else {
        changes
    };
    Ok(InitResult {
        ok: true,
        dry_run,
        root: root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .display()
            .to_string(),
        agents,
        skill_paths,
        changes,
    })
}

fn parse_agents(value: &str) -> Result<Vec<String>> {
    let parts: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let agents = match parts.as_slice() {
        ["all"] => vec!["codex", "claude", "android-studio", "gemini"],
        ["auto"] => vec!["codex"],
        [] => anyhow::bail!("--agents must not be empty"),
        _ => parts,
    };
    let valid = ["codex", "claude", "android-studio", "gemini"];
    for agent in &agents {
        if !valid.contains(agent) {
            anyhow::bail!("unknown agent: {agent}");
        }
    }
    Ok(agents.into_iter().map(str::to_string).collect())
}

fn skill_paths_for_agents(agents: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    for agent in agents {
        let roots: &[&str] = match agent.as_str() {
            "codex" => &[".agents/skills", ".codex/skills"],
            "claude" => &[".claude/skills"],
            "android-studio" => &[".skills", ".agent/skills"],
            "gemini" => &[".gemini/skills"],
            _ => &[],
        };
        for root in roots {
            for skill_name in [APP_NAVIGATION_SKILL_NAME, FIRST_RUN_MAPPING_SKILL_NAME] {
                let path = format!("{root}/{skill_name}/SKILL.md");
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }
    paths
}

fn plan_init(root: &Path, skill_paths: &[String]) -> Vec<InitChange> {
    let mut changes = Vec::new();
    for dir in MINIMAP_DIRS {
        changes.push(InitChange {
            kind: "directory".to_string(),
            path: dir.to_string(),
            status: if root.join(dir).exists() {
                "exists"
            } else {
                "create"
            }
            .to_string(),
            detail: String::new(),
        });
    }
    changes.push(InitChange {
        kind: "config".to_string(),
        path: ".minimap/config.json".to_string(),
        status: if root.join(".minimap/config.json").exists() {
            "exists"
        } else {
            "create"
        }
        .to_string(),
        detail: String::new(),
    });
    changes.push(InitChange {
        kind: "journal".to_string(),
        path: ".minimap/journal.jsonl".to_string(),
        status: if root.join(".minimap/journal.jsonl").exists() {
            "exists"
        } else {
            "create"
        }
        .to_string(),
        detail: String::new(),
    });
    let missing = missing_gitignore_entries(root);
    changes.push(InitChange {
        kind: "gitignore".to_string(),
        path: ".gitignore".to_string(),
        status: if missing.is_empty() {
            "exists"
        } else {
            "append"
        }
        .to_string(),
        detail: if missing.is_empty() {
            String::new()
        } else {
            format!("add {}", missing.join(", "))
        },
    });
    for path in skill_paths {
        changes.push(InitChange {
            kind: "skill".to_string(),
            path: path.clone(),
            status: if root.join(path).exists() {
                "exists"
            } else {
                "create"
            }
            .to_string(),
            detail: String::new(),
        });
    }
    changes
}

fn apply_init(root: &Path, changes: &[InitChange]) -> Result<()> {
    for change in changes {
        let path = root.join(&change.path);
        match (change.kind.as_str(), change.status.as_str()) {
            ("directory", "create") => fs::create_dir_all(&path)?,
            ("config", "create") => write_json(&path, &default_config())?,
            ("journal", "create") => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(false)
                    .open(&path)?;
            }
            ("gitignore", "append") => append_gitignore(root)?,
            ("skill", "create") => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, skill_body_for_path(&change.path))?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn skill_body_for_path(path: &str) -> &'static str {
    if path.contains(FIRST_RUN_MAPPING_SKILL_NAME) {
        FIRST_RUN_MAPPING_SKILL_BODY
    } else {
        APP_NAVIGATION_SKILL_BODY
    }
}

pub fn missing_gitignore_entries(root: &Path) -> Vec<String> {
    let content = fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
    let lines: Vec<_> = content.lines().map(str::trim).collect();
    GITIGNORE_ENTRIES
        .iter()
        .filter(|entry| !lines.contains(entry))
        .map(|entry| entry.to_string())
        .collect()
}

fn append_gitignore(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    let mut content = fs::read_to_string(&path).unwrap_or_default();
    let missing = missing_gitignore_entries(root);
    if missing.is_empty() {
        return Ok(());
    }
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    if !content.trim().is_empty() {
        content.push('\n');
    }
    content.push_str("# Minimap runtime artifacts\n");
    for entry in missing {
        content.push_str(&entry);
        content.push('\n');
    }
    fs::write(path, content)?;
    Ok(())
}

pub fn load_context(root: &Path) -> GraphContext {
    let value = read_json(&root.join(".minimap/config.json")).unwrap_or_else(|_| default_config());
    let context = value
        .get("context")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .collect();
    GraphContext(context)
}

pub struct Graph {
    pub screens: BTreeMap<String, ScreenNode>,
    pub edges: BTreeMap<String, NavigationEdge>,
    pub routes: BTreeMap<String, Route>,
}

pub fn load_graph(root: &Path) -> Result<Graph> {
    Ok(Graph {
        screens: load_objects(root.join(".minimap/graph/screens"), SCREEN_SCHEMA_VERSION)?,
        edges: load_objects(root.join(".minimap/graph/edges"), EDGE_SCHEMA_VERSION)?,
        routes: load_objects(root.join(".minimap/routes"), ROUTE_SCHEMA_VERSION)?,
    })
}

fn load_objects<T>(dir: PathBuf, schema: &str) -> Result<BTreeMap<String, T>>
where
    T: for<'de> serde::Deserialize<'de>,
    T: HasObjectId,
{
    let mut objects = BTreeMap::new();
    if !dir.exists() {
        return Ok(objects);
    }
    for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let value = read_json(&path)?;
        let actual = value.get("schema_version").and_then(Value::as_str);
        if actual != Some(schema) {
            anyhow::bail!(
                "{} has unsupported schema_version {actual:?}",
                path.display()
            );
        }
        let object: T = serde_json::from_value(value)?;
        objects.insert(object.object_id(), object);
    }
    Ok(objects)
}

pub trait HasObjectId {
    fn object_id(&self) -> String;
}

impl HasObjectId for ScreenNode {
    fn object_id(&self) -> String {
        self.id.clone()
    }
}

impl HasObjectId for NavigationEdge {
    fn object_id(&self) -> String {
        self.id.clone()
    }
}

impl HasObjectId for Route {
    fn object_id(&self) -> String {
        self.name.clone()
    }
}

pub fn read_json(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    Ok(serde_json::from_str(&content)?)
}

pub fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, canonical_json(value))?;
    Ok(())
}

pub fn proposal_path(root: &Path, id: &str) -> PathBuf {
    root.join(".minimap/proposals")
        .join(format!("{}.json", slugify(id)))
}

pub fn stage_proposal_value(root: &Path, proposal: &Value) -> Result<PathBuf> {
    let id = proposal
        .get("id")
        .and_then(Value::as_str)
        .context("proposal must include id")?;
    let path = proposal_path(root, id);
    write_json(&path, proposal)?;
    Ok(path)
}

pub fn accept_proposal(root: &Path, id: &str) -> Result<Vec<PathBuf>> {
    let value = read_json(&proposal_path(root, id))?;
    let proposal: Proposal = serde_json::from_value(value)?;
    if proposal.schema_version != PROPOSAL_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported proposal schema_version {}",
            proposal.schema_version
        );
    }
    let mut written = Vec::new();
    for change in proposal.changes {
        let object = change
            .get("object")
            .context("proposal change must include object")?;
        let schema = object.get("schema_version").and_then(Value::as_str);
        let path = match schema {
            Some(SCREEN_SCHEMA_VERSION) => commit_screen(root, object)?,
            Some(EDGE_SCHEMA_VERSION) => commit_edge(root, object)?,
            Some(ROUTE_SCHEMA_VERSION) => commit_route(root, object)?,
            other => anyhow::bail!("unsupported proposal object schema_version {other:?}"),
        };
        written.push(path);
    }
    Ok(written)
}

pub fn commit_screen(root: &Path, screen: &Value) -> Result<PathBuf> {
    let parsed: ScreenNode = serde_json::from_value(screen.clone())?;
    let path = root
        .join(".minimap/graph/screens")
        .join(format!("{}.json", screen_filename(&parsed.id)));
    write_json(&path, screen)?;
    Ok(path)
}

pub fn commit_edge(root: &Path, edge: &Value) -> Result<PathBuf> {
    let parsed: NavigationEdge = serde_json::from_value(edge.clone())?;
    let path = root
        .join(".minimap/graph/edges")
        .join(format!("{}.json", edge_filename(&parsed.id)));
    write_json(&path, edge)?;
    Ok(path)
}

pub fn commit_route(root: &Path, route: &Value) -> Result<PathBuf> {
    let parsed: Route = serde_json::from_value(route.clone())?;
    let path = root
        .join(".minimap/routes")
        .join(format!("{}.minimap.json", slugify(&parsed.name)));
    write_json(&path, route)?;
    Ok(path)
}

pub fn screen_path(root: &Path, screen_id: &str) -> PathBuf {
    root.join(".minimap/graph/screens")
        .join(format!("{}.json", screen_filename(screen_id)))
}

pub struct RenamedScreen {
    pub path: PathBuf,
    pub old_name: String,
}

pub fn rename_screen(root: &Path, screen_id: &str, new_name: &str) -> Result<RenamedScreen> {
    let path = screen_path(root, screen_id);
    if !path.exists() {
        anyhow::bail!("screen '{screen_id}' not found at {}", path.display());
    }
    let mut value = read_json(&path)?;
    let old_name = value
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if let Value::Object(map) = &mut value {
        map.insert("name".to_string(), Value::String(new_name.to_string()));
    } else {
        anyhow::bail!("screen file {} is not a JSON object", path.display());
    }
    commit_screen(root, &value)?;
    Ok(RenamedScreen { path, old_name })
}

pub fn append_journal_entry(root: &Path, entry: &JournalEntry) -> Result<()> {
    let path = root.join(".minimap/journal.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn slugify(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches(&['_', '-', '.'][..]).to_string();
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed
    }
}

fn screen_filename(id: &str) -> String {
    let slug = slugify(id);
    if slug.starts_with("screen_") {
        slug
    } else {
        format!("screen_{slug}")
    }
}

fn edge_filename(id: &str) -> String {
    let slug = slugify(id);
    if slug.starts_with("edge_") {
        slug
    } else {
        format!("edge_{slug}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_dry_run_does_not_write() {
        let temp = tempfile::tempdir().unwrap();
        let result = run_init(temp.path(), true, "all").unwrap();
        assert!(result.ok);
        assert!(!temp.path().join(".minimap").exists());
        assert!(result
            .skill_paths
            .contains(&".agents/skills/minimap-app-navigation/SKILL.md".to_string()));
        assert!(result
            .skill_paths
            .contains(&".agents/skills/minimap-first-run-mapping/SKILL.md".to_string()));
    }

    #[test]
    fn init_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        run_init(temp.path(), false, "codex").unwrap();
        let second = run_init(temp.path(), false, "codex").unwrap();
        assert!(second
            .changes
            .iter()
            .any(|change| change.path == ".minimap/config.json" && change.status == "exists"));
        let gitignore = fs::read_to_string(temp.path().join(".gitignore")).unwrap();
        assert_eq!(gitignore.matches(".minimap/journal.jsonl").count(), 1);
        assert!(!gitignore.contains(".minimap/runs/"));
        assert!(!gitignore.contains(".minimap/state/"));
    }

    #[test]
    fn init_installs_separate_first_run_mapping_skill_guidance() {
        let temp = tempfile::tempdir().unwrap();
        run_init(temp.path(), false, "codex").unwrap();
        let skill = fs::read_to_string(
            temp.path()
                .join(".agents/skills/minimap-first-run-mapping/SKILL.md"),
        )
        .unwrap();
        assert!(skill.contains("name: minimap-first-run-mapping"));
        assert!(skill.contains("First-Run Mapping Mode"));
        assert!(skill.contains("token-intensive"));
        assert!(skill.contains("do not accept or commit"));
        assert!(skill.contains("minimap map --discover <route-name> --max-actions 5 --stage"));
        assert!(skill.contains("only for bounded bulk surveys"));
        assert!(skill.contains("use `minimap-app-navigation` instead"));

        let navigation_skill = fs::read_to_string(
            temp.path()
                .join(".agents/skills/minimap-app-navigation/SKILL.md"),
        )
        .unwrap();
        assert!(navigation_skill.contains("name: minimap-app-navigation"));
        assert!(!navigation_skill.contains("First-Run Mapping Mode"));
    }
}
