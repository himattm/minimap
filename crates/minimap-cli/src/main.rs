use anyhow::Result;
use clap::{Parser, Subcommand};
use minimap_android::{
    layout_result, tap_label_result, tap_point_result, tap_selector_result, Adb, AndroidCli,
    CommandRunner, SubprocessRunner,
};
use minimap_core::{identity_hash, match_screen, normalize_layout};
use minimap_graph::{exit_code_for_status, resolve_route};
use minimap_repo::{
    accept_proposal, append_journal_entry, commit_edge, commit_route, commit_screen,
    derive_screen_name, detect_legacy_minimap, edge_id_for, load_context, load_graph,
    new_screen_id, post_tap_settle_ms, rename_screen, run_init, screen_path,
    stage_proposal_value, AcceptResolution, LEGACY_MINIMAP_MESSAGE,
};
use minimap_schemas::{
    canonical_json, JournalEntry, MinimapResult, NavigationEdge, Viewport,
    JOURNAL_ENTRY_SCHEMA_VERSION, RESULT_SCHEMA_VERSION, ROUTE_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Parser)]
#[command(name = "minimap")]
#[command(
    about = "Shared navigation memory and soft validation for AI agents working in Android codebases."
)]
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
    /// Initialize Minimap in this repo (creates .minimap/ layout and agent skills).
    Init {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "auto")]
        agents: String,
        /// Bypass the legacy `.minimap/` check and overwrite any 0.1.x tree.
        #[arg(long)]
        force: bool,
    },
    /// Diagnose the Minimap environment (config, graph dirs, android/adb on PATH).
    Doctor,
    /// Capture the current Android UI as redacted layout JSON; pass --diff for an in-session diff.
    Layout {
        #[arg(long)]
        diff: bool,
    },
    /// Tap a UI element by --selector kind=value, --point X,Y, or --label N (with --screenshot); pass --reason to record intent. Selector/label taps grow the graph atomically.
    Tap {
        #[arg(long)]
        selector: Option<String>,
        #[arg(long)]
        point: Option<String>,
        #[arg(long)]
        label: Option<i64>,
        #[arg(long)]
        screenshot: Option<String>,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long, default_value = "verified")]
        mode: String,
    },
    /// Manage routes: resolve a planned path or define a new route.
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    /// Manage screens in the committed graph.
    Screen {
        #[command(subcommand)]
        command: ScreenCommand,
    },
    /// Resolve and execute a route to the target, verifying each step and aborting on drift.
    Go {
        target: String,
        #[arg(long)]
        current_screen: Option<String>,
        #[arg(long, default_value = "verified")]
        mode: String,
    },
    /// Compare the current app state to the committed graph; stages a review proposal if drifted.
    Drift,
    /// Validate routes against the live device; --all, --changed-files <path>, --execute --current-screen <name>, or --screen current.
    Validate {
        #[arg(long)]
        all: bool,
        #[arg(long = "changed-files")]
        changed_files: Option<String>,
        #[arg(long)]
        execute: bool,
        #[arg(long)]
        current_screen: Option<String>,
        #[arg(long)]
        screen: Option<String>,
    },
    /// Accept a staged proposal by id; the only command that mutates the committed graph through the review path.
    Accept {
        proposal_id: String,
        /// For `selector_drift` proposals only: materialize the observed layout as a new
        /// screen and grow an edge from the source, instead of merging with the candidate.
        #[arg(long = "as-new")]
        as_new: bool,
    },
    /// Discard uncommitted changes to .minimap/graph and .minimap/routes via git checkout.
    Undo,
}

#[derive(Debug, Subcommand)]
enum RouteCommand {
    /// Resolve the planned navigation path to a screen or route from the current screen (no device action).
    Resolve {
        target: String,
        #[arg(long)]
        current_screen: Option<String>,
    },
    /// Define a new route from --from screen to --to screen. Pass `--triggers <glob>`
    /// multiple times for multiple glob patterns; each occurrence is one trigger entry
    /// (commas inside a glob like `{login,signup}/**` are preserved verbatim).
    Define {
        name: String,
        #[arg(long)]
        to: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long = "triggers")]
        triggers: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ScreenCommand {
    /// Rewrite the `name` field on a screen JSON; edges are untouched.
    Rename { id: String, new_name: String },
}

