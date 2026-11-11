// intercraft-llm: talk to an LLM provider and run its tool calls.
// Each provider owns its own message format and agent loop behind `ChatSession`,
// so adding another provider doesn't touch the app.
// ureq 3.5 (json), serde_json 1, anyhow 1

mod anthropic;
mod claude_code;
mod sse;

use std::sync::atomic::AtomicBool;

use anyhow::Result;
use serde_json::Value;

pub use anthropic::{AnthropicChat, AnthropicSettings, ANTHROPIC_MODELS};
pub use claude_code::{claude_code_version, ClaudeCodeChat, ClaudeCodeSettings, CLAUDE_CODE_MODELS};

/// A tool the model may call, in provider-neutral form.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Runs tool calls on behalf of the model.
pub trait ToolExecutor {
    fn definitions(&self) -> Vec<ToolDef>;
    /// Ok is shown to the model as a normal result, Err as a tool error it can recover from.
    fn call(&mut self, name: &str, input: &Value) -> Result<Value>;
}

/// What the UI hears while a reply is being produced.
#[derive(Debug, Clone)]
pub enum ChatEvent {
    /// A new assistant reply has started. Following deltas belong to it.
    ReplyStarted { model: String },
    TextDelta(String),
    /// Short notes the model writes between tool calls ("Now cutting the bore...").
    NoteDelta(String),
    ToolStarted { id: String, name: String, input: Value },
    ToolFinished { id: String, ok: bool, output: Value },
    Usage(Usage),
    /// The model declined or the reply was cut short. Shown to the user as is.
    Notice(String),
    Done,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
}

/// One conversation with one provider.
pub trait ChatSession: Send {
    /// Send a user message and keep going until the model stops calling tools.
    /// `cancel` is checked while streaming and between tool calls.
    fn send(
        &mut self,
        text: &str,
        tools: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(ChatEvent),
        cancel: &AtomicBool,
    ) -> Result<()>;

    /// Forget the conversation so far.
    fn reset(&mut self);
}

/// Thrown when the user pressed Stop, so callers can tell it apart from a real error.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped")
    }
}

impl std::error::Error for Cancelled {}
