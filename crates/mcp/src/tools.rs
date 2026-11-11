// The tools an LLM can call, and what each one does to the project.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use intercraft_core::{FeatureEdit, LoadedProject, NewFeature, Params, Part};
use intercraft_geo::{ExportFormat, FeatureInput, FeatureResult, PythonWorker, RebuildRequest};
use serde_json::{json, Value};

pub struct Server {
    dir: PathBuf,
    worker: Option<PythonWorker>,
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

impl Server {
    pub fn new(dir: PathBuf) -> Self {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        // Start the worker now so it is warm by the first tool call.
        Self { dir, worker: PythonWorker::spawn(|| {}).ok() }
    }

    pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
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
                    "folder": self.dir,
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
                    let path = self.dir.join("exports").join(format!("{}.{ext}", part.name));
                    let request = request_for(part);
                    files.push(self.with_worker(|w| w.export_blocking(request, format, &path))?);
                }
                Ok(json!({ "exported": files }))
            }
            other => bail!("unknown tool '{other}'"),
        }
    }

    /// Reads the project fresh on every call, because the user may have edited the files.
    fn load(&self) -> Result<LoadedProject> {
        if self.dir.join("project.json").exists() {
            return LoadedProject::load(&self.dir);
        }
        let name = self.dir.file_name().and_then(|n| n.to_str()).unwrap_or("Untitled");
        LoadedProject::create(&self.dir, name)
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
    fn build_report(&mut self, part: &Part, with_source: bool) -> Result<Value> {
        let request = request_for(part);
        let results = self.with_worker(|w| w.build_blocking(request))?;
        let by_id: HashMap<&str, &FeatureResult> =
            results.iter().map(|r| (r.feature_id.as_str(), r)).collect();

        let mut all_ok = true;
        let mut features = Vec::new();
        for feature in &part.features {
            let mut entry = json!({ "id": feature.id, "label": feature.label });
            if with_source {
                entry["params"] = Value::Object(feature.params.clone());
                entry["code"] = feature.code.clone().into();
                entry["prompt"] = feature.prompt.clone().into();
            }
            match by_id.get(feature.id.as_str()) {
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
        if !all_ok {
            report["note"] =
                "The change is saved but the part does not build cleanly. Fix it with edit_feature or delete_feature.".into();
        }
        Ok(report)
    }
}

fn request_for(part: &Part) -> RebuildRequest {
    RebuildRequest {
        part: part.name.clone(),
        features: part
            .features
            .iter()
            .map(|f| FeatureInput { id: f.id.clone(), code: f.code.clone(), params: f.params.clone() })
            .collect(),
    }
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
