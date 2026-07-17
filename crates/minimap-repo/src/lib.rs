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

pub const INCOMPLETE_MINIMAP_MESSAGE: &str = "this project has a partial lean v1 `.minimap/` layout. Run `minimap init` again to non-destructively create the missing config and graph directories.";

// Canonical skill text. `init` and the Claude Code plugin must install the same
// source, so embed the plugin's SKILL.md directly (see
// docs/MINIMAP_V1_LEAN_DESIGN.md skill-source invariant). The skill-equality
// test below guards against drift.
pub const APP_NAVIGATION_SKILL_BODY: &str =
    include_str!("../../../plugins/minimap-claude-code/skills/minimap-app-navigation/SKILL.md");

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
    config_with_package(String::new())
}

fn default_config_for_root(root: &Path) -> MinimapConfig {
    let package = inferred_debug_packages(root)
        .ok()
        .and_then(|packages| {
            let unique: BTreeSet<_> = packages
                .into_iter()
                .map(|candidate| candidate.package)
                .collect();
            (unique.len() == 1).then(|| unique.into_iter().next().unwrap())
        })
        .unwrap_or_default();
    config_with_package(package)
}

fn config_with_package(android_package: String) -> MinimapConfig {
    MinimapConfig {
        schema_version: CONFIG_SCHEMA_VERSION.to_string(),
        active_app_profile: "default".to_string(),
        app_profiles: BTreeMap::from([("default".to_string(), AppProfile { android_package })]),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferredAppPackage {
    pub package: String,
    pub build_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppPackageResolution {
    Configured {
        package: String,
        profile: String,
    },
    Inferred {
        package: String,
        build_files: Vec<PathBuf>,
    },
    Missing {
        profile: String,
    },
    Ambiguous {
        profile: String,
        candidates: Vec<String>,
    },
}

impl AppPackageResolution {
    pub fn package(&self) -> Option<&str> {
        match self {
            Self::Configured { package, .. } | Self::Inferred { package, .. } => Some(package),
            Self::Missing { .. } | Self::Ambiguous { .. } => None,
        }
    }
}

pub fn resolve_app_package(root: &Path) -> Result<AppPackageResolution> {
    let config = load_config(root)?;
    let profile_name = config.active_app_profile;
    let profile = config.app_profiles.get(&profile_name).with_context(|| {
        format!("active app profile `{profile_name}` is missing from app_profiles")
    })?;
    let configured = profile.android_package.trim();
    if !configured.is_empty() {
        return Ok(AppPackageResolution::Configured {
            package: configured.to_string(),
            profile: profile_name,
        });
    }

    let inferred = inferred_debug_packages(root)?;
    let mut files_by_package: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for candidate in inferred {
        files_by_package
            .entry(candidate.package)
            .or_default()
            .push(candidate.build_file);
    }
    match files_by_package.len() {
        0 => Ok(AppPackageResolution::Missing {
            profile: profile_name,
        }),
        1 => {
            let (package, build_files) = files_by_package.into_iter().next().unwrap();
            Ok(AppPackageResolution::Inferred {
                package,
                build_files,
            })
        }
        _ => Ok(AppPackageResolution::Ambiguous {
            profile: profile_name,
            candidates: files_by_package.into_keys().collect(),
        }),
    }
}

fn inferred_debug_packages(root: &Path) -> Result<Vec<InferredAppPackage>> {
    let mut build_files = Vec::new();
    for name in ["build.gradle.kts", "build.gradle"] {
        let path = root.join(name);
        if path.is_file() {
            build_files.push(path);
        }
    }
    if root.is_dir() {
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            for name in ["build.gradle.kts", "build.gradle"] {
                let build_file = path.join(name);
                if build_file.is_file() {
                    build_files.push(build_file);
                }
            }
        }
    }
    build_files.sort();

    let mut candidates = Vec::new();
    for build_file in build_files {
        let script = fs::read_to_string(&build_file)?;
        if !is_android_application_script(&script) {
            continue;
        }
        let suffix = debug_application_id_suffix(&script).unwrap_or_default();
        for application_id in quoted_assignments(&script, "applicationId") {
            candidates.push(InferredAppPackage {
                package: format!("{application_id}{suffix}"),
                build_file: build_file.clone(),
            });
        }
    }
    Ok(candidates)
}

fn is_android_application_script(script: &str) -> bool {
    script.contains("com.android.application") || script.contains("android.application")
}

fn quoted_assignments(script: &str, key: &str) -> Vec<String> {
    script
        .lines()
        .filter_map(|line| quoted_assignment(line, key))
        .collect()
}

fn quoted_assignment(line: &str, key: &str) -> Option<String> {
    let code = line.split("//").next()?.trim();
    let rest = code.strip_prefix(key)?;
    if rest
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return None;
    }
    let quote_index = rest.find(['"', '\''])?;
    let quote = rest.as_bytes()[quote_index] as char;
    let value = &rest[quote_index + 1..];
    let end = value.find(quote)?;
    let value = value[..end].trim();
    (!value.is_empty() && !value.contains('$')).then(|| value.to_string())
}

fn debug_application_id_suffix(script: &str) -> Option<String> {
    let mut in_debug = false;
    let mut depth = 0_i32;
    for line in script.lines() {
        let code = line.split("//").next().unwrap_or_default().trim();
        let starts_debug = !in_debug
            && (code.starts_with("debug {")
                || code.contains("getByName(\"debug\")")
                || code.contains("getByName('debug')"));
        if starts_debug {
            in_debug = true;
            depth = brace_delta(code);
            if let Some(suffix) = quoted_assignment(code, "applicationIdSuffix") {
                return Some(suffix);
            }
            if depth <= 0 {
                in_debug = false;
            }
            continue;
        }
        if in_debug {
            if let Some(suffix) = quoted_assignment(code, "applicationIdSuffix") {
                return Some(suffix);
            }
            depth += brace_delta(code);
            if depth <= 0 {
                in_debug = false;
            }
        }
    }
    None
}

fn brace_delta(line: &str) -> i32 {
    line.chars().fold(0, |depth, character| match character {
        '{' => depth + 1,
        '}' => depth - 1,
        _ => depth,
    })
}

pub fn detect_legacy_minimap(root: &Path) -> Vec<String> {
    LEGACY_MINIMAP_PATHS
        .iter()
        .filter(|path| root.join(path).exists())
        .map(|path| path.to_string())
        .collect()
}

/// A tree is a valid lean v1 graph when the config and both graph object
/// directories are present. A stray generic dir name (e.g. `.minimap/runs`)
/// must not brick reads on an otherwise-valid graph, so this short-circuits the
/// legacy guard.
pub fn is_lean_v1_layout(root: &Path) -> bool {
    root.join(".minimap/config.json").exists()
        && root.join(".minimap/graph/places").exists()
        && root.join(".minimap/graph/edges").exists()
}

pub fn run_init(root: &Path, options: InitOptions<'_>) -> Result<InitResult> {
    let agents = parse_agents(options.agents)?;
    let skill_paths = if options.no_skills {
        Vec::new()
    } else {
        skill_paths_for_agents(&agents)
    };
    if root.join(".minimap").exists() && !options.force {
        // Only an *actual* legacy path warrants the destructive `--force`
        // remediation. A lean-but-incomplete tree (missing config/places/edges
        // but no legacy paths) is repaired non-destructively below by
        // re-running the create steps, so it must not demand `--force`.
        let legacy = detect_legacy_minimap(root);
        if !legacy.is_empty() {
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
            ("config", "create") => {
                write_json(&path, &serde_json::to_value(default_config_for_root(root))?)?
            }
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
    // A complete lean v1 graph is always readable, even if a stray generic dir
    // name (e.g. `.minimap/runs`) happens to be present.
    if is_lean_v1_layout(root) {
        return Ok(());
    }
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
        let id = object.object_id();
        let expected = format!("{}.json", slugify(&id));
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if file_name != expected {
            anyhow::bail!(
                "{} has object id {id:?} but filename does not match expected {expected:?}",
                path.display()
            );
        }
        if objects.insert(id.clone(), object).is_some() {
            anyhow::bail!("duplicate object id {id:?} in {}", dir.display());
        }
    }
    Ok(objects)
}

/// Filesystem-level scan of the places directory that surfaces duplicate ids
/// and filename-vs-id mismatches for `doctor`, independent of whether the full
/// graph deserializes. Mirrors the invariants enforced in `load_objects`.
fn scan_place_ids(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let value = read_json(&path)?;
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            anyhow::bail!("{} is missing an id", path.display());
        };
        let expected = format!("{}.json", slugify(id));
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if file_name != expected {
            anyhow::bail!(
                "{} has place id {id:?} but filename does not match expected {expected:?}",
                path.display()
            );
        }
        if !seen.insert(id.to_string()) {
            anyhow::bail!("duplicate place id {id:?} in {}", dir.display());
        }
    }
    Ok(())
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
    write_atomic(path, canonical_json(value).as_bytes())
        .with_context(|| format!("write {}", path.display()))
}

