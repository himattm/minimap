use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use minimap_android::{
    android_analytics_spool_failure, parse_input_tap, parse_layout_output, resolve_selector_point,
    Adb, AndroidCli, AndroidDeviceIdentity, CommandFailure, CommandRunner, LayoutOutput,
    SubprocessRunner, TapPoint,
};
use minimap_core::{
    detect_overlay, fingerprint_layout, fingerprint_usable, match_place, normalize_label,
    place_id_for_slug, redact_layout,
};
use minimap_graph::{exit_code_for_status, resolve_path};
use minimap_repo::{
    commit_edge, commit_place, edge_path, load_config, load_graph, remove_place_file,
    resolve_app_package, run_init, validate_graph, AppPackageResolution, Graph, InitOptions,
};
use minimap_schemas::{
    canonical_json, ActionStep, Edge, EdgeEndpoint, MinimapResult, Place, PlaceBaseline, Point,
    Selector, Viewport, EDGE_SCHEMA_VERSION, PLACE_SCHEMA_VERSION, RESULT_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

const DEFAULT_ACTION_SETTLE_MS: u64 = 1_000;
const SESSION_TTL_SECS: u64 = 600;
const PENDING_TTL_SECS: u64 = 600;
const LAYOUT_CACHE_TTL_SECS: u64 = 30;

#[derive(Debug, Parser)]
#[command(name = "minimap")]
#[command(version)]
#[command(about = "Android navigation memory for AI agents.")]
struct Cli {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    quiet: bool,
    /// Include raw subprocess diagnostics in structured error output.
    #[arg(long, global = true)]
    verbose: bool,
    #[arg(long = "no-color")]
    no_color: bool,
    /// Android device serial to target when more than one device is attached.
    #[arg(
        long = "device",
        visible_alias = "serial",
        global = true,
        env = "ANDROID_SERIAL"
    )]
    device: Option<String>,
    /// Allow capture even when the foreground package differs from the active app profile.
    #[arg(long, global = true)]
    allow_package_mismatch: bool,
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
    Doctor {
        /// Probe the foreground app and perform one minimal Android layout capture.
        #[arg(long)]
        live: bool,
    },
    /// Identify the current semantic place from one Android layout observation.
    Whereami {
        #[arg(long)]
        label: Option<String>,
        /// If the label slug collides with a different known place, append the
        /// smallest free numeric suffix (e.g. `account-settings-2`) instead of
        /// returning label_mismatch.
        #[arg(long = "allow-duplicate-label")]
        allow_duplicate_label: bool,
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
        /// If the destination label slug collides with a different known place,
        /// append the smallest free numeric suffix (e.g. `account-settings-2`)
        /// instead of returning label_mismatch.
        #[arg(long = "allow-duplicate-label")]
        allow_duplicate_label: bool,
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
    allow_duplicate_label: bool,
}

#[derive(Debug, Clone)]
struct DeviceSelection {
    serial: Option<String>,
    source: &'static str,
}

enum CaptureGuard {
    Ready(Value),
    Rejected(Value),
}

macro_rules! require_ready_device {
    ($guard:expr) => {
        match $guard? {
            CaptureGuard::Ready(device) => device,
            CaptureGuard::Rejected(result) => return emit_result(result),
        }
    };
}

fn main() {
    let cli = Cli::parse();
    let verbose = cli.verbose;
    let code = match run(cli) {
        Ok(code) => code,
        Err(error) => {
            if let Some(result) = android_analytics_error_result(&error, verbose) {
                print_json(&result);
                6
            } else {
                let result = MinimapResult::new(
                    "config_error",
                    error.to_string(),
                    json!({"error": {"message": error.to_string()}}),
                );
                print_json(&serde_json::to_value(result).expect("error json"));
                7
            }
        }
    };
    std::process::exit(code);
}

fn android_analytics_error_result(error: &anyhow::Error, verbose: bool) -> Option<Value> {
    let failure = error.downcast_ref::<CommandFailure>()?;
    let analytics = android_analytics_spool_failure(failure)?;
    let mut result = json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "environment_error",
        "summary": "Android CLI analytics spool is not writable",
        "error": {
            "code": "android_cli_analytics_spool_unwritable",
            "blocked_path": analytics.blocked_path,
            "recovery": "Grant write access to the Android CLI analytics spool, or rerun Minimap in a filesystem profile where that path is writable."
        },
        "changed_graph": false,
        "changed_files": []
    });
    if verbose {
        result["debug"] = json!({
            "command": failure.result.args,
            "status": failure.result.status,
            "stdout": failure.result.stdout,
            "stderr": failure.result.stderr
        });
    }
    Some(result)
}

fn resolve_device_selection(root: &Path, requested: Option<String>) -> DeviceSelection {
    if let Some(serial) = requested.filter(|serial| !serial.trim().is_empty()) {
        return DeviceSelection {
            serial: Some(serial),
            source: "argument_or_environment",
        };
    }
    let configured = load_config(root).ok().and_then(|config| {
        config
            .app_profiles
            .get(&config.active_app_profile)
            .and_then(|profile| profile.android_device.clone())
            .filter(|serial| !serial.trim().is_empty())
    });
    match configured {
        Some(serial) => DeviceSelection {
            serial: Some(serial),
            source: "config",
        },
        None => DeviceSelection {
            serial: None,
            source: "single_attached",
        },
    }
}

fn device_json(identity: AndroidDeviceIdentity, selection_source: &str) -> Value {
    json!({
        "serial": identity.serial,
        "model": identity.model,
        "api_level": identity.api_level,
        "selection_source": selection_source
    })
}

fn attach_device(mut result: Value, device: Value) -> Value {
    result["device"] = device;
    result
}

