use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use minimap_android::{
    parse_input_tap, resolve_selector_point, Adb, AndroidCli, CommandRunner, SubprocessRunner,
    TapPoint,
};
use minimap_core::{
    fingerprint_layout, fingerprint_usable, match_place, normalize_label, place_id_for_slug,
    redact_layout,
};
use minimap_graph::{exit_code_for_status, resolve_path};
use minimap_repo::{
    commit_edge, commit_place, edge_path, load_config, load_graph, remove_place_file, run_init,
    validate_graph, Graph, InitOptions,
};
use minimap_schemas::{
    canonical_json, ActionStep, Edge, EdgeEndpoint, MinimapResult, Place, PlaceBaseline, Point,
    Selector, Viewport, EDGE_SCHEMA_VERSION, PLACE_SCHEMA_VERSION, RESULT_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, SystemTime};

const DEFAULT_ACTION_SETTLE_MS: u64 = 1_000;
const SESSION_TTL_SECS: u64 = 600;
const LAYOUT_CACHE_TTL_SECS: u64 = 30;

#[derive(Debug, Parser)]
#[command(name = "minimap")]
#[command(about = "Android navigation memory for AI agents.")]
struct Cli {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    quiet: bool,
    #[arg(long = "no-color")]
    no_color: bool,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize lean Minimap state and install agent skills.
    Init {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "auto")]
        agents: String,
        #[arg(long)]
        force: bool,
        #[arg(long = "refresh-skills")]
        refresh_skills: bool,
        #[arg(long = "no-skills")]
        no_skills: bool,
    },
    /// Check repo graph health and Android device readiness.
    Doctor,
    /// Identify the current semantic place from one Android layout observation.
    Whereami {
        #[arg(long)]
        label: Option<String>,
    },
    /// Navigate to a known place through verified graph edges.
    Go { target: String },
    /// Tap by selector, coordinate, or screenshot label; --label names the destination.
    Tap {
        #[arg(long)]
        selector: Option<String>,
        #[arg(long)]
        point: Option<String>,
        #[arg(long = "screenshot-label")]
        screenshot_label: Option<i64>,
        #[arg(long)]
        screenshot: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Scroll and retain the action as part of a pending transition recipe.
    Scroll {
        #[arg(long, default_value = "down")]
        direction: String,
    },
    /// Press Android Back and record a verified known transition if one occurs.
    Back,
    /// Return redacted Android layout plus read-only Minimap orientation metadata.
    Layout {
        #[arg(long)]
        diff: bool,
    },
}

#[derive(Debug, Clone)]
struct Orientation {
    status: String,
    baseline: PlaceBaseline,
    matched_place: Option<Place>,
    confidence: f64,
    hash_matched: bool,
    changed_files: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct PendingTransition {
    source: EdgeEndpoint,
    recipe: Vec<ActionStep>,
    destination: PlaceBaseline,
    intent: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionPlace {
    place: EdgeEndpoint,
    baseline: PlaceBaseline,
    layout: Value,
}

#[derive(Debug, Clone, Copy)]
struct TapRequest<'a> {
    selector: Option<&'a str>,
    point: Option<&'a str>,
    screenshot_label: Option<i64>,
    screenshot: Option<&'a str>,
    label: Option<&'a str>,
    reason: Option<&'a str>,
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(error) => {
            let result = MinimapResult::new(
                "config_error",
                error.to_string(),
                json!({"error": {"message": error.to_string()}}),
            );
            print_json(&serde_json::to_value(result).expect("error json"));
            7
        }
    };
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    let root = PathBuf::from(".");
    match cli.command {
        Commands::Init {
            dry_run,
            agents,
            force,
            refresh_skills,
            no_skills,
        } => {
            let result = run_init(
                &root,
                InitOptions {
                    dry_run,
                    agents: &agents,
                    force,
                    refresh_skills,
                    no_skills,
                },
            )?;
            print_json(&serde_json::to_value(result)?);
            Ok(0)
        }
        Commands::Doctor => {
            let result = doctor(&root);
            let ok = result["ok"].as_bool().unwrap_or(false);
            print_json(&result);
            Ok(if ok { 0 } else { 1 })
        }
        Commands::Whereami { label } => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = whereami_result(&root, &mut android, &mut adb, label.as_deref(), true)?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Go { target } => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = go_result(&root, &mut android, &mut adb, &target)?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Tap {
            selector,
            point,
            screenshot_label,
            screenshot,
            label,
            reason,
        } => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = tap_result(
                &root,
                &mut android,
                &mut adb,
                TapRequest {
                    selector: selector.as_deref(),
                    point: point.as_deref(),
                    screenshot_label,
                    screenshot: screenshot.as_deref(),
                    label: label.as_deref(),
                    reason: reason.as_deref(),
                },
            )?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Scroll { direction } => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = scroll_result(&root, &mut android, &mut adb, &direction)?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Back => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = back_result(&root, &mut android, &mut adb)?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Layout { diff } => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = layout_result(&root, &mut android, &mut adb, diff)?;
            print_json(&result);
            Ok(0)
        }
    }
}

fn observe_layout<R: CommandRunner>(android: &mut AndroidCli<R>, diff: bool) -> Result<Value> {
    let command = android.layout(diff)?;
    Ok(serde_json::from_str::<Value>(&command.stdout).unwrap_or(Value::String(command.stdout)))
}

fn observe_after_action<R: CommandRunner>(
    android: &mut AndroidCli<R>,
    previous: Option<&PlaceBaseline>,
) -> Result<Value> {
    let first = observe_layout(android, false)?;
    let first_baseline = fingerprint_layout(&first);
    let unchanged = previous
        .map(|baseline| baseline.identity_hash == first_baseline.identity_hash)
        .unwrap_or(false);
    if fingerprint_usable(&first_baseline) && !unchanged {
        return Ok(first);
    }
    let settle = action_settle_ms();
    thread::sleep(Duration::from_millis(settle));
    observe_layout(android, false)
}