fn main() {
    let cli = Cli::parse();
    let code = match run(cli) {
        Ok(code) => code,
        Err(error) => {
            let result = MinimapResult {
                schema_version: RESULT_SCHEMA_VERSION.to_string(),
                status: "config_error".to_string(),
                summary: Some(error.to_string()),
                data: json!({ "error": { "code": "minimap_error", "message": error.to_string() } }),
                recommended_action: None,
            };
            print_json(&serde_json::to_value(result).expect("error JSON"));
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
        } => {
            if !force {
                let legacy = detect_legacy_minimap(&root);
                if !legacy.is_empty() {
                    print_json(&json!({
                        "schema_version": RESULT_SCHEMA_VERSION,
                        "status": "config_error",
                        "summary": LEGACY_MINIMAP_MESSAGE,
                        "legacy_paths": legacy,
                    }));
                    return Ok(2);
                }
            }
            let result = run_init(&root, dry_run, &agents)?;
            print_json(&serde_json::to_value(result)?);
            Ok(0)
        }
        Commands::Doctor => {
            let result = doctor(&root);
            let ok = result
                .get("ok")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            print_json(&result);
            Ok(if ok { 0 } else { 1 })
        }
        Commands::Layout { diff } => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let result = layout_result(&mut android, diff)?;
            print_json(&result);
            Ok(0)
        }
        Commands::Tap {
            selector,
            point,
            label,
            screenshot,
            reason,
            mode,
        } => tap_atomic(&root, selector, point, label, screenshot, reason, &mode),
        Commands::Route { command } => match command {
            RouteCommand::Resolve {
                target,
                current_screen,
            } => {
                let graph = load_graph(&root)?;
                let context = load_context(&root);
                let plan = resolve_route(&graph, &target, current_screen.as_deref(), &context);
                let result = plan.to_result();
                let code = exit_code_for_status(&result.status);
                print_json(&serde_json::to_value(result)?);
                Ok(code)
            }
            RouteCommand::Define {
                name,
                to,
                from,
                triggers,
            } => route_define(&root, &name, &to, from.as_deref(), &triggers),
        },
        Commands::Screen { command } => match command {
            ScreenCommand::Rename { id, new_name } => screen_rename(&root, &id, &new_name),
        },
        Commands::Go {
            target,
            current_screen,
            mode,
        } => {
            let graph = load_graph(&root)?;
            let context = load_context(&root);
            let plan = resolve_route(&graph, &target, current_screen.as_deref(), &context);
            let result = plan.to_result();
            if result.status != "ok" {
                let code = exit_code_for_status(&result.status);
                print_json(&serde_json::to_value(result)?);
                return Ok(code);
            }
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = execute_route_plan(&root, &plan, &target, &mode, &mut android, &mut adb)?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Drift => {
            let mut android = AndroidCli::new(SubprocessRunner);
            let result = drift_result(&root, &mut android)?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Validate {
            all,
            changed_files,
            execute,
            current_screen,
            screen,
        } => {
            if let Some(screen) = screen {
                let mut android = AndroidCli::new(SubprocessRunner);
                return validate_screen(&root, &mut android, &screen);
            }
            let mut android = AndroidCli::new(SubprocessRunner);
            let mut adb = Adb::new(SubprocessRunner);
            let result = validate_result(
                &root,
                &mut android,
                &mut adb,
                all,
                changed_files.as_deref(),
                execute,
                current_screen.as_deref(),
            )?;
            let code = exit_code_for_status(result["status"].as_str().unwrap_or("ok"));
            print_json(&result);
            Ok(code)
        }
        Commands::Accept {
            proposal_id,
            as_new,
        } => {
            let resolution = if as_new {
                AcceptResolution::AsNew
            } else {
                AcceptResolution::Default
            };
            let written = accept_proposal(&root, &proposal_id, resolution)?;
            print_json(&json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "ok",
                "summary": "proposal accepted",
                "graph_objects_touched": written.iter().map(|path| path.display().to_string()).collect::<Vec<_>>()
            }));
            Ok(0)
        }
        Commands::Undo => undo(&root),
    }
}

#[derive(Debug, Clone)]
enum TapAction {
    Selector {
        selector: String,
    },
    Label {
        label: i64,
        screenshot: String,
    },
    Point {
        x: i64,
        y: i64,
        mode: String,
    },
}

enum StepOutcome {
    Matched {
        to_screen_id: String,
        edge_id: String,
    },
    NewScreen {
        to_screen_id: String,
        edge_id: String,
    },
    DriftStaged {
        proposal_id: String,
        candidate_screen_id: Option<String>,
    },
}

