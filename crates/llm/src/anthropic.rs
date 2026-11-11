// Claude via the Anthropic Messages API (raw HTTP + SSE, there is no official Rust SDK).
// API version 2026-10-21.

use std::collections::HashMap;
use std::io::BufReader;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::sse::SseReader;
use crate::{Cancelled, ChatEvent, ChatSession, ToolExecutor, Usage};

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const MAX_TOKENS: u32 = 64_000;
/// Stops a confused model from looping on tool calls forever.
const MAX_ROUNDS: usize = 40;
const MAX_RETRIES: u32 = 3;

/// Models offered in the settings, as (id, display name). The first is the default.
pub const ANTHROPIC_MODELS: &[(&str, &str)] = &[
    ("claude-opus-5-5", "Claude Opus 5.5"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("claude-haiku-5-5", "Claude Haiku 5.5"),
    ("claude-fable-5-2", "Claude Fable 5.2"),
];

#[derive(Debug, Clone)]
pub struct AnthropicSettings {
    pub api_key: String,
    pub model: String,
    /// low, medium, high, xhigh or max.
    pub effort: String,
    pub system: String,
}

/// Request features that differ between models.
struct Profile {
    /// `thinking.display: "updates"` returns the model's notes between tool calls.
    progress_updates: bool,
    /// Server-side retry on another model when a safety classifier declines.
    fallbacks: bool,
}

fn profile(model: &str) -> Profile {
    match model {
        "claude-opus-5-5" | "claude-sonnet-5-5" | "claude-fable-5-2" => {
            Profile { progress_updates: true, fallbacks: true }
        }
        _ => Profile { progress_updates: false, fallbacks: false },
    }
}

pub struct AnthropicChat {
    settings: AnthropicSettings,
    agent: ureq::Agent,
    /// Full conversation in API form. Only ever appended to, because the API
    /// checks that thinking blocks are replayed exactly as they were produced.
    messages: Vec<Value>,
}

impl AnthropicChat {
    pub fn new(settings: AnthropicSettings) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            // Long gaps are normal while the model thinks, but the server sends pings.
            .timeout_per_read(Some(Duration::from_secs(180)))
            .build()
            .into();
        Self { settings, agent, messages: Vec::new() }
    }

    /// Change model or effort without losing the conversation.
    pub fn update_settings(&mut self, settings: AnthropicSettings) {
        self.settings = settings;
    }

    fn request_body(&self, tools: &[crate::ToolDef]) -> Value {
        let p = profile(&self.settings.model);
        let tools: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                    // Snippets can be long, so stream them as they're written.
                    "eager_input_streaming": true,
                })
            })
            .collect();
        let mut body = json!({
            "model": self.settings.model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "system": self.settings.system,
            "tools": tools,
            "messages": self.messages,
            "thinking": {
                "type": "adaptive",
                "display": if p.progress_updates { "updates" } else { "summarized" },
            },
            "output_config": { "effort": self.settings.effort },
            // Caches tools, system and history up to the newest message.
            "cache_control": { "type": "ephemeral" },
        });
        if p.fallbacks {
            body["fallbacks"] = "default".into();
        }
        body
    }

    fn betas(&self) -> String {
        let p = profile(&self.settings.model);
        let mut betas = Vec::new();
        if p.fallbacks {
            betas.push("server-side-fallback-2026-07-01");
        }
        if p.progress_updates {
            betas.push("thinking-display-updates-2026-08-18");
        }
        betas.join(",")
    }

    /// Send the request, retrying overloads and rate limits before any output arrives.
    fn open_stream(&self, body: &Value, cancel: &AtomicBool) -> Result<ureq::http::Response<ureq::Body>> {
        let mut attempt = 0;
        loop {
            let mut request = self
                .agent
                .post(api_url())
                .header("x-api-key", &self.settings.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json");
            let betas = self.betas();
            if !betas.is_empty() {
                request = request.header("anthropic-beta", &betas);
            }

            let (retry_after, error) = match request.send_json(body) {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
                    let text = response.body_mut().read_to_string().unwrap_or_default();
                    let error = api_error(status, &text);
                    if !matches!(status, 429 | 500 | 502 | 503 | 504 | 529) {
                        return Err(error);
                    }
                    (retry_after, error)
                }
                Err(e) => (None, anyhow!(e).context("could not reach the Anthropic API")),
            };

            attempt += 1;
            if attempt > MAX_RETRIES {
                return Err(error);
            }
            let wait = retry_after.unwrap_or(1 << attempt).min(60);
            for _ in 0..wait * 10 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Cancelled.into());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// Stream one assistant turn.
    fn stream_turn(
        &self,
        body: &Value,
        on_event: &mut dyn FnMut(ChatEvent),
        cancel: &AtomicBool,
    ) -> Result<Turn> {
        let response = self.open_stream(body, cancel)?;
        let mut sse = SseReader::new(BufReader::new(response.into_body().into_reader()));
        let mut turn = Turn::default();
        let mut partial_json: HashMap<usize, String> = HashMap::new();

        while let Some((event, data)) = sse.next_event().context("reading the reply stream")? {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            let data: Value = match serde_json::from_str(&data) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match event.as_str() {
                "message_start" => {
                    let message = &data["message"];
                    on_event(ChatEvent::ReplyStarted {
                        model: message["model"].as_str().unwrap_or_default().to_owned(),
                    });
                    turn.add_usage(&message["usage"]);
                }
                "content_block_start" => {
                    let index = data["index"].as_u64().unwrap_or(0) as usize;
                    let block = data["content_block"].clone();
                    if block["type"] == "tool_use" {
                        partial_json.insert(index, String::new());
                    }
                    turn.set_block(index, block);
                }
                "content_block_delta" => {
                    let index = data["index"].as_u64().unwrap_or(0) as usize;
                    let delta = &data["delta"];
                    let Some(block) = turn.blocks.get_mut(&index) else { continue };
                    match delta["type"].as_str().unwrap_or_default() {
                        "text_delta" => {
                            let text = delta["text"].as_str().unwrap_or_default();
                            append(block, "text", text);
                            on_event(ChatEvent::TextDelta(text.to_owned()));
                        }
                        "thinking_delta" => {
                            let text = delta["thinking"].as_str().unwrap_or_default();
                            append(block, "thinking", text);
                            if !text.is_empty() {
                                on_event(ChatEvent::NoteDelta(text.to_owned()));
                            }
                        }
                        "signature_delta" => {
                            append(block, "signature", delta["signature"].as_str().unwrap_or_default());
                        }
                        "input_json_delta" => {
                            if let Some(buf) = partial_json.get_mut(&index) {
                                buf.push_str(delta["partial_json"].as_str().unwrap_or_default());
                            }
                        }
                        _ => {}
                    }
                }
                "content_block_stop" => {
                    let index = data["index"].as_u64().unwrap_or(0) as usize;
                    if let (Some(raw), Some(block)) = (partial_json.remove(&index), turn.blocks.get_mut(&index)) {
                        // Eager streaming skips server-side validation, so parse strictly here.
                        match parse_tool_input(&raw) {
                            Ok(input) => block["input"] = input,
                            Err(_) => {
                                block["input"] = json!({});
                                turn.invalid_inputs.insert(block["id"].as_str().unwrap_or_default().to_owned(), raw);
                            }
                        }
                    }
                }
                "message_delta" => {
                    if let Some(reason) = data["delta"]["stop_reason"].as_str() {
                        turn.stop_reason = reason.to_owned();
                    }
                    turn.add_usage(&data["usage"]);
                }
                "message_stop" => break,
                "error" => {
                    let message = data["error"]["message"].as_str().unwrap_or("unknown error");
                    bail!("the API reported an error mid-reply: {message}");
                }
                _ => {}
            }
        }
        on_event(ChatEvent::Usage(turn.usage));
        Ok(turn)
    }
}