fn action_settle_ms() -> u64 {
    std::env::var("MINIMAP_ACTION_SETTLE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_ACTION_SETTLE_MS)
}

fn whereami_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    label: Option<&str>,
    allow_write: bool,
) -> Result<Value> {
    if label.is_none() {
        if let Some(session) =
            load_recent_session_place(root, adb, Duration::from_secs(LAYOUT_CACHE_TTL_SECS))?
        {
            let graph = load_graph(root).unwrap_or_else(|_| Graph {
                places: Default::default(),
                edges: Default::default(),
            });
            if let Some(place) = graph_place_for_session(&graph, &session) {
                return Ok(cached_whereami_json(root, &session.baseline, &place));
            }
            clear_session_place(root, adb)?;
        }
    }

    let layout = observe_layout(android, false)?;
    let orientation = orient_layout(root, &layout, label, allow_write, adb)?;
    remember_orientation_session(root, adb, &orientation, &layout)?;
    Ok(orientation_json(root, &orientation, false))
}

fn orient_layout<DR: CommandRunner>(
    root: &Path,
    layout: &Value,
    label: Option<&str>,
    allow_write: bool,
    adb: &mut Adb<DR>,
) -> Result<Orientation> {
    let baseline = fingerprint_layout(layout);
    let graph = load_graph(root)?;
    let matched = match_place(&baseline, graph.places.values().cloned());
    let mut changed_files = Vec::new();
    let mut status = matched.status.clone();
    let mut matched_place = if matched.status == "unknown" {
        None
    } else {
        matched
            .place_id
            .as_deref()
            .and_then(|id| graph.places.get(id))
            .cloned()
    };

    if let Some(label) = label {
        let slug = normalize_label(label);
        if slug.is_empty() {
            anyhow::bail!("--label must normalize to a non-empty slug");
        }
        let existing_label_place = graph
            .places
            .values()
            .find(|place| place.slug == slug)
            .cloned();
        match (matched_place.clone(), existing_label_place) {
            (Some(place), Some(existing)) if place.id != existing.id => {
                return Ok(Orientation {
                    status: "label_mismatch".to_string(),
                    baseline,
                    matched_place: Some(place),
                    confidence: matched.confidence,
                    hash_matched: matched.hash_matched,
                    changed_files,
                });
            }
            (Some(mut place), Some(_)) => {
                if allow_write && remember_place_observation(&mut place, &baseline) {
                    changed_files.push(commit_place(root, &place)?);
                    matched_place = Some(place);
                    status = "known_changed".to_string();
                } else {
                    status = "known".to_string();
                    matched_place = Some(place);
                }
            }
            (Some(place), None) => {
                if allow_write {
                    let (new_place, mut files) = relabel_place(root, &place, label, &baseline)?;
                    changed_files.append(&mut files);
                    matched_place = Some(new_place);
                    status = "ok".to_string();
                }
            }
            (None, Some(_existing)) => {
                return Ok(Orientation {
                    status: "label_mismatch".to_string(),
                    baseline,
                    matched_place: None,
                    confidence: matched.confidence,
                    hash_matched: matched.hash_matched,
                    changed_files,
                });
            }
            (None, None) => {
                if allow_write && fingerprint_usable(&baseline) {
                    let place = place_from_label(label, &baseline);
                    changed_files.push(commit_place(root, &place)?);
                    if let Some(mut pending) = load_pending(root, adb)? {
                        if pending.destination.identity_hash == baseline.identity_hash {
                            let edge = edge_from_parts(
                                &pending.source,
                                &endpoint_for_place(&place),
                                pending.recipe.split_off(0),
                                pending.intent.as_deref(),
                            );
                            changed_files.push(commit_edge(root, &edge)?);
                            clear_pending(root, adb)?;
                        }
                    }
                    matched_place = Some(place);
                    status = "ok".to_string();
                }
            }
        }
    } else if allow_write && status == "known_changed" {
        if let Some(mut place) = matched_place.clone() {
            if remember_place_observation(&mut place, &baseline) {
                changed_files.push(commit_place(root, &place)?);
                matched_place = Some(place);
            }
        }
    }

    Ok(Orientation {
        status,
        baseline,
        matched_place,
        confidence: matched.confidence,
        hash_matched: matched.hash_matched,
        changed_files,
    })
}

