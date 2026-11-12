// intercraft-core: the project model (parts, features), loading, editing and saving.
// serde 1, serde_json 1, anyhow 1

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

pub type Params = serde_json::Map<String, serde_json::Value>;

/// Limits on what a feature can hold, so a runaway model can't bloat the project.
pub const MAX_CODE_CHARS: usize = 20_000;
pub const MAX_LABEL_CHARS: usize = 80;
pub const MAX_PROMPT_CHARS: usize = 4_000;
pub const MAX_FEATURES_PER_PART: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default = "default_units")]
    pub units: String,
    pub parts: Vec<Part>,
}

fn default_units() -> String {
    "mm".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PartKind {
    /// A part the LLM is allowed to change.
    #[default]
    Design,
    /// Imported geometry we design around. Never modified.
    Reference,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Part {
    pub name: String,
    #[serde(default)]
    pub kind: PartKind,
    /// An imported STEP file the part starts from, e.g. "imports/bracket.step".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub features: Vec<Feature>,
}

/// Largest STEP file accepted for import.
pub const MAX_IMPORT_BYTES: u64 = 200 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub label: String,
    /// The prompt that produced this feature, shown on the timeline.
    #[serde(default)]
    pub prompt: String,
    /// Snippet path relative to the project folder.
    pub file: String,
    #[serde(default)]
    pub params: Params,
    /// Snippet source. Read from `file` on load, not stored in project.json.
    #[serde(skip)]
    pub code: String,
}

/// Everything needed to add a feature.
#[derive(Debug, Clone, Default)]
pub struct NewFeature {
    pub label: String,
    pub code: String,
    pub params: Params,
    pub prompt: String,
    /// Insert after this feature id. None appends to the end.
    pub after: Option<String>,
}

/// Fields to change on an existing feature. None leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct FeatureEdit {
    pub label: Option<String>,
    pub code: Option<String>,
    pub params: Option<Params>,
    pub prompt: Option<String>,
}

/// A project plus where it lives, so it can be reloaded when files change.
#[derive(Debug, Clone)]
pub struct LoadedProject {
    pub dir: PathBuf,
    pub project: Project,
    /// Snippet files to delete on the next save.
    removed: Vec<PathBuf>,
}

impl LoadedProject {
    pub fn load(dir: &Path) -> Result<Self> {
        let manifest = dir.join("project.json");
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let mut project: Project =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;

        // Project files can come from anywhere, so nothing in them may reach outside the folder.
        let mut part_names = HashSet::new();
        for part in &mut project.parts {
            check_part_name(&part.name)?;
            if let Some(source) = &part.source {
                check_import_path(source)?;
            }
            if !part_names.insert(part.name.to_ascii_lowercase()) {
                bail!("project.json lists the part '{}' twice", part.name);
            }
            let mut ids = HashSet::new();
            for feature in &mut part.features {
                if !ids.insert(feature.id.clone()) {
                    bail!("part '{}' has two features with the id '{}'", part.name, feature.id);
                }
                check_snippet_path(&feature.file)?;
                let path = dir.join(&feature.file);
                feature.code = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading snippet {}", path.display()))?;
            }
        }
        Ok(Self { dir: dir.to_path_buf(), project, removed: Vec::new() })
    }

    /// Start an empty project in `dir` and write it to disk.
    pub fn create(dir: &Path, name: &str) -> Result<Self> {
        let project = Project { name: name.to_owned(), units: default_units(), parts: Vec::new() };
        let loaded = Self { dir: dir.to_path_buf(), project, removed: Vec::new() };
        loaded.write_files()?;
        Ok(loaded)
    }

