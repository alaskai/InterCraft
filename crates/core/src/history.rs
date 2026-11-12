// Undo history. After every save the whole project (manifest and snippet code) is
// kept as a numbered snapshot in .intercraft/history, so a change made by the user,
// the chat panel or an MCP client can be stepped back and forth.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{check_project, write_if_changed, LoadedProject, PartKind, Project};

/// Oldest snapshots are dropped past this many.
pub const MAX_ENTRIES: usize = 200;
/// A snapshot bigger than this didn't come from InterCraft.
const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;

const OUTSIDE_LABEL: &str = "Edited outside InterCraft";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub number: u64,
    /// What this change did, e.g. "Add 'Rod bore' to Bracket".
    pub label: String,
    /// Seconds since 1970.
    pub time: u64,
}

/// What the Edit menu needs to know.
#[derive(Debug, Clone, Default)]
pub struct HistoryInfo {
    /// Oldest first.
    pub entries: Vec<HistoryEntry>,
    /// The entry the project currently matches.
    pub head: Option<u64>,
    /// The files changed since the last snapshot, for example in a text editor.
    pub outside_changes: bool,
}

impl HistoryInfo {
    fn head_index(&self) -> Option<usize> {
        self.entries.iter().position(|e| Some(e.number) == self.head)
    }

