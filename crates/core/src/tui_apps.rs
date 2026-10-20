//! Pure-computation Rhai apps. Files belong to the tool, never to the interpreter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Result, anyhow, bail, ensure};
use rhai::packages::{
    BasicArrayPackage, BasicMapPackage, BasicMathPackage, CorePackage, LogicPackage,
    MoreStringPackage, Package,
};
use rhai::{AST, Dynamic, Engine, Map, Scope};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::interactive::{Document, Session, Tone};

/// Bundled example, installed only on an explicit open of a missing `rps`.
pub const RPS: &str = include_str!("../assets/tui_apps/rps/app.rhai");
/// Frontend-injected renderer; keeps terminal dependencies out of core.
pub type PreviewRenderer = fn(&View, u16, u16) -> Result<String>;
/// A queued open shared by the app and its toolboxes, without a new app event.
pub type PendingOpen = Arc<Mutex<Option<String>>>;

/// A single printable character at a bounded canvas coordinate.
#[derive(Debug, Deserialize)]
pub struct Cell(pub u16, pub u16, pub String, pub Tone);

/// Free drawing data, separate from the declarative node schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Canvas {
    /// Fixed height, in terminal rows.
    pub rows: u16,
    /// At most 2,000 character placements.
    pub cells: Vec<Cell>,
}

/// Validated scalar bindings and canvas overlays, indexed by document node.
pub struct View {
    /// Existing nexus-ui session, with inert actions and canvas placeholders.
    pub session: Session,
    /// Canvas nodes replacing empty text placeholders in the document.
    pub canvases: BTreeMap<usize, Canvas>,
}

impl View {
    /// Height of a node, including canvas replacements.
    pub fn rows(&self, index: usize) -> u16 {
        self.canvases.get(&index).map_or_else(
            || self.session.document().nodes()[index].rows(),
            |canvas| canvas.rows,
        )
    }
}

/// One bounded interpreter and in-memory state per app instance.
pub struct Runner {
    engine: Engine,
    ast: AST,
    state: Map,
    log: Arc<Mutex<String>>,
}

fn script_error(error: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("script error: {error}")
}

impl Runner {
    /// Compile the script and call `init`. No host capabilities are registered.
    pub fn new(source: &str) -> Result<Self> {
        let mut engine = Engine::new_raw();
        CorePackage::new().register_into_engine(&mut engine);
        LogicPackage::new().register_into_engine(&mut engine);
        BasicMathPackage::new().register_into_engine(&mut engine);
        MoreStringPackage::new().register_into_engine(&mut engine);
        BasicArrayPackage::new().register_into_engine(&mut engine);
        BasicMapPackage::new().register_into_engine(&mut engine);
        engine.disable_symbol("eval");
        engine.set_max_operations(200_000);
        engine.set_max_expr_depths(64, 32);
        engine.set_max_string_size(16 * 1024);
        engine.set_max_array_size(1_000);
        engine.set_max_map_size(256);
        engine.set_max_call_levels(32);
        let log = Arc::new(Mutex::new(String::new()));
        let printed = Arc::clone(&log);
        engine.on_print(move |text| append_log(&printed, text));
        let debugged = Arc::clone(&log);
        engine.on_debug(move |text, _, _| append_log(&debugged, text));
        let ast = engine.compile(source).map_err(script_error)?;
        let state = engine
            .call_fn::<Map>(&mut Scope::new(), &ast, "init", ())
            .map_err(|error| {
                anyhow!(
                    "script error: {error}\nLog:\n{}",
                    log.lock().expect("script log")
                )
            })?;
        engine
            .ensure_data_size_within_limits(&Dynamic::from(state.clone()))
            .map_err(script_error)?;
        Ok(Self {
            engine,
            ast,
            state,
            log,
        })
    }

    /// Deliver a validated event. Disabled ticks are ignored, including in preview.
    pub fn update(&mut self, event: &Value) -> Result<()> {
        validate_event(event)?;
        if event["type"] == "tick" && self.tick_ms().is_none() {
            return Ok(());
        }
        let event = rhai::serde::to_dynamic(event).map_err(script_error)?;
        let state = self
            .engine
            .call_fn::<Map>(
                &mut Scope::new(),
                &self.ast,
                "update",
                (self.state.clone(), event),
            )
            .map_err(script_error)?;
        // Index assignment in Rhai can grow maps without checking their root.
        self.engine
            .ensure_data_size_within_limits(&Dynamic::from(state.clone()))
            .map_err(script_error)?;
        self.state = state;
        Ok(())
    }

    /// Opt-in tick period. Values below 100 ms (or non-integers) disable ticks.
    pub fn tick_ms(&self) -> Option<u64> {
        self.state
            .get("tick_ms")?
            .as_int()
            .ok()
            .and_then(|ms| u64::try_from(ms).ok())
            .filter(|ms| *ms >= 100)
    }