fn orientation_json(root: &Path, orientation: &Orientation, include_exits: bool) -> Value {
    let changed_files = changed_files_json(&orientation.changed_files);
    let graph = load_graph(root).ok();
    let place_json = orientation.matched_place.as_ref().map(|place| {
        json!({
            "id": place.id,
            "slug": place.slug,
            "label": place.label
        })
    });
    let known_exits = if include_exits {
        graph
            .as_ref()
            .and_then(|graph| {
                orientation
                    .matched_place
                    .as_ref()
                    .map(|place| (graph, place))
            })
            .map(|(graph, place)| {
                graph
                    .edges
                    .values()
                    .filter(|edge| edge.from.id == place.id)
                    .map(|edge| {
                        json!({
                            "edge": edge.id,
                            "to": edge.to.slug,
                            "intent": edge.intent,
                            "recipe": edge.recipe
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut value = json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": orientation.status,
        "summary": match orientation.status.as_str() {
            "known" => "current place is known",
            "known_changed" => "current place matched and baseline was updated",
            "label_mismatch" => "label belongs to a different known place",
            "ok" => "place label applied",
            _ => "current place is unknown"
        },
        "place": place_json,
        "match": {
            "confidence": orientation.confidence,
            "hash_matched": orientation.hash_matched,
            "identity_hash": orientation.baseline.identity_hash
        },
        "known_exits": known_exits,
        "changed_graph": !orientation.changed_files.is_empty(),
        "changed_files": changed_files
    });
    if orientation.status != "known" {
        value["fingerprint_summary"] = fingerprint_summary(&orientation.baseline);
    }
    value
}

fn cached_whereami_json(root: &Path, baseline: &PlaceBaseline, place: &Place) -> Value {
    let orientation = Orientation {
        status: "known".to_string(),
        baseline: baseline.clone(),
        matched_place: Some(place.clone()),
        confidence: 1.0,
        hash_matched: true,
        changed_files: Vec::new(),
    };
    let mut value = orientation_json(root, &orientation, false);
    value["cache"] = json!({
        "hit": true,
        "source": "session-place",
        "max_age_secs": LAYOUT_CACHE_TTL_SECS
    });
    value["metrics"] = json!({
        "layout_calls_total": 0,
        "layout_json_returned_to_agent": false
    });
    value
}

fn layout_minimap_json(
    graph: &Graph,
    baseline: &PlaceBaseline,
    place: Option<&Place>,
    status: &str,
    confidence: f64,
    hash_matched: bool,
) -> Value {
    let known_exits = place
        .map(|place| {
            graph
                .edges
                .values()
                .filter(|edge| edge.from.id == place.id)
                .map(|edge| json!({"edge": edge.id, "to": edge.to.slug, "intent": edge.intent}))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "status": status,
        "place": place.map(|place| {
            json!({"id": place.id, "slug": place.slug, "label": place.label})
        }),
        "match": {
            "confidence": confidence,
            "hash_matched": hash_matched,
            "identity_hash": baseline.identity_hash
        },
        "known_exits": known_exits
    })
}

fn layout_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    diff: bool,
) -> Result<Value> {
    if !diff {
        if let Some(session) =
            load_recent_session_place(root, adb, Duration::from_secs(LAYOUT_CACHE_TTL_SECS))?
        {
            let graph = load_graph(root).unwrap_or_else(|_| Graph {
                places: Default::default(),
                edges: Default::default(),
            });
            if let Some(place) = graph_place_for_session(&graph, &session) {
                return Ok(json!({
                    "schema_version": RESULT_SCHEMA_VERSION,
                    "status": "ok",
                    "kind": "android_layout",
                    "layout": session.layout,
                    "minimap": layout_minimap_json(
                        &graph,
                        &session.baseline,
                        Some(&place),
                        "known",
                        1.0,
                        true
                    ),
                    "cache": {
                        "hit": true,
                        "source": "session-place",
                        "max_age_secs": LAYOUT_CACHE_TTL_SECS
                    },
                    "metrics": {
                        "layout_calls_total": 0,
                        "layout_json_returned_to_agent": true
                    },
                    "changed_graph": false,
                    "changed_files": []
                }));
            }
        }
    }

    let layout = observe_layout(android, diff)?;
    let (minimap, cache_hit) = if diff {
        (json!({"orientation": "unavailable_for_diff"}), false)
    } else {
        let orientation = orient_layout(root, &layout, None, false, adb)?;
        remember_orientation_session(root, adb, &orientation, &layout)?;
        let graph = load_graph(root).unwrap_or_else(|_| Graph {
            places: Default::default(),
            edges: Default::default(),
        });
        (
            layout_minimap_json(
                &graph,
                &orientation.baseline,
                orientation.matched_place.as_ref(),
                &orientation.status,
                orientation.confidence,
                orientation.hash_matched,
            ),
            false,
        )
    };
    Ok(json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "ok",
        "kind": if diff { "android_layout_diff" } else { "android_layout" },
        "layout": redact_layout(&layout),
        "minimap": minimap,
        "cache": {"hit": cache_hit},
        "metrics": {
            "layout_calls_total": 1,
            "layout_json_returned_to_agent": true
        },
        "changed_graph": false,
        "changed_files": []
    }))
}

fn tap_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    request: TapRequest<'_>,
) -> Result<Value> {
    let action_count = request.selector.is_some() as u8
        + request.point.is_some() as u8
        + request.screenshot_label.is_some() as u8;
    if action_count != 1 {
        anyhow::bail!("tap requires exactly one of --selector, --point, or --screenshot-label");
    }
    if request.screenshot_label.is_some() && request.screenshot.is_none() {
        anyhow::bail!("--screenshot-label requires --screenshot");
    }

    let pre_layout = observe_layout(android, false)?;
    let pre_orientation = orient_layout(root, &pre_layout, None, true, adb)?;
    let pre_pending = load_pending(root, adb)?;
    let source_place = match pre_orientation.matched_place.clone() {
        Some(place) => place,
        None => {
            let pending_source = pre_pending
                .as_ref()
                .filter(|pending| {
                    pending.destination.identity_hash == pre_orientation.baseline.identity_hash
                })
                .map(|pending| pending.source.id.clone());
            if let Some(source_id) = pending_source {
                load_graph(root)?
                    .places
                    .get(&source_id)
                    .cloned()
                    .context("pending transition source place missing")?
            } else {
                return Ok(result_with_data(
                    "needs_label",
                    "current source place is unknown; run whereami --label before recording a transition",
                    json!({
                        "orientation": orientation_json(root, &pre_orientation, true)
                    }),
                ));
            }
        }
    };
    let action = build_and_execute_tap_action(
        android,
        adb,
        &pre_layout,
        request.selector,
        request.point,
        request.screenshot_label,
        request.screenshot,
    )?;
    let pending = pre_pending.filter(|pending| {
        pending.source.id == source_place.id
            && pending.destination.identity_hash == pre_orientation.baseline.identity_hash
    });
    let mut recipe = pending
        .as_ref()
        .map(|pending| pending.recipe.clone())
        .unwrap_or_default();
    recipe.push(action.clone());
    let edge_source = pending
        .as_ref()
        .map(|pending| pending.source.clone())
        .unwrap_or_else(|| endpoint_for_place(&source_place));
    let edge_intent = request.reason.or_else(|| {
        pending
            .as_ref()
            .and_then(|pending| pending.intent.as_deref())
    });
    let post_layout = observe_after_action(android, Some(&pre_orientation.baseline))?;
    let post_baseline = fingerprint_layout(&post_layout);
    let mut graph = load_graph(root)?;
    let post_match = match_place(&post_baseline, graph.places.values().cloned());
    let matched_post = if post_match.status == "unknown" {
        None
    } else {
        post_match
            .place_id
            .as_deref()
            .and_then(|id| graph.places.get(id))
            .cloned()
    };
    let mut changed_files = Vec::new();

    if matched_post
        .as_ref()
        .map(|place| place.id == source_place.id)
        .unwrap_or(false)
    {
        clear_pending(root, adb)?;
        save_session_place(
            root,
            adb,
            &endpoint_for_place(&source_place),
            &post_baseline,
            &post_layout,
        )?;
        return Ok(result_with_data(
            "ok",
            "tap stayed on the same known place; no navigation edge recorded",
            json!({
                "source": source_place.slug,
                "changed_graph": false,
                "changed_files": []
            }),
        ));
    }

    let destination = match request.label {
        Some(label) => {
            let slug = normalize_label(label);
            if slug.is_empty() {
                anyhow::bail!("--label must normalize to a non-empty slug");
            }
            let label_place = graph
                .places
                .values()
                .find(|place| place.slug == slug)
                .cloned();
            match (label_place, matched_post) {
                (Some(target), Some(observed)) if target.id != observed.id => {
                    return Ok(result_with_data(
                        "label_mismatch",
                        "tap reached a different known place than the requested label",
                        json!({
                            "requested_label": slug,
                            "observed": observed.slug,
                            "changed_graph": false,
                            "changed_files": []
                        }),
                    ));
                }
                (Some(mut target), _) => {
                    if target.baseline.identity_hash != post_baseline.identity_hash
                        && !fingerprint_usable(&post_baseline)
                    {
                        return Ok(result_with_data(
                            "unknown",
                            "destination layout has no usable fingerprint",
                            json!({"changed_graph": false, "changed_files": []}),
                        ));
                    }
                    if remember_place_observation(&mut target, &post_baseline) {
                        changed_files.push(commit_place(root, &target)?);
                    }
                    target
                }
                (None, Some(observed)) => {
                    return Ok(result_with_data(
                        "label_mismatch",
                        "tap reached a known place with a different label",
                        json!({
                            "requested_label": slug,
                            "observed": observed.slug,
                            "changed_graph": false,
                            "changed_files": []
                        }),
                    ));
                }
                (None, None) => {
                    if !fingerprint_usable(&post_baseline) {
                        clear_session_place(root, adb)?;
                        return Ok(result_with_data(
                            "unknown",
                            "destination layout has no usable fingerprint",
                            json!({"changed_graph": false, "changed_files": []}),
                        ));
                    }
                    let place = place_from_label(label, &post_baseline);
                    changed_files.push(commit_place(root, &place)?);
                    graph.places.insert(place.id.clone(), place.clone());
                    place
                }
            }
        }
        None => {
            if let Some(place) = matched_post {
                place
            } else {
                save_pending(
                    root,
                    adb,
                    &PendingTransition {
                        source: edge_source.clone(),
                        recipe: recipe.clone(),
                        destination: post_baseline,
                        intent: edge_intent.map(str::to_string),
                    },
                )?;
                clear_session_place(root, adb)?;
                return Ok(result_with_data(
                    "needs_label",
                    "tap reached an unknown destination; rerun whereami --label to commit it",
                    json!({
                        "source": source_place.slug,
                        "changed_graph": false,
                        "changed_files": []
                    }),
                ));
            }
        }
    };

    let edge = edge_from_parts(
        &edge_source,
        &endpoint_for_place(&destination),
        recipe,
        edge_intent,
    );
    changed_files.push(commit_edge(root, &edge)?);
    clear_pending(root, adb)?;
    save_session_place(
        root,
        adb,
        &endpoint_for_place(&destination),
        &post_baseline,
        &post_layout,
    )?;

    Ok(result_with_data(
        "ok",
        "tap transition recorded",
        json!({
            "from": edge_source.slug,
            "to": destination.slug,
            "edge": edge.id,
            "changed_graph": !changed_files.is_empty(),
            "changed_files": changed_files_json(&changed_files)
        }),
    ))
}

fn build_and_execute_tap_action<AR: CommandRunner, DR: CommandRunner>(
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    pre_layout: &Value,
    selector: Option<&str>,
    point: Option<&str>,
    screenshot_label: Option<i64>,
    screenshot: Option<&str>,
) -> Result<ActionStep> {
    if let Some(selector) = selector {
        let tap_point = resolve_selector_point(pre_layout, selector)?;
        adb.tap(tap_point)?;
        let (kind, value) = parse_selector(selector)?;
        return Ok(ActionStep {
            kind: "tap".to_string(),
            selector: Some(Selector { kind, value }),
            point: None,
            viewport: None,
            direction: None,
        });
    }
    if let Some(point) = point {
        let (x, y) = parse_point(point)?;
        adb.tap(TapPoint { x, y })?;
        let viewport = adb.display_size().ok();
        if viewport.is_none() {
            anyhow::bail!("viewport_required_for_geometry_edge");
        }
        return Ok(ActionStep {
            kind: "tap".to_string(),
            selector: None,
            point: Some(Point { x, y }),
            viewport,
            direction: None,
        });
    }
    let label = screenshot_label.expect("checked action count");
    let screenshot = screenshot.expect("checked screenshot");
    android.screen_capture(screenshot, true)?;
    let resolved = android.screen_resolve(screenshot, &format!("input tap #{label}"))?;
    let tap_point = parse_input_tap(&resolved.stdout)?;
    adb.tap(tap_point)?;
    let viewport = adb.display_size().ok();
    if viewport.is_none() {
        anyhow::bail!("viewport_required_for_geometry_edge");
    }
    Ok(ActionStep {
        kind: "tap".to_string(),
        selector: None,
        point: Some(Point {
            x: tap_point.x,
            y: tap_point.y,
        }),
        viewport,
        direction: None,
    })
}

fn scroll_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    direction: &str,
) -> Result<Value> {
    let pre_layout = observe_layout(android, false)?;
    let pre_orientation = orient_layout(root, &pre_layout, None, true, adb)?;
    let source = pre_orientation.matched_place.clone();
    let viewport = adb.display_size().ok().unwrap_or(Viewport {
        width: 1080,
        height: 2400,
    });
    let (sx, sy, ex, ey) = swipe_for_direction(direction, viewport);
    adb.swipe(sx, sy, ex, ey, 350)?;
    let post_layout = observe_after_action(android, Some(&pre_orientation.baseline))?;
    let post_orientation = orient_layout(root, &post_layout, None, true, adb)?;
    let step = ActionStep {
        kind: "scroll".to_string(),
        selector: None,
        point: None,
        viewport: None,
        direction: Some(direction.to_string()),
    };
    if let Some(source) = source.clone() {
        if let Some(dest) = post_orientation
            .matched_place
            .clone()
            .filter(|dest| source.id != dest.id)
        {
            let edge = edge_from_parts(
                &endpoint_for_place(&source),
                &endpoint_for_place(&dest),
                vec![step],
                Some("scroll"),
            );
            let path = commit_edge(root, &edge)?;
            save_session_place(
                root,
                adb,
                &endpoint_for_place(&dest),
                &post_orientation.baseline,
                &post_layout,
            )?;
            return Ok(result_with_data(
                "ok",
                "scroll transition recorded",
                json!({
                    "from": source.slug,
                    "to": dest.slug,
                    "edge": edge.id,
                    "changed_graph": true,
                    "changed_files": changed_files_json(&[path])
                }),
            ));
        }
        save_pending(
            root,
            adb,
            &PendingTransition {
                source: endpoint_for_place(&source),
                recipe: vec![step],
                destination: post_orientation.baseline.clone(),
                intent: Some("scroll".to_string()),
            },
        )?;
    }
    remember_orientation_session(root, adb, &post_orientation, &post_layout)?;
    Ok(result_with_data(
        "ok",
        "scroll executed",
        json!({
            "place": post_orientation.matched_place.map(|place| place.slug),
            "changed_graph": !post_orientation.changed_files.is_empty(),
            "changed_files": changed_files_json(&post_orientation.changed_files)
        }),
    ))
}

