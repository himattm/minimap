use anyhow::{bail, Context, Result};
use minimap_schemas::Viewport;
use serde::Serialize;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub args: Vec<String>,
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    fn run(&mut self, args: &[String]) -> Result<CommandResult>;
}

#[derive(Default)]
pub struct SubprocessRunner;

impl CommandRunner for SubprocessRunner {
    fn run(&mut self, args: &[String]) -> Result<CommandResult> {
        let (program, rest) = args.split_first().context("empty command")?;
        let output = Command::new(program).args(rest).output().with_context(|| {
            format!(
                "failed to execute {}",
                args.first().cloned().unwrap_or_default()
            )
        })?;
        Ok(CommandResult {
            args: args.to_vec(),
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8(output.stdout)?,
            stderr: String::from_utf8(output.stderr)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TapPoint {
    pub x: i64,
    pub y: i64,
}

pub fn parse_input_tap(output: &str) -> Result<TapPoint> {
    let words: Vec<_> = output.split_whitespace().collect();
    for window in words.windows(4) {
        if window[0] == "input" && window[1] == "tap" {
            return Ok(TapPoint {
                x: window[2].parse()?,
                y: window[3].parse()?,
            });
        }
    }
    for window in words.windows(6) {
        if window[0] == "adb" && window[1] == "shell" && window[2] == "input" && window[3] == "tap"
        {
            return Ok(TapPoint {
                x: window[4].parse()?,
                y: window[5].parse()?,
            });
        }
    }
    bail!("Expected android screen resolve to return 'input tap X Y'")
}

pub struct AndroidCli<R> {
    runner: R,
    android_bin: String,
}

impl<R: CommandRunner> AndroidCli<R> {
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            android_bin: "android".to_string(),
        }
    }

    pub fn layout(&mut self, diff: bool) -> Result<CommandResult> {
        let mut args = vec![self.android_bin.clone(), "layout".to_string()];
        if diff {
            args.push("--diff".to_string());
        }
        run_checked(&mut self.runner, args)
    }

    pub fn screen_capture(&mut self, output: &str, annotate: bool) -> Result<CommandResult> {
        let mut args = vec![
            self.android_bin.clone(),
            "screen".to_string(),
            "capture".to_string(),
        ];
        if annotate {
            args.push("--annotate".to_string());
        }
        args.push(format!("--output={output}"));
        run_checked(&mut self.runner, args)
    }

    pub fn screen_resolve(&mut self, screenshot: &str, string: &str) -> Result<CommandResult> {
        run_checked(
            &mut self.runner,
            vec![
                self.android_bin.clone(),
                "screen".to_string(),
                "resolve".to_string(),
                format!("--screenshot={screenshot}"),
                format!("--string={string}"),
            ],
        )
    }
}

pub struct Adb<R> {
    runner: R,
    adb_bin: String,
}

impl<R: CommandRunner> Adb<R> {
    pub fn new(runner: R) -> Self {
        Self {
            runner,
            adb_bin: "adb".to_string(),
        }
    }

    pub fn tap(&mut self, point: TapPoint) -> Result<CommandResult> {
        run_checked(
            &mut self.runner,
            vec![
                self.adb_bin.clone(),
                "shell".to_string(),
                "input".to_string(),
                "tap".to_string(),
                point.x.to_string(),
                point.y.to_string(),
            ],
        )
    }

    pub fn display_size(&mut self) -> Result<Viewport> {
        let result = run_checked(
            &mut self.runner,
            vec![
                self.adb_bin.clone(),
                "shell".to_string(),
                "wm".to_string(),
                "size".to_string(),
            ],
        )?;
        parse_wm_size(&result.stdout)
    }
}

/// Parse `adb shell wm size` output. When `Override size:` is present it wins
/// because that is what the device actually renders at; otherwise fall back to
/// `Physical size:`.
pub fn parse_wm_size(stdout: &str) -> Result<Viewport> {
    let mut physical: Option<Viewport> = None;
    let mut override_size: Option<Viewport> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Physical size:") {
            physical = parse_size_value(rest.trim());
        } else if let Some(rest) = line.strip_prefix("Override size:") {
            override_size = parse_size_value(rest.trim());
        }
    }
    override_size
        .or(physical)
        .context("could not parse `wm size` output")
}

