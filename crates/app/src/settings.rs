// User settings. Model and effort live in a small JSON file in the user's config folder.
// The API key lives in the OS credential store (Windows Credential Manager, macOS
// Keychain, Secret Service on Linux), never in a file or the project folder.
// keyring 4.2, dirs 7, serde 1

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "InterCraft";
const KEYRING_USER: &str = "anthropic-api-key";

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// Where the chat panel sends messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Anthropic Messages API with the user's API key.
    #[default]
    Anthropic,
    /// The user's own Claude Code install, signed in with their subscription.
    ClaudeCode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: Provider,
    pub model: String,
    pub effort: String,
    /// Alias for Claude Code's --model, empty for the user's own default.
    pub claude_code_model: String,
    /// Most recent first.
    pub recent_projects: Vec<String>,
    /// "system", "dark" or "light".
    pub theme: String,
}

const MAX_RECENT: usize = 8;

impl Default for Settings {
    fn default() -> Self {
        Self {
            provider: Provider::default(),
            model: intercraft_llm::ANTHROPIC_MODELS[0].0.to_owned(),
            // Multi-step tool work does better above the model's default effort.
            effort: "high".to_owned(),
            claude_code_model: String::new(),
            recent_projects: Vec::new(),
            theme: "system".to_owned(),
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("InterCraft").join("settings.json"))
}

impl Settings {
    pub fn load() -> Self {
        settings_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Change some fields and save. Reads the file first, so the chat panel and the
    /// rest of the app never overwrite each other's settings.
    pub fn update(change: impl FnOnce(&mut Settings)) -> anyhow::Result<Settings> {
        let mut settings = Settings::load();
        change(&mut settings);
        settings.save()?;
        Ok(settings)
    }

    pub fn remember_project(dir: &std::path::Path) -> anyhow::Result<Settings> {
        let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        let dir = dir.to_string_lossy().into_owned();
        Settings::update(|s| {
            s.recent_projects.retain(|p| !p.eq_ignore_ascii_case(&dir));
            s.recent_projects.insert(0, dir);
            s.recent_projects.truncate(MAX_RECENT);
        })
    }

    pub fn theme_preference(&self) -> eframe::egui::ThemePreference {
        match self.theme.as_str() {
            "dark" => eframe::egui::ThemePreference::Dark,
            "light" => eframe::egui::ThemePreference::Light,
            _ => eframe::egui::ThemePreference::System,
        }
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let path = settings_path().ok_or_else(|| anyhow::anyhow!("no config folder on this system"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// Where the API key came from, so the UI can say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    Environment,
    Missing,
}

pub fn load_api_key() -> (String, KeySource) {
    if let Ok(key) = keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER).and_then(|e| e.get_password()) {
        if !key.trim().is_empty() {
            return (key, KeySource::Keyring);
        }
    }
    match std::env::var("ANTHROPIC_API_KEY") {
        Ok(key) if !key.trim().is_empty() => (key, KeySource::Environment),
        _ => (String::new(), KeySource::Missing),
    }
}

pub fn save_api_key(key: &str) -> anyhow::Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?.set_password(key.trim())?;
    Ok(())
}

pub fn clear_api_key() -> anyhow::Result<()> {
    match keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