fn tap_atomic(
    root: &Path,
    selector: Option<String>,
    point: Option<String>,
    label: Option<i64>,
    screenshot: Option<String>,
    reason: Option<String>,
    mode: &str,
) -> Result<i32> {
    let action = match (selector, point, label) {
        (Some(selector), None, None) => TapAction::Selector { selector },
        (None, Some(point), None) => {
            let (x, y) = parse_point(&point)?;
            TapAction::Point {
                x,
                y,
                mode: mode.to_string(),
            }
        }
        (None, None, Some(label)) => {
            let screenshot = screenshot
                .ok_or_else(|| anyhow::anyhow!("--label requires --screenshot"))?;
            TapAction::Label { label, screenshot }
        }
        (None, None, None) => anyhow::bail!("tap requires --selector, --point, or --label"),
        _ => anyhow::bail!("tap accepts exactly one of --selector, --point, --label"),
    };

    let mut android = AndroidCli::new(SubprocessRunner);
    let mut adb = Adb::new(SubprocessRunner);

    // Pre-tap layout → classify from_screen.
    let pre = layout_result(&mut android, false)?;
    let pre_layout = pre["layout"].clone();
    let from_screen_id = classify_screen_id(root, &pre_layout)?;

    // Execute the tap. On failure: journal as tap_failed, exit 2.
    let tap_result = match &action {
        TapAction::Selector { selector } => {
            tap_selector_result(&mut android, &mut adb, selector, reason.as_deref())
        }
        TapAction::Point { x, y, mode } => tap_point_result(&mut adb, *x, *y, mode),
        TapAction::Label { label, screenshot } => {
            tap_label_result(&mut android, &mut adb, *label, screenshot)
        }
    };
    let tap_value = match tap_result {
        Ok(value) => value,
        Err(error) => {
            append_journal_entry(
                root,
                &journal_entry(
                    from_screen_id.clone(),
                    None,
                    None,
                    reason.as_deref(),
                    "tap_failed",
                    None,
                ),
            )?;
            print_json(&json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "tap_failed",
                "summary": error.to_string(),
                "from_screen_id": from_screen_id,
                "outcome": "tap_failed",
            }));
            return Ok(2);
        }
    };

    // Coordinate path: journal only, never grow the graph (selector unknown).
    if let TapAction::Point { x, y, .. } = &action {
        append_journal_entry(
            root,
            &journal_entry(
                from_screen_id.clone(),
                None,
                None,
                reason.as_deref(),
                "coord_journal_only",
                viewport_from_action(&tap_value["action"]),
            ),
        )?;
        print_json(&json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "status": "ok",
            "summary": "tap recorded; no edge added — use --selector or --label to grow the graph",
            "from_screen_id": from_screen_id,
            "action": tap_value["action"],
            "outcome": "coord_journal_only",
            "point": {"x": x, "y": y},
        }));
        return Ok(0);
    }

    // Post-tap settle + re-query layout. Settle window comes from
    // `navigation.post_tap_settle_ms` in `.minimap/config.json`, defaulting to 500.
    thread::sleep(Duration::from_millis(post_tap_settle_ms(root)));
    let post = layout_result(&mut android, false)?;
    let post_layout = post["layout"].clone();

    // Unknown from-screen → execute and journal but cannot grow the graph from an unanchored source.
    let Some(from_id) = from_screen_id.clone() else {
        append_journal_entry(
            root,
            &journal_entry(
                None,
                None,
                None,
                reason.as_deref(),
                "from_screen_unknown",
                viewport_from_action(&tap_value["action"]),
            ),
        )?;
        print_json(&json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "status": "ok",
            "summary": "tap recorded; no edge added — current screen unknown to the graph",
            "from_screen_id": Value::Null,
            "action": tap_value["action"],
            "outcome": "from_screen_unknown",
        }));
        return Ok(0);
    };

    // Selector / label path: commit_step decides match / new / drift.
    let outcome = commit_step(root, &from_id, &action, &post_layout, reason.as_deref())?;
    match outcome {
        StepOutcome::Matched {
            to_screen_id,
            edge_id,
        } => {
            append_journal_entry(
                root,
                &journal_entry(
                    Some(from_id.clone()),
                    Some(edge_id.clone()),
                    Some(to_screen_id.clone()),
                    reason.as_deref(),
                    "matched",
                    viewport_from_action(&tap_value["action"]),
                ),
            )?;
            print_json(&json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "ok",
                "summary": "tap matched an existing screen; edge recorded",
                "from_screen_id": from_id,
                "to_screen_id": to_screen_id,
                "edge_id": edge_id,
                "action": tap_value["action"],
                "outcome": "matched",
            }));
            Ok(0)
        }
        StepOutcome::NewScreen {
            to_screen_id,
            edge_id,
        } => {
            append_journal_entry(
                root,
                &journal_entry(
                    Some(from_id.clone()),
                    Some(edge_id.clone()),
                    Some(to_screen_id.clone()),
                    reason.as_deref(),
                    "new_screen",
                    viewport_from_action(&tap_value["action"]),
                ),
            )?;
            print_json(&json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "ok",
                "summary": "tap reached a new screen; screen and edge committed",
                "from_screen_id": from_id,
                "to_screen_id": to_screen_id,
                "edge_id": edge_id,
                "action": tap_value["action"],
                "outcome": "new_screen",
            }));
            Ok(0)
        }
        StepOutcome::DriftStaged {
            proposal_id,
            candidate_screen_id,
        } => {
            append_journal_entry(
                root,
                &journal_entry(
                    Some(from_id.clone()),
                    None,
                    candidate_screen_id.clone(),
                    reason.as_deref(),
                    "drift_staged",
                    viewport_from_action(&tap_value["action"]),
                ),
            )?;
            print_json(&json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "changed_requires_review",
                "summary": "tap landed on a drift candidate; staged proposal for review",
                "from_screen_id": from_id,
                "candidate_screen_id": candidate_screen_id,
                "proposal_id": proposal_id,
                "action": tap_value["action"],
                "outcome": "drift_staged",
                "human_approval_required": true,
            }));
            Ok(1)
        }
    }
}