fn run(cli: Cli) -> Result<i32> {
    let root = PathBuf::from(".");
    let selection = resolve_device_selection(&root, cli.device);
    let serial = selection.serial.clone();
    let allow_package_mismatch = cli.allow_package_mismatch;
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
        Commands::Doctor { live } => {
            let result = doctor(&root, &selection, live, cli.verbose);
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Whereami {
            label,
            allow_duplicate_label,
        } => {
            let mut android = AndroidCli::new(SubprocessRunner, serial.clone());
            let mut adb = Adb::new(SubprocessRunner, serial);
            let device = require_ready_device!(capture_guard_result(
                &root,
                &mut adb,
                allow_package_mismatch,
                selection.source,
            ));
            let result = whereami_result(
                &root,
                &mut android,
                &mut adb,
                label.as_deref(),
                allow_duplicate_label,
                true,
            )?;
            emit_result(attach_device(result, device))
        }
        Commands::Go { target } => {
            let mut android = AndroidCli::new(SubprocessRunner, serial.clone());
            let mut adb = Adb::new(SubprocessRunner, serial);
            let device = require_ready_device!(capture_guard_result(
                &root,
                &mut adb,
                allow_package_mismatch,
                selection.source,
            ));
            let result = go_result(&root, &mut android, &mut adb, &target)?;
            emit_result(attach_device(result, device))
        }
        Commands::Tap {
            selector,
            point,
            screenshot_label,
            screenshot,
            label,
            reason,
            allow_duplicate_label,
        } => {
            let mut android = AndroidCli::new(SubprocessRunner, serial.clone());
            let mut adb = Adb::new(SubprocessRunner, serial);
            let device = require_ready_device!(capture_guard_result(
                &root,
                &mut adb,
                allow_package_mismatch,
                selection.source,
            ));
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
                    allow_duplicate_label,
                },
            )?;
            emit_result(attach_device(result, device))
        }
        Commands::Scroll { direction } => {
            let mut android = AndroidCli::new(SubprocessRunner, serial.clone());
            let mut adb = Adb::new(SubprocessRunner, serial);
            let device = require_ready_device!(capture_guard_result(
                &root,
                &mut adb,
                allow_package_mismatch,
                selection.source,
            ));
            let result = scroll_result(&root, &mut android, &mut adb, &direction)?;
            emit_result(attach_device(result, device))
        }
        Commands::Back => {
            let mut android = AndroidCli::new(SubprocessRunner, serial.clone());
            let mut adb = Adb::new(SubprocessRunner, serial);
            let device = require_ready_device!(capture_guard_result(
                &root,
                &mut adb,
                allow_package_mismatch,
                selection.source,
            ));
            let result = back_result(&root, &mut android, &mut adb)?;
            emit_result(attach_device(result, device))
        }
        Commands::Layout { diff } => {
            let mut android = AndroidCli::new(SubprocessRunner, serial.clone());
            let mut adb = Adb::new(SubprocessRunner, serial);
            let device = require_ready_device!(capture_guard_result(
                &root,
                &mut adb,
                allow_package_mismatch,
                selection.source,
            ));
            let result = layout_result(&root, &mut android, &mut adb, diff)?;
            emit_result(attach_device(result, device))
        }
    }
}

fn observe_layout<R: CommandRunner>(android: &mut AndroidCli<R>, diff: bool) -> Result<Value> {
    Ok(observe_layout_output(android, diff)?.layout)
}

fn observe_layout_output<R: CommandRunner>(
    android: &mut AndroidCli<R>,
    diff: bool,
) -> Result<LayoutOutput> {
    let command = android.layout(diff)?;
    parse_layout_output(&command.stdout)
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
    allow_duplicate_label: bool,
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
    let orientation = orient_layout(
        root,
        &layout,
        label,
        allow_duplicate_label,
        allow_write,
        adb,
    )?;
    remember_orientation_session(root, adb, &orientation, &layout)?;
    Ok(orientation_json(root, &orientation, true))
}