    /// Write changed snippets first, then project.json, then remove deleted snippets.
    /// That order means a reader never sees a manifest pointing at a missing file.
    pub fn save(&mut self) -> Result<()> {
        self.write_files()?;
        for path in self.removed.drain(..) {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    fn write_files(&self) -> Result<()> {
        for part in &self.project.parts {
            for feature in &part.features {
                write_if_changed(&self.dir.join(&feature.file), &feature.code)?;
            }
        }
        let manifest = serde_json::to_string_pretty(&self.project)? + "\n";
        write_if_changed(&self.dir.join("project.json"), &manifest)
    }

    pub fn part(&self, name: &str) -> Result<&Part> {
        self.project
            .parts
            .iter()
            .find(|p| p.name == name)
            .with_context(|| format!("no part named '{name}'"))
    }

    fn design_part_mut(&mut self, name: &str) -> Result<&mut Part> {
        let part = self
            .project
            .parts
            .iter_mut()
            .find(|p| p.name == name)
            .with_context(|| format!("no part named '{name}'"))?;
        if part.kind == PartKind::Reference {
            bail!("'{name}' is a reference part and can't be changed");
        }
        Ok(part)
    }

    pub fn add_part(&mut self, name: &str) -> Result<()> {
        check_part_name(name)?;
        if self.project.parts.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
            bail!("a part named '{name}' already exists");
        }
        self.project.parts.push(Part {
            name: name.to_owned(),
            kind: PartKind::Design,
            source: None,
            features: Vec::new(),
        });
        Ok(())
    }

    /// Copy a STEP file into the project's imports folder and add a part that starts
    /// from it. Returns the new part's name. Call `save` afterwards.
    pub fn import_step(&mut self, file: &Path) -> Result<String> {
        let ext = file.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        if ext != "step" && ext != "stp" {
            bail!("only .step and .stp files can be imported");
        }
        let size = std::fs::metadata(file).with_context(|| format!("reading {}", file.display()))?.len();
        if size > MAX_IMPORT_BYTES {
            bail!("that file is {} MB; the limit is {} MB", size >> 20, MAX_IMPORT_BYTES >> 20);
        }

        // A part name from the file name, made safe and unique.
        let stem = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let mut base: String = stem
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect::<String>()
            .trim_matches('_')
            .chars()
            .take(48)
            .collect();
        if base.is_empty() || check_part_name(&base).is_err() {
            base = "Imported".to_owned();
        }
        let taken = |name: &str| self.project.parts.iter().any(|p| p.name.eq_ignore_ascii_case(name));
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) || self.dir.join("imports").join(format!("{name}.{ext}")).exists() {
            name = format!("{base}_{n}");
            n += 1;
        }

        let relative = format!("imports/{name}.{ext}");
        let target = self.dir.join(&relative);
        std::fs::create_dir_all(self.dir.join("imports"))?;
        std::fs::copy(file, &target).with_context(|| format!("copying into {}", target.display()))?;
        self.project.parts.push(Part {
            name: name.clone(),
            kind: PartKind::Design,
            source: Some(relative),
            features: Vec::new(),
        });
        Ok(name)
    }

    /// Lock a part (reference) or unlock it (design).
    pub fn set_kind(&mut self, part_name: &str, kind: PartKind) -> Result<()> {
        let part = self
            .project
            .parts
            .iter_mut()
            .find(|p| p.name == part_name)
            .with_context(|| format!("no part named '{part_name}'"))?;
        part.kind = kind;
        Ok(())
    }

    /// Returns the new feature's id.
    pub fn add_feature(&mut self, part_name: &str, new: NewFeature) -> Result<String> {
        check_feature_text(Some(&new.label), Some(&new.code), Some(&new.prompt))?;
        let part = self.design_part_mut(part_name)?;
        if part.features.len() >= MAX_FEATURES_PER_PART {
            bail!("'{part_name}' already has {MAX_FEATURES_PER_PART} features, the most a part can hold");
        }
        let index = match &new.after {
            Some(after) => {
                1 + part
                    .features
                    .iter()
                    .position(|f| &f.id == after)
                    .with_context(|| format!("no feature '{after}' in '{part_name}'"))?
            }
            None => part.features.len(),
        };

        // Ids count up in tens, like the file names, and are never reused within a session.
        let highest = part
            .features
            .iter()
            .filter_map(|f| f.id.trim_start_matches('f').parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        let number = (highest / 10 + 1) * 10;
        let id = format!("f{number:03}");
        let file = format!("{part_name}/{number:03}_{}.py", slug(&new.label));

        part.features.insert(
            index,
            Feature {
                id: id.clone(),
                label: new.label,
                prompt: new.prompt,
                file,
                params: new.params,
                code: ensure_trailing_newline(new.code),
            },
        );
        Ok(id)
    }

    pub fn edit_feature(&mut self, part_name: &str, id: &str, edit: FeatureEdit) -> Result<()> {
        check_feature_text(edit.label.as_deref(), edit.code.as_deref(), edit.prompt.as_deref())?;
        let feature = self.feature_mut(part_name, id)?;
        if let Some(label) = edit.label {
            feature.label = label;
        }
        if let Some(code) = edit.code {
            feature.code = ensure_trailing_newline(code);
        }
        if let Some(params) = edit.params {
            feature.params = params;
        }
        if let Some(prompt) = edit.prompt {
            feature.prompt = prompt;
        }
        Ok(())
    }

    pub fn set_param(
        &mut self,
        part_name: &str,
        id: &str,
        name: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        self.feature_mut(part_name, id)?.params.insert(name.to_owned(), value);
        Ok(())
    }

    pub fn delete_feature(&mut self, part_name: &str, id: &str) -> Result<()> {
        let dir = self.dir.clone();
        let part = self.design_part_mut(part_name)?;
        let index = part
            .features
            .iter()
            .position(|f| f.id == id)
            .with_context(|| format!("no feature '{id}' in '{part_name}'"))?;
        let feature = part.features.remove(index);
        self.removed.push(dir.join(feature.file));
        Ok(())
    }

    fn feature_mut(&mut self, part_name: &str, id: &str) -> Result<&mut Feature> {
        self.design_part_mut(part_name)?
            .features
            .iter_mut()
            .find(|f| f.id == id)
            .with_context(|| format!("no feature '{id}' in '{part_name}'"))
    }

    /// Every file that should trigger a rebuild when it changes.
    pub fn watched_files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.dir.join("project.json")];
        for part in &self.project.parts {
            if let Some(source) = &part.source {
                files.push(self.dir.join(source));
            }
            for feature in &part.features {
                files.push(self.dir.join(&feature.file));
            }
        }
        files
    }

    /// Newest modification time across the watched files.
    pub fn latest_mtime(&self) -> Option<SystemTime> {
        self.watched_files()
            .iter()
            .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
            .max()
    }
}