/// Resolve the current screen ID by matching the pre-tap layout against the committed graph.
/// Returns `None` when the layout is `screen_unknown` (no anchor for edge growth).
fn classify_screen_id(root: &Path, layout: &Value) -> Result<Option<String>> {
    let graph = load_graph(root)?;
    let normalized = normalize_layout(layout);
    let result = match_screen(&normalized, graph.screens.into_values());
    match result.status.as_str() {
        "matched" | "repair_candidate" => Ok(result.matched_screen),
        _ => Ok(None),
    }
}

/// Classify a post-tap layout and either auto-commit (match / new) or stage a drift proposal.
fn commit_step(
    root: &Path,
    from_screen_id: &str,
    action: &TapAction,
    post_tap_layout: &Value,
    reason: Option<&str>,
) -> Result<StepOutcome> {
    let normalized = normalize_layout(post_tap_layout);
    let hash = identity_hash(&normalized);
    let graph = load_graph(root)?;
    let match_result = match_screen(&normalized, graph.screens.values().cloned());

    let selector_candidates = selector_candidates_for_action(action);

    match match_result.status.as_str() {
        "matched" => {
            let to_screen_id = match_result
                .matched_screen
                .clone()
                .ok_or_else(|| anyhow::anyhow!("matched without screen id"))?;
            let edge_id = edge_id_for(from_screen_id, &to_screen_id, &hash);
            let edge = build_edge_value(
                &edge_id,
                from_screen_id,
                &to_screen_id,
                &selector_candidates,
                reason,
            );
            commit_edge(root, &edge)?;
            Ok(StepOutcome::Matched {
                to_screen_id,
                edge_id,
            })
        }
        "repair_candidate" => {
            let candidate = match_result.matched_screen.clone();
            let proposal_id = format!("proposal-drift-{}", &hash[7..15.min(hash.len())]);
            // Default resolution: grow an edge from `from_screen_id` to the existing candidate.
            // `accept_proposal` (Default) iterates this `changes` array; `--as-new` ignores it
            // and synthesizes a fresh screen + edge from the diagnostic fields below.
            let default_changes = if let Some(candidate_id) = candidate.clone() {
                // Mix the proposal id into the hash material so the edge id is stable per
                // proposal — re-accepting the same proposal overwrites the same edge file.
                let edge_hash = format!("{hash}:{proposal_id}");
                let edge_id = edge_id_for(from_screen_id, &candidate_id, &edge_hash);
                let edge = build_edge_value(
                    &edge_id,
                    from_screen_id,
                    &candidate_id,
                    &selector_candidates,
                    reason,
                );
                vec![json!({ "op": "add", "object": edge })]
            } else {
                Vec::new()
            };
            let proposal = json!({
                "schema_version": "minimap.proposal.v1",
                "id": proposal_id.clone(),
                "kind": "selector_drift",
                "reason": "Post-tap layout is similar but below match threshold; review the drift.",
                "candidate_screen_id": candidate,
                "from_screen_id": from_screen_id,
                "match_confidence": match_result.match_confidence,
                "identity_hash": hash,
                "observed_layout": post_tap_layout,
                "observed_normalized": normalized,
                "selector_candidates": selector_candidates,
                "tap_reason": reason,
                "changes": default_changes
            });
            stage_proposal_value(root, &proposal)?;
            Ok(StepOutcome::DriftStaged {
                proposal_id,
                candidate_screen_id: candidate,
            })
        }
        _ => {
            // screen_unknown — commit a new screen + edge.
            let to_screen_id = new_screen_id(&hash);
            let name = derive_screen_name(post_tap_layout, &to_screen_id);
            let screen = json!({
                "schema_version": "minimap.screen.v1",
                "id": to_screen_id.clone(),
                "name": name,
                "identity_hash": hash.clone(),
                "normalized": normalized,
                "aliases": []
            });
            commit_screen(root, &screen)?;
            let edge_id = edge_id_for(from_screen_id, &to_screen_id, &hash);
            let edge = build_edge_value(
                &edge_id,
                from_screen_id,
                &to_screen_id,
                &selector_candidates,
                reason,
            );
            commit_edge(root, &edge)?;
            Ok(StepOutcome::NewScreen {
                to_screen_id,
                edge_id,
            })
        }
    }
}

fn selector_candidates_for_action(action: &TapAction) -> Vec<Value> {
    match action {
        TapAction::Selector { selector } => {
            let (kind, value) = selector
                .split_once('=')
                .map(|(kind, value)| (kind.to_string(), value.to_string()))
                .unwrap_or_else(|| ("selector".to_string(), selector.to_string()));
            vec![json!({
                "kind": kind,
                "value": value,
                "score": 0.7
            })]
        }
        TapAction::Label { label, .. } => vec![json!({
            "kind": "annotated_screenshot_label",
            "value": label.to_string(),
            "score": 0.3
        })],
        TapAction::Point { .. } => Vec::new(),
    }
}