fn parse_size_value(value: &str) -> Option<Viewport> {
    let (w, h) = value.split_once('x')?;
    let width = w.trim().parse().ok()?;
    let height = h.trim().parse().ok()?;
    Some(Viewport { width, height })
}

fn run_checked<R: CommandRunner>(runner: &mut R, args: Vec<String>) -> Result<CommandResult> {
    let result = runner.run(&args)?;
    if result.status != 0 {
        bail!(
            "command failed: {} (status {}, stderr: {})",
            result.args.join(" "),
            result.status,
            result.stderr
        );
    }
    Ok(result)
}

pub fn layout_result<R: CommandRunner>(android: &mut AndroidCli<R>, diff: bool) -> Result<Value> {
    let command = android.layout(diff)?;
    let layout =
        serde_json::from_str::<Value>(&command.stdout).unwrap_or(Value::String(command.stdout));
    let mut result = json!({
        "status": "ok",
        "kind": if diff { "android_layout_diff" } else { "android_layout" },
        "layout": layout,
        "metrics": {
            "layout_calls_total": 1,
            "layout_json_returned_to_agent": true,
            "adb_taps_total": 0
        }
    });
    if diff {
        result["diff_scope"] = Value::String("android_in_session".to_string());
    }
    Ok(result)
}

pub fn tap_selector_result<AR: CommandRunner, DR: CommandRunner>(
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    selector: &str,
    reason: Option<&str>,
) -> Result<Value> {
    let command = android.layout(false)?;
    let layout: Value = serde_json::from_str(&command.stdout)?;
    let point = resolve_selector_point(&layout, selector)?;
    adb.tap(point)?;
    let viewport = capture_viewport(adb);
    let mut action = json!({
        "kind": "tap",
        "path": "layout_selector",
        "selector": selector,
        "point": { "x": point.x, "y": point.y }
    });
    if let Some(viewport) = viewport {
        action["viewport"] = json!({ "width": viewport.width, "height": viewport.height });
    }
    if let Some(reason) = reason {
        action["reason"] = Value::String(reason.to_string());
    }
    Ok(json!({
        "status": "ok",
        "action": action,
        "metrics": {
            "layout_calls_total": 1,
            "layout_json_returned_to_agent": false,
            "adb_taps_total": 1
        }
    }))
}

pub fn tap_point_result<R: CommandRunner>(
    adb: &mut Adb<R>,
    x: i64,
    y: i64,
    mode: &str,
) -> Result<Value> {
    if mode == "fast" {
        bail!("fast coordinate taps require a guard");
    }
    let point = TapPoint { x, y };
    adb.tap(point)?;
    let viewport = capture_viewport(adb);
    let mut action = json!({
        "kind": "tap",
        "path": "coordinate",
        "mode": mode,
        "point": { "x": x, "y": y }
    });
    if let Some(viewport) = viewport {
        action["viewport"] = json!({ "width": viewport.width, "height": viewport.height });
    }
    Ok(json!({
        "status": "ok",
        "action": action,
        "metrics": {
            "layout_calls_total": 0,
            "layout_json_returned_to_agent": false,
            "adb_taps_total": 1
        }
    }))
}

pub fn tap_label_result<AR: CommandRunner, DR: CommandRunner>(
    android: &mut AndroidCli<AR>,
    adb: &mut Adb<DR>,
    label: i64,
    screenshot: &str,
) -> Result<Value> {
    android.screen_capture(screenshot, true)?;
    let resolved = android.screen_resolve(screenshot, &format!("input tap #{label}"))?;
    let point = parse_input_tap(&resolved.stdout)?;
    adb.tap(point)?;
    let viewport = capture_viewport(adb);
    let mut action = json!({
        "kind": "tap",
        "path": "annotated_screenshot_label",
        "label": label,
        "screenshot": screenshot,
        "point": { "x": point.x, "y": point.y }
    });
    if let Some(viewport) = viewport {
        action["viewport"] = json!({ "width": viewport.width, "height": viewport.height });
    }
    Ok(json!({
        "status": "ok",
        "action": action,
        "metrics": {
            "layout_calls_total": 0,
            "layout_json_returned_to_agent": false,
            "adb_taps_total": 1
        }
    }))
}