impl ChatSession for AnthropicChat {
    fn send(
        &mut self,
        text: &str,
        tools: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(ChatEvent),
        cancel: &AtomicBool,
    ) -> Result<()> {
        if self.settings.api_key.trim().is_empty() {
            bail!("no Anthropic API key set. Add one in the chat settings.");
        }
        self.push_user_content(vec![json!({ "type": "text", "text": text })]);
        let definitions = tools.definitions();

        for _ in 0..MAX_ROUNDS {
            let body = self.request_body(&definitions);
            let turn = self.stream_turn(&body, on_event, cancel)?;
            let content = turn.content();

            match turn.stop_reason.as_str() {
                "refusal" => {
                    // Partial output from a declined reply is discarded, not replayed.
                    on_event(ChatEvent::Notice(
                        "The model declined this request. Try rewording it.".to_owned(),
                    ));
                    on_event(ChatEvent::Done);
                    return Ok(());
                }
                // The server paused a long turn. Sending the history again resumes it.
                "pause_turn" => {
                    self.messages.push(json!({ "role": "assistant", "content": echo_content(&content) }));
                    continue;
                }
                _ => {}
            }

            self.messages.push(json!({ "role": "assistant", "content": echo_content(&content) }));
            let calls = tool_calls(&content);

            if turn.stop_reason == "tool_use" && !calls.is_empty() {
                let results = run_tools(&calls, &turn.invalid_inputs, tools, on_event, cancel);
                self.push_user_content(results);
                if cancel.load(Ordering::Relaxed) {
                    return Err(Cancelled.into());
                }
                continue;
            }

            // Any other stop. Tool calls in a cut-off reply are answered but not run,
            // so the history stays valid for the next message.
            if !calls.is_empty() {
                let skipped = calls
                    .iter()
                    .map(|(id, _, _)| tool_error(id, "Not run: the reply was cut off before this call finished."))
                    .collect();
                self.push_user_content(skipped);
            }
            match turn.stop_reason.as_str() {
                "end_turn" | "stop_sequence" | "tool_use" => {}
                "max_tokens" => on_event(ChatEvent::Notice("The reply hit the length limit and was cut off.".into())),
                "model_context_window_exceeded" => on_event(ChatEvent::Notice(
                    "The conversation is too long for the model. Start a new chat.".into(),
                )),
                other => on_event(ChatEvent::Notice(format!("The reply stopped early ({other})."))),
            }
            on_event(ChatEvent::Done);
            return Ok(());
        }

        on_event(ChatEvent::Notice(format!("Stopped after {MAX_ROUNDS} tool rounds in one reply.")));
        on_event(ChatEvent::Done);
        Ok(())
    }

