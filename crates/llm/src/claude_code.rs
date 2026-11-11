// Drives the user's own Claude Code install in headless mode, so people with a
// Claude subscription can use the chat panel without an API key.
//
// InterCraft never sees or handles the user's login: it starts the unmodified
// `claude` binary, which signs in by itself. The session is locked to the
// InterCraft MCP tools only (no built-in tools, no other MCP servers).
//
// Stream format checked against Claude Code 2.1.322 (`-p --input-format stream-json
// --output-format stream-json --verbose --include-partial-messages`).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::{Cancelled, ChatEvent, ChatSession, ToolExecutor, Usage};

/// Claude Code prefixes MCP tool names with the server name.
const TOOL_PREFIX: &str = "mcp__intercraft__";

/// Model choices for Claude Code, as (alias, display name). Empty means the user's own default.
pub const CLAUDE_CODE_MODELS: &[(&str, &str)] = &[
    ("", "Your Claude Code default"),
    ("opus", "Opus"),
    ("sonnet", "Sonnet"),
    ("haiku", "Haiku"),
];

#[derive(Debug, Clone)]
pub struct ClaudeCodeSettings {
    /// Folder Claude Code runs in. The project folder, so its sessions are grouped there.
    pub project_dir: PathBuf,
    /// Command and arguments that start the InterCraft MCP server for this project.
    pub mcp_command: String,
    pub mcp_args: Vec<String>,
    /// Alias passed to `--model`, or empty for the user's default.
    pub model: String,
    pub extra_prompt: String,
}

struct Running {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

pub struct ClaudeCodeChat {
    settings: ClaudeCodeSettings,
    /// Kept so a restarted process (after Stop or a model change) carries on the conversation.
    session_id: Option<String>,
    running: Option<Running>,
}

/// `claude --version`, or None when Claude Code isn't installed or isn't on PATH.
pub fn claude_code_version() -> Option<String> {
    let mut cmd = Command::new(claude_program());
    cmd.arg("--version").stdin(Stdio::null()).stderr(Stdio::null());
    hide_console(&mut cmd);
    let output = cmd.output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// INTERCRAFT_CLAUDE_BIN swaps in another program, used by the tests for a fake Claude Code.
fn claude_program() -> std::ffi::OsString {
    std::env::var_os("INTERCRAFT_CLAUDE_BIN").unwrap_or_else(|| "claude".into())
}

fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

impl ClaudeCodeChat {
    pub fn new(settings: ClaudeCodeSettings) -> Self {
        Self { settings, session_id: None, running: None }
    }

    pub fn update_settings(&mut self, settings: ClaudeCodeSettings) {
        // A running process keeps its model and MCP setup, so restart it on the next message.
        let restart = settings.model != self.settings.model
            || settings.project_dir != self.settings.project_dir
            || settings.mcp_args != self.settings.mcp_args;
        self.settings = settings;
        if restart {
            self.stop();
        }
    }

    fn stop(&mut self) {
        if let Some(mut running) = self.running.take() {
            let _ = running.child.kill();
            let _ = running.child.wait();
        }
    }

    fn spawn(&mut self) -> Result<&mut Running> {
        if self.running.is_none() {
            let mcp_config = json!({
                "mcpServers": {
                    "intercraft": { "command": self.settings.mcp_command, "args": self.settings.mcp_args }
                }
            });
            let mut cmd = Command::new(claude_program());
            cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json"])
                .args(["--verbose", "--include-partial-messages"])
                .arg("--mcp-config")
                .arg(mcp_config.to_string())
                // Only InterCraft's tools: no shell, no file editing, no other MCP servers.
                .args(["--strict-mcp-config", "--tools", "", "--allowedTools", "mcp__intercraft"])
                .args(["--permission-mode", "dontAsk"])
                // Keeps the user's personal hooks and settings out of this session.
                .args(["--setting-sources", "project"])
                .arg("--append-system-prompt")
                .arg(&self.settings.extra_prompt);
            if !self.settings.model.is_empty() {
                cmd.arg("--model").arg(&self.settings.model);
            }
            if let Some(id) = &self.session_id {
                cmd.arg("--resume").arg(id);
            }
            // "Your subscription" means it: without these, an API key in the environment
            // would quietly switch Claude Code to billing that key instead.
            cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
            cmd.current_dir(&self.settings.project_dir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            hide_console(&mut cmd);

            let mut child = cmd.spawn().context(
                "couldn't start Claude Code. Check that `claude` runs in a terminal and you're signed in",
            )?;
            let stdin = child.stdin.take().context("Claude Code stdin")?;
            let stdout = child.stdout.take().context("Claude Code stdout")?;
            let mut stderr_pipe = child.stderr.take().context("Claude Code stderr")?;

            let (tx, lines) = channel();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            // Keep the tail of stderr for error messages if the process dies.
            let stderr = Arc::new(Mutex::new(String::new()));
            let tail = stderr.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                while let Ok(n) = stderr_pipe.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut text = tail.lock().unwrap_or_else(|e| e.into_inner());
                    text.push_str(&String::from_utf8_lossy(&buf[..n]));
                    let excess = text.len().saturating_sub(4000);
                    if excess > 0 {
                        let cut = text.char_indices().find(|(i, _)| *i >= excess).map_or(0, |(i, _)| i);
                        text.drain(..cut);
                    }
                }
            });

            self.running = Some(Running { child, stdin, lines, stderr });
        }
        Ok(self.running.as_mut().expect("process was just started"))
    }