fn capture_viewport<R: CommandRunner>(adb: &mut Adb<R>) -> Option<Viewport> {
    match adb.display_size() {
        Ok(viewport) => Some(viewport),
        Err(error) => {
            eprintln!("minimap: viewport capture failed: {error}");
            None
        }
    }
}

pub fn resolve_selector_point(layout: &Value, selector: &str) -> Result<TapPoint> {
    let (key, expected) = selector
        .split_once('=')
        .context("selectors must use key=value syntax")?;
    // `android layout` emits hyphenated keys (content-desc, resource-id); legacy
    // UIAutomator dumps use camelCase. Try both so we don't break either shape.
    let candidates: Vec<&str> = match key.trim() {
        "content_desc" | "content_description" | "desc" => {
            vec!["content-desc", "contentDescription"]
        }
        "id" | "resource_id" => vec!["resource-id", "resourceId"],
        "test_tag" => vec!["testTag", "test-tag"],
        other => vec![other],
    };
    let expected = expected.trim();
    for node in walk_nodes(layout) {
        for k in &candidates {
            if node.get(*k).and_then(Value::as_str) == Some(expected) {
                return center_of(node).context("matched node has no tap bounds");
            }
        }
    }
    bail!("Selector not found: {selector}")
}

fn walk_nodes(value: &Value) -> Vec<&serde_json::Map<String, Value>> {
    let mut nodes = Vec::new();
    if let Value::Object(map) = value {
        nodes.push(map);
        for key in ["children", "nodes"] {
            if let Some(Value::Array(children)) = map.get(key) {
                for child in children {
                    nodes.extend(walk_nodes(child));
                }
            }
        }
    } else if let Value::Array(values) = value {
        for value in values {
            nodes.extend(walk_nodes(value));
        }
    }
    nodes
}

fn center_of(node: &serde_json::Map<String, Value>) -> Option<TapPoint> {
    if let Some(point) = bounds_center(node) {
        return Some(point);
    }
    // Real `android layout` output exposes precomputed center as a "[x,y]" string.
    node.get("center")
        .and_then(Value::as_str)
        .and_then(parse_center_string)
}

fn bounds_center(node: &serde_json::Map<String, Value>) -> Option<TapPoint> {
    match node.get("bounds")? {
        Value::Object(bounds) => {
            let left = number(bounds, &["left", "x", "minX"])?;
            let top = number(bounds, &["top", "y", "minY"])?;
            let right = number(bounds, &["right", "maxX"]).or_else(|| {
                let width = number(bounds, &["width"])?;
                Some(left + width)
            })?;
            let bottom = number(bounds, &["bottom", "maxY"]).or_else(|| {
                let height = number(bounds, &["height"])?;
                Some(top + height)
            })?;
            Some(TapPoint {
                x: ((left + right) / 2.0).round() as i64,
                y: ((top + bottom) / 2.0).round() as i64,
            })
        }
        Value::Array(values) if values.len() == 4 => {
            let left = values[0].as_f64()?;
            let top = values[1].as_f64()?;
            let right = values[2].as_f64()?;
            let bottom = values[3].as_f64()?;
            Some(TapPoint {
                x: ((left + right) / 2.0).round() as i64,
                y: ((top + bottom) / 2.0).round() as i64,
            })
        }
        _ => None,
    }
}

fn parse_center_string(s: &str) -> Option<TapPoint> {
    let inner = s.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (x, y) = inner.split_once(',')?;
    Some(TapPoint {
        x: x.trim().parse().ok()?,
        y: y.trim().parse().ok()?,
    })
}

fn number(map: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<f64> {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_f64))
}