/// Write `bytes` to `path` atomically and durably: stage into a sibling temp
/// file in the same directory, flush + `sync_all`, then rename over the final
/// path. A crash or `ENOSPC` mid-write leaves the existing file intact instead
/// of a truncated one that would fail the whole graph load.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let tmp = tmp_sibling_path(path);
    // Scope the file handle so it is closed before the rename.
    {
        let mut file = fs::File::create(&tmp)
            .with_context(|| format!("create temp file {}", tmp.display()))?;
        if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            // Best-effort cleanup so a failed write never leaves a temp file behind.
            let _ = fs::remove_file(&tmp);
            return Err(error).with_context(|| format!("write temp file {}", tmp.display()));
        }
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()));
    }
    Ok(())
}

fn tmp_sibling_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".tmp");
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
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
    if config.is_some() {
        checks.push(match resolve_app_package(root) {
            Ok(AppPackageResolution::Configured { package, profile }) => json!({
                "name": "app_package",
                "status": "pass",
                "source": "config",
                "profile": profile,
                "package": package
            }),
            Ok(AppPackageResolution::Inferred {
                package,
                build_files,
            }) => json!({
                "name": "app_package",
                "status": "pass",
                "source": "gradle_debug_variant",
                "package": package,
                "build_files": build_files
            }),
            Ok(AppPackageResolution::Missing { profile }) => json!({
                "name": "app_package",
                "status": "fail",
                "code": "app_package_missing",
                "profile": profile,
                "detail": "configure android_package or add an inferable Android application module"
            }),
            Ok(AppPackageResolution::Ambiguous {
                profile,
                candidates,
            }) => json!({
                "name": "app_package",
                "status": "fail",
                "code": "app_package_ambiguous",
                "profile": profile,
                "candidates": candidates,
                "detail": "configure android_package for the active app profile"
            }),
            Err(error) => json!({
                "name": "app_package",
                "status": "fail",
                "code": "app_package_invalid",
                "detail": error.to_string()
            }),
        });
    }
    match scan_place_ids(&root.join(".minimap/graph/places")) {
        Ok(()) => checks.push(json!({"name": "place_ids", "status": "pass"})),
        Err(error) => {
            checks.push(json!({"name": "place_ids", "status": "fail", "detail": error.to_string()}))
        }
    }
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
    fn claude_skill_text_matches_plugin_skill_file() {
        let skill_md = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../plugins/minimap-claude-code/skills/minimap-app-navigation/SKILL.md");
        let contents = fs::read_to_string(&skill_md)
            .unwrap_or_else(|err| panic!("read {}: {err}", skill_md.display()));
        assert_eq!(
            APP_NAVIGATION_SKILL_BODY, contents,
            "skill text installed by init must match plugin SKILL.md"
        );
    }

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

    fn sample_place(id: &str, slug: &str) -> Place {
        Place {
            schema_version: PLACE_SCHEMA_VERSION.to_string(),
            id: id.to_string(),
            slug: slug.to_string(),
            label: slug.to_string(),
            baseline: minimap_schemas::PlaceBaseline {
                identity_hash: format!("hash-{id}"),
                fingerprint: minimap_schemas::Fingerprint {
                    selectors: Vec::new(),
                    static_text: Vec::new(),
                    roles: BTreeMap::new(),
                },
            },
            variants: Vec::new(),
        }
    }

    // FIX 1: atomic writes.
    #[test]
    fn write_json_writes_content_and_leaves_no_temp_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("nested/value.json");
        write_json(&path, &json!({"b": 2, "a": 1})).unwrap();
        let written = fs::read_to_string(&path).unwrap();
        assert_eq!(written, canonical_json(&json!({"b": 2, "a": 1})));
        // No leftover sibling temp file.
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "stray temp files: {leftovers:?}");
    }

    #[test]
    fn write_json_replaces_existing_file_only_once_fully_written() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("value.json");
        write_json(&path, &json!({"v": 1})).unwrap();
        write_json(&path, &json!({"v": 2})).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            canonical_json(&json!({"v": 2}))
        );
        // The temp sibling used for staging must not survive a successful write.
        assert!(!tmp_sibling_path(&path).exists());
    }

    // FIX 2: duplicate ids and filename/id mismatch.
    //
    // Before the fix, two files carrying the same object id would silently
    // overwrite each other in the BTreeMap (readdir order wins). The fix
    // enforces `filename == slugify(id)`, which both flags the mismatch and
    // makes a same-id duplicate impossible to load silently: the second file
    // cannot also be named the canonical slug, so it trips the filename check.
    #[test]
    fn load_objects_rejects_two_files_sharing_an_id() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".minimap/graph/places");
        fs::create_dir_all(&dir).unwrap();
        // Both files claim id "home". Pre-fix, one would be silently dropped.
        let canonical = sample_place("home", "home");
        let dupe = sample_place("home", "home-dupe");
        fs::write(
            dir.join("home.json"),
            canonical_json(&serde_json::to_value(&canonical).unwrap()),
        )
        .unwrap();
        fs::write(
            dir.join("home-2.json"),
            canonical_json(&serde_json::to_value(&dupe).unwrap()),
        )
        .unwrap();
        let err = load_graph(temp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("filename does not match") || msg.contains("duplicate"),
            "same-id duplicate must error, got: {msg}"
        );
    }

    #[test]
    fn load_objects_flags_filename_not_matching_id() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join(".minimap/graph/places");
        fs::create_dir_all(&dir).unwrap();
        let place = sample_place("home", "home");
        // Filename intentionally differs from slugify(id) == "home.json".
        fs::write(
            dir.join("renamed.json"),
            canonical_json(&serde_json::to_value(&place).unwrap()),
        )
        .unwrap();
        let err = load_graph(temp.path()).unwrap_err();
        assert!(
            err.to_string().contains("filename does not match"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_graph_flags_filename_id_mismatch_via_place_ids_check() {
        let temp = tempfile::tempdir().unwrap();
        write_json(
            &temp.path().join(".minimap/config.json"),
            &serde_json::to_value(default_config()).unwrap(),
        )
        .unwrap();
        let dir = temp.path().join(".minimap/graph/places");
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(temp.path().join(".minimap/graph/edges")).unwrap();
        let place = sample_place("home", "home");
        fs::write(
            dir.join("renamed.json"),
            canonical_json(&serde_json::to_value(&place).unwrap()),
        )
        .unwrap();
        let checks = validate_graph(temp.path());
        let place_ids = checks
            .iter()
            .find(|c| c.get("name").and_then(Value::as_str) == Some("place_ids"))
            .expect("place_ids check present");
        assert_eq!(
            place_ids.get("status").and_then(Value::as_str),
            Some("fail"),
            "place_ids should fail: {place_ids}"
        );
    }

    // FIX 3: legacy detection is existence-only and over-eager.
    #[test]
    fn valid_lean_tree_with_stray_legacy_dir_still_loads() {
        let temp = tempfile::tempdir().unwrap();
        write_json(
            &temp.path().join(".minimap/config.json"),
            &serde_json::to_value(default_config()).unwrap(),
        )
        .unwrap();
        fs::create_dir_all(temp.path().join(".minimap/graph/places")).unwrap();
        fs::create_dir_all(temp.path().join(".minimap/graph/edges")).unwrap();
        // A stray generic legacy dir name on an otherwise valid lean graph.
        fs::create_dir_all(temp.path().join(".minimap/runs")).unwrap();
        let graph = load_graph(temp.path()).expect("valid lean graph must still load");
        assert!(graph.places.is_empty());
    }

    #[test]
    fn init_on_lean_tree_missing_config_does_not_demand_force() {
        let temp = tempfile::tempdir().unwrap();
        // Lean-but-incomplete: directories exist but config.json is missing, and
        // there are no legacy paths.
        fs::create_dir_all(temp.path().join(".minimap/graph/places")).unwrap();
        fs::create_dir_all(temp.path().join(".minimap/graph/edges")).unwrap();
        let result = run_init(
            temp.path(),
            InitOptions {
                dry_run: false,
                agents: "codex",
                force: false,
                refresh_skills: false,
                no_skills: true,
            },
        )
        .expect("incomplete lean tree must repair without --force");
        assert!(result.ok);
        // The missing config was created non-destructively.
        assert!(temp.path().join(".minimap/config.json").exists());
    }
}