fn build_edge_value(
    edge_id: &str,
    from_screen_id: &str,
    to_screen_id: &str,
    selector_candidates: &[Value],
    reason: Option<&str>,
) -> Value {
    json!({
        "schema_version": "minimap.edge.v1",
        "id": edge_id,
        "from_screen": from_screen_id,
        "to_screen": to_screen_id,
        "intent": reason.unwrap_or("learned tap"),
        "action": {
            "kind": "tap",
            "description": reason.unwrap_or("learned tap"),
            "selector_candidates": selector_candidates
        },
        "expectations": [{"kind": "screen_reached", "screen": to_screen_id}],
        "learned_from": {"source": "atomic_tap"}
    })
}

fn journal_entry(
    from_screen_id: Option<String>,
    edge_id: Option<String>,
    to_screen_id: Option<String>,
    reason: Option<&str>,
    outcome: &str,
    viewport: Option<Viewport>,
) -> JournalEntry {
    JournalEntry {
        schema_version: JOURNAL_ENTRY_SCHEMA_VERSION.to_string(),
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0),
        from_screen_id,
        edge_id,
        to_screen_id,
        reason: reason.map(str::to_string),
        outcome: outcome.to_string(),
        viewport,
    }
}

fn viewport_from_action(action: &Value) -> Option<Viewport> {
    serde_json::from_value::<Option<Viewport>>(action.get("viewport").cloned().unwrap_or(Value::Null))
        .unwrap_or(None)
}

fn parse_point(point: &str) -> Result<(i64, i64)> {
    let (x, y) = point
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("--point must be X,Y"))?;
    Ok((x.trim().parse()?, y.trim().parse()?))
}

fn match_current_screen<R: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<R>,
) -> Result<serde_json::Value> {
    let layout = layout_result(android, false)?;
    let graph = load_graph(root)?;
    let normalized = normalize_layout(&layout["layout"]);
    let identity = identity_hash(&normalized);
    let screen_match = match_screen(&normalized, graph.screens.into_values());
    Ok(json!({
        "current_screen": {
            "status": screen_match.status,
            "matched_screen": screen_match.matched_screen,
            "match_confidence": screen_match.match_confidence,
            "hash_matched": screen_match.hash_matched,
            "identity_hash": identity
        },
        "metrics": {
            "layout_calls_total": 1,
            "layout_json_returned_to_agent": false,
            "adb_taps_total": 0
        },
        "layout_observed": layout["layout"].is_object()
    }))
}

fn execute_route_plan<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    plan: &minimap_graph::RoutePlan,
    target: &str,
    mode: &str,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
) -> Result<serde_json::Value> {
    let mut executed = Vec::new();
    let mut layout_calls_total = 0;
    let mut adb_taps_total = 0;
    let mut final_screen = serde_json::Value::Null;
    for edge in &plan.edges {
        let selector = selector_for_edge(edge)?;
        let action_result = tap_selector_result(
            android,
            adb,
            &selector,
            edge.intent
                .as_deref()
                .or(edge.action.description.as_deref()),
        )?;
        layout_calls_total += action_result["metrics"]["layout_calls_total"]
            .as_i64()
            .unwrap_or(0);
        adb_taps_total += action_result["metrics"]["adb_taps_total"]
            .as_i64()
            .unwrap_or(0);
        let action = action_result["action"].clone();
        let verification = match_current_screen(root, android)?;
        layout_calls_total += verification["metrics"]["layout_calls_total"]
            .as_i64()
            .unwrap_or(0);
        final_screen = verification["current_screen"].clone();
        let reached = final_screen["matched_screen"]
            .as_str()
            .map(|screen| screen == edge.to_screen)
            .unwrap_or(false);
        if !reached {
            return Ok(json!({
                "schema_version": RESULT_SCHEMA_VERSION,
                "status": "route_broken",
                "summary": "route edge did not reach expected screen",
                "target": target,
                "mode": mode,
                "edge": edge.id,
                "expected_screen": edge.to_screen,
                "observed": final_screen,
                "executed": executed,
                "metrics": {
                    "layout_calls_total": layout_calls_total,
                    "layout_json_returned_to_agent": false,
                    "adb_taps_total": adb_taps_total
                },
                "recommended_action": "inspect the app and stage a graph repair proposal"
            }));
        }
        executed.push(json!({
            "edge": edge.id,
            "selector": selector,
            "action": action,
            "verification": final_screen
        }));
    }
    Ok(json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "ok",
        "summary": "route executed",
        "target": target,
        "mode": mode,
        "edge_ids": plan.edges.iter().map(|edge| edge.id.clone()).collect::<Vec<_>>(),
        "executed": executed,
        "final_screen": final_screen,
        "preferred_path_used": plan.preferred_path_used,
        "graph_fallback_used": plan.graph_fallback_used,
        "estimated_layout_calls_saved": std::cmp::max(plan.edges.len(), 1),
        "metrics": {
            "layout_calls_total": layout_calls_total,
            "layout_json_returned_to_agent": false,
            "adb_taps_total": adb_taps_total
        }
    }))
}

