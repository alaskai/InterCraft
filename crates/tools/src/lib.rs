// intercraft-tools: the tools an LLM can call, and what each one does to the project.
// Shared by the MCP server and the built-in chat panel, so both behave the same.
// serde_json 1, anyhow 1, dirs 7

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use intercraft_core::{FeatureEdit, LoadedProject, NewFeature, Params, Part};
use intercraft_geo::{ExportFormat, FeatureInput, FeatureResult, PythonWorker, RebuildRequest};
use serde_json::{json, Value};

/// How to use the tools well. Sent as MCP server instructions and as the chat system prompt.
pub const GUIDE: &str = r#"InterCraft holds parametric CAD parts as an ordered list of features. The user watches the model update in a live viewer after every tool call, so make one small change per call.

Writing a feature:
- A feature is a short build123d snippet (algebra mode). `from build123d import *` and `math` are already imported.
- The snippet reads `part` (the shape so far, None for the first feature) and must reassign `part`.
- Parameters are in the dict `p`. Put every dimension a user may want to change in params, not in the code.
- All lengths are mm. Z is up.
- Examples:
    part = Box(p["width"], p["depth"], p["height"])
    part = part - Pos(0, 0, 5) * Cylinder(p["hole_d"] / 2, 50)
    part = fillet(part.edges().filter_by(Axis.Z), p["radius"])

Working method:
- Call get_project first. It returns every part, feature, snippet and current build numbers.
- Every edit returns a build report. Check it before moving on: a cut must lower the volume, an addition must raise it, and the size must be what you intended.
- If a feature fails, the error is in the report and the change stays saved. Fix it with edit_feature, or remove it with delete_feature.
- Prefer set_param for dimension changes. Later features rebuild automatically.
- Pass the user's request in `prompt` when adding a feature. It is shown on their timeline.
- State which values you measured from a report and which you assumed.

Imported parts and measuring:
- A part can start from an imported STEP file (its first step is "import"). In the first feature after it, `part` is that imported shape, so cuts, holes and fillets work on it like any other.
- Before editing an imported part, call inspect_part to find its faces, holes and their exact positions instead of guessing.
- Use measure to check clearances, hole spacing and wall thickness after a change.

Undo:
- Every change is recorded. If an edit went wrong, or the user asks to go back, call undo instead of rebuilding by hand. redo steps forward again.
- delete_part removes a whole part. Reference parts can only be removed by the user."#;

/// Which project the tools edit.
enum Target {
    Fixed(PathBuf),
    /// Whatever the InterCraft viewer opened most recently, checked on every call.
    FollowViewer,
}

pub struct ToolHost {
    target: Target,
    worker: Option<PythonWorker>,
    /// Offer `ask_user`. Only InterCraft's own chat panel can show the question.
    ask_user: bool,
}

/// The editing tools plus `ask_user`, for agents talking through InterCraft's chat panel.
pub fn chat_panel_definitions() -> Value {
    let mut tools = definitions();
    if let Some(list) = tools.as_array_mut() {
        list.push(json!({
            "name": "ask_user",
            "description": "Ask the user to choose when the decision is genuinely theirs: design intent, a trade-off, \
or a dimension you can't measure or sensibly assume. The options appear as buttons in InterCraft's chat. \
After calling this, end your reply without further tool calls; the answer arrives as the user's next message. \
Don't use it for things you can work out from the project or the build reports.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "One clear question, ending with a question mark." },
                    "options": {
                        "type": "array",
                        "minItems": 2,
                        "maxItems": 4,
                        "description": "2 to 4 distinct choices. The user can also type their own answer.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string", "description": "Short choice, 1 to 6 words." },
                                "description": { "type": "string", "description": "What picking it means, one sentence." }
                            },
                            "required": ["label"]
                        }
                    },
                    "multi_select": { "type": "boolean", "description": "Let the user pick more than one option." }
                },
                "required": ["question", "options"]
            },
        }));
    }
    tools
}

/// Checks an `ask_user` call so the chat panel never has to draw nonsense.
pub fn validate_question(args: &Value) -> Result<()> {
    let question = text(args, "question")?.trim();
    if question.is_empty() || question.chars().count() > 300 {
        bail!("the question must be 1 to 300 characters");
    }
    let options = args.get("options").and_then(Value::as_array).context("'options' must be a list")?;
    if !(2..=4).contains(&options.len()) {
        bail!("give 2 to 4 options");
    }
    let mut labels = std::collections::HashSet::new();
    for option in options {
        let label = option["label"].as_str().map(str::trim).unwrap_or_default();
        if label.is_empty() || label.chars().count() > 60 {
            bail!("each option needs a label of 1 to 60 characters");
        }
        if !labels.insert(label.to_lowercase()) {
            bail!("two options have the label '{label}'");
        }
        if option["description"].as_str().is_some_and(|d| d.chars().count() > 200) {
            bail!("option descriptions are limited to 200 characters");
        }
    }
    if args.get("multi_select").is_some_and(|m| !m.is_boolean()) {
        bail!("'multi_select' must be true or false");
    }
    Ok(())
}

