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
    /// Any server with an OpenAI-style API: OpenAI, OpenRouter, Ollama, LM Studio.
    OpenAi,
}

/// How much the app and the model explain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Explain {
    /// New to CAD: every action and term explained in plain words.
    Beginner,
    #[default]
    Standard,
    /// Knows CAD: numbers and decisions, no explanations of the basics.
    Expert,
}

impl Explain {
    pub const ALL: [Explain; 3] = [Explain::Beginner, Explain::Standard, Explain::Expert];

    pub fn name(self) -> &'static str {
        match self {
            Explain::Beginner => "Beginner",
            Explain::Standard => "Standard",
            Explain::Expert => "Expert",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Explain::Beginner => "Every action and design term explained in plain words",
            Explain::Standard => "Short explanations of design choices",
            Explain::Expert => "Terse: numbers and decisions only",
        }
    }

    /// Added to the model's instructions.
    pub fn model_note(self) -> &'static str {
        match self {
            Explain::Beginner => "The user is new to CAD. As you work, explain each step in plain words, and briefly explain any design term (fillet, clearance, boss and so on) the first time you use it. Keep explanations short.",
            Explain::Standard => "Explain design choices briefly as you go.",
            Explain::Expert => "The user knows CAD. Be terse: give the numbers and decisions, and skip explanations of basic terms.",
        }
    }
}

/// How much of the interface shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// Chat and the 3D view first. Code, file paths and raw parameter names hidden.
    Simple,
    #[default]
    Advanced,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub provider: Provider,
    pub model: String,
    pub effort: String,
    /// Alias for Claude Code's --model, empty for the user's own default.
    pub claude_code_model: String,
    /// Server address for the OpenAI-compatible route, up to and including /v1.
    pub openai_base_url: String,
    pub openai_model: String,
    /// That model can read images, so it gets `view_model` pictures.
    pub openai_vision: bool,
    /// The server's prices in US dollars per million tokens, if the user entered them.
    pub openai_price_input: f64,
    pub openai_price_output: f64,
    /// Most recent first.
    pub recent_projects: Vec<String>,
    /// "system", "dark" or "light".
    pub theme: String,
    pub explain: Explain,
    pub layout: Layout,
    /// The first-run question ("new to CAD or not?") has been answered.
    pub onboarded: bool,
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
            openai_base_url: intercraft_llm::OPENAI_PRESETS[0].1.to_owned(),
            openai_model: String::new(),
            openai_vision: false,
            openai_price_input: 0.0,
            openai_price_output: 0.0,
            recent_projects: Vec::new(),
            theme: "system".to_owned(),
            explain: Explain::default(),
            layout: Layout::default(),
            onboarded: false,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(intercraft_tools::config_dir()?.join("settings.json"))
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

/// Credential store entry for an OpenAI-compatible server. Each server gets its own
/// entry, so the key saved for one service is never sent to another.
fn server_key_user(base_url: &str) -> String {
    let rest = base_url.trim().split_once("://").map_or(base_url.trim(), |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default().to_ascii_lowercase();
    format!("api-key:{host}")
}

pub fn load_server_key(base_url: &str) -> String {
    keyring::Entry::new(KEYRING_SERVICE, &server_key_user(base_url))
        .and_then(|e| e.get_password())
        .map(|key| key.trim().to_owned())
        .unwrap_or_default()
}

pub fn save_server_key(base_url: &str, key: &str) -> anyhow::Result<()> {
    keyring::Entry::new(KEYRING_SERVICE, &server_key_user(base_url))?.set_password(key.trim())?;
    Ok(())
}

pub fn clear_server_key(base_url: &str) -> anyhow::Result<()> {
    match keyring::Entry::new(KEYRING_SERVICE, &server_key_user(base_url))?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_server_has_its_own_key_entry() {
        assert_eq!(server_key_user("https://api.openai.com/v1"), "api-key:api.openai.com");
        assert_eq!(server_key_user(" https://API.OpenAI.com/v1/ "), "api-key:api.openai.com");
        assert_eq!(server_key_user("http://localhost:11434/v1"), "api-key:localhost:11434");
        assert_ne!(server_key_user("https://openrouter.ai/api/v1"), server_key_user("https://api.openai.com/v1"));
    }

    #[test]
    fn levels_default_sensibly_and_each_tells_the_model_something_different() {
        let old: Settings = serde_json::from_str(r#"{"provider":"anthropic"}"#).unwrap();
        assert_eq!((old.explain, old.layout, old.onboarded), (Explain::Standard, Layout::Advanced, false));
        let new: Settings = serde_json::from_str(r#"{"explain":"beginner","layout":"simple","onboarded":true}"#).unwrap();
        assert_eq!((new.explain, new.layout), (Explain::Beginner, Layout::Simple));
        let notes: std::collections::HashSet<_> = Explain::ALL.iter().map(|e| e.model_note()).collect();
        assert_eq!(notes.len(), 3);
    }

    #[test]
    fn old_settings_files_still_load() {
        let old = r#"{"provider":"claude_code","model":"claude-opus-5-5","effort":"high"}"#;
        let settings: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.provider, Provider::ClaudeCode);
        assert_eq!(settings.openai_base_url, "https://api.openai.com/v1");
        let new: Settings = serde_json::from_str(r#"{"provider":"open_ai"}"#).unwrap();
        assert_eq!(new.provider, Provider::OpenAi);
    }
}