fn orient_layout<DR: CommandRunner>(
    root: &Path,
    layout: &Value,
    label: Option<&str>,
    allow_duplicate_label: bool,
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
        // normalize_label always yields a non-empty pure-ASCII slug (Tranche C),
        // so there is no empty-slug case to guard.
        let slug = normalize_label(label);
        let existing_label_place = graph
            .places
            .values()
            .find(|place| place.slug == slug)
            .cloned();
        match (matched_place.clone(), existing_label_place) {
            (Some(place), Some(existing)) if place.id != existing.id => {
                // The current screen matched a known place, but the requested
                // label slug is already owned by a DIFFERENT place. By default
                // this is a label_mismatch; under --allow-duplicate-label we
                // relabel the matched place with the smallest free numeric
                // suffix instead.
                if allow_write && allow_duplicate_label {
                    let unique = unique_label(&graph, label, &slug);
                    let (new_place, mut files) = relabel_place(root, &place, &unique, &baseline)?;
                    changed_files.append(&mut files);
                    matched_place = Some(new_place);
                    status = "ok".to_string();
                } else {
                    return Ok(Orientation {
                        status: "label_mismatch".to_string(),
                        baseline,
                        matched_place: Some(place),
                        confidence: matched.confidence,
                        hash_matched: matched.hash_matched,
                        changed_files,
                    });
                }
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
                // A NEW fingerprint whose slug collides with a different existing
                // place: label_mismatch (no write) by default, or a fresh
                // suffixed place under --allow-duplicate-label.
                if allow_write && allow_duplicate_label && fingerprint_usable(&baseline) {
                    let unique = unique_label(&graph, label, &slug);
                    let place = place_from_label(&unique, &baseline);
                    changed_files.push(commit_place(root, &place)?);
                    commit_pending_edge_for_place(root, adb, &graph, &place, &baseline)?
                        .into_iter()
                        .for_each(|file| changed_files.push(file));
                    matched_place = Some(place);
                    status = "ok".to_string();
                } else {
                    return Ok(Orientation {
                        status: "label_mismatch".to_string(),
                        baseline,
                        matched_place: None,
                        confidence: matched.confidence,
                        hash_matched: matched.hash_matched,
                        changed_files,
                    });
                }
            }
            (None, None) => {
                if allow_write && fingerprint_usable(&baseline) {
                    let place = place_from_label(label, &baseline);
                    changed_files.push(commit_place(root, &place)?);
                    commit_pending_edge_for_place(root, adb, &graph, &place, &baseline)?
                        .into_iter()
                        .for_each(|file| changed_files.push(file));
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
            .zip(orientation.matched_place.as_ref())
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
    let mut value = orientation_json(root, &orientation, true);
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
                // Session files written by older Minimap versions may contain
                // the raw Android stdout as a JSON string. Normalize on read so
                // a cache hit cannot reintroduce the unstable public contract.
                let cached_output = parse_layout_output(&serde_json::to_string(&session.layout)?)?;
                return Ok(json!({
                    "schema_version": RESULT_SCHEMA_VERSION,
                    "status": "ok",
                    "kind": "android_layout",
                    "layout": cached_output.layout,
                    "android_cli_notices": cached_output.notices,
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

    let output = observe_layout_output(android, diff)?;
    let layout = output.layout;
    let (minimap, cache_hit) = if diff {
        (json!({"orientation": "unavailable_for_diff"}), false)
    } else {
        let orientation = orient_layout(root, &layout, None, false, false, adb)?;
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
        "android_cli_notices": output.notices,
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

fn capture_guard_result<R: CommandRunner>(
    root: &Path,
    adb: &mut Adb<R>,
    allow_package_mismatch: bool,
    selection_source: &str,
) -> Result<CaptureGuard> {
    if let Some(result) = device_unavailable_result(adb)? {
        return Ok(CaptureGuard::Rejected(result));
    }

    let identity = match adb.device_identity() {
        Ok(identity) => identity,
        Err(error) => {
            return Ok(CaptureGuard::Rejected(json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "environment_error",
                "summary": "Android device identity could not be read",
                "error": {
                    "code": "device_identity_unavailable",
                    "detail": error.to_string(),
                    "recovery": "Verify ADB can read ro.product.model and ro.build.version.sdk from the selected device."
                },
                "changed_graph": false,
                "changed_files": []
            })));
        }
    };
    let device = device_json(identity, selection_source);

    let resolution = resolve_app_package(root)?;
    let (expected_package, source) = match resolution {
        AppPackageResolution::Configured { package, .. } => (package, "config"),
        AppPackageResolution::Inferred { package, .. } => (package, "gradle_debug_variant"),
        AppPackageResolution::Missing { profile } => {
            return Ok(CaptureGuard::Rejected(attach_device(
                json!({
                    "schema_version": RESULT_SCHEMA_VERSION,
                    "status": "config_error",
                    "summary": "Android application package is not configured and could not be inferred",
                    "error": {
                        "code": "app_package_missing",
                        "profile": profile,
                        "recovery": "Set app_profiles.<profile>.android_package or add an inferable Android application Gradle module."
                    },
                    "changed_graph": false,
                    "changed_files": []
                }),
                device,
            )));
        }
        AppPackageResolution::Ambiguous {
            profile,
            candidates,
        } => {
            return Ok(CaptureGuard::Rejected(attach_device(
                json!({
                    "schema_version": RESULT_SCHEMA_VERSION,
                    "status": "config_error",
                    "summary": "Multiple Android application packages were inferred",
                    "error": {
                        "code": "app_package_ambiguous",
                        "profile": profile,
                        "candidates": candidates,
                        "recovery": "Set android_package explicitly for the active app profile."
                    },
                    "changed_graph": false,
                    "changed_files": []
                }),
                device,
            )));
        }
    };

    let foreground_package = match adb.foreground_package() {
        Ok(package) => package,
        Err(_) if allow_package_mismatch => return Ok(CaptureGuard::Ready(device)),
        Err(error) => {
            return Ok(CaptureGuard::Rejected(attach_device(
                json!({
                    "schema_version": RESULT_SCHEMA_VERSION,
                    "status": "environment_error",
                    "summary": "Android foreground package could not be verified",
                    "error": {
                        "code": "foreground_package_unknown",
                        "expected_package": expected_package,
                        "expected_package_source": source,
                        "detail": error.to_string(),
                        "recovery": "Launch the expected app and retry, or pass --allow-package-mismatch to override this guard."
                    },
                    "changed_graph": false,
                    "changed_files": []
                }),
                device,
            )));
        }
    };
    if foreground_package != expected_package && !allow_package_mismatch {
        return Ok(CaptureGuard::Rejected(attach_device(
            json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "app_mismatch",
                "summary": "Foreground Android app does not match the active Minimap app profile",
                "error": {
                    "code": "foreground_package_mismatch",
                    "expected_package": expected_package,
                    "expected_package_source": source,
                    "foreground_package": foreground_package,
                    "recovery": "Bring the expected app to the foreground, or pass --allow-package-mismatch for this capture."
                },
                "changed_graph": false,
                "changed_files": []
            }),
            device,
        )));
    }
    Ok(CaptureGuard::Ready(device))
}

fn device_unavailable_result<R: CommandRunner>(adb: &mut Adb<R>) -> Result<Option<Value>> {
    let devices = adb.devices()?;
    let attempted_serial = adb.configured_serial().map(str::to_string);
    let selected = attempted_serial
        .as_ref()
        .and_then(|serial| devices.iter().find(|device| device.serial == *serial));
    let ready_devices = devices
        .iter()
        .filter(|device| device.state == "device")
        .count();
    let ready = match &attempted_serial {
        Some(_) => selected.is_some_and(|device| device.state == "device"),
        None => ready_devices == 1,
    };
    if ready {
        return Ok(None);
    }

    let (code, summary, recovery) = if attempted_serial.is_none() && ready_devices > 1 {
        (
            "device_ambiguous",
            "multiple ready Android devices require an explicit selection".to_string(),
            "Pass --device <SERIAL>, set ANDROID_SERIAL, or configure android_device for the active app profile."
                .to_string(),
        )
    } else if devices.is_empty() {
        (
            "no_device",
            "no connected Android device is available".to_string(),
            "Start an emulator or connect a device, then rerun `minimap layout`.".to_string(),
        )
    } else if let Some(serial) = &attempted_serial {
        if selected.is_none() {
            (
                "device_not_found",
                format!("selected Android device `{serial}` is not attached"),
                format!("Start or connect `{serial}`, or choose an attached device with --device."),
            )
        } else {
            (
                "device_not_ready",
                format!("selected Android device `{serial}` is not ready"),
                format!(
                    "Bring `{serial}` online and authorize debugging, then rerun `minimap layout`."
                ),
            )
        }
    } else {
        (
            "no_ready_device",
            "attached Android devices are not ready".to_string(),
            "Bring one attached device online and authorize debugging, then rerun `minimap layout`."
                .to_string(),
        )
    };

    Ok(Some(json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "device_unavailable",
        "summary": summary,
        "kind": "android_layout",
        "layout": [],
        "android_cli_notices": [],
        "minimap": {"orientation": "unavailable"},
        "error": {
            "code": code,
            "attempted_serial": attempted_serial,
            "devices": devices,
            "recovery": recovery
        },
        "metrics": {
            "layout_calls_total": 0,
            "layout_json_returned_to_agent": true
        },
        "changed_graph": false,
        "changed_files": []
    })))
}

fn emit_result(result: Value) -> Result<i32> {
    let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
    print_json(&result);
    Ok(code)
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
    // Validate the supplied value before observing/orienting the layout (which can
    // write to the graph). A malformed coordinate/selector must be a guaranteed
    // no-op, not a partial mutation that then errors out.
    if let Some(point) = request.point {
        parse_point(point)?;
    }
    if let Some(selector) = request.selector {
        parse_selector(selector)?;
    }

    let pre_layout = observe_layout(android, false)?;
    let pre_orientation = orient_layout(root, &pre_layout, None, false, true, adb)?;
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
    let action = match build_and_execute_tap_action(
        android,
        adb,
        &pre_layout,
        request.selector,
        request.point,
        request.screenshot_label,
        request.screenshot,
    )? {
        TapActionOutcome::Recorded(action) => action,
        TapActionOutcome::SelectorNotFound(message) => {
            return Ok(result_with_data(
                "action_failed",
                &message,
                json!({"changed_graph": false, "changed_files": []}),
            ));
        }
        TapActionOutcome::ViewportUnavailable => {
            return Ok(result_with_data(
                "environment_error",
                "device viewport unavailable for geometry edge",
                json!({"changed_graph": false, "changed_files": []}),
            ));
        }
    };
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
            // normalize_label always yields a non-empty pure-ASCII slug.
            let slug = normalize_label(label);
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
                // The post-layout fingerprint matched the place that already owns
                // this label slug: a legitimate same-place observation, so fold it
                // in as a variant.
                (Some(mut target), Some(_observed)) => {
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
                // A NEW fingerprint (no similarity match) whose label slug collides
                // with a DIFFERENT existing place. Previously this silently merged
                // the new screen into the slug owner; now it is a label_mismatch
                // (no write) by default, or a fresh suffixed place under
                // --allow-duplicate-label.
                (Some(target), None) => {
                    if !request.allow_duplicate_label {
                        return Ok(result_with_data(
                            "label_mismatch",
                            "tap reached a new place whose label collides with a different known place; pass --allow-duplicate-label to keep both",
                            json!({
                                "requested_label": slug,
                                "collides_with": target.slug,
                                "changed_graph": false,
                                "changed_files": []
                            }),
                        ));
                    }
                    if !fingerprint_usable(&post_baseline) {
                        clear_session_place(root, adb)?;
                        return Ok(result_with_data(
                            "unknown",
                            "destination layout has no usable fingerprint",
                            json!({"changed_graph": false, "changed_files": []}),
                        ));
                    }
                    let unique = unique_label(&graph, label, &slug);
                    let place = place_from_label(&unique, &post_baseline);
                    changed_files.push(commit_place(root, &place)?);
                    graph.places.insert(place.id.clone(), place.clone());
                    place
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
                if let Some(reason) = detect_overlay(&post_layout) {
                    return Ok(result_with_data(
                        "blocked_by_overlay",
                        "a blocking overlay (e.g. a permission dialog) intercepted the transition; no edge recorded",
                        json!({
                            "reason": reason,
                            "changed_graph": false,
                            "changed_files": []
                        }),
                    ));
                }
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

/// Outcome of attempting a tap action. Runtime/device failures are surfaced as
/// structured variants so `tap_result` can map them to the right status
/// (`action_failed` / `environment_error`) instead of letting them bubble up to
/// the `main()` catch-all and collapse into `config_error`.
enum TapActionOutcome {
    Recorded(ActionStep),
    SelectorNotFound(String),
    ViewportUnavailable,
}

fn build_and_execute_tap_action<AR: CommandRunner, DR: CommandRunner>(
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    pre_layout: &Value,
    selector: Option<&str>,
    point: Option<&str>,
    screenshot_label: Option<i64>,
    screenshot: Option<&str>,
) -> Result<TapActionOutcome> {
    if let Some(selector) = selector {
        let tap_point = match resolve_selector_point(pre_layout, selector) {
            Ok(point) => point,
            Err(error) => return Ok(TapActionOutcome::SelectorNotFound(error.to_string())),
        };
        adb.tap(tap_point)?;
        let (kind, value) = parse_selector(selector)?;
        return Ok(TapActionOutcome::Recorded(ActionStep {
            kind: "tap".to_string(),
            selector: Some(Selector { kind, value }),
            point: None,
            viewport: None,
            direction: None,
        }));
    }
    if let Some(point) = point {
        let (x, y) = parse_point(point)?;
        adb.tap(TapPoint { x, y })?;
        let Some(viewport) = adb.display_size().ok() else {
            return Ok(TapActionOutcome::ViewportUnavailable);
        };
        return Ok(TapActionOutcome::Recorded(ActionStep {
            kind: "tap".to_string(),
            selector: None,
            point: Some(Point { x, y }),
            viewport: Some(viewport),
            direction: None,
        }));
    }
    let label = screenshot_label.expect("checked action count");
    let screenshot = screenshot.expect("checked screenshot");
    android.screen_capture(screenshot, true)?;
    let resolved = android.screen_resolve(screenshot, &format!("input tap #{label}"))?;
    let tap_point = parse_input_tap(&resolved.stdout)?;
    adb.tap(tap_point)?;
    let Some(viewport) = adb.display_size().ok() else {
        return Ok(TapActionOutcome::ViewportUnavailable);
    };
    Ok(TapActionOutcome::Recorded(ActionStep {
        kind: "tap".to_string(),
        selector: None,
        point: Some(Point {
            x: tap_point.x,
            y: tap_point.y,
        }),
        viewport: Some(viewport),
        direction: None,
    }))
}

fn scroll_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    direction: &str,
) -> Result<Value> {
    let pre_layout = observe_layout(android, false)?;
    let pre_orientation = orient_layout(root, &pre_layout, None, false, true, adb)?;
    let source = pre_orientation.matched_place.clone();
    let viewport = adb.display_size().ok().unwrap_or(Viewport {
        width: 1080,
        height: 2400,
    });
    let (sx, sy, ex, ey) = swipe_for_direction(direction, viewport);
    adb.swipe(sx, sy, ex, ey, 350)?;
    let post_layout = observe_after_action(android, Some(&pre_orientation.baseline))?;
    let post_orientation = orient_layout(root, &post_layout, None, false, true, adb)?;
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
    let pre_orientation = orient_layout(root, &pre_layout, None, false, true, adb)?;
    adb.back()?;
    let post_layout = observe_after_action(android, Some(&pre_orientation.baseline))?;
    let post_orientation = orient_layout(root, &post_layout, None, false, true, adb)?;
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
            let orientation = orient_layout(root, &layout, None, false, true, adb)?;
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
        let orientation = orient_layout(root, &layout, None, false, true, adb)?;
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
    let viewport = adb.display_size().ok();
    let viewport_used = viewport.is_some();
    let plan = resolve_path(&graph, target, &current_place.id, viewport);
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
        if let Err(error) = execute_recipe(android, adb, &edge.recipe, Some(&current_layout)) {
            return Ok(result_with_data(
                "action_failed",
                &error.to_string(),
                json!({
                    "edge": edge.id,
                    "changed_graph": false,
                    "changed_files": []
                }),
            ));
        }
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
                if let Some(reason) = detect_overlay(&post_layout) {
                    return Ok(result_with_data(
                        "blocked_by_overlay",
                        "a blocking overlay (e.g. a permission dialog) intercepted the transition; no edge recorded",
                        json!({
                            "reason": reason,
                            "changed_graph": false,
                            "changed_files": []
                        }),
                    ));
                }
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
    let current_display_size = adb.display_size().ok();
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
                    if step.is_geometry()
                        && (step.viewport.is_none() || current_display_size != step.viewport)
                    {
                        anyhow::bail!(
                            "geometry edge requires a matching device viewport; refusing to tap raw pixels"
                        );
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

fn doctor(root: &Path, selection: &DeviceSelection, live: bool, verbose: bool) -> Value {
    let serial = selection.serial.as_deref();
    let repo_checks = validate_graph(root);
    let repo_ok = repo_checks.iter().all(|check| check["status"] == "pass");
    let android_ok = command_on_path("android");
    let adb_ok = command_on_path("adb");
    let devices = if adb_ok {
        Adb::new(SubprocessRunner, None)
            .devices()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let ready_devices = devices
        .iter()
        .filter(|device| device.state == "device")
        .count();
    let selected = serial.and_then(|serial| devices.iter().find(|device| device.serial == serial));
    let multi_device = serial.is_none() && ready_devices > 1;
    let device_ready = match serial {
        Some(_) => selected.is_some_and(|device| device.state == "device"),
        None => ready_devices == 1,
    };
    let identity_result = device_ready
        .then(|| Adb::new(SubprocessRunner, selection.serial.clone()).device_identity());
    let device_identity = identity_result
        .as_ref()
        .and_then(|result| result.as_ref().ok());
    let device_ok = device_identity.is_some();
    let mut device_check = json!({
        "name": "device",
        "status": if device_ok { "pass" } else { "fail" }
    });
    if !adb_ok {
        device_check["code"] = json!("adb_missing");
        device_check["hint"] = json!("Install ADB and ensure it is on PATH.");
    } else if multi_device {
        device_check["code"] = json!("device_ambiguous");
        device_check["hint"] =
            json!("multiple devices attached; pass --device or set ANDROID_SERIAL");
    } else if devices.is_empty() {
        device_check["code"] = json!("no_device");
        device_check["hint"] = json!("Start an emulator or connect a device.");
    } else if serial.is_some() && selected.is_none() {
        device_check["code"] = json!("device_not_found");
        device_check["hint"] = json!("Choose an attached serial with --device.");
    } else if !device_ready {
        device_check["code"] = json!("device_not_ready");
        device_check["hint"] = json!("Bring the selected device online and authorize debugging.");
    } else if let Some(Err(error)) = &identity_result {
        device_check["code"] = json!("device_identity_unavailable");
        device_check["hint"] = json!("Verify ADB can read the device model and API level.");
        if verbose {
            device_check["detail"] = json!(error.to_string());
        }
    }
    if let Some(identity) = device_identity {
        device_check["serial"] = json!(&identity.serial);
        device_check["model"] = json!(&identity.model);
        device_check["api_level"] = json!(identity.api_level);
        device_check["selection_source"] = json!(selection.source);
    } else if let Some(serial) = serial {
        device_check["serial"] = json!(serial);
        device_check["selection_source"] = json!(selection.source);
    }

    let mut live_checks = Vec::new();
    let mut live_status = "ok";
    if live && repo_ok && android_ok && adb_ok && device_ok {
        let resolution = resolve_app_package(root);
        let expected_package = resolution
            .as_ref()
            .ok()
            .and_then(AppPackageResolution::package)
            .map(str::to_string);
        match expected_package {
            None => {
                live_status = "config_error";
                live_checks.push(json!({
                    "name": "foreground_app",
                    "status": "fail",
                    "code": "app_package_missing",
                    "remediation": "Configure android_package for the active app profile."
                }));
            }
            Some(expected_package) => {
                let mut adb = Adb::new(SubprocessRunner, selection.serial.clone());
                match adb.foreground_package() {
                    Err(error) => {
                        live_status = "environment_error";
                        live_checks.push(json!({
                            "name": "foreground_app",
                            "status": "fail",
                            "code": "foreground_package_unknown",
                            "expected_package": expected_package,
                            "remediation": "Launch the expected app and retry the live doctor probe.",
                            "detail": error.to_string()
                        }));
                    }
                    Ok(foreground_package) if foreground_package != expected_package => {
                        live_status = "app_mismatch";
                        live_checks.push(json!({
                            "name": "foreground_app",
                            "status": "fail",
                            "code": "foreground_package_mismatch",
                            "expected_package": expected_package,
                            "foreground_package": foreground_package,
                            "remediation": "Bring the expected app to the foreground before capture."
                        }));
                    }
                    Ok(foreground_package) => {
                        live_checks.push(json!({
                            "name": "foreground_app",
                            "status": "pass",
                            "expected_package": expected_package,
                            "foreground_package": foreground_package
                        }));
                        let mut android =
                            AndroidCli::new(SubprocessRunner, selection.serial.clone());
                        match android.layout(false) {
                            Ok(command) => match parse_layout_output(&command.stdout) {
                                Ok(output) => live_checks.push(json!({
                                    "name": "layout_capture",
                                    "status": "pass",
                                    "element_count": output.layout.as_array().map(Vec::len).unwrap_or(0),
                                    "notices": output.notices
                                })),
                                Err(error) => {
                                    live_status = "environment_error";
                                    live_checks.push(json!({
                                        "name": "layout_capture",
                                        "status": "fail",
                                        "code": "android_layout_invalid",
                                        "remediation": "Run minimap layout --verbose and inspect the Android CLI output.",
                                        "detail": error.to_string()
                                    }));
                                }
                            },
                            Err(error) => {
                                live_status = "environment_error";
                                live_checks.push(live_layout_failure_check(&error, verbose));
                            }
                        }
                    }
                }
            }
        }
    }

    let status = if !repo_ok {
        "config_error"
    } else if !android_ok || !adb_ok || !device_ok {
        if live {
            "environment_error"
        } else {
            "config_error"
        }
    } else if live {
        live_status
    } else {
        "ok"
    };
    let mut result = json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": status,
        "ok": status == "ok",
        "repo_ok": repo_ok,
        "device_ok": device_ok,
        "checks": {
            "repo": repo_checks,
            "environment": [
                {"name": "android", "status": if android_ok { "pass" } else { "fail" }},
                {"name": "adb", "status": if adb_ok { "pass" } else { "fail" }},
                device_check
            ]
        }
    });
    if live {
        result["checks"]["live"] = json!(live_checks);
    }
    result
}

fn live_layout_failure_check(error: &anyhow::Error, verbose: bool) -> Value {
    if let Some(failure) = error.downcast_ref::<CommandFailure>() {
        if let Some(analytics) = android_analytics_spool_failure(failure) {
            let mut check = json!({
                "name": "layout_capture",
                "status": "fail",
                "code": "android_cli_analytics_spool_unwritable",
                "blocked_path": analytics.blocked_path,
                "remediation": "Grant write access to the Android CLI analytics spool or use a writable filesystem profile."
            });
            if verbose {
                check["debug"] = json!({
                    "command": failure.result.args,
                    "status": failure.result.status,
                    "stdout": failure.result.stdout,
                    "stderr": failure.result.stderr
                });
            }
            return check;
        }
    }
    let mut check = json!({
        "name": "layout_capture",
        "status": "fail",
        "code": "android_layout_capture_failed",
        "remediation": "Run minimap layout --verbose and inspect the Android CLI failure."
    });
    if verbose {
        check["detail"] = json!(error.to_string());
    }
    check
}

fn command_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|path| path.join(name).exists()))
        .unwrap_or(false)
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

/// Derive a label whose slug does not collide with any existing place by
/// appending the smallest free numeric suffix (e.g. `Account Settings` ->
/// `Account Settings 2`, normalizing to `account-settings-2`). `slug` is the
/// already-normalized form of `label`; it is assumed to be taken.
fn unique_label(graph: &Graph, label: &str, slug: &str) -> String {
    let taken = |candidate: &str| graph.places.values().any(|place| place.slug == candidate);
    debug_assert!(taken(slug), "unique_label called for a free slug");
    let base = label.trim();
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{base} {suffix}");
        if !taken(&normalize_label(&candidate)) {
            return candidate;
        }
        suffix += 1;
    }
}

/// If a pending transition lands on `place` (its destination hash matches and its
/// source is a known place), commit the edge and clear the pending state.
/// Returns the committed edge file path (if any) so the caller can record it.
fn commit_pending_edge_for_place<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
    graph: &Graph,
    place: &Place,
    baseline: &PlaceBaseline,
) -> Result<Option<PathBuf>> {
    if let Some(mut pending) = load_pending(root, adb)? {
        if pending.destination.identity_hash == baseline.identity_hash
            && graph.places.contains_key(&pending.source.id)
        {
            let edge = edge_from_parts(
                &pending.source,
                &endpoint_for_place(place),
                pending.recipe.split_off(0),
                pending.intent.as_deref(),
            );
            let file = commit_edge(root, &edge)?;
            clear_pending(root, adb)?;
            return Ok(Some(file));
        }
    }
    Ok(None)
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
    remember_place_observation(&mut new_place, baseline);
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
        // The readable form is too long, so derive a stable, collision-resistant
        // id from the recipe digest. Keep a char-boundary-truncated readable
        // prefix for legibility, then append the full fixed-length hex digest.
        let digest = format!(
            "{:x}",
            Sha256::digest(canonical_json(&serde_json::to_value(recipe).unwrap()).as_bytes())
        );
        let prefix: String = slug.chars().take(32).collect();
        sanitize_id(&format!("{prefix}__{digest}"))
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

/// Resolve the cache path for a given file name. Returns `Ok(None)` when the
/// device serial cannot be resolved: without a serial we cannot safely scope the
/// cache to one device, and a shared "unknown-device" bucket would let two
/// devices read each other's cached place, so we skip the cache entirely.
fn pending_path<DR: CommandRunner>(root: &Path, adb: &mut Adb<DR>) -> Result<Option<PathBuf>> {
    let repo = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf())
        .display()
        .to_string();
    let repo_hash = format!("{:x}", Sha256::digest(repo.as_bytes()));
    let serial = match adb.serial() {
        Ok(serial) if !serial.trim().is_empty() => serial,
        _ => return Ok(None),
    };
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
    Ok(Some(
        std::env::temp_dir()
            .join("minimap")
            .join(&repo_hash[..16])
            .join(sanitize_id(&serial))
            .join(sanitize_id(&package))
            .join("pending-transition.json"),
    ))
}

fn session_path<DR: CommandRunner>(root: &Path, adb: &mut Adb<DR>) -> Result<Option<PathBuf>> {
    let Some(mut path) = pending_path(root, adb)? else {
        return Ok(None);
    };
    path.set_file_name("session-place.json");
    Ok(Some(path))
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
    let Some(path) = session_path(root, adb)? else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        restrict_cache_dir_permissions(parent);
    }
    let value = json!({
        "place": place,
        "baseline": baseline,
        "layout": redact_layout(layout)
    });
    fs::write(&path, canonical_json(&value))?;
    restrict_cache_file_permissions(&path);
    Ok(())
}