fn drift_result<R: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<R>,
) -> Result<serde_json::Value> {
    let current = match_current_screen(root, android)?;
    let current_screen = &current["current_screen"];
    let status = match current_screen["status"]
        .as_str()
        .unwrap_or("screen_unknown")
    {
        "matched" => {
            let graph = load_graph(root)?;
            let context = load_context(root);
            let matched = current_screen["matched_screen"].as_str();
            let mismatches = matched
                .and_then(|screen_id| {
                    graph
                        .screens
                        .values()
                        .find(|screen| screen.id == screen_id)
                        .map(|screen| context.mismatches(&screen.context_guard))
                })
                .unwrap_or_default();
            if mismatches.is_empty() {
                "passed"
            } else {
                "context_mismatch"
            }
        }
        "repair_candidate" => "selector_drift",
        _ => "screen_unknown",
    };
    let mut result = json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": status,
        "summary": match status {
            "passed" => "current app state matches committed graph",
            "context_mismatch" => "current context does not satisfy matched screen guard",
            "selector_drift" => "current screen is similar but below match threshold",
            _ => "current app state is not known in committed graph"
        },
        "current_screen": current_screen,
        "metrics": current["metrics"],
        "human_approval_required": false
    });
    if matches!(status, "selector_drift" | "screen_unknown") {
        let proposal = json!({
            "schema_version": "minimap.proposal.v1",
            "id": format!("proposal-drift-{}", current_screen["identity_hash"].as_str().unwrap_or("unknown").replace(':', "_")),
            "kind": status,
            "reason": "Review current app state drift against committed Minimap graph.",
            "changes": []
        });
        let path = stage_proposal_value(root, &proposal)?;
        result["proposal_id"] = proposal["id"].clone();
        result["proposal_path"] = json!(path.display().to_string());
        result["human_approval_required"] = json!(true);
    }
    Ok(result)
}

fn validate_result<AR: CommandRunner, DR: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    all: bool,
    changed_files: Option<&str>,
    execute: bool,
    current_screen: Option<&str>,
) -> Result<serde_json::Value> {
    let drift = drift_result(root, android)?;
    let selected_routes = selected_routes_for_validation(root, all, changed_files)?;
    let status = drift["status"].as_str().unwrap_or("passed");
    let mut result = json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": status,
        "summary": if status == "passed" { "validation passed" } else { "validation found graph drift" },
        "drift": drift,
        "selected_routes": selected_routes,
        "impact_analysis": {
            "mode": if all { "all" } else if changed_files.is_some() { "changed_files" } else { "current" },
            "precise": execute
        }
    });
    if !execute || status != "passed" {
        return Ok(result);
    }

    let graph = load_graph(root)?;
    let context = load_context(root);
    let mut active_screen = current_screen.map(str::to_string).or_else(|| {
        result["drift"]["current_screen"]["matched_screen"]
            .as_str()
            .map(str::to_string)
    });
    let mut route_results = Vec::new();
    let mut skipped_routes = Vec::new();
    let mut aggregate_status = "passed".to_string();

    for route_name in selected_routes_for_validation(root, all, changed_files)? {
        let Some(route) = graph.routes.get(&route_name) else {
            skipped_routes.push(json!({"route": route_name, "reason": "route_unresolved"}));
            continue;
        };
        let Some(screen) = active_screen.as_deref() else {
            skipped_routes.push(json!({"route": route_name, "reason": "current_screen_unknown"}));
            continue;
        };
        if route.from_screen() != Some(screen) {
            skipped_routes.push(json!({
                "route": route_name,
                "reason": "start_screen_mismatch",
                "expected_start": route.from_screen(),
                "current_screen": screen
            }));
            continue;
        }
        let plan = resolve_route(&graph, &route_name, Some(screen), &context);
        if !plan.context_mismatches.is_empty() {
            skipped_routes.push(json!({
                "route": route_name,
                "reason": "context_mismatch",
                "mismatches": plan.context_mismatches
            }));
            continue;
        }
        if plan.status != "ok" {
            skipped_routes.push(json!({
                "route": route_name,
                "reason": "route_unresolved",
                "status": plan.status
            }));
            continue;
        }
        let route_result = execute_route_plan(root, &plan, &route_name, "verified", android, adb)?;
        if route_result["status"] != "ok" {
            aggregate_status = route_result["status"]
                .as_str()
                .unwrap_or("route_broken")
                .to_string();
        }
        if let Some(final_screen) = route_result["final_screen"]["matched_screen"].as_str() {
            active_screen = Some(final_screen.to_string());
        }
        route_results.push(json!({
            "route": route_name,
            "result": route_result
        }));
        if aggregate_status != "passed" {
            break;
        }
    }
    result["status"] = json!(aggregate_status);
    result["summary"] = json!(if aggregate_status == "passed" {
        "validation passed"
    } else {
        "validation route execution failed"
    });
    result["route_results"] = json!(route_results);
    result["skipped_routes"] = json!(skipped_routes);
    result["final_screen"] = json!(active_screen);
    Ok(result)
}