fn back_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
) -> Result<Value> {
    let pre_layout = observe_layout(android, false)?;
    let pre_orientation = orient_layout(root, &pre_layout, None, true, adb)?;
    adb.back()?;
    let post_layout = observe_after_action(android, Some(&pre_orientation.baseline))?;
    let post_orientation = orient_layout(root, &post_layout, None, true, adb)?;
    if let (Some(source), Some(dest)) = (
        pre_orientation.matched_place.clone(),
        post_orientation.matched_place.clone(),
    ) {
        if source.id != dest.id {
            let edge = edge_from_parts(
                &endpoint_for_place(&source),
                &endpoint_for_place(&dest),
                vec![ActionStep {
                    kind: "press_back".to_string(),
                    selector: None,
                    point: None,
                    viewport: None,
                    direction: None,
                }],
                Some("press back"),
            );
            let path = commit_edge(root, &edge)?;
            save_session_place(
                root,
                adb,
                &endpoint_for_place(&dest),
                &post_orientation.baseline,
                &post_layout,
            )?;
            return Ok(result_with_data(
                "ok",
                "back transition recorded",
                json!({
                    "from": source.slug,
                    "to": dest.slug,
                    "edge": edge.id,
                    "changed_graph": true,
                    "changed_files": changed_files_json(&[path])
                }),
            ));
        }
    }
    remember_orientation_session(root, adb, &post_orientation, &post_layout)?;
    Ok(result_with_data(
        "ok",
        "back executed",
        json!({
            "place": post_orientation.matched_place.map(|place| place.slug),
            "changed_graph": !post_orientation.changed_files.is_empty(),
            "changed_files": changed_files_json(&post_orientation.changed_files)
        }),
    ))
}

