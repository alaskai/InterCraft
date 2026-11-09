// intercraft-core: the project model (parts, features) and loading it from disk.
// serde 1, serde_json 1, anyhow 1

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    #[serde(default)]
    pub features: Vec<Feature>,
}

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
    pub params: serde_json::Map<String, serde_json::Value>,
    /// Snippet source. Read from `file` on load, not stored in project.json.
    #[serde(skip)]
    pub code: String,
}

/// A project plus where it lives, so it can be reloaded when files change.
#[derive(Debug, Clone)]
pub struct LoadedProject {
    pub dir: PathBuf,
    pub project: Project,
}

impl LoadedProject {
    pub fn load(dir: &Path) -> Result<Self> {
        let manifest = dir.join("project.json");
        let text = std::fs::read_to_string(&manifest)
            .with_context(|| format!("reading {}", manifest.display()))?;
        let mut project: Project =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;

        for part in &mut project.parts {
            for feature in &mut part.features {
                let path = dir.join(&feature.file);
                feature.code = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading snippet {}", path.display()))?;
            }
        }
        Ok(Self { dir: dir.to_path_buf(), project })
    }

    /// Every file that should trigger a rebuild when it changes.
    pub fn watched_files(&self) -> Vec<PathBuf> {
        let mut files = vec![self.dir.join("project.json")];
        for part in &self.project.parts {
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