fn selected_routes_for_validation(
    root: &Path,
    all: bool,
    changed_files: Option<&str>,
) -> Result<Vec<String>> {
    let graph = load_graph(root)?;
    if all {
        return Ok(graph.routes.keys().cloned().collect());
    }
    let Some(path) = changed_files else {
        return Ok(Vec::new());
    };
    let changed = std::fs::read_to_string(path).unwrap_or_default();
    let mut selected = Vec::new();
    for route in graph.routes.values() {
        let route_text = serde_json::to_string(&route.triggers).unwrap_or_default();
        if changed
            .lines()
            .any(|line| !line.is_empty() && route_text.contains(line))
        {
            selected.push(route.name.clone());
        }
    }
    Ok(selected)
}

fn selector_for_edge(edge: &NavigationEdge) -> Result<String> {
    let candidate = edge
        .action
        .selector_candidates
        .iter()
        .filter(|candidate| candidate.value.is_some())
        .max_by(|left, right| {
            left.score
                .unwrap_or(0.0)
                .partial_cmp(&right.score.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .ok_or_else(|| anyhow::anyhow!("edge {} has no executable selector candidate", edge.id))?;
    let value = candidate.value.as_deref().unwrap_or_default();
    let key = match candidate.kind.as_str() {
        "visible_text" | "visible_text_fuzzy" => "text",
        "accessibility" | "accessibility_or_semantic" => "content_description",
        "resource_id" => "resource_id",
        "test_tag" => "test_tag",
        other => other,
    };
    Ok(format!("{key}={value}"))
}

fn doctor(root: &std::path::Path) -> serde_json::Value {
    let config_exists = root.join(".minimap/config.json").exists();
    let graph_dirs_exist = [
        ".minimap/graph/screens",
        ".minimap/graph/edges",
        ".minimap/routes",
        ".minimap/proposals",
    ]
    .iter()
    .all(|path| root.join(path).is_dir());
    let journal_status = journal_writable_status(root);
    let graph_tracked = graph_git_tracked_status(root);
    let mut checks = vec![
        json!({"name": "config", "status": if config_exists { "pass" } else { "fail" }}),
        json!({"name": "graph_dirs", "status": if graph_dirs_exist { "pass" } else { "fail" }}),
        json!({"name": "journal_writable", "status": journal_status}),
        graph_tracked,
        json!({"name": "android_cli", "status": if command_on_path("android") { "pass" } else { "warn" }}),
        json!({"name": "adb", "status": if command_on_path("adb") { "pass" } else { "warn" }}),
    ];
    let hint = checks
        .iter()
        .find(|check| check["name"] == "graph_tracked")
        .and_then(|check| check.get("hint").cloned());
    let fail = checks
        .iter()
        .filter(|check| check["status"] == "fail")
        .count();
    let warn = checks
        .iter()
        .filter(|check| check["status"] == "warn")
        .count();
    let pass = checks
        .iter()
        .filter(|check| check["status"] == "pass")
        .count();
    // Strip the helper `hint` field from individual check objects before serializing.
    for check in &mut checks {
        if let Value::Object(map) = check {
            map.remove("hint");
        }
    }
    let mut payload = json!({
        "ok": fail == 0,
        "root": root.canonicalize().unwrap_or_else(|_| root.to_path_buf()).display().to_string(),
        "summary": { "pass": pass, "warn": warn, "fail": fail },
        "checks": checks
    });
    if let Some(hint) = hint {
        payload["hint"] = hint;
    }
    payload
}

fn journal_writable_status(root: &Path) -> &'static str {
    let path = root.join(".minimap/journal.jsonl");
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return "not_writable";
        }
    }
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(_) => "ok",
        Err(_) => "not_writable",
    }
}

fn graph_git_tracked_status(root: &Path) -> Value {
    if !is_inside_git_work_tree(root) {
        return json!({
            "name": "graph_tracked",
            "status": "warn",
            "detail": "not in a git work tree"
        });
    }
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", ".minimap/graph", ".minimap/routes"])
        .output();
    let tracked_count = match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.is_empty())
            .count(),
        _ => 0,
    };
    if tracked_count > 0 {
        json!({
            "name": "graph_tracked",
            "status": "pass",
            "tracked_files": tracked_count
        })
    } else {
        json!({
            "name": "graph_tracked",
            "status": "warn",
            "hint": "tip: `git add .minimap/graph .minimap/routes` to enable `minimap undo`"
        })
    }
}

fn command_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|path| path.join(name).exists()))
        .unwrap_or(false)
}