    fn reset(&mut self) {
        self.messages.clear();
    }
}

impl AnthropicChat {
    /// Adds to the trailing user message if there is one, so roles keep alternating
    /// (for example after Stop left tool results as the last message).
    fn push_user_content(&mut self, mut blocks: Vec<Value>) {
        if let Some(last) = self.messages.last_mut() {
            if last["role"] == "user" {
                if let Some(content) = last["content"].as_array_mut() {
                    content.append(&mut blocks);
                    return;
                }
            }
        }
        self.messages.push(json!({ "role": "user", "content": blocks }));
    }
}

#[derive(Default)]
struct Turn {
    blocks: std::collections::BTreeMap<usize, Value>,
    stop_reason: String,
    usage: Usage,
    /// tool_use id -> raw text that wasn't valid JSON
    invalid_inputs: HashMap<String, String>,
}

impl Turn {
    fn set_block(&mut self, index: usize, block: Value) {
        self.blocks.insert(index, block);
    }

    fn content(&self) -> Vec<Value> {
        self.blocks.values().cloned().collect()
    }

    fn add_usage(&mut self, usage: &Value) {
        let get = |key: &str| usage[key].as_u64();
        if let Some(v) = get("input_tokens") {
            self.usage.input_tokens = v;
        }
        if let Some(v) = get("cache_read_input_tokens") {
            self.usage.cache_read_tokens = v;
        }
        if let Some(v) = get("cache_creation_input_tokens") {
            self.usage.cache_write_tokens = v;
        }
        if let Some(v) = get("output_tokens") {
            self.usage.output_tokens = v;
        }
    }
}

fn append(block: &mut Value, key: &str, text: &str) {
    let current = block[key].as_str().unwrap_or_default();
    block[key] = Value::String(format!("{current}{text}"));
}

fn parse_tool_input(raw: &str) -> Result<Value> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(raw)?;
    if !value.is_object() {
        bail!("tool input is not an object");
    }
    Ok(value)
}

/// Index of the last `fallback` block, if the reply switched models part way.
fn fallback_boundary(content: &[Value]) -> Option<usize> {
    content.iter().rposition(|b| b["type"] == "fallback")
}

/// The assistant content to send back next time. After a mid-reply fallback, only
/// text survives from before the boundary, and the boundary block stays in place.
fn echo_content(content: &[Value]) -> Vec<Value> {
    let Some(boundary) = fallback_boundary(content) else { return content.to_vec() };
    content
        .iter()
        .enumerate()
        .filter(|(i, b)| *i >= boundary || b["type"] == "text")
        .map(|(_, b)| b.clone())
        .collect()
}

/// (id, name, input) for each tool call the model actually made in this reply.
fn tool_calls(content: &[Value]) -> Vec<(String, String, Value)> {
    let start = fallback_boundary(content).map_or(0, |i| i + 1);
    content[start..]
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| {
            (
                b["id"].as_str().unwrap_or_default().to_owned(),
                b["name"].as_str().unwrap_or_default().to_owned(),
                b["input"].clone(),
            )
        })
        .collect()
}