fn go_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    target: &str,
) -> Result<Value> {
    let graph = load_graph(root)?;
    let session = load_session_place(root, adb)?;
    let mut session_used = false;
    let (mut current_layout, current_place, orientation_changes) = if let Some(session) = session {
        if let Some(place) = graph
            .places
            .get(&session.place.id)
            .cloned()
            .filter(|place| {
                place.baseline.identity_hash == session.baseline.identity_hash
                    || place
                        .variants
                        .iter()
                        .any(|variant| variant.identity_hash == session.baseline.identity_hash)
            })
        {
            session_used = true;
            (session.layout, place, Vec::new())
        } else {
            clear_session_place(root, adb)?;
            let layout = observe_layout(android, false)?;
            let orientation = orient_layout(root, &layout, None, true, adb)?;
            let Some(place) = orientation.matched_place.clone() else {
                return Ok(result_with_data(
                    "unknown",
                    "current place is unknown; label it before using go",
                    json!({
                        "whereami": orientation_json(root, &orientation, false),
                        "changed_graph": false,
                        "changed_files": []
                    }),
                ));
            };
            (layout, place, orientation.changed_files.clone())
        }
    } else {
        let layout = observe_layout(android, false)?;
        let orientation = orient_layout(root, &layout, None, true, adb)?;
        let Some(place) = orientation.matched_place.clone() else {
            return Ok(result_with_data(
                "unknown",
                "current place is unknown; label it before using go",
                json!({
                    "whereami": orientation_json(root, &orientation, false),
                    "changed_graph": false,
                    "changed_files": []
                }),
            ));
        };
        (layout, place, orientation.changed_files.clone())
    };
    let mut viewport_used = false;
    let mut plan = resolve_path(&graph, target, &current_place.id, None);
    if plan.status == "no_compatible_path" {
        let viewport = adb.display_size().ok();
        viewport_used = viewport.is_some();
        plan = resolve_path(&graph, target, &current_place.id, viewport);
    }
    if plan.status != "ok" {
        return Ok(result_with_data(
            &plan.status,
            "no executable known UI path",
            json!({"plan": plan.to_json(), "changed_graph": false, "changed_files": []}),
        ));
    }
    let mut executed = Vec::new();
    let mut changed_files = orientation_changes;
    let mut last_place = current_place.clone();
    for edge in &plan.edges {
        execute_recipe(android, adb, &edge.recipe, Some(&current_layout))?;
        let previous_baseline = fingerprint_layout(&current_layout);
        let post_layout = observe_after_action(android, Some(&previous_baseline))?;
        let post_baseline = fingerprint_layout(&post_layout);
        let graph = load_graph(root)?;
        let post_match = match_place(&post_baseline, graph.places.values().cloned());
        let observed = if post_match.status == "unknown" {
            None
        } else {
            post_match
                .place_id
                .as_deref()
                .and_then(|id| graph.places.get(id))
                .cloned()
        };
        match observed {
            Some(mut place) if place.id == edge.to.id => {
                if remember_place_observation(&mut place, &post_baseline) {
                    changed_files.push(commit_place(root, &place)?);
                }
                last_place = place.clone();
                executed.push(json!({"edge": edge.id, "to": edge.to.slug, "status": "ok"}));
            }
            Some(place) => {
                return Ok(result_with_data(
                    "label_mismatch",
                    "edge reached a different known place",
                    json!({
                        "edge": edge.id,
                        "expected": edge.to.slug,
                        "observed": place.slug,
                        "executed": executed,
                        "changed_graph": !changed_files.is_empty(),
                        "changed_files": changed_files_json(&changed_files)
                    }),
                ));
            }
            None => {
                return Ok(result_with_data(
                    "unknown",
                    "edge reached an unknown layout; graph unchanged",
                    json!({
                        "edge": edge.id,
                        "expected": edge.to.slug,
                        "executed": executed,
                        "changed_graph": !changed_files.is_empty(),
                        "changed_files": changed_files_json(&changed_files)
                    }),
                ));
            }
        }
        current_layout = post_layout;
    }
    let current_baseline = fingerprint_layout(&current_layout);
    save_session_place(
        root,
        adb,
        &endpoint_for_place(&last_place),
        &current_baseline,
        &current_layout,
    )?;
    Ok(result_with_data(
        "ok",
        "navigation completed",
        json!({
            "target": normalize_label(target),
            "planned_path": plan.edges.iter().map(|edge| edge.id.clone()).collect::<Vec<_>>(),
            "executed_steps": executed,
            "start_source": if session_used { "session" } else { "layout" },
            "viewport_used": viewport_used,
            "changed_graph": !changed_files.is_empty(),
            "changed_files": changed_files_json(&changed_files)
        }),
    ))
}

