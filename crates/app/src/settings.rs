// User settings. Model and effort live in a small JSON file in the user's config folder.
// The API key lives in the OS credential store (Windows Credential Manager, macOS
// Keychain, Secret Service on Linux), never in a file or the project folder.
// keyring 4.2, dirs 7, serde 1

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const KEYRING_SERVICE: &str = "InterCraft";
const KEYRING_USER: &str = "anthropic-api-key";

pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub model: String,
    pub effort: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: intercraft_llm::ANTHROPIC_MODELS[0].0.to_owned(),
            // Multi-step tool work does better above the model's default effort.
            effort: "high".to_owned(),
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
