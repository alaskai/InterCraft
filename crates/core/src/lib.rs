// intercraft-core: the project model (parts, features), loading, editing and saving.
// serde 1, serde_json 1, anyhow 1

mod history;
mod joints;

pub use history::{HistoryEntry, HistoryInfo};
pub use joints::{Joint, JointEdit, JointKind, Pose, MAX_JOINTS};

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
pub const MAX_WHY_CHARS: usize = 600;
/// The design brief is notes, not a document. A brief this long has stopped being useful.
pub const MAX_BRIEF_CHARS: usize = 20_000;

pub const BRIEF_FILE: &str = "brief.md";

/// What a new project's brief starts as. The model fills it in with the user.
pub const BRIEF_TEMPLATE: &str = "# Brief

## What it is

## How it will be made

## Key dimensions

| Feature | Value | Source |
|---|---|---|

## Decisions

## Open items
";
pub const MAX_FEATURES_PER_PART: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    #[serde(default = "default_units")]
    pub units: String,
    pub parts: Vec<Part>,
    /// How parts move against each other: hinges and sliders.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joints: Vec<Joint>,
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
    /// The imported assembly this part came out of, shown as a folder in the parts list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    /// Where the part sits in the assembly, in mm. Features are written in the
    /// part's own frame and never see this.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub position: [f64; 3],
    /// Rotation about X, Y and Z in degrees, applied before the move.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rotation: [f64; 3],
    #[serde(default)]
    pub features: Vec<Feature>,
}

fn is_zero(v: &[f64; 3]) -> bool {
    v.iter().all(|x| *x == 0.0)
}

impl Part {
    fn new(name: &str, source: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            kind: PartKind::Design,
            source,
            group: None,
            position: [0.0; 3],
            rotation: [0.0; 3],
            features: Vec::new(),
        }
    }

    /// Position and rotation as one list, or None when the part hasn't been moved.
    pub fn placement(&self) -> Option<[f64; 6]> {
        let [x, y, z] = self.position;
        let [rx, ry, rz] = self.rotation;
        (!is_zero(&self.position) || !is_zero(&self.rotation)).then_some([x, y, z, rx, ry, rz])
    }
}

/// Largest STEP file accepted for import.
pub const MAX_IMPORT_BYTES: u64 = 200 * 1024 * 1024;

/// Most bodies one STEP file can be split into. Each one becomes a part to build.
pub const MAX_SPLIT_BODIES: usize = 500;

/// Where each body of a split STEP import goes. Made by `plan_split_import`; the
/// worker writes the files, then `add_split_import` adds the parts.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitImport {
    pub group: String,
    /// The folder inside imports/ that the body files go in.
    pub folder: PathBuf,
    /// Part name and file (relative to the project) for each body, in the file's order.
    pub parts: Vec<(String, String)>,
    /// Each part's group path: the import's group, then any sub-assemblies it was in.
    pub groups: Vec<String>,
}

/// How deep groups can nest, counting the outermost.
pub const MAX_GROUP_DEPTH: usize = 6;

/// Whether a part's group is `group` itself or somewhere inside it.
pub fn in_group(part_group: Option<&str>, group: &str) -> bool {
    part_group.is_some_and(|g| g == group || (g.starts_with(group) && g[group.len()..].starts_with('/')))
}

/// One level of a group path made from any text, such as a sub-assembly's name.
fn group_segment(text: &str) -> String {
    let clean: String = text.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { ' ' } else { c }).collect();
    clean.trim().chars().take(64).collect::<String>().trim().to_owned()
}