/// Small file where the viewer records the project it has open, so an MCP server
/// started by another app (Claude Code, Cursor...) edits the same project.
/// InterCraft's folder for settings and the active project. INTERCRAFT_CONFIG_DIR
/// overrides it, so tests never touch the user's real settings.
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("INTERCRAFT_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(dirs::config_dir()?.join("InterCraft")),
    }
}

fn active_project_file() -> Option<PathBuf> {
    Some(config_dir()?.join("active_project.txt"))
}

/// Called by the viewer when it opens a project.
pub fn record_active_project(dir: &Path) -> Result<()> {
    let file = active_project_file().context("no config folder on this system")?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    std::fs::write(file, dir.to_string_lossy().as_bytes())?;
    Ok(())
}

pub fn active_project() -> Option<PathBuf> {
    let text = std::fs::read_to_string(active_project_file()?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

pub fn definitions() -> Value {
    let part = json!({ "type": "string", "description": "Part name" });
    let feature_id = json!({ "type": "string", "description": "Feature id, e.g. f020" });
    let params = json!({
        "type": "object",
        "description": "Named dimensions for the snippet, read as p[\"name\"]. Lengths in mm.",
    });
    let code = json!({
        "type": "string",
        "description": "build123d snippet that reads and reassigns `part`. Parameters are in `p`.",
    });

    json!([
        {
            "name": "get_project",
            "description": "Read the whole project: every part and feature with its snippet, parameters and current build numbers (volume, size, errors). Call this first.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "add_part",
            "description": "Create a new empty design part. Names may use letters, digits, '_' and '-'.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": part },
                "required": ["name"],
            },
        },
        {
            "name": "add_feature",
            "description": "Add one modelling step to a part and build it. Returns the build report for the part.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "label": { "type": "string", "description": "Short name shown in the hierarchy, e.g. 'Rod bore'" },
                    "code": code,
                    "params": params,
                    "prompt": { "type": "string", "description": "The user's request that led to this feature. Shown on their timeline." },
                    "after": { "type": "string", "description": "Insert after this feature id. Omit to add at the end." },
                },
                "required": ["part", "label", "code"],
            },
        },
        {
            "name": "edit_feature",
            "description": "Change an existing feature and rebuild from it onwards. Only the fields you pass are changed. `params` replaces the whole parameter set.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "feature_id": feature_id,
                    "label": { "type": "string" },
                    "code": code,
                    "params": params,
                    "prompt": { "type": "string" },
                },
                "required": ["part", "feature_id"],
            },
        },
        {
            "name": "set_param",
            "description": "Change one parameter of a feature and rebuild. The cheapest way to change a dimension.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "feature_id": feature_id,
                    "name": { "type": "string" },
                    "value": { "description": "New value. Usually a number in mm." },
                },
                "required": ["part", "feature_id", "name", "value"],
            },
        },
        {
            "name": "delete_feature",
            "description": "Remove a feature and rebuild the features after it.",
            "inputSchema": {
                "type": "object",
                "properties": { "part": part, "feature_id": feature_id },
                "required": ["part", "feature_id"],
            },
        },
        {
            "name": "delete_part",
            "description": "Remove a whole design part and its features. It can be brought back with undo.",
            "inputSchema": {
                "type": "object",
                "properties": { "part": part },
                "required": ["part"],
            },
        },
        {
            "name": "undo",
            "description": "Undo the most recent change to the project, whoever made it (you, the user or another agent). Returns what was undone.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "redo",
            "description": "Redo the change most recently undone. Fails if the project changed since the undo.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "inspect_part",
            "description": "Describe a part's built shape precisely: volume, bounding box, cylindrical faces (holes and bosses) with diameter and axis, circular edge diameters, and the largest faces with type, area, centre and normal. Face and edge indices match measure. Use it before editing an imported part or when you need exact positions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "feature_id": { "type": "string", "description": "Inspect the shape as it was after this step. Omit for the latest." },
                },
                "required": ["part"],
            },
        },
        {
            "name": "measure",
            "description": "Measure exactly with the CAD kernel. With 'a' only: describe one face, edge or vertex (length, radius, diameter, area, normal). With 'a' and 'b': the minimum distance between them, the closest points, the X/Y/Z offset, and the angle when both are straight edges or flat faces. Each selection is {\"kind\": \"face\"|\"edge\"|\"vertex\", \"index\": n, \"part\": optional} using indices from inspect_part, or {\"point\": [x, y, z]}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": { "type": "string", "description": "Default part for selections that don't name one." },
                    "a": { "type": "object", "description": "First selection." },
                    "b": { "type": "object", "description": "Second selection. Omit to describe 'a' alone." },
                },
                "required": ["a"],
            },
        },
        {
            "name": "export_part",
            "description": "Write the finished part to the project's exports folder. STEP is exact and editable, STL is for printing. Omit `format` to write both.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "format": { "type": "string", "enum": ["step", "stl"] },
                },
                "required": ["part"],
            },
        },
    ])
}

