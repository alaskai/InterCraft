// The chat saved with a project, in .intercraft/chat.json, so closing and reopening
// the project keeps the conversation. Holds the transcript and each provider's
// conversation state. Never keys or settings.

use std::path::{Path, PathBuf};

use serde_json::Value;

const VERSION: u64 = 1;
/// A saved chat bigger than this is ignored on load, and trimmed before saving.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

fn path(project: &Path) -> PathBuf {
    project.join(".intercraft").join("chat.json")
}

/// The saved chat, or None when there is none or it can't be trusted. The file sits
/// in the project folder, so it could have come from anywhere: it is size-checked
/// here, and every part of it is checked again by whoever uses it.
pub fn load(project: &Path) -> Option<Value> {
    let path = path(project);
    if std::fs::metadata(&path).ok()?.len() > MAX_BYTES {
        return None;
    }
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    (saved["version"] == VERSION && saved["messages"].is_array()).then_some(saved)
}

/// Write the chat, whole or not at all: to a temporary file first, then renamed over
/// the old one, so a crash mid-write can't leave half a file.
pub fn save(project: &Path, messages: Value, sessions: &Value, cost: Option<f64>) -> std::io::Result<()> {
    let path = path(project);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let saved = serde_json::json!({ "version": VERSION, "messages": messages, "sessions": sessions, "cost_usd": cost });
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, saved.to_string())?;
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Forget the saved chat (New chat).
pub fn clear(project: &Path) {
    let _ = std::fs::remove_file(path(project));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_chat_round_trips_and_bad_files_are_ignored() {
        let dir = std::env::temp_dir().join(format!("intercraft-chat-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load(&dir).is_none(), "nothing saved yet");

        save(&dir, json!([{ "user": "hello" }]), &json!({ "claude_code": { "session_id": "abc" } }), Some(0.25)).unwrap();
        let back = load(&dir).unwrap();
        assert_eq!(back["messages"][0]["user"], "hello");
        assert_eq!(back["sessions"]["claude_code"]["session_id"], "abc");
        assert_eq!(back["cost_usd"], 0.25);
        assert!(!dir.join(".intercraft").read_dir().unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));

        for bad in ["not json", r#"{"version": 99, "messages": []}"#, r#"{"version": 1, "messages": "no"}"#] {
            std::fs::write(path(&dir), bad).unwrap();
            assert!(load(&dir).is_none(), "accepted {bad}");
        }
        clear(&dir);
        assert!(!path(&dir).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