/// A typed group path with stray spaces and empty levels removed.
fn clean_group(path: &str) -> String {
    path.split('/').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Feature {
    pub id: String,
    pub label: String,
    /// The prompt that produced this feature, shown on the timeline.
    #[serde(default)]
    pub prompt: String,
    /// The design reason behind the feature, in one plain sentence, for someone new
    /// to CAD. Written by the model, shown on the timeline and in the Inspector.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// Snippet path relative to the project folder.
    pub file: String,
    #[serde(default)]
    pub params: Params,
    /// Plain names for the parameters, e.g. "bore_d" -> "Bore diameter", shown in
    /// place of the code names in the simple layout.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub labels: std::collections::BTreeMap<String, String>,
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
    pub why: String,
    pub labels: std::collections::BTreeMap<String, String>,
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
    pub why: Option<String>,
    /// Replaces all the parameter labels.
    pub labels: Option<std::collections::BTreeMap<String, String>>,
}

/// A project plus where it lives, so it can be reloaded when files change.
#[derive(Debug, Clone)]
pub struct LoadedProject {
    pub dir: PathBuf,
    pub project: Project,
    /// Snippet files to delete on the next save.
    removed: Vec<PathBuf>,
    /// The project as last loaded or saved (JSON), to tell what a save changed.
    baseline: String,
    /// The design brief (`brief.md`): what the project is for, how it will be made,
    /// key dimensions, decisions and open items. Empty when there isn't one.
    pub brief: String,
}

impl LoadedProject {
    pub fn load(dir: &Path) -> Result<Self> {
        let manifest = dir.join("project.json");
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let mut project: Project =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;

        check_project(&project)?;
        for feature in project.parts.iter_mut().flat_map(|p| p.features.iter_mut()) {
            let path = dir.join(&feature.file);
            feature.code =
                std::fs::read_to_string(&path).with_context(|| format!("reading snippet {}", path.display()))?;
        }
        let brief = read_brief(dir)?;
        Ok(Self::from_project(dir, project, brief))
    }

    fn from_project(dir: &Path, project: Project, brief: String) -> Self {
        let mut loaded = Self { dir: dir.to_path_buf(), project, removed: Vec::new(), baseline: String::new(), brief };
        loaded.baseline = loaded.content_key();
        loaded
    }

    /// Start an empty project in `dir`, with a blank brief, and write it to disk.
    pub fn create(dir: &Path, name: &str) -> Result<Self> {
        let project = Project { name: name.to_owned(), units: default_units(), parts: Vec::new(), joints: Vec::new() };
        let loaded = Self::from_project(dir, project, BRIEF_TEMPLATE.to_owned());
        loaded.write_files()?;
        Ok(loaded)
    }

    /// Replace the text under one `## heading` of the brief, or add the section at the
    /// end if it isn't there. The heading match ignores case. Call `save` afterwards.
    pub fn update_brief_section(&mut self, heading: &str, body: &str) -> Result<()> {
        let heading = heading.trim().trim_start_matches('#').trim();
        if heading.is_empty() || heading.chars().count() > 80 || heading.contains('\n') {
            bail!("a brief section heading must be one line of 1 to 80 characters");
        }
        let body = body.trim();
        if body.lines().any(|line| line.starts_with("## ")) {
            bail!("the section text can't contain its own '## ' headings; update one section at a time");
        }
        let mut brief = if self.brief.trim().is_empty() { BRIEF_TEMPLATE.to_owned() } else { self.brief.clone() };
        let lines: Vec<&str> = brief.lines().collect();
        let is_heading = |line: &str| line.strip_prefix("## ").is_some_and(|h| h.trim().eq_ignore_ascii_case(heading));
        let section = |heading: &str| if body.is_empty() { format!("## {heading}\n") } else { format!("## {heading}\n\n{body}\n") };
        brief = match lines.iter().position(|l| is_heading(l)) {
            Some(start) => {
                // Keep the heading as it was written, whatever case the caller used.
                let existing = lines[start].trim_start_matches("## ").trim();
                let end = lines[start + 1..].iter().position(|l| l.starts_with("## ")).map_or(lines.len(), |i| start + 1 + i);
                let before = lines[..start].join("\n");
                let after = lines[end..].join("\n");
                let mut text = String::new();
                if !before.is_empty() {
                    text += &before;
                    text += "\n";
                }
                text += &section(existing);
                if !after.is_empty() {
                    text += "\n";
                    text += &after;
                    text += "\n";
                }
                text
            }
            None => format!("{}\n\n{}", brief.trim_end(), section(heading)),
        };
        self.set_brief(brief)
    }

    /// Replace the whole brief. Call `save` afterwards.
    pub fn set_brief(&mut self, text: String) -> Result<()> {
        if text.chars().count() > MAX_BRIEF_CHARS {
            bail!("the brief is limited to {MAX_BRIEF_CHARS} characters; keep it to notes");
        }
        self.brief = text;
        Ok(())
    }

    /// Write changed snippets first, then project.json, then remove deleted snippets.
    /// That order means a reader never sees a manifest pointing at a missing file.
    /// Each save is also recorded in the undo history.
    pub fn save(&mut self) -> Result<()> {
        self.write_files()?;
        self.remove_deleted();
        // The change itself is saved by now; a history that can't be written
        // (a full disk, say) shouldn't turn it into an error.
        let _ = self.record();
        Ok(())
    }

    fn remove_deleted(&mut self) {
        for path in self.removed.drain(..) {
            let _ = std::fs::remove_file(&path);
            // Drops the part's folder once its last snippet is gone. Fails harmlessly otherwise.
            if let Some(parent) = path.parent().filter(|p| *p != self.dir) {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }

    fn write_files(&self) -> Result<()> {
        for part in &self.project.parts {
            for feature in &part.features {
                write_if_changed(&self.dir.join(&feature.file), &feature.code)?;
            }
        }
        // No brief and no file: nothing to write. A brief that was cleared is kept as an empty file.
        let brief_path = self.dir.join(BRIEF_FILE);
        if !self.brief.is_empty() || brief_path.exists() {
            write_if_changed(&brief_path, &self.brief)?;
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
        self.project.parts.push(Part::new(name, None));
        Ok(())
    }

    /// Give a part a new name. Its snippets move to a folder with the new name.
    pub fn rename_part(&mut self, old: &str, new: &str) -> Result<()> {
        check_part_name(new)?;
        if self.project.parts.iter().any(|p| p.name != old && p.name.eq_ignore_ascii_case(new)) {
            bail!("a part named '{new}' already exists");
        }
        let dir = self.dir.clone();
        let part = self.part_mut(old)?;
        part.name = new.to_owned();
        let prefix = format!("{old}/");
        let mut left_behind = Vec::new();
        for feature in &mut part.features {
            if let Some(rest) = feature.file.strip_prefix(&prefix) {
                let moved = format!("{new}/{rest}");
                let before = std::mem::replace(&mut feature.file, moved);
                left_behind.push(dir.join(before));
            }
        }
        self.removed.extend(left_behind);
        for joint in &mut self.project.joints {
            for name in [&mut joint.base, &mut joint.part] {
                if name.as_str() == old {
                    *name = new.to_owned();
                }
            }
        }
        Ok(())
    }

    /// Copy a part, features and all. Returns the copy's name. The copy is always a
    /// design part, so a locked reference can be copied and then changed.
    pub fn duplicate_part(&mut self, name: &str, new_name: Option<&str>) -> Result<String> {
        let original = self.part(name)?.clone();
        let taken = |n: &str| self.project.parts.iter().any(|p| p.name.eq_ignore_ascii_case(n));
        let new = match new_name {
            Some(n) => {
                check_part_name(n)?;
                if taken(n) {
                    bail!("a part named '{n}' already exists");
                }
                n.to_owned()
            }
            None => {
                let base: String = original.name.chars().take(56).collect();
                let mut n = format!("{base}_copy");
                let mut i = 2;
                while taken(&n) {
                    n = format!("{base}_copy{i}");
                    i += 1;
                }
                n
            }
        };
        let mut copy = Part { name: new.clone(), kind: PartKind::Design, ..original };
        let prefix = format!("{name}/");
        for feature in &mut copy.features {
            let rest = feature.file.strip_prefix(&prefix).unwrap_or(&feature.file);
            // Flatten any odd path from a hand-written project into the copy's own folder.
            let file_name = rest.rsplit('/').next().unwrap_or(rest);
            feature.file = format!("{new}/{file_name}");
        }
        let at = self.project.parts.iter().position(|p| p.name == name).map_or(self.project.parts.len(), |i| i + 1);
        self.project.parts.insert(at, copy);
        Ok(new)
    }

    /// Move or turn a part in the assembly. Rotation is in degrees about X, Y and Z.
    pub fn set_placement(&mut self, name: &str, position: [f64; 3], rotation: [f64; 3]) -> Result<()> {
        if position.iter().chain(&rotation).any(|v| !v.is_finite() || v.abs() > 1e6) {
            bail!("positions and rotations must be ordinary numbers");
        }
        let part = self.part_mut(name)?;
        part.position = position;
        part.rotation = rotation;
        Ok(())
    }

    fn part_mut(&mut self, name: &str) -> Result<&mut Part> {
        self.project.parts.iter_mut().find(|p| p.name == name).with_context(|| format!("no part named '{name}'"))
    }

    /// Copy a STEP file into the project's imports folder and add a part that starts
    /// from it. Returns the new part's name. Call `save` afterwards.
    pub fn import_step(&mut self, file: &Path) -> Result<String> {
        let ext = check_step_file(file)?;
        let base = plain_name(&file.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default(), "Imported");
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
        self.project.parts.push(Part::new(&name, Some(relative)));
        Ok(name)
    }

    /// Plan splitting a STEP file into one part per body. `bodies` are the body names
    /// the worker found, in order. Nothing is written yet.
    pub fn plan_split_import(&self, file: &Path, bodies: &[String]) -> Result<SplitImport> {
        let flat: Vec<(String, Vec<String>)> = bodies.iter().map(|b| (b.clone(), Vec::new())).collect();
        self.plan_split_import_nested(file, &flat)
    }

    /// As `plan_split_import`, with each body's sub-assemblies (outermost first), which
    /// become groups inside the import's own group.
    pub fn plan_split_import_nested(&self, file: &Path, bodies: &[(String, Vec<String>)]) -> Result<SplitImport> {
        check_step_file(file)?;
        if bodies.is_empty() {
            bail!("the STEP file has no solid bodies");
        }
        if bodies.len() > MAX_SPLIT_BODIES {
            bail!(
                "that file has {} bodies; up to {MAX_SPLIT_BODIES} can be split into parts. Import it as one part instead",
                bodies.len()
            );
        }
        let stem = plain_name(&file.file_stem().map(|s| s.to_string_lossy()).unwrap_or_default(), "Imported");
        let mut folder = stem.clone();
        let mut n = 2;
        while self.dir.join("imports").join(&folder).exists() || self.groups().iter().any(|g| *g == folder) {
            folder = format!("{stem}_{n}");
            n += 1;
        }

        // Repeated bodies (a box of identical screws) get numbered.
        let mut taken: HashSet<String> = self.project.parts.iter().map(|p| p.name.to_ascii_lowercase()).collect();
        let mut parts = Vec::new();
        let mut groups = Vec::new();
        for (i, (body, path)) in bodies.iter().enumerate() {
            let inner = path.iter().map(|s| group_segment(s)).filter(|s| !s.is_empty()).take(MAX_GROUP_DEPTH - 1);
            groups.push(std::iter::once(folder.clone()).chain(inner).collect::<Vec<_>>().join("/"));
            let base = plain_name(body, &format!("Body_{}", i + 1));
            let mut name = base.clone();
            let mut n = 2;
            while taken.contains(&name.to_ascii_lowercase()) {
                let suffix = format!("_{n}");
                name = format!("{}{suffix}", base.chars().take(64 - suffix.len()).collect::<String>());
                n += 1;
            }
            taken.insert(name.to_ascii_lowercase());
            parts.push((name.clone(), format!("imports/{folder}/{name}.step")));
        }
        Ok(SplitImport { group: folder.clone(), folder: self.dir.join("imports").join(&folder), parts, groups })
    }

    /// Add the parts of a split import once the worker has written every body's file.
    /// Call `save` afterwards.
    pub fn add_split_import(&mut self, split: &SplitImport) -> Result<()> {
        check_group_name(&split.group)?;
        for (name, source) in &split.parts {
            check_part_name(name)?;
            check_import_path(source)?;
            if self.project.parts.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
                bail!("a part named '{name}' already exists");
            }
            if !self.dir.join(source).is_file() {
                bail!("{source} wasn't written");
            }
        }
        for group in &split.groups {
            check_group_name(group)?;
        }
        for (i, (name, source)) in split.parts.iter().enumerate() {
            let mut part = Part::new(name, Some(source.clone()));
            part.group = Some(split.groups.get(i).unwrap_or(&split.group).clone());
            self.project.parts.push(part);
        }
        Ok(())
    }

    /// Every group, as a path like "Printer/Extruder", in the order its first part
    /// appears. A group with only sub-groups in it is listed too.
    pub fn groups(&self) -> Vec<String> {
        let mut groups: Vec<String> = Vec::new();
        for group in self.project.parts.iter().filter_map(|p| p.group.as_deref()) {
            let mut path = String::new();
            for segment in group.split('/') {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(segment);
                if !groups.contains(&path) {
                    groups.push(path.clone());
                }
            }
        }
        groups
    }

    /// Put parts in a group (a path like "Printer/Extruder"; it is made if it's new),
    /// or take them out of any group with None. Grouping only tidies the parts list.
    pub fn set_group(&mut self, parts: &[String], group: Option<&str>) -> Result<()> {
        let group = group.map(clean_group).filter(|g| !g.is_empty());
        if let Some(group) = &group {
            check_group_name(group)?;
        }
        if parts.is_empty() {
            bail!("name at least one part to group");
        }
        for name in parts {
            self.part(name)?;
        }
        for part in self.project.parts.iter_mut().filter(|p| parts.contains(&p.name)) {
            part.group = group.clone();
        }
        Ok(())
    }

    /// Rename a group, or move it: `new` is the whole new path, so "A/B" to "C/B" puts
    /// B inside C, and to "B" lifts it out. Everything inside comes along. An empty
    /// `new` dissolves the group: its parts and sub-groups move up one level.
    pub fn rename_group(&mut self, old: &str, new: &str) -> Result<()> {
        let new = clean_group(new);
        if !self.groups().iter().any(|g| g == old) {
            bail!("no group named '{old}'");
        }
        let new = if new.is_empty() { old.rsplit_once('/').map(|(up, _)| up.to_owned()) } else { Some(new) };
        if let Some(new) = &new {
            if in_group(Some(new), old) && new != old {
                bail!("a group can't be moved inside itself");
            }
        }
        for part in self.project.parts.iter_mut().filter(|p| in_group(p.group.as_deref(), old)) {
            let rest = part.group.as_deref().unwrap_or_default()[old.len()..].trim_start_matches('/');
            let path = match (&new, rest.is_empty()) {
                (Some(new), true) => Some(new.clone()),
                (Some(new), false) => Some(format!("{new}/{rest}")),
                (None, true) => None,
                (None, false) => Some(rest.to_owned()),
            };
            if let Some(path) = &path {
                check_group_name(path)?;
            }
            part.group = path;
        }
        Ok(())
    }

    /// Move a whole group as one rigid piece: every part is turned (degrees about X,
    /// Y, Z through the assembly origin) and then shifted, on top of where it is now.
    /// Joints that move a part of the group go with it.
    pub fn move_group(&mut self, group: &str, position: [f64; 3], rotation: [f64; 3]) -> Result<()> {
        if position.iter().chain(&rotation).any(|v| !v.is_finite() || v.abs() > 1e6) {
            bail!("positions and rotations must be ordinary numbers");
        }
        let by = Pose::from_placement(position, rotation);
        let mut moved = HashSet::new();
        for part in self.project.parts.iter_mut().filter(|p| in_group(p.group.as_deref(), group)) {
            let [x, y, z, rx, ry, rz] = Pose::from_placement(part.position, part.rotation).then(&by).to_placement();
            part.position = [x, y, z];
            part.rotation = [rx, ry, rz];
            moved.insert(part.name.clone());
        }
        if moved.is_empty() {
            bail!("no group named '{group}'");
        }
        for joint in self.project.joints.iter_mut().filter(|j| moved.contains(&j.part)) {
            joint.origin = by.apply(joint.origin);
            joint.axis = by.turn(joint.axis);
        }
        Ok(())
    }

    /// Remove every part in a group, as `remove_part` does for each.
    pub fn remove_group(&mut self, group: &str) -> Result<()> {
        let names: Vec<String> =
            self.project.parts.iter().filter(|p| in_group(p.group.as_deref(), group)).map(|p| p.name.clone()).collect();
        if names.is_empty() {
            bail!("no group named '{group}'");
        }
        for name in names {
            self.remove_part(&name)?;
        }
        Ok(())
    }

    /// Remove a part and its snippets. An imported STEP file stays in the imports
    /// folder, so undo can bring the part back.
    pub fn remove_part(&mut self, name: &str) -> Result<()> {
        let index = self
            .project
            .parts
            .iter()
            .position(|p| p.name == name)
            .with_context(|| format!("no part named '{name}'"))?;
        let part = self.project.parts.remove(index);
        for feature in part.features {
            self.removed.push(self.dir.join(feature.file));
        }
        // A joint can't outlive either of its parts. Undo brings them back together.
        self.project.joints.retain(|j| j.base != name && j.part != name);
        Ok(())
    }

    /// Add a hinge or slider between two parts. Call `save` afterwards.
    pub fn add_joint(&mut self, joint: Joint) -> Result<()> {
        if self.project.joint(&joint.name).is_some() {
            bail!("a joint called '{}' already exists; change it with edit_joint", joint.name);
        }
        let mut joint = joint;
        joint.name = joint.name.trim().to_owned();
        joint.why = joint.why.trim().to_owned();
        self.project.joints.push(joint);
        self.check_joints_or_undo(|p| {
            p.joints.pop();
        })
    }

    /// Change a joint, for example to move it to a new position (`at`).
    pub fn edit_joint(&mut self, name: &str, edit: JointEdit) -> Result<()> {
        let index = self.joint_index(name)?;
        let before = self.project.joints[index].clone();
        edit.apply(&mut self.project.joints[index]);
        self.check_joints_or_undo(|p| p.joints[index] = before)
    }

    pub fn delete_joint(&mut self, name: &str) -> Result<()> {
        let index = self.joint_index(name)?;
        self.project.joints.remove(index);
        Ok(())
    }

    fn joint_index(&self, name: &str) -> Result<usize> {
        self.project
            .joints
            .iter()
            .position(|j| j.name.eq_ignore_ascii_case(name))
            .with_context(|| format!("no joint called '{name}'"))
    }

    /// Checks the joints after a change, and puts things back if they don't hold up.
    fn check_joints_or_undo(&mut self, undo: impl FnOnce(&mut Project)) -> Result<()> {
        let result = joints::check_joints(&self.project);
        if result.is_err() {
            undo(&mut self.project);
        }
        result
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
        check_why(Some(&new.why))?;
        check_labels(Some(&new.labels))?;
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
                why: new.why.trim().to_owned(),
                labels: new.labels,
                file,
                params: new.params,
                code: ensure_trailing_newline(new.code),
            },
        );
        Ok(id)
    }

    pub fn edit_feature(&mut self, part_name: &str, id: &str, edit: FeatureEdit) -> Result<()> {
        check_feature_text(edit.label.as_deref(), edit.code.as_deref(), edit.prompt.as_deref())?;
        check_why(edit.why.as_deref())?;
        check_labels(edit.labels.as_ref())?;
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
        if let Some(why) = edit.why {
            feature.why = why.trim().to_owned();
        }
        if let Some(labels) = edit.labels {
            feature.labels = labels;
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

    /// Move a feature so it comes straight after `after`, or first when `after` is None.
    pub fn move_feature(&mut self, part_name: &str, id: &str, after: Option<&str>) -> Result<()> {
        if after == Some(id) {
            bail!("a feature can't be moved after itself");
        }
        let part = self.design_part_mut(part_name)?;
        let find = |features: &[Feature], wanted: &str| {
            features.iter().position(|f| f.id == wanted).with_context(|| format!("no feature '{wanted}' in '{part_name}'"))
        };
        let from = find(&part.features, id)?;
        if let Some(after) = after {
            find(&part.features, after)?;
        }
        let feature = part.features.remove(from);
        let to = match after {
            Some(after) => find(&part.features, after)? + 1,
            None => 0,
        };
        part.features.insert(to, feature);
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
        let mut files = vec![self.dir.join("project.json"), self.dir.join(BRIEF_FILE)];
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

/// The project's brief, or an empty string when it has none.
fn read_brief(dir: &Path) -> Result<String> {
    let path = dir.join(BRIEF_FILE);
    let Ok(meta) = std::fs::metadata(&path) else { return Ok(String::new()) };
    // Four bytes a character at most, so this is a safe upper bound before reading.
    if meta.len() > (MAX_BRIEF_CHARS * 4) as u64 {
        bail!("{} is too large to be a brief", path.display());
    }
    std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))
}

/// Hold the returned file while changing a project, so the viewer and MCP clients
/// never write at the same time. Waits for whoever holds it.
pub fn lock(dir: &Path) -> Result<std::fs::File> {
    let file = open_lock(dir)?;
    file.lock().context("waiting for the project lock")?;
    Ok(file)
}

/// Like `lock`, but returns None straight away if someone else holds it.
pub fn try_lock(dir: &Path) -> Result<Option<std::fs::File>> {
    let file = open_lock(dir)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("taking the project lock"),
    }
}

fn open_lock(dir: &Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".intercraft.lock"))
        .context("opening the project lock")
}