impl ToolHost {
    /// The geometry worker starts on the first tool call. Call `warm_up` to start it sooner.
    pub fn new(dir: PathBuf) -> Self {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        Self { target: Target::Fixed(dir), worker: None, ask_user: false }
    }

    /// Also accept `ask_user` calls (InterCraft's chat panel shows them).
    pub fn with_ask_user(mut self) -> Self {
        self.ask_user = true;
        self
    }

    /// Edit whichever project the viewer has open.
    pub fn follow_viewer() -> Self {
        Self { target: Target::FollowViewer, worker: None, ask_user: false }
    }

    /// Start the worker now so the first tool call doesn't wait for OpenCascade to load.
    pub fn warm_up(&mut self) {
        if self.worker.is_none() {
            self.worker = PythonWorker::spawn(|| {}).ok();
        }
    }

    pub fn project_dir(&self) -> Result<PathBuf> {
        match &self.target {
            Target::Fixed(dir) => Ok(dir.clone()),
            Target::FollowViewer => active_project().context(
                "no InterCraft project is open. Ask the user to start the viewer with `cargo run -- <project folder>` and try again",
            ),
        }
    }

    pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        if name == "ask_user" && self.ask_user {
            validate_question(args)?;
            return Ok(json!({
                "shown": true,
                "next": "The question is on the user's screen. End your reply now without more tool calls. Their answer arrives as their next message.",
            }));
        }
        // One tool call at a time per project, across processes, so two agents
        // (say the chat panel and a Claude Code terminal) can't overwrite each other.
        let _lock = self.lock_project()?;
        let mut project = self.load()?;
        match name {
            "get_project" => {
                let mut parts = Vec::new();
                for part in &project.project.parts {
                    parts.push(self.build_report(part, true)?);
                }
                Ok(json!({
                    "name": project.project.name,
                    "units": project.project.units,
                    "folder": project.dir,
                    "parts": parts,
                }))
            }
            "add_part" => {
                let name = text(args, "name")?;
                project.add_part(name)?;
                project.save()?;
                Ok(json!({ "added_part": name }))
            }
            "add_feature" => {
                let part = text(args, "part")?;
                let id = project.add_feature(
                    part,
                    NewFeature {
                        label: text(args, "label")?.to_owned(),
                        code: text(args, "code")?.to_owned(),
                        params: optional_params(args)?.unwrap_or_default(),
                        prompt: optional_text(args, "prompt").unwrap_or_default().to_owned(),
                        after: optional_text(args, "after").map(str::to_owned),
                    },
                )?;
                project.save()?;
                let mut report = self.build_report(project.part(part)?, false)?;
                report["added_feature"] = id.into();
                Ok(report)
            }
            "edit_feature" => {
                let part = text(args, "part")?;
                project.edit_feature(
                    part,
                    text(args, "feature_id")?,
                    FeatureEdit {
                        label: optional_text(args, "label").map(str::to_owned),
                        code: optional_text(args, "code").map(str::to_owned),
                        params: optional_params(args)?,
                        prompt: optional_text(args, "prompt").map(str::to_owned),
                    },
                )?;
                project.save()?;
                self.build_report(project.part(part)?, false)
            }
            "set_param" => {
                let part = text(args, "part")?;
                let value = args.get("value").cloned().context("missing 'value'")?;
                project.set_param(part, text(args, "feature_id")?, text(args, "name")?, value)?;
                project.save()?;
                self.build_report(project.part(part)?, false)
            }
            "delete_feature" => {
                let part = text(args, "part")?;
                project.delete_feature(part, text(args, "feature_id")?)?;
                project.save()?;
                self.build_report(project.part(part)?, false)
            }
            "delete_part" => {
                let name = text(args, "part")?;
                if project.part(name)?.kind == intercraft_core::PartKind::Reference {
                    bail!("'{name}' is a reference part. Only the user can remove it");
                }
                project.remove_part(name)?;
                project.save()?;
                Ok(json!({ "deleted_part": name, "undo": "call undo to bring it back" }))
            }
            "undo" => Ok(json!({ "undone": project.undo()? })),
            "redo" => Ok(json!({ "redone": project.redo()? })),
            "inspect_part" => {
                let part = project.part(text(args, "part")?)?.clone();
                let key = self.built_key(&project.dir, &part, optional_text(args, "feature_id"))?;
                let mut data = self.with_worker(|w| w.inspect_blocking(&key))?;
                data["part"] = part.name.into();
                Ok(data)
            }
            "measure" => {
                let default_part = optional_text(args, "part").map(str::to_owned);
                let dir = project.dir.clone();
                let parts = project.project.parts.clone();
                let mut key_for = |name: &str| -> Result<String> {
                    let part = parts.iter().find(|p| p.name == name).with_context(|| format!("no part named '{name}'"))?;
                    self.built_key(&dir, part, None)
                };
                let a = selection(&mut key_for, default_part.as_deref(), args.get("a").context("missing 'a'")?)?;
                let b = match args.get("b") {
                    Some(b) if !b.is_null() => Some(selection(&mut key_for, default_part.as_deref(), b)?),
                    _ => None,
                };
                self.with_worker(|w| w.measure_blocking(a, b))
            }
            "export_part" => {
                let part = project.part(text(args, "part")?)?;
                let formats = match optional_text(args, "format") {
                    Some("step") => vec![ExportFormat::Step],
                    Some("stl") => vec![ExportFormat::Stl],
                    None => vec![ExportFormat::Step, ExportFormat::Stl],
                    Some(other) => bail!("unknown format '{other}', use 'step' or 'stl'"),
                };
                let mut files = Vec::new();
                for format in formats {
                    let ext = if format == ExportFormat::Step { "step" } else { "stl" };
                    let path = project.dir.join("exports").join(format!("{}.{ext}", part.name));
                    let request = request_for(&project.dir, part);
                    files.push(self.with_worker(|w| w.export_blocking(request, format, &path))?);
                }
                Ok(json!({ "exported": files }))
            }
            other => bail!("unknown tool '{other}'"),
        }
    }

    /// Reads the project fresh on every call, because the user may have edited the files.
    /// Held for the whole call; released when the returned file is dropped.
    fn lock_project(&self) -> Result<std::fs::File> {
        intercraft_core::lock(&self.project_dir()?)
    }

    /// A missing project is created, so a new folder opened in the viewer just works.
    fn load(&self) -> Result<LoadedProject> {
        let dir = self.project_dir()?;
        if dir.join("project.json").exists() {
            return LoadedProject::load(&dir);
        }
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("Untitled");
        LoadedProject::create(&dir, name)
    }

    /// Runs `job` on the worker, starting it if needed. A failed worker is dropped
    /// so the next call starts a clean one.
    fn with_worker<T>(&mut self, job: impl FnOnce(&mut PythonWorker) -> Result<T>) -> Result<T> {
        if self.worker.is_none() {
            self.worker = Some(PythonWorker::spawn(|| {}).context("starting the geometry worker")?);
        }
        let result = job(self.worker.as_mut().expect("worker was just set"));
        if result.is_err() {
            self.worker = None;
        }
        result
    }

    /// Builds a part and describes each feature's outcome in numbers.
    /// Build a part and return the cache key of its final shape (or of `feature_id`).
    fn built_key(&mut self, dir: &Path, part: &Part, feature_id: Option<&str>) -> Result<String> {
        let request = request_for(dir, part);
        let results = self.with_worker(|w| w.build_blocking(request))?;
        let wanted = match feature_id {
            Some(id) => results.iter().find(|r| r.feature_id == id).with_context(|| format!("no step '{id}' in '{}'", part.name))?,
            None => results.last().with_context(|| format!("'{}' has nothing to measure yet", part.name))?,
        };
        if !wanted.ok {
            bail!("step '{}' of '{}' doesn't build, so it can't be inspected", wanted.feature_id, part.name);
        }
        wanted.mesh.as_deref().and_then(intercraft_geo::Mesh::key_of).context("the worker gave no shape")
    }

    fn build_report(&mut self, part: &Part, with_source: bool) -> Result<Value> {
        let dir = self.project_dir()?;
        let request = request_for(&dir, part);
        let results = self.with_worker(|w| w.build_blocking(request))?;
        let by_id: HashMap<&str, &FeatureResult> =
            results.iter().map(|r| (r.feature_id.as_str(), r)).collect();

        let mut all_ok = true;
        let mut features = Vec::new();
        // The imported file, if any, is step 0.
        let import_step = part.source.as_ref().map(|source| (intercraft_geo::IMPORT_STEP_ID.to_owned(), source.clone()));
        let steps = import_step
            .iter()
            .map(|(id, source)| (id.as_str(), format!("Imported {source}"), None))
            .chain(part.features.iter().map(|f| (f.id.as_str(), f.label.clone(), Some(f))));
        for (id, label, feature) in steps {
            let mut entry = json!({ "id": id, "label": label });
            if let (true, Some(feature)) = (with_source, feature) {
                entry["params"] = Value::Object(feature.params.clone());
                entry["code"] = feature.code.clone().into();
                entry["prompt"] = feature.prompt.clone().into();
            }
            match by_id.get(id) {
                Some(result) if result.ok => {
                    entry["status"] = "built".into();
                    if let Some(r) = &result.report {
                        entry["volume_mm3"] = round(r.volume);
                        if let Some(before) = r.volume_before {
                            entry["volume_change_mm3"] = round(r.volume - before);
                        }
                        entry["size_mm"] = json!([
                            round(r.bbox_max[0] - r.bbox_min[0]),
                            round(r.bbox_max[1] - r.bbox_min[1]),
                            round(r.bbox_max[2] - r.bbox_min[2]),
                        ]);
                        entry["bbox_min"] = json!(r.bbox_min.map(round));
                        entry["bbox_max"] = json!(r.bbox_max.map(round));
                        entry["faces"] = r.faces.into();
                        entry["edges"] = r.edges.into();
                        entry["valid_solid"] = r.valid.into();
                        all_ok &= r.valid;
                    }
                }
                Some(result) => {
                    all_ok = false;
                    entry["status"] = "failed".into();
                    entry["error"] = result.error.clone().unwrap_or_default().into();
                }
                None => {
                    all_ok = false;
                    entry["status"] = "not built (an earlier feature failed)".into();
                }
            }
            features.push(entry);
        }

        let mut report = json!({ "part": part.name, "kind": part.kind, "ok": all_ok, "features": features });
        if let Some(source) = &part.source {
            report["imported_from"] = source.clone().into();
        }
        if !all_ok {
            report["note"] =
                "The change is saved but the part does not build cleanly. Fix it with edit_feature or delete_feature.".into();
        }
        Ok(report)
    }
}