fn route_define(
    root: &Path,
    name: &str,
    to: &str,
    from: Option<&str>,
    triggers: &[String],
) -> Result<i32> {
    if !screen_path(root, to).exists() {
        anyhow::bail!(
            "target screen '{to}' not found in .minimap/graph/screens — define the screen before the route"
        );
    }
    if let Some(from_id) = from {
        if !screen_path(root, from_id).exists() {
            anyhow::bail!(
                "from screen '{from_id}' not found in .minimap/graph/screens — define the screen before the route"
            );
        }
    }
    let expanded_triggers = expand_triggers(triggers);
    let trigger_values: Vec<Value> = expanded_triggers
        .iter()
        .map(|trigger| Value::String(trigger.clone()))
        .collect();
    let mut route_value = json!({
        "schema_version": ROUTE_SCHEMA_VERSION,
        "name": name,
        "target": { "screen": to },
        "triggers": trigger_values,
        "aliases": []
    });
    if let Some(from_id) = from {
        route_value["from"] = json!({ "screen": from_id });
    }
    let path = commit_route(root, &route_value)?;
    print_json(&json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "ok",
        "summary": format!(
            "defined route '{name}': {} → {to}",
            from.unwrap_or("(none)")
        ),
        "route_name": name,
        "route_path": path.display().to_string(),
        "from_screen": from,
        "to_screen": to,
        "triggers": expanded_triggers
    }));
    Ok(0)
}

fn expand_triggers(triggers: &[String]) -> Vec<String> {
    // Each `--triggers` arg is one entry verbatim — commas are part of glob syntax
    // (e.g. `{login,signup}/**`) and must not be split.
    triggers
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn screen_rename(root: &Path, id: &str, new_name: &str) -> Result<i32> {
    let renamed = rename_screen(root, id, new_name)?;
    print_json(&json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "ok",
        "summary": format!("renamed {id}: {} → {new_name}", renamed.old_name),
        "screen_id": id,
        "old_name": renamed.old_name,
        "new_name": new_name,
        "screen_path": renamed.path.display().to_string()
    }));
    Ok(0)
}

fn undo(root: &Path) -> Result<i32> {
    if !is_inside_git_work_tree(root) {
        print_json(&json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "status": "config_error",
            "summary": "not a git repo — undo requires git; commit your graph regularly to enable rollback."
        }));
        return Ok(1);
    }
    let paths: Vec<&str> = [".minimap/graph", ".minimap/routes"]
        .iter()
        .copied()
        .filter(|path| root.join(path).exists())
        .collect();
    if paths.is_empty() {
        print_json(&json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "status": "ok",
            "summary": "nothing to undo",
            "dropped": 0
        }));
        return Ok(0);
    }
    let mut status_args = vec!["status", "--porcelain", "--"];
    status_args.extend(paths.iter().copied());
    let porcelain = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(&status_args)
        .output()
        .map_err(|error| anyhow::anyhow!("failed to invoke git status: {error}"))?;
    if !porcelain.status.success() {
        let stderr = String::from_utf8_lossy(&porcelain.stderr).to_string();
        anyhow::bail!("git status failed: {}", stderr.trim());
    }
    let stdout = String::from_utf8_lossy(&porcelain.stdout);
    let changed_count = stdout.lines().filter(|line| !line.is_empty()).count();
    if changed_count == 0 {
        print_json(&json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "status": "ok",
            "summary": "nothing to undo",
            "dropped": 0
        }));
        return Ok(0);
    }
    let mut checkout_args = vec!["checkout", "--"];
    checkout_args.extend(paths.iter().copied());
    let checkout = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(&checkout_args)
        .output()
        .map_err(|error| anyhow::anyhow!("failed to invoke git checkout: {error}"))?;
    if !checkout.status.success() {
        let stderr = String::from_utf8_lossy(&checkout.stderr).to_string();
        anyhow::bail!("git checkout failed: {}", stderr.trim());
    }
    print_json(&json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": "ok",
        "summary": format!("dropped {changed_count} uncommitted graph changes"),
        "dropped": changed_count
    }));
    Ok(0)
}

fn is_inside_git_work_tree(root: &Path) -> bool {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();
    match output {
        Ok(output) => {
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).trim() == "true"
        }
        Err(_) => false,
    }
}

fn validate_screen<R: CommandRunner>(
    root: &Path,
    android: &mut AndroidCli<R>,
    screen: &str,
) -> Result<i32> {
    if screen != "current" {
        print_json(&json!({
            "schema_version": RESULT_SCHEMA_VERSION,
            "status": "config_error",
            "summary": "validate --screen <id> for a specific screen is not implemented; use --screen current",
            "screen": screen
        }));
        return Ok(7);
    }
    let current = match_current_screen(root, android)?;
    let current_screen = &current["current_screen"];
    let status_in = current_screen["status"].as_str().unwrap_or("screen_unknown");
    let (status_out, summary, code) = match status_in {
        "matched" => (
            "matched",
            "current app state matches committed graph",
            0,
        ),
        "repair_candidate" => (
            "drift",
            "current screen is similar but below match threshold",
            1,
        ),
        _ => (
            "unknown",
            "current app state is not known in committed graph",
            2,
        ),
    };
    print_json(&json!({
        "schema_version": RESULT_SCHEMA_VERSION,
        "status": status_out,
        "summary": summary,
        "screen": "current",
        "current_screen": current_screen,
        "metrics": current["metrics"]
    }));
    Ok(code)
}

fn print_json(value: &serde_json::Value) {
    print!("{}", canonical_json(value));
}