fn execute_recipe<AR: CommandRunner, DR: CommandRunner>(
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    recipe: &[ActionStep],
    initial_layout: Option<&Value>,
) -> Result<()> {
    let mut cached_layout = initial_layout.cloned();
    for step in recipe {
        match step.kind.as_str() {
            "tap" => {
                if let Some(selector) = &step.selector {
                    let layout = match cached_layout.take() {
                        Some(layout) => layout,
                        None => observe_layout(android, false)?,
                    };
                    let point = resolve_selector_point(
                        &layout,
                        &format!("{}={}", selector.kind, selector.value),
                    )?;
                    adb.tap(point)?;
                } else if let Some(point) = step.point {
                    if step.viewport.is_some() && adb.display_size().ok() != step.viewport {
                        anyhow::bail!("geometry edge viewport does not match current device");
                    }
                    adb.tap(TapPoint {
                        x: point.x,
                        y: point.y,
                    })?;
                } else {
                    anyhow::bail!("tap step has neither selector nor point");
                }
                cached_layout = None;
            }
            "scroll" => {
                let viewport = adb.display_size().ok().unwrap_or(Viewport {
                    width: 1080,
                    height: 2400,
                });
                let direction = step.direction.as_deref().unwrap_or("down");
                let (sx, sy, ex, ey) = swipe_for_direction(direction, viewport);
                adb.swipe(sx, sy, ex, ey, 350)?;
                cached_layout = None;
            }
            "press_back" => {
                adb.back()?;
                cached_layout = None;
            }
            other => anyhow::bail!("unsupported recipe step: {other}"),
        }
    }
    Ok(())
}

fn doctor(root: &Path) -> Value {
    let repo_checks = validate_graph(root);
    let repo_ok = repo_checks.iter().all(|check| check["status"] == "pass");
    let android_ok = command_on_path("android");
    let adb_ok = command_on_path("adb");
    let device_ok = adb_ok && adb_device_ready();
    json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": if repo_ok && android_ok && adb_ok && device_ok { "ok" } else { "config_error" },
        "ok": repo_ok && android_ok && adb_ok && device_ok,
        "repo_ok": repo_ok,
        "device_ok": device_ok,
        "checks": {
            "repo": repo_checks,
            "environment": [
                {"name": "android", "status": if android_ok { "pass" } else { "fail" }},
                {"name": "adb", "status": if adb_ok { "pass" } else { "fail" }},
                {"name": "device", "status": if device_ok { "pass" } else { "fail" }}
            ]
        }
    })
}