    fn died(&mut self) -> anyhow::Error {
        let stderr = self
            .running
            .as_ref()
            .map(|r| r.stderr.lock().unwrap_or_else(|e| e.into_inner()).trim().to_owned())
            .unwrap_or_default();
        self.stop();
        if stderr.is_empty() {
            anyhow!("Claude Code stopped unexpectedly")
        } else {
            anyhow!("Claude Code stopped unexpectedly: {stderr}")
        }
    }
}

impl ChatSession for ClaudeCodeChat {
    fn send(
        &mut self,
        text: &str,
        _tools: &mut dyn ToolExecutor, // Claude Code calls the InterCraft tools itself, over MCP.
        on_event: &mut dyn FnMut(ChatEvent),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let line = json!({ "type": "user", "message": { "role": "user", "content": text } });
        let running = self.spawn()?;
        if writeln!(running.stdin, "{line}").and_then(|()| running.stdin.flush()).is_err() {
            return Err(self.died());
        }

        let mut model = String::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                // Kill it. The next message resumes the same session in a new process.
                self.stop();
                return Err(Cancelled.into());
            }
            let running = self.running.as_mut().expect("process is running");
            let line = match running.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(self.died()),
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };

            match msg["type"].as_str().unwrap_or_default() {
                "system" if msg["subtype"] == "init" => {
                    model = msg["model"].as_str().unwrap_or_default().to_owned();
                    if let Some(id) = msg["session_id"].as_str() {
                        self.session_id = Some(id.to_owned());
                    }
                }
                "stream_event" => {
                    // Raw Messages API events. Subagent output (with a parent id) is skipped.
                    if !msg["parent_tool_use_id"].is_null() {
                        continue;
                    }
                    let event = &msg["event"];
                    match event["type"].as_str().unwrap_or_default() {
                        "message_start" => on_event(ChatEvent::ReplyStarted { model: model.clone() }),
                        "content_block_delta" => {
                            let delta = &event["delta"];
                            match delta["type"].as_str().unwrap_or_default() {
                                "text_delta" => on_event(ChatEvent::TextDelta(
                                    delta["text"].as_str().unwrap_or_default().to_owned(),
                                )),
                                "thinking_delta" => {
                                    let note = delta["thinking"].as_str().unwrap_or_default();
                                    if !note.is_empty() {
                                        on_event(ChatEvent::NoteDelta(note.to_owned()));
                                    }
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                // Complete tool calls arrive here, once their input has finished streaming.
                "assistant" if msg["parent_tool_use_id"].is_null() => {
                    for block in msg["message"]["content"].as_array().into_iter().flatten() {
                        if block["type"] == "tool_use" {
                            let name = block["name"].as_str().unwrap_or_default();
                            on_event(ChatEvent::ToolStarted {
                                id: block["id"].as_str().unwrap_or_default().to_owned(),
                                name: name.strip_prefix(TOOL_PREFIX).unwrap_or(name).to_owned(),
                                input: block["input"].clone(),
                            });
                        }
                    }
                }
                "user" => {
                    for block in msg["message"]["content"].as_array().into_iter().flatten() {
                        if block["type"] == "tool_result" {
                            on_event(ChatEvent::ToolFinished {
                                id: block["tool_use_id"].as_str().unwrap_or_default().to_owned(),
                                ok: block["is_error"] != true,
                                output: tool_output(&block["content"]),
                            });
                        }
                    }
                }
                "rate_limit_event" => {
                    let info = &msg["rate_limit_info"];
                    if info["status"].as_str().is_some_and(|s| s != "allowed") {
                        on_event(ChatEvent::Notice(
                            "Your Claude usage limit has been reached. It resets later; check `claude` in a terminal for the time.".into(),
                        ));
                    }
                }
                "result" => {
                    if let Some(id) = msg["session_id"].as_str() {
                        self.session_id = Some(id.to_owned());
                    }
                    let usage = &msg["usage"];
                    on_event(ChatEvent::Usage(Usage {
                        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                        cache_read_tokens: usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
                        cache_write_tokens: usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                    }));
                    if msg["is_error"] == true {
                        let reason = msg["result"]
                            .as_str()
                            .or(msg["subtype"].as_str())
                            .unwrap_or("unknown error");
                        bail!("Claude Code reported an error: {reason}");
                    }
                    on_event(ChatEvent::Done);
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    fn reset(&mut self) {
        self.stop();
        self.session_id = None;
    }
}

impl Drop for ClaudeCodeChat {
    fn drop(&mut self) {
        self.stop();
    }
}

/// MCP results come back as a list of text blocks. InterCraft's tools return JSON text.
fn tool_output(content: &Value) -> Value {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    };
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_parses_json_text_blocks() {
        let content = json!([{ "type": "text", "text": "{\"ok\": true, \"features\": []}" }]);
        assert_eq!(tool_output(&content)["ok"], true);
        let error = json!([{ "type": "text", "text": "no part named 'X'" }]);
        assert_eq!(tool_output(&error), json!("no part named 'X'"));
    }
}