/// Part names become folder names, so keep them plain.
fn check_part_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("part names may only use letters, digits, '_' and '-' (up to 64 characters), not '{name}'");
    }
    // Windows treats these as devices, not folders, whatever the case.
    let upper = name.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if device {
        bail!("'{name}' is reserved by Windows and can't be used as a part name");
    }
    Ok(())
}

/// A snippet path must be a plain relative path inside the project, like "Part/010_base.py".
fn check_snippet_path(file: &str) -> Result<()> {
    let path = Path::new(file);
    let plain = path.components().all(|c| matches!(c, Component::Normal(_)));
    if file.is_empty() || !plain || path.extension().is_none_or(|e| e != "py") {
        bail!("snippet path '{file}' must be a .py file inside the project folder");
    }
    Ok(())
}

/// An imported file must sit directly in the project's imports folder.
fn check_import_path(source: &str) -> Result<()> {
    let path = Path::new(source);
    let parts: Vec<_> = path.components().collect();
    let in_imports = parts.len() == 2
        && matches!(parts[0], Component::Normal(first) if first == "imports")
        && matches!(parts[1], Component::Normal(_));
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
    if !in_imports || !matches!(ext.as_deref(), Some("step") | Some("stp")) {
        bail!("imported file '{source}' must be a .step or .stp file in the project's imports folder");
    }
    Ok(())
}

fn check_feature_text(label: Option<&str>, code: Option<&str>, prompt: Option<&str>) -> Result<()> {
    if let Some(label) = label {
        if label.trim().is_empty() || label.chars().count() > MAX_LABEL_CHARS {
            bail!("a feature label must be 1 to {MAX_LABEL_CHARS} characters");
        }
    }
    if code.is_some_and(|c| c.chars().count() > MAX_CODE_CHARS) {
        bail!("snippets are limited to {MAX_CODE_CHARS} characters; split the work into smaller features");
    }
    if prompt.is_some_and(|p| p.chars().count() > MAX_PROMPT_CHARS) {
        bail!("the prompt is limited to {MAX_PROMPT_CHARS} characters");
    }
    Ok(())
}

fn ensure_trailing_newline(mut code: String) -> String {
    if !code.ends_with('\n') {
        code.push('\n');
    }
    code
}

/// "Rod bore (8 mm)" -> "rod_bore_8_mm"
fn slug(label: &str) -> String {
    let mut out = String::new();
    for c in label.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
        if out.len() >= 24 {
            break;
        }
    }
    let out = out.trim_end_matches('_');
    if out.is_empty() { "feature".to_owned() } else { out.to_owned() }
}