    pub fn undo_label(&self) -> Option<&str> {
        if self.outside_changes {
            return Some(OUTSIDE_LABEL);
        }
        let i = self.head_index()?;
        (i > 0).then(|| self.entries[i].label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        if self.outside_changes {
            return None;
        }
        Some(self.entries.get(self.head_index()? + 1)?.label.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Content {
    project: Project,
    /// Snippet file -> code.
    code: BTreeMap<String, String>,
    /// Left out when empty, so snapshots from before briefs existed still match.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    brief: String,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    label: String,
    time: u64,
    #[serde(flatten)]
    content: Content,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    head: Option<u64>,
    entries: Vec<HistoryEntry>,
}

struct History {
    dir: PathBuf,
}

impl History {
    fn open(project_dir: &Path) -> Self {
        Self { dir: project_dir.join(".intercraft").join("history") }
    }

    fn index(&self) -> Index {
        // A damaged index only loses the history, never the project.
        std::fs::read_to_string(self.dir.join("index.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save_index(&self, index: &Index) -> Result<()> {
        write_if_changed(&self.dir.join("index.json"), &serde_json::to_string_pretty(index)?)
    }

    fn file(&self, number: u64) -> PathBuf {
        self.dir.join(format!("{number:06}.json"))
    }

    fn read(&self, number: u64) -> Result<Snapshot> {
        let path = self.file(number);
        let size = std::fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?.len();
        if size > MAX_SNAPSHOT_BYTES {
            bail!("history entry {number} is too large to be one of ours");
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// Add a snapshot after the head. Anything that was undone is dropped, as in any editor.
    fn push(&self, index: &mut Index, label: String, content: &Content) -> Result<()> {
        let head_at = index.entries.iter().position(|e| Some(e.number) == index.head);
        let keep = head_at.map_or(0, |i| i + 1);
        for dropped in index.entries.drain(keep..) {
            let _ = std::fs::remove_file(self.file(dropped.number));
        }
        let number = index.entries.iter().map(|e| e.number).max().unwrap_or(0) + 1;
        let time = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let snapshot = Snapshot { label: label.clone(), time, content: content.clone() };
        write_if_changed(&self.file(number), &serde_json::to_string(&snapshot)?)?;
        index.entries.push(HistoryEntry { number, label, time });
        index.head = Some(number);
        while index.entries.len() > MAX_ENTRIES {
            let old = index.entries.remove(0);
            let _ = std::fs::remove_file(self.file(old.number));
        }
        self.save_index(index)
    }

    fn head_key(&self, index: &Index) -> Option<String> {
        let snapshot = self.read(index.head?).ok()?;
        Some(key(&snapshot.content))
    }
}

fn key(content: &Content) -> String {
    serde_json::to_string(content).unwrap_or_default()
}

impl LoadedProject {
    pub(crate) fn content(&self) -> Content {
        let code = self
            .project
            .parts
            .iter()
            .flat_map(|p| &p.features)
            .map(|f| (f.file.clone(), f.code.clone()))
            .collect();
        Content { project: self.project.clone(), code, brief: self.brief.clone() }
    }

    pub(crate) fn content_key(&self) -> String {
        key(&self.content())
    }

    /// Called by `save`: records the change if there was one.
    pub(crate) fn record(&mut self) -> Result<()> {
        let now = self.content();
        let now_key = key(&now);
        if now_key == self.baseline {
            return Ok(());
        }
        let history = History::open(&self.dir);
        let mut index = history.index();
        let before: Content = serde_json::from_str(&self.baseline)?;
        // The state we started from isn't in the history yet: a new history, or
        // files changed by something other than InterCraft.
        if history.head_key(&index).as_deref() != Some(self.baseline.as_str()) {
            let label = if index.entries.is_empty() { "Start" } else { OUTSIDE_LABEL };
            history.push(&mut index, label.to_owned(), &before)?;
        }
        history.push(&mut index, describe(&before, &now), &now)?;
        self.baseline = now_key;
        Ok(())
    }

    pub fn history(&self) -> HistoryInfo {
        let history = History::open(&self.dir);
        let index = history.index();
        let outside_changes =
            !index.entries.is_empty() && history.head_key(&index).as_deref() != Some(self.baseline.as_str());
        HistoryInfo { entries: index.entries, head: index.head, outside_changes }
    }

    /// Step back one change. Returns what was undone.
    pub fn undo(&mut self) -> Result<String> {
        let history = History::open(&self.dir);
        let mut index = history.index();
        self.keep_outside_changes(&history, &mut index)?;
        let at = index.entries.iter().position(|e| Some(e.number) == index.head);
        let Some(at) = at.filter(|&i| i > 0) else { bail!("there's nothing to undo") };
        let label = index.entries[at].label.clone();
        let previous = index.entries[at - 1].number;
        self.restore(&history, &mut index, previous)?;
        Ok(label)
    }

    /// Step forward again after an undo. Returns what was redone.
    pub fn redo(&mut self) -> Result<String> {
        let history = History::open(&self.dir);
        let mut index = history.index();
        if history.head_key(&index).as_deref() != Some(self.baseline.as_str()) {
            bail!("the project changed after the last undo, so there's nothing to redo");
        }
        let at = index.entries.iter().position(|e| Some(e.number) == index.head);
        let Some(next) = at.and_then(|i| index.entries.get(i + 1)) else { bail!("there's nothing to redo") };
        let (number, label) = (next.number, next.label.clone());
        self.restore(&history, &mut index, number)?;
        Ok(label)
    }

    /// Jump straight to any entry in the history.
    pub fn go_to(&mut self, number: u64) -> Result<()> {
        let history = History::open(&self.dir);
        let mut index = history.index();
        self.keep_outside_changes(&history, &mut index)?;
        if !index.entries.iter().any(|e| e.number == number) {
            bail!("that history entry is gone");
        }
        self.restore(&history, &mut index, number)
    }

    /// Changes made in another editor become an entry of their own, so undo
    /// reverts them instead of losing them silently.
    fn keep_outside_changes(&mut self, history: &History, index: &mut Index) -> Result<()> {
        if index.entries.is_empty() || history.head_key(index).as_deref() == Some(self.baseline.as_str()) {
            return Ok(());
        }
        let now = self.content();
        history.push(index, OUTSIDE_LABEL.to_owned(), &now)?;
        self.baseline = key(&now);
        Ok(())
    }

    fn restore(&mut self, history: &History, index: &mut Index, number: u64) -> Result<()> {
        let snapshot = history.read(number)?;
        let mut project = snapshot.content.project;
        // Snapshots live in the project folder, so they get the same checks as project.json.
        check_project(&project)?;
        for feature in project.parts.iter_mut().flat_map(|p| p.features.iter_mut()) {
            feature.code = snapshot
                .content
                .code
                .get(&feature.file)
                .cloned()
                .with_context(|| format!("history entry {number} is missing {}", feature.file))?;
        }
        let keep: std::collections::HashSet<&str> =
            project.parts.iter().flat_map(|p| &p.features).map(|f| f.file.as_str()).collect();
        let dir = self.dir.clone();
        let gone: Vec<PathBuf> = self
            .project
            .parts
            .iter()
            .flat_map(|p| &p.features)
            .filter(|f| !keep.contains(f.file.as_str()))
            .map(|f| dir.join(&f.file))
            .collect();
        if snapshot.content.brief.chars().count() > crate::MAX_BRIEF_CHARS {
            bail!("history entry {number} has an oversized brief");
        }
        self.project = project;
        self.brief = snapshot.content.brief;
        self.removed.extend(gone);
        self.write_files()?;
        self.remove_deleted();
        self.baseline = self.content_key();
        index.head = Some(number);
        history.save_index(index)
    }
}

/// A short description of what changed between two versions of a project.
fn describe(before: &Content, after: &Content) -> String {
    let (old, new) = (&before.project, &after.project);
    // Same parts in the same places with one name changed is a rename.
    if old.parts.len() == new.parts.len() {
        let mut renamed = old.parts.iter().zip(&new.parts).filter(|(a, b)| a.name != b.name);
        if let (Some((a, b)), None) = (renamed.next(), renamed.next()) {
            if !old.parts.iter().any(|p| p.name == b.name) {
                return format!("Rename '{}' to '{}'", a.name, b.name);
            }
        }
    }
    let mut changes = Vec::new();
    if before.brief != after.brief {
        changes.push("Update the brief".to_owned());
    }
    if old.name != new.name {
        changes.push(format!("Rename project to '{}'", new.name));
    }
    for part in &new.parts {
        let Some(was) = old.parts.iter().find(|p| p.name == part.name) else {
            changes.push(match part.source {
                Some(_) => format!("Import '{}'", part.name),
                None => format!("Add part '{}'", part.name),
            });
            continue;
        };
        if was.kind != part.kind {
            changes.push(match part.kind {
                PartKind::Reference => format!("Lock '{}'", part.name),
                PartKind::Design => format!("Unlock '{}'", part.name),
            });
        }
        if was.position != part.position || was.rotation != part.rotation {
            changes.push(format!("Move '{}'", part.name));
        }
        for f in &part.features {
            let Some(w) = was.features.iter().find(|w| w.id == f.id) else {
                changes.push(format!("Add '{}' to {}", f.label, part.name));
                continue;
            };
            let code_changed = before.code.get(&w.file) != after.code.get(&f.file);
            if code_changed || w.label != f.label || w.prompt != f.prompt || w.why != f.why || w.labels != f.labels {
                changes.push(format!("Edit '{}'", f.label));
            } else if w.params != f.params {
                let changed: Vec<_> = f.params.iter().filter(|(k, v)| w.params.get(*k) != Some(*v)).collect();
                match changed.as_slice() {
                    // One value changed or added, nothing removed.
                    [(name, value)] if w.params.keys().all(|k| f.params.contains_key(k)) => {
                        changes.push(format!("Set {name} = {value} on '{}'", f.label))
                    }
                    _ => changes.push(format!("Change parameters of '{}'", f.label)),
                }
            }
        }
        for w in &was.features {
            if !part.features.iter().any(|f| f.id == w.id) {
                changes.push(format!("Delete '{}' from {}", w.label, part.name));
            }
        }
        let order = |features: &[crate::Feature], other: &[crate::Feature]| -> Vec<String> {
            features.iter().filter(|f| other.iter().any(|o| o.id == f.id)).map(|f| f.id.clone()).collect()
        };
        if order(&part.features, &was.features) != order(&was.features, &part.features) {
            changes.push(format!("Reorder {}", part.name));
        }
    }
    for was in &old.parts {
        if !new.parts.iter().any(|p| p.name == was.name) {
            changes.push(format!("Delete part '{}'", was.name));
        }
    }
    match changes.len() {
        0 => "Change project".to_owned(),
        1 => changes.remove(0),
        n => format!("{} and {} more", changes[0], n - 1),
    }
}

#[cfg(test)]
mod tests {
    use crate::{LoadedProject, NewFeature};

    fn project(name: &str) -> LoadedProject {
        let dir = std::env::temp_dir().join(format!("intercraft-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        LoadedProject::create(&dir, "Test").unwrap()
    }

    fn feature(label: &str) -> NewFeature {
        NewFeature { label: label.into(), code: "part = Box(1, 1, 1)".into(), ..Default::default() }
    }

    fn reload(p: &LoadedProject) -> LoadedProject {
        LoadedProject::load(&p.dir).unwrap()
    }

    #[test]
    fn undo_and_redo_walk_through_saves() {
        let mut p = project("walk");
        p.add_part("Bracket").unwrap();
        p.save().unwrap();
        let id = p.add_feature("Bracket", feature("Base")).unwrap();
        p.save().unwrap();
        p.set_param("Bracket", &id, "w", 40.into()).unwrap();
        p.save().unwrap();

        let info = p.history();
        assert_eq!(info.undo_label(), Some("Set w = 40 on 'Base'"));
        assert_eq!(info.entries.first().unwrap().label, "Start");

        // Each undo works on a fresh load, the way the viewer and tools use it.
        let mut q = reload(&p);
        assert_eq!(q.undo().unwrap(), "Set w = 40 on 'Base'");
        assert!(reload(&p).project.parts[0].features[0].params.is_empty());
        let mut q = reload(&q);
        assert_eq!(q.undo().unwrap(), "Add 'Base' to Bracket");
        let snippet = p.dir.join(&p.project.parts[0].features[0].file);
        assert!(!snippet.exists(), "undoing an add removes its snippet");

        let mut q = reload(&q);
        assert_eq!(q.redo().unwrap(), "Add 'Base' to Bracket");
        assert_eq!(std::fs::read_to_string(&snippet).unwrap(), "part = Box(1, 1, 1)\n");
        assert_eq!(q.history().redo_label(), Some("Set w = 40 on 'Base'"));

        // A new change after an undo drops the redo.
        let mut q = reload(&q);
        q.add_part("Other").unwrap();
        q.save().unwrap();
        assert_eq!(q.history().redo_label(), None);
        assert_eq!(q.history().undo_label(), Some("Add part 'Other'"));
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn removed_parts_come_back() {
        let mut p = project("remove");
        p.add_part("Bracket").unwrap();
        p.add_feature("Bracket", feature("Base")).unwrap();
        p.save().unwrap();
        let folder = p.dir.join("Bracket");
        assert!(folder.exists());

        p.remove_part("Bracket").unwrap();
        p.save().unwrap();
        assert!(!folder.exists(), "the empty snippet folder goes too");
        assert_eq!(p.history().undo_label(), Some("Delete part 'Bracket'"));

        let mut q = reload(&p);
        q.undo().unwrap();
        let back = reload(&p);
        assert_eq!(back.project.parts[0].features[0].code, "part = Box(1, 1, 1)\n");
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn outside_edits_are_kept_as_their_own_step() {
        let mut p = project("outside");
        p.add_part("Bracket").unwrap();
        p.add_feature("Bracket", feature("Base")).unwrap();
        p.save().unwrap();
        let snippet = p.dir.join(&p.project.parts[0].features[0].file);
        std::fs::write(&snippet, "part = Box(2, 2, 2)\n").unwrap();

        let mut q = reload(&p);
        assert!(q.history().outside_changes);
        assert_eq!(q.history().undo_label(), Some("Edited outside InterCraft"));
        q.undo().unwrap();
        assert_eq!(std::fs::read_to_string(&snippet).unwrap(), "part = Box(1, 1, 1)\n");
        // ...and redo brings the outside edit back.
        reload(&p).redo().unwrap();
        assert_eq!(std::fs::read_to_string(&snippet).unwrap(), "part = Box(2, 2, 2)\n");
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn hostile_snapshots_are_refused() {
        let mut p = project("hostile");
        p.add_part("Bracket").unwrap();
        p.save().unwrap();
        p.add_part("Two").unwrap();
        p.save().unwrap();
        // Point the previous snapshot at a file outside the project.
        let file = p.dir.join(".intercraft/history/000002.json");
        let text = std::fs::read_to_string(&file).unwrap();
        let evil = text.replace(
            r#""features":[]"#,
            r#""features":[{"id":"f010","label":"x","file":"../../evil.py"}]"#,
        );
        assert_ne!(text, evil);
        std::fs::write(&file, evil).unwrap();
        assert!(reload(&p).undo().is_err());
        assert!(!p.dir.join("../../evil.py").exists());
        std::fs::remove_dir_all(&p.dir).unwrap();
    }

    #[test]
    fn history_is_capped() {
        let mut p = project("cap");
        for i in 0..(super::MAX_ENTRIES + 5) {
            p.add_part(&format!("P{i}")).unwrap();
            p.save().unwrap();
        }
        let info = p.history();
        assert_eq!(info.entries.len(), super::MAX_ENTRIES);
        let files = std::fs::read_dir(p.dir.join(".intercraft/history")).unwrap().count();
        assert_eq!(files, super::MAX_ENTRIES + 1, "snapshots plus the index");
        std::fs::remove_dir_all(&p.dir).unwrap();
    }
}