fn command_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|path| path.join(name).exists()))
        .unwrap_or(false)
}

fn adb_device_ready() -> bool {
    let output = ProcessCommand::new("adb").arg("get-state").output();
    matches!(output, Ok(output) if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "device")
}

fn place_from_label(label: &str, baseline: &PlaceBaseline) -> Place {
    let slug = normalize_label(label);
    Place {
        schema_version: PLACE_SCHEMA_VERSION.to_string(),
        id: place_id_for_slug(&slug),
        slug,
        label: label.trim().to_string(),
        baseline: baseline.clone(),
        variants: Vec::new(),
    }
}

fn remember_place_observation(place: &mut Place, baseline: &PlaceBaseline) -> bool {
    if place.baseline.identity_hash == baseline.identity_hash
        || place
            .variants
            .iter()
            .any(|variant| variant.identity_hash == baseline.identity_hash)
    {
        return false;
    }
    if !fingerprint_usable(baseline) {
        return false;
    }
    if !fingerprint_usable(&place.baseline) {
        place.baseline = baseline.clone();
        return true;
    }
    place.variants.push(baseline.clone());
    place
        .variants
        .sort_by(|left, right| left.identity_hash.cmp(&right.identity_hash));
    true
}

fn endpoint_for_place(place: &Place) -> EdgeEndpoint {
    EdgeEndpoint {
        id: place.id.clone(),
        slug: place.slug.clone(),
    }
}

fn relabel_place(
    root: &Path,
    place: &Place,
    label: &str,
    baseline: &PlaceBaseline,
) -> Result<(Place, Vec<PathBuf>)> {
    let graph = load_graph(root)?;
    let mut changed = Vec::new();
    let new_slug = normalize_label(label);
    let mut new_place = place.clone();
    let old_id = new_place.id.clone();
    new_place.slug = new_slug.clone();
    new_place.label = label.trim().to_string();
    new_place.id = place_id_for_slug(&new_slug);
    new_place.baseline = baseline.clone();
    remove_place_file(root, &old_id)?;
    changed.push(commit_place(root, &new_place)?);
    for edge in graph.edges.values() {
        let mut updated = edge.clone();
        let mut touched = false;
        if updated.from.id == old_id {
            updated.from = endpoint_for_place(&new_place);
            touched = true;
        }
        if updated.to.id == old_id {
            updated.to = endpoint_for_place(&new_place);
            touched = true;
        }
        if touched {
            let old_path = edge_path(root, &updated.id);
            if old_path.exists() {
                fs::remove_file(old_path)?;
            }
            updated.id = edge_id(&updated.from, &updated.to, &updated.recipe);
            changed.push(commit_edge(root, &updated)?);
        }
    }
    Ok((new_place, changed))
}

fn edge_from_parts(
    from: &EdgeEndpoint,
    to: &EdgeEndpoint,
    recipe: Vec<ActionStep>,
    intent: Option<&str>,
) -> Edge {
    Edge {
        schema_version: EDGE_SCHEMA_VERSION.to_string(),
        id: edge_id(from, to, &recipe),
        from: from.clone(),
        to: to.clone(),
        intent: intent.map(str::to_string),
        recipe,
    }
}

fn edge_id(from: &EdgeEndpoint, to: &EdgeEndpoint, recipe: &[ActionStep]) -> String {
    let primary = recipe
        .first()
        .map(action_fingerprint)
        .unwrap_or_else(|| "action".to_string());
    let readable = format!("edge_{}__{}__{}", from.slug, to.slug, primary);
    let slug = sanitize_id(&readable);
    if slug.len() <= 96 {
        slug
    } else {
        let digest =
            Sha256::digest(canonical_json(&serde_json::to_value(recipe).unwrap()).as_bytes());
        format!("edge_{}__{}__{:x}", from.slug, to.slug, digest)[..96].to_string()
    }
}

fn action_fingerprint(step: &ActionStep) -> String {
    match step.kind.as_str() {
        "tap" => {
            if let Some(selector) = &step.selector {
                format!("tap_{}_{}", selector.kind, selector.value)
            } else if let Some(point) = step.point {
                format!("tap_point_{}_{}", point.x, point.y)
            } else {
                "tap".to_string()
            }
        }
        "scroll" => format!("scroll_{}", step.direction.as_deref().unwrap_or("down")),
        "press_back" => "press_back".to_string(),
        other => other.to_string(),
    }
}

fn sanitize_id(value: &str) -> String {
    let mut out = String::new();
    let mut last_underscore = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            out.push(ch);
            last_underscore = false;
        } else if !last_underscore {
            out.push('_');
            last_underscore = true;
        }
    }
    out.trim_matches('_').to_string()
}

fn parse_selector(selector: &str) -> Result<(String, String)> {
    let (kind, value) = selector
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("selector must use kind=value syntax"))?;
    Ok((kind.trim().to_string(), value.trim().to_string()))
}

fn parse_point(point: &str) -> Result<(i64, i64)> {
    let (x, y) = point
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("--point must be x,y"))?;
    Ok((x.trim().parse()?, y.trim().parse()?))
}

fn swipe_for_direction(direction: &str, viewport: Viewport) -> (i64, i64, i64, i64) {
    let x = viewport.width / 2;
    match direction {
        "up" => (x, viewport.height / 4, x, viewport.height * 5 / 6),
        "left" => (
            viewport.width * 2 / 3,
            viewport.height / 2,
            viewport.width / 3,
            viewport.height / 2,
        ),
        "right" => (
            viewport.width / 3,
            viewport.height / 2,
            viewport.width * 2 / 3,
            viewport.height / 2,
        ),
        _ => (x, viewport.height * 5 / 6, x, viewport.height / 4),
    }
}

