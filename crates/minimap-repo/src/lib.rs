use anyhow::{Context, Result};
use minimap_schemas::{
    canonical_json, AppProfile, Edge, MinimapConfig, Place, CONFIG_SCHEMA_VERSION,
    EDGE_SCHEMA_VERSION, PLACE_SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const APP_NAVIGATION_SKILL_NAME: &str = "minimap-app-navigation";
pub const MINIMAP_DIRS: &[&str] = &[
    ".minimap",
    ".minimap/graph",
    ".minimap/graph/places",
    ".minimap/graph/edges",
];
pub const LEGACY_MINIMAP_PATHS: &[&str] = &[
    ".minimap/graph/screens",
    ".minimap/proposals",
    ".minimap/routes",
    ".minimap/runs",
    ".minimap/state",
    ".minimap/checks",
    ".minimap/journal.jsonl",
    ".minimap/current.json",
];

pub const LEGACY_MINIMAP_MESSAGE: &str = "this project has an incompatible pre-lean-v1 `.minimap/` layout. Run `minimap init --force` to replace `.minimap/` with the lean v1 config and graph directories.";

pub const APP_NAVIGATION_SKILL_BODY: &str = r#"---
name: minimap-app-navigation
description: Use in an Android codebase for app navigation with Minimap. Minimap records proven navigation paths as a repo graph so agents can reuse them. Prefer minimap whereami/go/tap/scroll/back before raw android layout or adb commands.
metadata:
  author: minimap
  version: "2.0"
---

# Minimap App Navigation

Minimap is this repo's Android navigation memory for agents. It stores only
verified places and transitions under `.minimap/graph`.

Use this command loop:

```bash
minimap whereami
minimap go <label>
minimap tap --selector "<kind>=<value>" --label <destination> --reason "<intent>"
minimap scroll --direction down
minimap back
```

Rules:

- `go <label>` follows known UI paths and verifies each transition.
- Unlabeled `whereami` may reuse very fresh verified session state for cheap orientation.
- `tap --label <destination>` labels the post-tap destination.
- Unknown destinations without `--label` are not committed.
- `layout` is the raw Android layout escape hatch for business verification or finding selectors. Immediately after a verified Minimap observation, it may reuse the fresh session layout instead of calling Android layout again.
- Do not use removed workflows: observe, learn, map, route, screen, accept, repair, validate, undo.
- Review graph changes through normal git diff/PR review.

Selector preference: test tag, resource id, content description, stable visible text. Use points or screenshot labels only when selectors are not available; those edges are viewport-guarded and fragile.
"#;

#[derive(Debug, Clone, Serialize)]
pub struct InitChange {
    pub kind: String,
    pub path: String,
    pub status: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitOptions<'a> {
    pub dry_run: bool,
    pub agents: &'a str,
    pub force: bool,
    pub refresh_skills: bool,
    pub no_skills: bool,
}

pub fn default_config() -> MinimapConfig {
    MinimapConfig {
        schema_version: CONFIG_SCHEMA_VERSION.to_string(),
        active_app_profile: "default".to_string(),
        app_profiles: BTreeMap::from([(
            "default".to_string(),
            AppProfile {
                android_package: String::new(),
            },
        )]),
    }
}

pub fn detect_legacy_minimap(root: &Path) -> Vec<String> {
    LEGACY_MINIMAP_PATHS
        .iter()
        .filter(|path| root.join(path).exists())
        .map(|path| path.to_string())
        .collect()
}

pub fn run_init(root: &Path, options: InitOptions<'_>) -> Result<InitResult> {
    let agents = parse_agents(options.agents)?;
    let skill_paths = if options.no_skills {
        Vec::new()
    } else {
        skill_paths_for_agents(&agents)
    };
    if root.join(".minimap").exists() && !options.force {
        let legacy = detect_legacy_minimap(root);
        let has_config = root.join(".minimap/config.json").exists();
        let has_places = root.join(".minimap/graph/places").exists();
        let has_edges = root.join(".minimap/graph/edges").exists();
        if !legacy.is_empty() || !has_config || !has_places || !has_edges {
            anyhow::bail!("{LEGACY_MINIMAP_MESSAGE}");
        }
    }
    let mut changes = plan_init(root, &skill_paths, options.force, options.refresh_skills);
    if !options.dry_run {
        apply_init(root, &changes, options.force, options.refresh_skills)?;
    } else {
        for change in &mut changes {
            if matches!(change.status.as_str(), "create" | "replace" | "refresh") {
                change.status = "planned".to_string();
            }
        }
    }
    Ok(InitResult {
        ok: true,
        dry_run: options.dry_run,
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
            let path = format!("{root}/{APP_NAVIGATION_SKILL_NAME}/SKILL.md");
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

fn plan_init(
    root: &Path,
    skill_paths: &[String],
    force: bool,
    refresh_skills: bool,
) -> Vec<InitChange> {
    let mut changes = Vec::new();
    if force && root.join(".minimap").exists() {
        changes.push(InitChange {
            kind: "minimap".to_string(),
            path: ".minimap".to_string(),
            status: "replace".to_string(),
        });
    }
    for dir in MINIMAP_DIRS {
        changes.push(InitChange {
            kind: "directory".to_string(),
            path: dir.to_string(),
            status: if root.join(dir).exists() && !force {
                "exists"
            } else {
                "create"
            }
            .to_string(),
        });
    }
    changes.push(InitChange {
        kind: "config".to_string(),
        path: ".minimap/config.json".to_string(),
        status: if root.join(".minimap/config.json").exists() && !force {
            "exists"
        } else {
            "create"
        }
        .to_string(),
    });
    for path in skill_paths {
        changes.push(InitChange {
            kind: "skill".to_string(),
            path: path.clone(),
            status: if refresh_skills {
                "refresh"
            } else if root.join(path).exists() {
                "exists"
            } else {
                "create"
            }
            .to_string(),
        });
    }
    changes
}

fn apply_init(
    root: &Path,
    changes: &[InitChange],
    force: bool,
    refresh_skills: bool,
) -> Result<()> {
    if force && root.join(".minimap").exists() {
        fs::remove_dir_all(root.join(".minimap"))?;
    }
    for change in changes {
        let path = root.join(&change.path);
        match (change.kind.as_str(), change.status.as_str()) {
            ("directory", "create") => fs::create_dir_all(&path)?,
            ("config", "create") => write_json(&path, &serde_json::to_value(default_config())?)?,
            ("skill", "create") | ("skill", "refresh")
                if refresh_skills || change.status == "create" =>
            {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(path, APP_NAVIGATION_SKILL_BODY)?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Graph {
    pub places: BTreeMap<String, Place>,
    pub edges: BTreeMap<String, Edge>,
}

pub fn load_config(root: &Path) -> Result<MinimapConfig> {
    let value = read_json(&root.join(".minimap/config.json"))?;
    minimap_schemas::require_schema(&value, CONFIG_SCHEMA_VERSION)?;
    Ok(serde_json::from_value(value)?)
}

pub fn load_graph(root: &Path) -> Result<Graph> {
    reject_legacy_layout(root)?;
    Ok(Graph {
        places: load_objects(root.join(".minimap/graph/places"), PLACE_SCHEMA_VERSION)?,
        edges: load_objects(root.join(".minimap/graph/edges"), EDGE_SCHEMA_VERSION)?,
    })
}

fn reject_legacy_layout(root: &Path) -> Result<()> {
    let legacy = detect_legacy_minimap(root);
    if legacy.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("{LEGACY_MINIMAP_MESSAGE}")
    }
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

trait HasObjectId {
    fn object_id(&self) -> String;
}

impl HasObjectId for Place {
    fn object_id(&self) -> String {
        self.id.clone()
    }
}

impl HasObjectId for Edge {
    fn object_id(&self) -> String {
        self.id.clone()
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

pub fn commit_place(root: &Path, place: &Place) -> Result<PathBuf> {
    let path = place_path(root, &place.id);
    write_json(&path, &serde_json::to_value(place)?)?;
    Ok(path)
}

pub fn commit_edge(root: &Path, edge: &Edge) -> Result<PathBuf> {
    let path = edge_path(root, &edge.id);
    write_json(&path, &serde_json::to_value(edge)?)?;
    Ok(path)
}

pub fn place_path(root: &Path, place_id: &str) -> PathBuf {
    root.join(".minimap/graph/places")
        .join(format!("{}.json", slugify(place_id)))
}

pub fn edge_path(root: &Path, edge_id: &str) -> PathBuf {
    root.join(".minimap/graph/edges")
        .join(format!("{}.json", slugify(edge_id)))
}

pub fn remove_place_file(root: &Path, place_id: &str) -> Result<()> {
    let path = place_path(root, place_id);
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

pub fn validate_graph(root: &Path) -> Vec<Value> {
    let mut checks = Vec::new();
    let config = match load_config(root) {
        Ok(config) => {
            checks.push(json!({"name": "config", "status": "pass"}));
            Some(config)
        }
        Err(error) => {
            checks.push(json!({"name": "config", "status": "fail", "detail": error.to_string()}));
            None
        }
    };
    let graph = match load_graph(root) {
        Ok(graph) => {
            checks.push(json!({"name": "graph_schema", "status": "pass"}));
            graph
        }
        Err(error) => {
            checks.push(
                json!({"name": "graph_schema", "status": "fail", "detail": error.to_string()}),
            );
            return checks;
        }
    };
    if config.is_some() {
        let mut slugs = BTreeSet::new();
        let mut duplicate = None;
        for place in graph.places.values() {
            if !slugs.insert(place.slug.clone()) {
                duplicate = Some(place.slug.clone());
                break;
            }
        }
        checks.push(json!({
            "name": "unique_labels",
            "status": if duplicate.is_none() { "pass" } else { "fail" },
            "detail": duplicate
        }));
        let dangling = graph
            .edges
            .values()
            .find(|edge| {
                !graph.places.contains_key(&edge.from.id) || !graph.places.contains_key(&edge.to.id)
            })
            .map(|edge| edge.id.clone());
        checks.push(json!({
            "name": "edge_refs",
            "status": if dangling.is_none() { "pass" } else { "fail" },
            "detail": dangling
        }));
    }
    checks
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_minimal_layout() {
        let temp = tempfile::tempdir().unwrap();
        let result = run_init(
            temp.path(),
            InitOptions {
                dry_run: false,
                agents: "codex",
                force: false,
                refresh_skills: false,
                no_skills: false,
            },
        )
        .unwrap();
        assert!(result.ok);
        assert!(temp.path().join(".minimap/config.json").exists());
        assert!(temp.path().join(".minimap/graph/places").is_dir());
        assert!(temp.path().join(".minimap/graph/edges").is_dir());
        assert!(!temp.path().join(".minimap/journal.jsonl").exists());
    }

    #[test]
    fn init_force_replaces_old_layout() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".minimap/proposals")).unwrap();
        fs::write(temp.path().join(".minimap/old.txt"), "old").unwrap();
        run_init(
            temp.path(),
            InitOptions {
                dry_run: false,
                agents: "codex",
                force: true,
                refresh_skills: false,
                no_skills: true,
            },
        )
        .unwrap();
        assert!(!temp.path().join(".minimap/old.txt").exists());
        assert!(temp.path().join(".minimap/graph/places").is_dir());
    }
}