/// Project files can come from anywhere, so nothing in them may reach outside the folder.
fn check_project(project: &Project) -> Result<()> {
    let mut part_names = HashSet::new();
    for part in &project.parts {
        check_part_name(&part.name)?;
        if let Some(source) = &part.source {
            check_import_path(source)?;
        }
        if let Some(group) = &part.group {
            check_group_name(group)?;
        }
        if !part_names.insert(part.name.to_ascii_lowercase()) {
            bail!("project.json lists the part '{}' twice", part.name);
        }
        let mut ids = HashSet::new();
        for feature in &part.features {
            if !ids.insert(feature.id.clone()) {
                bail!("part '{}' has two features with the id '{}'", part.name, feature.id);
            }
            check_snippet_path(&feature.file)?;
        }
    }
    joints::check_joints(project)
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

/// A STEP file the user picked: right extension and not too big. Returns the
/// extension in lower case.
fn check_step_file(file: &Path) -> Result<String> {
    let ext = file.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if ext != "step" && ext != "stp" {
        bail!("only .step and .stp files can be imported");
    }
    let size = std::fs::metadata(file).with_context(|| format!("reading {}", file.display()))?.len();
    if size > MAX_IMPORT_BYTES {
        bail!("that file is {} MB; the limit is {} MB", size >> 20, MAX_IMPORT_BYTES >> 20);
    }
    Ok(ext)
}

/// A part or folder name made from any text (a file name, a body's label in a STEP
/// file), or `fallback` when nothing usable is left.
fn plain_name(text: &str, fallback: &str) -> String {
    let name: String = text
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .chars()
        .take(48)
        .collect();
    if name.is_empty() || check_part_name(&name).is_err() { fallback.to_owned() } else { name }
}

/// Group names are only shown, never used as paths, so anything readable will do.
fn check_group_name(name: &str) -> Result<()> {
    let levels: Vec<&str> = name.split('/').collect();
    if levels.len() > MAX_GROUP_DEPTH {
        bail!("groups can nest up to {MAX_GROUP_DEPTH} deep");
    }
    for level in levels {
        if level.trim().is_empty() || level != level.trim() || level.chars().count() > 64 || level.chars().any(char::is_control) {
            bail!("each group name must be 1 to 64 characters (use '/' to nest groups)");
        }
    }
    Ok(())
}

/// An imported file must sit in the project's imports folder, or one folder inside it
/// (the bodies of a split assembly).
fn check_import_path(source: &str) -> Result<()> {
    let path = Path::new(source);
    let parts: Vec<_> = path.components().collect();
    let in_imports = (2..=3).contains(&parts.len())
        && matches!(parts[0], Component::Normal(first) if first == "imports")
        && parts[1..].iter().all(|c| matches!(c, Component::Normal(_)));
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

fn check_labels(labels: Option<&std::collections::BTreeMap<String, String>>) -> Result<()> {
    let Some(labels) = labels else { return Ok(()) };
    if labels.len() > 50 {
        bail!("give at most 50 parameter labels");
    }
    for (name, label) in labels {
        if name.is_empty() || name.len() > 64 || label.trim().is_empty() || label.chars().count() > 40 {
            bail!("each parameter label must be 1 to 40 characters, for a parameter name of 1 to 64");
        }
    }
    Ok(())
}

fn check_why(why: Option<&str>) -> Result<()> {
    if why.is_some_and(|w| w.chars().count() > MAX_WHY_CHARS) {
        bail!("keep the why to one or two sentences ({MAX_WHY_CHARS} characters at most)");
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

    fn hinge(name: &str, base: &str, part: &str) -> Joint {
        Joint {
            name: name.into(),
            kind: JointKind::Hinge,
            base: base.into(),
            part: part.into(),
            origin: [0.0, 0.0, 10.0],
            axis: [1.0, 0.0, 0.0],
            min: 0.0,
            max: 110.0,
            at: 0.0,
            why: "Lets the lid swing open.".into(),
        }
    }

    #[test]
    fn joints_follow_their_parts() {
        let mut p = temp_project("joints");
        for name in ["Box", "Lid", "Latch"] {
            p.add_part(name).unwrap();
        }
        p.save().unwrap();
        p.add_joint(hinge("Lid hinge", "Box", "Lid")).unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Add joint 'Lid hinge'"));

        // Broken joints are refused and leave the project as it was.
        assert!(p.add_joint(hinge("lid HINGE", "Box", "Latch")).is_err(), "names are unique");
        assert!(p.add_joint(hinge("Second", "Box", "Lid")).is_err(), "one joint per moving part");
        assert!(p.add_joint(hinge("Loop", "Lid", "Box")).is_err(), "no loops");
        assert!(p.add_joint(hinge("Self", "Box", "Box")).is_err());
        assert!(p.add_joint(Joint { axis: [0.0; 3], ..hinge("Flat", "Box", "Latch") }).is_err());
        assert!(p.add_joint(Joint { at: 200.0, ..hinge("Far", "Box", "Latch") }).is_err());
        assert!(p.add_joint(hinge("Ghost", "Box", "Nothing")).is_err());
        assert_eq!(p.project.joints.len(), 1);
        let edit = JointEdit { at: Some(500.0), ..Default::default() };
        assert!(p.edit_joint("Lid hinge", edit).is_err());
        assert_eq!(p.project.joints[0].at, 0.0, "a refused edit changes nothing");

        // A chain: the latch rides on the lid.
        p.add_joint(Joint { origin: [0.0, 5.0, 10.0], ..hinge("Latch pivot", "Lid", "Latch") }).unwrap();
        p.save().unwrap();
        assert_eq!(p.project.moved_by("Lid hinge"), ["Lid", "Latch"]);
        let latch = p.part("Latch").unwrap().clone();
        assert_eq!(p.project.shown_placement(&latch), None, "nothing moves at zero");
        p.edit_joint("lid hinge", JointEdit { at: Some(90.0), ..Default::default() }).unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Move 'Lid hinge' to 90°"));
        // Turned 90° about X through (0, 0, 10): the latch's origin lands at (0, 10, 10).
        let shown = p.project.shown_placement(&latch).unwrap();
        assert_eq!(shown, [0.0, 10.0, 10.0, 90.0, 0.0, 0.0]);

        // Renames carry through, and removing a part drops its joints.
        p.rename_part("Lid", "Cover").unwrap();
        assert_eq!((p.project.joints[0].part.as_str(), p.project.joints[1].base.as_str()), ("Cover", "Cover"));
        p.save().unwrap();
        assert_eq!(LoadedProject::load(&p.dir).unwrap().project.joints.len(), 2);
        p.remove_part("Cover").unwrap();
        assert!(p.project.joints.is_empty());
        std::fs::remove_dir_all(&p.dir).unwrap();
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
        let bad = ["../x.step", "C:/x.step", "imports/../../x.step", "imports/x.txt", "x.step", "imports/a/b/c.step", "imports/a/../../x.step"];
        for source in bad {
            let json = format!(r#"{{"name":"T","parts":[{{"name":"P","source":"{source}","features":[]}}]}}"#);
            std::fs::write(p.dir.join("project.json"), json).unwrap();
            assert!(LoadedProject::load(&p.dir).is_err(), "accepted {source}");
        }
        // One folder down is where a split assembly's bodies go.
        let json = r#"{"name":"T","parts":[{"name":"P","source":"imports/Kit/P.step","group":"Kit","features":[]}]}"#;
        std::fs::write(p.dir.join("project.json"), json).unwrap();
        assert!(LoadedProject::load(&p.dir).is_ok());
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn a_split_import_becomes_a_group_of_parts() {
        let mut p = temp_project("split");
        p.add_part("Bolt").unwrap();
        p.save().unwrap();
        let src = p.dir.join("Gear box (v3).step");
        std::fs::write(&src, "ISO-10303-21;").unwrap();
        let bodies: Vec<String> = ["Housing", "Bolt", "Bolt", "M3 screw (1)", "", "Bolt"].map(String::from).to_vec();
        let split = p.plan_split_import(&src, &bodies).unwrap();
        assert_eq!(split.group, "Gear_box__v3");
        let names: Vec<&str> = split.parts.iter().map(|(n, _)| n.as_str()).collect();
        // "Bolt" is already a part, so every copy is numbered; a blank label gets a stand-in.
        assert_eq!(names, ["Housing", "Bolt_2", "Bolt_3", "M3_screw__1", "Body_5", "Bolt_4"]);
        assert_eq!(split.parts[0].1, "imports/Gear_box__v3/Housing.step");

        // The worker hasn't written the files yet, so nothing is added.
        assert!(p.add_split_import(&split).is_err());
        assert_eq!(p.project.parts.len(), 1);
        std::fs::create_dir_all(&split.folder).unwrap();
        for (_, source) in &split.parts {
            std::fs::write(p.dir.join(source), "ISO-10303-21;").unwrap();
        }
        p.add_split_import(&split).unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Import 'Gear_box__v3' (6 parts)"));
        assert_eq!(p.groups(), ["Gear_box__v3"]);

        // Importing the same file again makes a second group in its own folder.
        let again = p.plan_split_import(&src, &bodies[..1]).unwrap();
        assert_eq!((again.group.as_str(), again.parts[0].0.as_str()), ("Gear_box__v3_2", "Housing_2"));
        assert!(p.plan_split_import(&src, &vec!["X".to_owned(); MAX_SPLIT_BODIES + 1]).is_err());

        // Standing a Y-up model upright turns every part together about the origin.
        p.set_placement("Housing", [0.0, 10.0, 0.0], [0.0; 3]).unwrap();
        p.save().unwrap();
        p.move_group("Gear_box__v3", [0.0, 0.0, 5.0], [90.0, 0.0, 0.0]).unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Move 'Gear_box__v3' (6 parts)"));
        let housing = p.part("Housing").unwrap();
        assert_eq!((housing.position, housing.rotation), ([0.0, 0.0, 15.0], [90.0, 0.0, 0.0]));
        assert_eq!(p.part("Bolt_2").unwrap().position, [0.0, 0.0, 5.0]);
        assert_eq!(p.part("Bolt").unwrap().position, [0.0; 3], "parts outside the group stay put");
        assert!(p.move_group("Nothing", [1.0; 3], [0.0; 3]).is_err());

        // Groups nest: tidy the bolts into a group inside the import.
        let bolts: Vec<String> = ["Bolt_2", "Bolt_3", "Bolt_4"].map(String::from).to_vec();
        p.set_group(&bolts, Some(" Gear_box__v3 / Fasteners ")).unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Group 3 parts as 'Gear_box__v3/Fasteners'"));
        assert_eq!(p.groups(), ["Gear_box__v3", "Gear_box__v3/Fasteners"]);
        assert!(p.set_group(&bolts, Some("a/b/c/d/e/f/g")).is_err(), "too deep");
        assert!(p.set_group(&["Ghost".to_owned()], Some("X")).is_err());
        // A plain design part can be grouped too, and taken out again.
        p.set_group(&["Bolt".to_owned()], Some("Spares")).unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Put 'Bolt' in group 'Spares'"));
        p.set_group(&["Bolt".to_owned()], None).unwrap();
        p.save().unwrap();
        // Moving the outer group takes the inner one along; a group can't go inside itself.
        p.move_group("Gear_box__v3", [1.0, 0.0, 0.0], [0.0; 3]).unwrap();
        assert_eq!(p.part("Bolt_3").unwrap().position, [1.0, 0.0, 5.0]);
        assert!(p.rename_group("Gear_box__v3", "Gear_box__v3/Fasteners/Deeper").is_err());
        // Renaming with a path moves a group; an empty name dissolves it into its parent.
        p.rename_group("Gear_box__v3/Fasteners", "Gear_box__v3/Hardware/Bolts").unwrap();
        assert_eq!(p.part("Bolt_2").unwrap().group.as_deref(), Some("Gear_box__v3/Hardware/Bolts"));
        p.rename_group("Gear_box__v3/Hardware", "").unwrap();
        assert_eq!(p.part("Bolt_2").unwrap().group.as_deref(), Some("Gear_box__v3/Bolts"));
        p.rename_group("Gear_box__v3/Bolts", "").unwrap();
        assert_eq!(p.part("Bolt_2").unwrap().group.as_deref(), Some("Gear_box__v3"));
        p.save().unwrap();

        // Sub-assemblies in the file become groups inside the import's group.
        let nested = [("Motor".to_owned(), vec!["Extruder/v3".to_owned(), "Main".to_owned()]), ("Frame".to_owned(), vec![])];
        let plan = p.plan_split_import_nested(&src, &nested).unwrap();
        assert_eq!(plan.groups, ["Gear_box__v3_2/Extruder v3/Main", "Gear_box__v3_2"]);

        p.rename_group("Gear_box__v3", "Gear box").unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Rename group 'Gear_box__v3' to 'Gear box'"));
        let loaded = LoadedProject::load(&p.dir).unwrap();
        assert_eq!(loaded.project.parts[1].group.as_deref(), Some("Gear box"));

        p.remove_group("Gear box").unwrap();
        p.save().unwrap();
        assert_eq!(p.history().undo_label(), Some("Delete 'Gear box' (6 parts)"));
        assert_eq!(p.project.parts.len(), 1);
        p.undo().unwrap();
        assert_eq!(p.project.parts.len(), 7);
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn rename_duplicate_and_place() {
        let mut p = temp_project("assembly");
        p.add_part("Arm").unwrap();
        p.add_feature("Arm", feature("Base", None)).unwrap();
        p.save().unwrap();
        let old_file = p.dir.join("Arm/010_base.py");
        assert!(old_file.exists());

        p.rename_part("Arm", "Lever").unwrap();
        assert!(p.rename_part("Lever", "bad name").is_err());
        let copy = p.duplicate_part("Lever", None).unwrap();
        assert_eq!(copy, "Lever_copy");
        assert_eq!(p.duplicate_part("Lever", None).unwrap(), "Lever_copy2");
        assert!(p.duplicate_part("Lever", Some("lever")).is_err());
        p.set_placement("Lever_copy", [10.0, 0.0, -5.0], [0.0, 0.0, 90.0]).unwrap();
        assert!(p.set_placement("Lever", [f64::NAN, 0.0, 0.0], [0.0; 3]).is_err());
        p.save().unwrap();

        assert!(!old_file.exists() && !p.dir.join("Arm").exists(), "the old folder is gone");
        let loaded = LoadedProject::load(&p.dir).unwrap();
        let names: Vec<_> = loaded.project.parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Lever", "Lever_copy2", "Lever_copy"]);
        assert_eq!(loaded.project.parts[2].features[0].file, "Lever_copy/010_base.py");
        assert_eq!(loaded.project.parts[2].placement(), Some([10.0, 0.0, -5.0, 0.0, 0.0, 90.0]));
        assert_eq!(loaded.project.parts[0].placement(), None);
        // An unmoved part writes no placement at all.
        let text = std::fs::read_to_string(p.dir.join("project.json")).unwrap();
        assert_eq!(text.matches("\"position\"").count(), 1);
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn parameter_labels_are_kept_and_checked() {
        let mut p = temp_project("labels");
        p.add_part("P").unwrap();
        let mut new = feature("Bore", None);
        new.labels.insert("bore_d".into(), "Bore diameter".into());
        let id = p.add_feature("P", new).unwrap();
        p.save().unwrap();
        let loaded = LoadedProject::load(&p.dir).unwrap();
        assert_eq!(loaded.project.parts[0].features[0].labels["bore_d"], "Bore diameter");
        let too_long = [("x".to_owned(), "y".repeat(41))].into_iter().collect();
        assert!(p.edit_feature("P", &id, FeatureEdit { labels: Some(too_long), ..Default::default() }).is_err());
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn the_why_is_kept_and_old_projects_still_load() {
        let mut p = temp_project("why");
        p.add_part("P").unwrap();
        let mut new = feature("Bore", None);
        new.why = " Sized for an 8 mm rod with 0.2 mm clearance so it slides in. ".into();
        let id = p.add_feature("P", new).unwrap();
        p.save().unwrap();
        let loaded = LoadedProject::load(&p.dir).unwrap();
        assert_eq!(loaded.project.parts[0].features[0].why, "Sized for an 8 mm rod with 0.2 mm clearance so it slides in.");

        let edit = FeatureEdit { why: Some("x".repeat(MAX_WHY_CHARS + 1)), ..Default::default() };
        assert!(p.edit_feature("P", &id, edit).is_err());
        let edit = FeatureEdit { why: Some(String::new()), ..Default::default() };
        p.edit_feature("P", &id, edit).unwrap();
        p.save().unwrap();
        // An empty why isn't written, so project files stay as they were before it existed.
        let text = std::fs::read_to_string(p.dir.join("project.json")).unwrap();
        assert!(!text.contains("\"why\""), "{text}");
        assert!(LoadedProject::load(&p.dir).is_ok());
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn the_brief_is_written_updated_by_section_and_reloaded() {
        let mut p = temp_project("brief");
        assert_eq!(std::fs::read_to_string(p.dir.join(BRIEF_FILE)).unwrap(), BRIEF_TEMPLATE, "new projects get the template");

        p.update_brief_section("What it is", "A wall mount for a 25 mm broom handle.").unwrap();
        p.update_brief_section("open ITEMS", "- Wall screw size not confirmed").unwrap();
        p.update_brief_section("Print settings", "0.2 mm layers, PLA").unwrap();
        p.save().unwrap();
        let loaded = LoadedProject::load(&p.dir).unwrap();
        let brief = &loaded.brief;
        assert!(brief.contains("## What it is\n\nA wall mount for a 25 mm broom handle.\n\n## How it will be made"), "{brief}");
        assert!(brief.contains("## Open items\n\n- Wall screw size not confirmed\n"), "{brief}");
        assert!(brief.trim_end().ends_with("## Print settings\n\n0.2 mm layers, PLA"), "{brief}");
        assert_eq!(brief.matches("## What it is").count(), 1);

        assert!(p.update_brief_section("", "x").is_err());
        assert!(p.update_brief_section("Decisions", "## Sneaky\nmore").is_err());
        assert!(p.set_brief("x".repeat(MAX_BRIEF_CHARS + 1)).is_err());

        // The brief is part of the undo history.
        let mut q = LoadedProject::load(&p.dir).unwrap();
        q.undo().unwrap();
        let back = LoadedProject::load(&p.dir).unwrap();
        assert!(!back.brief.contains("broom"), "{}", back.brief);
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn a_project_without_a_brief_gets_no_file() {
        let p = temp_project("nobrief");
        std::fs::remove_file(p.dir.join(BRIEF_FILE)).unwrap();
        let mut loaded = LoadedProject::load(&p.dir).unwrap();
        assert!(loaded.brief.is_empty());
        loaded.add_part("A").unwrap();
        loaded.save().unwrap();
        assert!(!p.dir.join(BRIEF_FILE).exists());
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn features_can_be_reordered() {
        let mut p = temp_project("reorder");
        p.add_part("P").unwrap();
        for label in ["A", "B", "C"] {
            p.add_feature("P", feature(label, None)).unwrap();
        }
        let order = |p: &LoadedProject| p.project.parts[0].features.iter().map(|f| f.label.clone()).collect::<String>();
        p.move_feature("P", "f030", None).unwrap();
        assert_eq!(order(&p), "CAB");
        p.move_feature("P", "f030", Some("f020")).unwrap();
        assert_eq!(order(&p), "ABC");
        p.move_feature("P", "f010", Some("f020")).unwrap();
        assert_eq!(order(&p), "BAC");
        assert!(p.move_feature("P", "f010", Some("f010")).is_err());
        assert!(p.move_feature("P", "f010", Some("f999")).is_err());
        assert!(p.move_feature("P", "f999", None).is_err());
        assert_eq!(order(&p), "BAC", "a refused move changes nothing");
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn slugs_are_file_safe() {
        assert_eq!(slug("Rod bore (8 mm)"), "rod_bore_8_mm");
        assert_eq!(slug("///"), "feature");
    }
}