pub fn write_fake_executable(path: &Path, body: &str) -> Result<()> {
    fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeRunner {
        outputs: Vec<CommandResult>,
        calls: Vec<Vec<String>>,
    }

    impl FakeRunner {
        fn new(outputs: Vec<CommandResult>) -> Self {
            Self {
                outputs,
                calls: vec![],
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&mut self, args: &[String]) -> Result<CommandResult> {
            self.calls.push(args.to_vec());
            Ok(self.outputs.remove(0))
        }
    }

    fn ok(args: &[&str], stdout: &str) -> CommandResult {
        CommandResult {
            args: args.iter().map(|arg| arg.to_string()).collect(),
            status: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn parses_input_tap() {
        assert_eq!(
            parse_input_tap("input tap 500 1000").unwrap(),
            TapPoint { x: 500, y: 1000 }
        );
        assert_eq!(
            parse_input_tap("resolved: adb shell input tap 10 20").unwrap(),
            TapPoint { x: 10, y: 20 }
        );
    }

    #[test]
    fn layout_diff_uses_android_in_session_scope() {
        let mut android =
            AndroidCli::new(FakeRunner::new(vec![ok(&["android"], r#"{"changed":[]}"#)]));
        let result = layout_result(&mut android, true).unwrap();
        assert_eq!(result["kind"], "android_layout_diff");
        assert_eq!(result["diff_scope"], "android_in_session");
    }

    #[test]
    fn tap_selector_resolves_bounds_and_taps() {
        let layout = r#"{"children":[{"text":"Settings","bounds":{"left":100,"top":200,"right":300,"bottom":400}}]}"#;
        let mut android = AndroidCli::new(FakeRunner::new(vec![ok(&["android"], layout)]));
        let mut adb = Adb::new(FakeRunner::new(vec![
            ok(&["adb"], ""),
            ok(&["adb"], "Physical size: 1080x2400\n"),
        ]));
        let result = tap_selector_result(
            &mut android,
            &mut adb,
            "text=Settings",
            Some("open settings"),
        )
        .unwrap();
        assert_eq!(result["action"]["point"], json!({"x": 200, "y": 300}));
        assert_eq!(
            result["action"]["viewport"],
            json!({"width": 1080, "height": 2400})
        );
    }

    #[test]
    fn tap_selector_resolves_real_cli_shape() {
        // Mirrors what `android layout` actually emits: flat array of nodes
        // with hyphenated keys and a stringified center.
        let layout = r#"[{"content-desc":"Settings","center":"[1006,147]","key":1},{"text":"Commute","center":"[585,659]","key":2}]"#;
        let mut android = AndroidCli::new(FakeRunner::new(vec![ok(&["android"], layout)]));
        let mut adb = Adb::new(FakeRunner::new(vec![
            ok(&["adb"], ""),
            ok(&["adb"], "Physical size: 1080x2400\n"),
        ]));
        let result = tap_selector_result(
            &mut android,
            &mut adb,
            "content_desc=Settings",
            Some("open settings"),
        )
        .unwrap();
        assert_eq!(result["action"]["point"], json!({"x": 1006, "y": 147}));
    }

    #[test]
    fn parse_center_string_parses_bracketed_pair() {
        assert_eq!(
            parse_center_string("[1006,147]").unwrap(),
            TapPoint { x: 1006, y: 147 }
        );
        assert_eq!(
            parse_center_string(" [ 12 , 34 ] ").unwrap(),
            TapPoint { x: 12, y: 34 }
        );
        assert!(parse_center_string("1006,147").is_none());
        assert!(parse_center_string("[abc,def]").is_none());
    }

    #[test]
    fn parse_wm_size_physical_only() {
        let viewport = parse_wm_size("Physical size: 1080x2400\n").unwrap();
        assert_eq!(
            viewport,
            Viewport {
                width: 1080,
                height: 2400
            }
        );
    }

    #[test]
    fn parse_wm_size_override_wins() {
        let viewport =
            parse_wm_size("Physical size: 1080x2400\nOverride size: 720x1600\n").unwrap();
        assert_eq!(
            viewport,
            Viewport {
                width: 720,
                height: 1600
            }
        );
    }

    #[test]
    fn parse_wm_size_rejects_garbage() {
        assert!(parse_wm_size("no size here").is_err());
    }

    #[test]
    fn adb_display_size_returns_viewport_from_runner() {
        let mut adb = Adb::new(FakeRunner::new(vec![ok(
            &["adb"],
            "Physical size: 1080x2400\n",
        )]));
        let viewport = adb.display_size().unwrap();
        assert_eq!(
            viewport,
            Viewport {
                width: 1080,
                height: 2400
            }
        );
    }
}