fn fingerprint_summary(baseline: &PlaceBaseline) -> Value {
    json!({
        "identity_hash": baseline.identity_hash,
        "selectors": baseline.fingerprint.selectors.iter().take(12).collect::<Vec<_>>(),
        "static_text": baseline.fingerprint.static_text.iter().take(12).collect::<Vec<_>>(),
        "roles": baseline.fingerprint.roles
    })
}

fn result_with_data(status: &str, summary: &str, data: Value) -> Value {
    let mut result = serde_json::to_value(MinimapResult::new(status, summary, data)).unwrap();
    if status == "needs_label" {
        result["recommended_action"] = json!(
            "run whereami --label <place> if the current destination should be added to the graph"
        );
    }
    result
}

fn changed_files_json(paths: &[PathBuf]) -> Value {
    json!(paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>())
}

fn pending_path<DR: CommandRunner>(root: &Path, adb: &mut Adb<DR>) -> Result<PathBuf> {
    let repo = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string();
    let repo_hash = format!("{:x}", Sha256::digest(repo.as_bytes()));
    let serial = adb
        .serial()
        .unwrap_or_else(|_| "unknown-device".to_string());
    let package = load_config(root)
        .ok()
        .and_then(|config| {
            config
                .app_profiles
                .get(&config.active_app_profile)
                .map(|profile| profile.android_package.clone())
        })
        .filter(|package| !package.is_empty())
        .unwrap_or_else(|| "default-package".to_string());
    Ok(std::env::temp_dir()
        .join("minimap")
        .join(&repo_hash[..16])
        .join(sanitize_id(&serial))
        .join(sanitize_id(&package))
        .join("pending-transition.json"))
}

fn session_path<DR: CommandRunner>(root: &Path, adb: &mut Adb<DR>) -> Result<PathBuf> {
    let mut path = pending_path(root, adb)?;
    path.set_file_name("session-place.json");
    Ok(path)
}

fn remember_orientation_session<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
    orientation: &Orientation,
    layout: &Value,
) -> Result<()> {
    if let Some(place) = &orientation.matched_place {
        save_session_place(
            root,
            adb,
            &endpoint_for_place(place),
            &orientation.baseline,
            layout,
        )
    } else {
        clear_session_place(root, adb)
    }
}

fn save_session_place<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
    place: &EdgeEndpoint,
    baseline: &PlaceBaseline,
    layout: &Value,
) -> Result<()> {
    let path = session_path(root, adb)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let value = json!({
        "place": place,
        "baseline": baseline,
        "layout": redact_layout(layout)
    });
    fs::write(path, canonical_json(&value))?;
    Ok(())
}

fn load_session_place<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
) -> Result<Option<SessionPlace>> {
    let path = session_path(root, adb)?;
    if !path.exists() {
        return Ok(None);
    }
    if let Ok(metadata) = fs::metadata(&path) {
        if let Ok(modified) = metadata.modified() {
            let expired = SystemTime::now()
                .duration_since(modified)
                .map(|age| age > Duration::from_secs(SESSION_TTL_SECS))
                .unwrap_or(true);
            if expired {
                let _ = fs::remove_file(&path);
                return Ok(None);
            }
        }
    }
    let value: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    Ok(Some(SessionPlace {
        place: serde_json::from_value(value["place"].clone())?,
        baseline: serde_json::from_value(value["baseline"].clone())?,
        layout: value["layout"].clone(),
    }))
}

fn load_recent_session_place<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
    max_age: Duration,
) -> Result<Option<SessionPlace>> {
    let path = session_path(root, adb)?;
    if !path.exists() {
        return Ok(None);
    }
    let fresh = fs::metadata(&path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age <= max_age)
        .unwrap_or(false);
    if !fresh {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    Ok(Some(SessionPlace {
        place: serde_json::from_value(value["place"].clone())?,
        baseline: serde_json::from_value(value["baseline"].clone())?,
        layout: value["layout"].clone(),
    }))
}

fn graph_place_for_session(graph: &Graph, session: &SessionPlace) -> Option<Place> {
    graph
        .places
        .get(&session.place.id)
        .cloned()
        .filter(|place| {
            place.baseline.identity_hash == session.baseline.identity_hash
                || place
                    .variants
                    .iter()
                    .any(|variant| variant.identity_hash == session.baseline.identity_hash)
        })
}

fn clear_session_place<DR: CommandRunner>(root: &Path, adb: &mut Adb<DR>) -> Result<()> {
    let path = session_path(root, adb)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn save_pending<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
    pending: &PendingTransition,
) -> Result<()> {
    let path = pending_path(root, adb)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let value = json!({
        "source": pending.source,
        "recipe": pending.recipe,
        "destination": pending.destination,
        "intent": pending.intent
    });
    fs::write(path, canonical_json(&value))?;
    Ok(())
}

fn load_pending<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
) -> Result<Option<PendingTransition>> {
    let path = pending_path(root, adb)?;
    if !path.exists() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(&fs::read_to_string(path)?)?;
    Ok(Some(PendingTransition {
        source: serde_json::from_value(value["source"].clone())?,
        recipe: serde_json::from_value(value["recipe"].clone())?,
        destination: serde_json::from_value(value["destination"].clone())?,
        intent: value["intent"].as_str().map(str::to_string),
    }))
}

fn clear_pending<DR: CommandRunner>(root: &Path, adb: &mut Adb<DR>) -> Result<()> {
    let path = pending_path(root, adb)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn print_json(value: &Value) {
    print!("{}", canonical_json(value));
}