fn load_session_place<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
) -> Result<Option<SessionPlace>> {
    let Some(path) = session_path(root, adb)? else {
        return Ok(None);
    };
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
    let Some(path) = session_path(root, adb)? else {
        return Ok(None);
    };
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
    let Some(path) = session_path(root, adb)? else {
        return Ok(());
    };
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
    let Some(path) = pending_path(root, adb)? else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        restrict_cache_dir_permissions(parent);
    }
    let value = json!({
        "source": pending.source,
        "recipe": pending.recipe,
        "destination": pending.destination,
        "intent": pending.intent
    });
    fs::write(&path, canonical_json(&value))?;
    restrict_cache_file_permissions(&path);
    Ok(())
}

fn load_pending<DR: CommandRunner>(
    root: &Path,
    adb: &mut Adb<DR>,
) -> Result<Option<PendingTransition>> {
    let Some(path) = pending_path(root, adb)? else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }
    if let Ok(metadata) = fs::metadata(&path) {
        if let Ok(modified) = metadata.modified() {
            let expired = SystemTime::now()
                .duration_since(modified)
                .map(|age| age > Duration::from_secs(PENDING_TTL_SECS))
                .unwrap_or(true);
            if expired {
                let _ = fs::remove_file(&path);
                return Ok(None);
            }
        }
    }
    let value: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    Ok(Some(PendingTransition {
        source: serde_json::from_value(value["source"].clone())?,
        recipe: serde_json::from_value(value["recipe"].clone())?,
        destination: serde_json::from_value(value["destination"].clone())?,
        intent: value["intent"].as_str().map(str::to_string),
    }))
}