fn run_tools(
    calls: &[(String, String, Value)],
    invalid_inputs: &HashMap<String, String>,
    tools: &mut dyn ToolExecutor,
    on_event: &mut dyn FnMut(ChatEvent),
    cancel: &AtomicBool,
) -> Vec<Value> {
    let mut results = Vec::new();
    for (id, name, input) in calls {
        // Every call needs a result, even when we don't run it.
        if cancel.load(Ordering::Relaxed) {
            results.push(tool_error(id, "Not run: the user pressed Stop."));
            continue;
        }
        if let Some(raw) = invalid_inputs.get(id) {
            let message = json!({ "INVALID_JSON": raw }).to_string();
            on_event(ChatEvent::ToolStarted { id: id.clone(), name: name.clone(), input: json!({}) });
            on_event(ChatEvent::ToolFinished { id: id.clone(), ok: false, output: Value::String(message.clone()) });
            results.push(tool_error(id, &message));
            continue;
        }

        on_event(ChatEvent::ToolStarted { id: id.clone(), name: name.clone(), input: input.clone() });
        let (ok, output) = match tools.call(name, input) {
            Ok(value) => (true, value),
            Err(e) => (false, Value::String(format!("{e:#}"))),
        };
        on_event(ChatEvent::ToolFinished { id: id.clone(), ok, output: output.clone() });

        let text = match &output {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        let mut result = Map::new();
        result.insert("type".into(), "tool_result".into());
        result.insert("tool_use_id".into(), id.clone().into());
        result.insert("content".into(), text.into());
        if !ok {
            result.insert("is_error".into(), true.into());
        }
        results.push(Value::Object(result));
    }
    results
}

fn tool_error(id: &str, message: &str) -> Value {
    json!({ "type": "tool_result", "tool_use_id": id, "content": message, "is_error": true })
}

/// INTERCRAFT_ANTHROPIC_URL points the client at a local mock server for testing.
fn api_url() -> String {
    std::env::var("INTERCRAFT_ANTHROPIC_URL").unwrap_or_else(|_| API_URL.to_owned())
}

fn api_error(status: u16, body: &str) -> anyhow::Error {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(300).collect());
    match status {
        401 => anyhow!("the Anthropic API rejected the API key ({message})"),
        403 => anyhow!("this API key can't use that model ({message})"),
        429 => anyhow!("rate limited by the Anthropic API, try again shortly ({message})"),
        529 => anyhow!("the Anthropic API is overloaded, try again shortly"),
        _ => anyhow!("Anthropic API error {status}: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_drops_pre_fallback_internals() {
        let content = vec![
            json!({"type": "thinking", "thinking": "", "signature": "a"}),
            json!({"type": "text", "text": "partial"}),
            json!({"type": "tool_use", "id": "t1", "name": "x", "input": {}}),
            json!({"type": "fallback", "from": {}, "to": {}}),
            json!({"type": "thinking", "thinking": "", "signature": "b"}),
            json!({"type": "tool_use", "id": "t2", "name": "y", "input": {}}),
        ];
        let echoed: Vec<_> = echo_content(&content).iter().map(|b| b["type"].as_str().unwrap().to_owned()).collect();
        assert_eq!(echoed, ["text", "fallback", "thinking", "tool_use"]);
        let calls: Vec<_> = tool_calls(&content).into_iter().map(|c| c.0).collect();
        assert_eq!(calls, ["t2"]);
    }

    #[test]
    fn echo_keeps_everything_without_fallback() {
        let content = vec![json!({"type": "thinking"}), json!({"type": "tool_use", "id": "t1"})];
        assert_eq!(echo_content(&content).len(), 2);
    }

    #[test]
    fn tool_input_must_be_an_object() {
        assert_eq!(parse_tool_input("").unwrap(), json!({}));
        assert!(parse_tool_input("{\"a\": 1").is_err());
        assert!(parse_tool_input("[1]").is_err());
    }

    #[test]
    fn trailing_user_message_is_extended() {
        let mut chat = AnthropicChat::new(AnthropicSettings {
            api_key: String::new(),
            model: "claude-opus-5-5".into(),
            effort: "high".into(),
            system: String::new(),
        });
        chat.push_user_content(vec![json!({"type": "text", "text": "a"})]);
        chat.push_user_content(vec![json!({"type": "text", "text": "b"})]);
        assert_eq!(chat.messages.len(), 1);
        assert_eq!(chat.messages[0]["content"].as_array().unwrap().len(), 2);
    }
}