/// Everything the worker needs to build a part: the imported file first, if any,
/// then each feature. Shared by the viewer and the tools so both build the same thing.
pub fn request_for(dir: &Path, part: &Part) -> RebuildRequest {
    let mut features = Vec::new();
    if let Some(source) = &part.source {
        features.push(FeatureInput::import(dir.join(source)));
    }
    features.extend(part.features.iter().map(|f| FeatureInput::snippet(&f.id, &f.code, &f.params)));
    RebuildRequest { part: part.name.clone(), features }
}

/// One selection for `measure`, as the model writes it.
fn selection(dir_part: &mut dyn FnMut(&str) -> Result<String>, default_part: Option<&str>, value: &Value) -> Result<Value> {
    if let Some(point) = value.get("point") {
        let coords: Vec<f64> = point.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        if coords.len() != 3 {
            bail!("'point' must be [x, y, z] in mm");
        }
        return Ok(json!({ "kind": "point", "point": coords }));
    }
    let kind = value.get("kind").and_then(Value::as_str).context("give 'kind' (face, edge or vertex) and 'index', or a 'point'")?;
    if !matches!(kind, "face" | "edge" | "vertex") {
        bail!("'kind' must be face, edge or vertex");
    }
    let index = value.get("index").and_then(Value::as_u64).context("'index' must be a whole number")?;
    let part = value.get("part").and_then(Value::as_str).or(default_part).context("say which 'part'")?;
    Ok(json!({ "mesh": dir_part(part)?, "kind": kind, "index": index }))
}

/// Three decimals is plenty for mm and keeps the reports short.
fn round(value: f64) -> Value {
    json!((value * 1000.0).round() / 1000.0)
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    optional_text(args, key).with_context(|| format!("missing '{key}'"))
}

fn optional_text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)?.as_str()
}

fn optional_params(args: &Value) -> Result<Option<Params>> {
    match args.get("params") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map.clone())),
        Some(_) => bail!("'params' must be an object"),
    }
}