/// Skips the write when nothing changed, so file watchers don't fire for no reason.
fn write_if_changed(path: &Path, content: &str) -> Result<()> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == content) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    // Write next to the target and rename, so readers never see half a file. The temp
    // name is unique so two writers (chat panel and an MCP client) never share one.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file_name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{file_name}.{}-{n}.tmp", std::process::id()));
    std::fs::write(&tmp, content).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project(name: &str) -> LoadedProject {
        let dir = std::env::temp_dir().join(format!("intercraft-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        LoadedProject::create(&dir, "Test").unwrap()
    }

    fn feature(label: &str, after: Option<&str>) -> NewFeature {
        NewFeature {
            label: label.into(),
            code: "part = Box(1, 1, 1)".into(),
            after: after.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn add_edit_delete_round_trip() {
        let mut p = temp_project("roundtrip");
        p.add_part("Bracket").unwrap();
        let a = p.add_feature("Bracket", feature("Base block", None)).unwrap();
        let b = p.add_feature("Bracket", feature("Rod bore", None)).unwrap();
        // Inserted between the two, but the id still counts on from the highest.
        let c = p.add_feature("Bracket", feature("Chamfer", Some(&a))).unwrap();
        assert_eq!((a.as_str(), b.as_str(), c.as_str()), ("f010", "f020", "f030"));

        p.set_param("Bracket", &b, "bore_d", 8.2.into()).unwrap();
        p.save().unwrap();

        let loaded = LoadedProject::load(&p.dir).unwrap();
        let ids: Vec<_> = loaded.project.parts[0].features.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["f010", "f030", "f020"]);
        assert_eq!(loaded.project.parts[0].features[2].params["bore_d"], 8.2);
        assert_eq!(loaded.project.parts[0].features[0].code, "part = Box(1, 1, 1)\n");

        let file = p.dir.join(&p.project.parts[0].features[1].file);
        p.delete_feature("Bracket", &c).unwrap();
        p.save().unwrap();
        assert!(!file.exists());
        assert_eq!(LoadedProject::load(&p.dir).unwrap().project.parts[0].features.len(), 2);

        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn rejects_bad_names_and_reference_edits() {
        let mut p = temp_project("guards");
        assert!(p.add_part("../escape").is_err());
        p.add_part("Rail").unwrap();
        assert!(p.add_part("rail").is_err());

        p.project.parts[0].kind = PartKind::Reference;
        assert!(p.add_feature("Rail", feature("Cut", None)).is_err());

        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn load_refuses_paths_outside_the_project() {
        let p = temp_project("traversal");
        let bad_files = ["../outside.py", "C:/Windows/evil.py", "/etc/passwd", "Part/../../x.py", "Part/notes.txt", ""];
        for file in bad_files {
            let json = format!(
                r#"{{"name":"T","parts":[{{"name":"Part","features":[{{"id":"f010","label":"x","file":"{file}"}}]}}]}}"#
            );
            std::fs::write(p.dir.join("project.json"), json).unwrap();
            assert!(LoadedProject::load(&p.dir).is_err(), "accepted {file}");
        }
        let bad_parts = ["..", "a/b", "C:", ""];
        for name in bad_parts {
            let json = format!(r#"{{"name":"T","parts":[{{"name":"{name}","features":[]}}]}}"#);
            std::fs::write(p.dir.join("project.json"), json).unwrap();
            assert!(LoadedProject::load(&p.dir).is_err(), "accepted part {name}");
        }
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn rejects_oversized_features() {
        let mut p = temp_project("limits");
        p.add_part("Bracket").unwrap();
        let mut big = feature("Block", None);
        big.code = "x = 1\n".repeat(5000);
        assert!(p.add_feature("Bracket", big).is_err());
        assert!(p.add_feature("Bracket", feature(&"L".repeat(200), None)).is_err());
        assert!(p.add_feature("Bracket", feature("   ", None)).is_err());
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn import_copies_the_file_and_names_the_part() {
        let mut p = temp_project("import");
        let src = p.dir.join("Corner Bracket v2 (final).STEP");
        std::fs::write(&src, "ISO-10303-21;").unwrap();
        let first = p.import_step(&src).unwrap();
        let second = p.import_step(&src).unwrap();
        assert_eq!(first, "Corner_Bracket_v2__final");
        assert_eq!(second, "Corner_Bracket_v2__final_2");
        p.save().unwrap();
        let loaded = LoadedProject::load(&p.dir).unwrap();
        let source = loaded.project.parts[0].source.clone().unwrap();
        assert_eq!(source, "imports/Corner_Bracket_v2__final.step");
        assert!(p.dir.join(&source).exists());
        assert!(p.import_step(&p.dir.join("notes.txt")).is_err());
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn load_refuses_imports_outside_the_imports_folder() {
        let p = temp_project("import-paths");
        for source in ["../x.step", "C:/x.step", "imports/../../x.step", "imports/x.txt", "x.step", "imports/a/b.step"] {
            let json = format!(r#"{{"name":"T","parts":[{{"name":"P","source":"{source}","features":[]}}]}}"#);
            std::fs::write(p.dir.join("project.json"), json).unwrap();
            assert!(LoadedProject::load(&p.dir).is_err(), "accepted {source}");
        }
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn slugs_are_file_safe() {
        assert_eq!(slug("Rod bore (8 mm)"), "rod_bore_8_mm");
        assert_eq!(slug("///"), "feature");
    }
}