fn clear_pending<DR: CommandRunner>(root: &Path, adb: &mut Adb<DR>) -> Result<()> {
    let Some(path) = pending_path(root, adb)? else {
        return Ok(());
    };
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// On Unix, restrict the minimap cache directory tree to the owner (0o700) so
/// cache files on a shared /tmp are not world-readable. Best-effort: failures to
/// adjust permissions are ignored. No-op on non-Unix platforms.
#[cfg(unix)]
fn restrict_cache_dir_permissions(dir: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let temp_root = std::env::temp_dir().join("minimap");
    let mut current = Some(dir);
    while let Some(path) = current {
        if !path.starts_with(&temp_root) {
            break;
        }
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        if path == temp_root.as_path() {
            break;
        }
        current = path.parent();
    }
}

#[cfg(not(unix))]
fn restrict_cache_dir_permissions(_dir: &Path) {}

/// On Unix, restrict a written cache file to the owner (0o600). Best-effort; a
/// no-op on non-Unix platforms.
#[cfg(unix)]
fn restrict_cache_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_cache_file_permissions(_path: &Path) {}

fn print_json(value: &Value) {
    print!("{}", canonical_json(value));
}

#[cfg(test)]
mod tests {
    use super::*;
    use minimap_android::CommandResult;
    use minimap_schemas::{Fingerprint, StaticText};
    use std::collections::BTreeMap;

    /// Minimal fake `CommandRunner`. When `serial` is `Some`, `get-serialno`
    /// succeeds with that value; when `None`, every command fails so `serial()`
    /// errors and the cache is skipped.
    struct FakeRunner {
        serial: Option<String>,
    }

    impl CommandRunner for FakeRunner {
        fn run(&mut self, args: &[String], _env: &[(String, String)]) -> Result<CommandResult> {
            if args.iter().any(|arg| arg == "get-serialno") {
                if let Some(serial) = &self.serial {
                    return Ok(CommandResult {
                        args: args.to_vec(),
                        status: 0,
                        stdout: format!("{serial}\n"),
                        stderr: String::new(),
                    });
                }
            }
            Ok(CommandResult {
                args: args.to_vec(),
                status: 1,
                stdout: String::new(),
                stderr: "no device".to_string(),
            })
        }
    }

    fn fake_adb(serial: Option<&str>) -> Adb<FakeRunner> {
        Adb::new(
            FakeRunner {
                serial: serial.map(str::to_string),
            },
            None,
        )
    }

    fn endpoint(slug: &str) -> EdgeEndpoint {
        EdgeEndpoint {
            id: format!("place_{slug}"),
            slug: slug.to_string(),
        }
    }

    fn baseline(hash: &str) -> PlaceBaseline {
        PlaceBaseline {
            identity_hash: format!("sha256:{hash}"),
            fingerprint: Fingerprint {
                selectors: Vec::new(),
                static_text: vec![StaticText {
                    value: hash.to_string(),
                }],
                roles: BTreeMap::new(),
            },
        }
    }

    fn tap_step(value: &str) -> ActionStep {
        ActionStep {
            kind: "tap".to_string(),
            selector: Some(Selector {
                kind: "text".to_string(),
                value: value.to_string(),
            }),
            point: None,
            viewport: None,
            direction: None,
        }
    }

    // FIX 1: edge_id must never panic regardless of selector/slug length and must
    // yield a valid sanitized id.
    #[test]
    fn edge_id_handles_long_selectors_without_panicking() {
        let long_value = "x".repeat(200);
        let from = endpoint("home");
        let to = endpoint("search");
        let recipe = vec![tap_step(&long_value)];
        let id = edge_id(&from, &to, &recipe);
        assert!(
            id.starts_with("edge_"),
            "id should keep readable prefix: {id}"
        );
        assert!(
            id.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'),
            "id must be a sanitized slug: {id}"
        );
        // Deterministic for the same recipe.
        assert_eq!(id, edge_id(&from, &to, &recipe));
    }

    #[test]
    fn edge_id_long_unicode_selector_does_not_panic() {
        // Multi-byte chars near the truncation boundary must not split mid-UTF8.
        let unicode_value = "é".repeat(120);
        let from = endpoint("éhome");
        let to = endpoint("search");
        let recipe = vec![tap_step(&unicode_value)];
        let id = edge_id(&from, &to, &recipe);
        assert!(!id.is_empty());
    }

    // FIX 4: with no resolvable serial, the cache path is None (no shared bucket),
    // so loads return None and saves are skipped (no file written).
    #[test]
    fn no_serial_skips_cache_entirely() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut adb = fake_adb(None);

        assert!(pending_path(root, &mut adb).unwrap().is_none());
        assert!(session_path(root, &mut adb).unwrap().is_none());

        let pending = PendingTransition {
            source: endpoint("home"),
            recipe: vec![tap_step("SEARCH")],
            destination: baseline("dest"),
            intent: None,
        };
        save_pending(root, &mut adb, &pending).unwrap();
        assert!(load_pending(root, &mut adb).unwrap().is_none());

        // Nothing should have been written to the shared minimap temp tree on
        // behalf of this serial-less invocation.
        let path = pending_path(root, &mut adb).unwrap();
        assert!(path.is_none());
    }

    // FIX 2: a pending file older than the TTL is ignored and removed.
    #[test]
    fn stale_pending_is_ignored_and_removed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut adb = fake_adb(Some("fix2-serial"));

        let pending = PendingTransition {
            source: endpoint("home"),
            recipe: vec![tap_step("SEARCH")],
            destination: baseline("dest"),
            intent: None,
        };
        save_pending(root, &mut adb, &pending).unwrap();
        let path = pending_path(root, &mut adb).unwrap().unwrap();
        assert!(path.exists());

        // Backdate the file well beyond the TTL.
        let old = SystemTime::now() - Duration::from_secs(PENDING_TTL_SECS + 60);
        filetime_set(&path, old);

        assert!(load_pending(root, &mut adb).unwrap().is_none());
        assert!(!path.exists(), "stale pending should be removed");
    }

    // FIX 3: a pending whose source.id is absent from the graph must NOT be
    // committed as a dangling edge when orienting a new destination place.
    #[test]
    fn orient_does_not_commit_edge_from_missing_source() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        run_init(
            root,
            InitOptions {
                dry_run: false,
                agents: "codex",
                force: false,
                refresh_skills: false,
                no_skills: true,
            },
        )
        .unwrap();
        let mut adb = fake_adb(Some("fix3-serial"));

        // A pending transition whose source place is NOT in the graph.
        let dest_layout = json!({
            "class": "Column",
            "children": [
                {"class": "Text", "text": "Brand New Destination Screen Title"},
                {"class": "Text", "text": "A second distinctive line of body copy here"}
            ]
        });
        let dest_baseline = fingerprint_layout(&dest_layout);
        let pending = PendingTransition {
            source: endpoint("ghost-source"),
            recipe: vec![tap_step("SEARCH")],
            destination: dest_baseline.clone(),
            intent: Some("open ghost".to_string()),
        };
        save_pending(root, &mut adb, &pending).unwrap();

        // Orient on the destination layout with a label -> a new place is created,
        // but the edge must NOT be committed because the source is missing.
        let orientation =
            orient_layout(root, &dest_layout, Some("newdest"), false, true, &mut adb).unwrap();
        assert_eq!(orientation.status, "ok");

        let graph = load_graph(root).unwrap();
        assert!(
            graph.edges.is_empty(),
            "no edge should be committed from a missing source: {:?}",
            graph.edges
        );
    }

    // FIX 7 (Unix only): written cache files are mode 0o600.
    #[cfg(unix)]
    #[test]
    fn cache_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut adb = fake_adb(Some("fix7-serial"));

        let pending = PendingTransition {
            source: endpoint("home"),
            recipe: vec![tap_step("SEARCH")],
            destination: baseline("dest"),
            intent: None,
        };
        save_pending(root, &mut adb, &pending).unwrap();
        let path = pending_path(root, &mut adb).unwrap().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "cache file should be owner-only");
    }

    /// Set a file's mtime to `when` without an extra crate dependency.
    /// `set_accessed`/`set_modified` live on `FileTimes` itself (cross-platform).
    fn filetime_set(path: &Path, when: SystemTime) {
        let times = fs::FileTimes::new().set_accessed(when).set_modified(when);
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(times).unwrap();
    }
}