    /// Call `view` and validate all components against scalar state bindings.
    pub fn view(&self, width: u16) -> Result<View> {
        let value = self
            .engine
            .call_fn::<Dynamic>(&mut Scope::new(), &self.ast, "view", (self.state.clone(),))
            .map_err(script_error)?;
        self.engine
            .ensure_data_size_within_limits(&value)
            .map_err(script_error)?;
        let mut value: Value = rhai::serde::from_dynamic(&value).map_err(script_error)?;
        ensure!(value.is_object(), "view must return a map");
        ensure!(
            value.as_object().is_some_and(|map| map.len() == 2
                && map.contains_key("title")
                && map.contains_key("view")),
            "view needs only title and view"
        );
        ensure!(
            serde_json::to_string(&value)?.len() <= crate::interactive::MAX_SOURCE,
            "view exceeds 16 KiB"
        );
        let nodes = value["view"]
            .as_array_mut()
            .ok_or_else(|| anyhow!("view must be an array"))?;
        let mut canvases = BTreeMap::new();
        let mut actions = serde_json::Map::new();
        for (index, node) in nodes.iter_mut().enumerate() {
            if node["type"] == "canvas" {
                let mut data = node.clone();
                data.as_object_mut().expect("canvas object").remove("type");
                let canvas: Canvas = serde_json::from_value(data)?;
                ensure!((1..=16).contains(&canvas.rows), "canvas rows must be 1–16");
                ensure!(canvas.cells.len() <= 2_000, "too many canvas cells");
                for Cell(x, y, text, _) in &canvas.cells {
                    ensure!(*x < width && *y < canvas.rows, "canvas cell out of bounds");
                    ensure!(
                        text.chars().count() == 1
                            && text.chars().all(|ch| ch == ' '
                                || ch.is_ascii_graphic()
                                || ch.escape_debug().count() == 1),
                        "canvas cells need one printable character"
                    );
                }
                canvases.insert(index, canvas);
                *node = json!({"type":"text", "text":""});
            } else if let Some(buttons) = node
                .get("items")
                .and_then(Value::as_array)
                .filter(|_| node["type"] == "buttons")
            {
                for button in buttons {
                    if let Some(action) = button["action"].as_str() {
                        actions.insert(action.into(), json!([]));
                    }
                }
            }
        }
        // Only scalars cross the nexus-ui binding seam. Arrays/maps remain private
        // computation state; unknown or wrongly typed bindings still fail validation.
        let state: Value =
            rhai::serde::from_dynamic(&Dynamic::from(self.state.clone())).map_err(script_error)?;
        let state: serde_json::Map<_, _> = state
            .as_object()
            .expect("state map")
            .iter()
            .filter(|(_, value)| value.is_boolean() || value.is_number() || value.is_string())
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let document = Document::parse(&json!({"version":1, "title":value["title"], "state":state, "view":value["view"], "actions":actions}).to_string())?;
        let view = View {
            session: Session::new(document)?,
            canvases,
        };
        let rows: u16 = (0..view.session.document().nodes().len())
            .map(|index| view.rows(index))
            .sum();
        ensure!(
            rows <= crate::interactive::ROWS - 3,
            "UI exceeds its fixed display budget"
        );
        Ok(view)
    }

    /// Captured, bounded print/debug output, with terminal controls removed.
    pub fn log(&self) -> String {
        self.log.lock().expect("script log").clone()
    }
}

fn append_log(log: &Mutex<String>, text: &str) {
    let mut log = log.lock().expect("script log");
    for ch in text
        .chars()
        .filter(|ch| !ch.is_control() || *ch == '\n')
        .chain(std::iter::once('\n'))
    {
        if log.len() + ch.len_utf8() > 16 * 1024 {
            break;
        }
        log.push(ch);
    }
}

/// Accept only app events; Escape never reaches a script.
pub fn validate_event(event: &Value) -> Result<()> {
    let object = event
        .as_object()
        .ok_or_else(|| anyhow!("event must be a map"))?;
    let fields: &[&str] = match event["type"].as_str() {
        Some("button") => {
            ensure!(
                event["action"].as_str().is_some_and(|s| !s.is_empty()
                    && s.len() <= 120
                    && !s.chars().any(char::is_control)),
                "invalid button action"
            );
            &["type", "action"]
        }
        Some("key") => {
            let key = event["key"].as_str().unwrap_or_default();
            ensure!(
                (key.len() == 1 && key.bytes().all(|ch| ch.is_ascii_alphanumeric()))
                    || ["up", "down", "left", "right", "enter", "space"].contains(&key),
                "invalid or reserved key"
            );
            &["type", "key"]
        }
        Some("input") => {
            ensure!(
                event["key"].as_str().is_some_and(|s| !s.is_empty()
                    && s.len() <= 120
                    && !s.chars().any(char::is_control)),
                "invalid input key"
            );
            let value = &event["value"];
            ensure!(
                value.is_boolean()
                    || value.is_number()
                    || value
                        .as_str()
                        .is_some_and(|s| s.len() <= 1024 && !s.chars().any(char::is_control)),
                "invalid input value"
            );
            &["type", "key", "value"]
        }
        Some("tick") => &["type"],
        _ => bail!("unknown app event"),
    };
    ensure!(
        object.len() == fields.len() && object.keys().all(|key| fields.contains(&key.as_str())),
        "invalid event fields"
    );
    Ok(())
}

/// Validate the space-local app name.
pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == b'-')
}

/// Confine even a symlinked app or `tui_apps` directory to the space.
pub fn app_path(root: &Path, name: &str) -> Result<PathBuf> {
    ensure!(valid_name(name), "invalid app name: use [a-z0-9-]{{1,32}}");
    let space = root
        .parent()
        .ok_or_else(|| anyhow!("missing space directory"))?;
    crate::tools::confined_join(space, &format!("tui_apps/{name}/app.rhai"))
        .map_err(|error| anyhow!(error))?;
    crate::tools::confined_join(root, &format!("{name}/app.rhai")).map_err(|error| anyhow!(error))
}

/// Read an app, copying the bundled example only for an explicit open.
pub fn load(root: &Path, name: &str, install_example: bool) -> Result<String> {
    let path = app_path(root, name)?;
    if install_example && name == "rps" && !path.try_exists()? {
        std::fs::create_dir_all(path.parent().expect("app parent"))?;
        std::fs::write(&path, RPS)?;
    }
    Ok(std::fs::read_to_string(path)?)
}

/// Space-file actions plus sandboxed preview. `open` queues a frontend request.
pub fn tool(
    root: &Path,
    action: &str,
    args: &Value,
    renderer: Option<PreviewRenderer>,
    pending: &PendingOpen,
) -> Result<String> {
    if action == "list" {
        let mut names = Vec::new();
        if root.try_exists()? {
            // Check the root before enumerating it, not just individual files.
            app_path(root, "probe")?;
            for entry in std::fs::read_dir(root)? {
                let name = entry?.file_name().to_string_lossy().into_owned();
                if valid_name(&name) && app_path(root, &name)?.is_file() {
                    names.push(name);
                }
            }
        }
        names.sort();
        return Ok(serde_json::to_string(&names)?);
    }
    let name = args["name"].as_str().unwrap_or_default();
    let path = app_path(root, name)?;
    match action {
        "read" => {
            let text = std::fs::read_to_string(path)?;
            Ok(crate::tools::number_lines_with_hash(
                &text.lines().collect::<Vec<_>>(),
                0,
            ))
        }
        "write" => {
            let content = args["content"]
                .as_str()
                .ok_or_else(|| anyhow!("content must be a string"))?;
            std::fs::create_dir_all(path.parent().expect("app parent"))?;
            std::fs::write(path, content)?;
            Ok(format!("wrote {name}/app.rhai ({} bytes)", content.len()))
        }
        "edit" => {
            let edits = args["edits"]
                .as_array()
                .ok_or_else(|| anyhow!("edits must be an array"))?
                .iter()
                .map(|edit| {
                    Ok((
                        edit["hash"]
                            .as_str()
                            .ok_or_else(|| anyhow!("edit needs hash"))?
                            .to_string(),
                        edit["line"].as_u64().and_then(|n| usize::try_from(n).ok()),
                        edit["new"].as_str().map(str::to_string),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            let text = std::fs::read_to_string(&path)?;
            let (text, diff) = crate::tools::apply_hashline_edits(&text, &edits)
                .map_err(|error| anyhow!(error))?;
            std::fs::write(path, text)?;
            Ok(format!("edited {name} — {diff}"))
        }
        "open" => {
            load(root, name, true)?;
            *pending.lock().expect("pending app") = Some(name.into());
            Ok(format!("opening {name}"))
        }
        "preview" => {
            let source = load(root, name, false)?;
            let width = dimension(args, "width", 60, 160)?;
            let height = dimension(args, "height", 20, 60)?;
            let mut runner = match Runner::new(&source) {
                Ok(runner) => runner,
                Err(error) => return Ok(error.to_string()),
            };
            let result = (|| -> Result<String> {
                if let Some(events) = args.get("events") {
                    let events = events
                        .as_array()
                        .ok_or_else(|| anyhow!("events must be an array"))?;
                    ensure!(events.len() <= 100, "preview allows at most 100 events");
                    for event in events {
                        runner.update(event)?;
                    }
                }
                let view = runner.view(width)?;
                renderer.ok_or_else(|| anyhow!("preview needs an interactive renderer"))?(
                    &view, width, height,
                )
            })();
            Ok(format!(
                "{}\nLog:\n{}",
                result.unwrap_or_else(|error| format!("Error: {error}")),
                runner.log()
            ))
        }
        _ => bail!("unknown tui_app action"),
    }
}

fn dimension(args: &Value, key: &str, default: u16, max: u16) -> Result<u16> {
    let Some(value) = args.get(key) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .filter(|n| (1..=max).contains(n))
        .ok_or_else(|| anyhow!("{key} must be 1–{max}"))?;
    Ok(value)
}

#[cfg(test)]
mod tests;
