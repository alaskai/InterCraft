// Any server that speaks the OpenAI Chat Completions API: OpenAI itself, OpenRouter,
// and local models through Ollama or LM Studio. Raw HTTP + SSE, like the Anthropic client.

use std::io::BufReader;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::sse::SseReader;
use crate::{Cancelled, ChatEvent, ChatSession, ToolExecutor, Usage};

/// Stops a confused model from looping on tool calls forever.
const MAX_ROUNDS: usize = 40;
const MAX_RETRIES: u32 = 3;

/// Common servers, as (name, base URL), offered as shortcuts in the settings.
pub const OPENAI_PRESETS: &[(&str, &str)] = &[
    ("OpenAI", "https://api.openai.com/v1"),
    ("OpenRouter", "https://openrouter.ai/api/v1"),
    ("Ollama (this computer)", "http://localhost:11434/v1"),
    ("LM Studio (this computer)", "http://localhost:1234/v1"),
];

#[derive(Clone, Default)]
pub struct OpenAiSettings {
    /// Up to and including the version, e.g. "https://api.openai.com/v1".
    pub base_url: String,
    /// Empty for local servers that don't need one.
    pub api_key: String,
    pub model: String,
    pub system: String,
    /// The model can read images, so `view_model` pictures are sent to it.
    pub vision: bool,
    /// US dollars per million input and output tokens, as the user entered them.
    /// Zero for both means unknown, and no cost is shown.
    pub price_input: f64,
    pub price_output: f64,
}

// Written by hand so the key never ends up in a log or error message.
impl std::fmt::Debug for OpenAiSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiSettings")
            .field("base_url", &self.base_url)
            .field("api_key", &if self.api_key.is_empty() { "<none>" } else { "<redacted>" })
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

/// The address to post to, or why this base URL can't be used. A key is only ever
/// sent over HTTPS, or to a server on this computer.
pub fn completions_url(base_url: &str) -> Result<String> {
    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        bail!("set the server address in Settings, for example https://api.openai.com/v1");
    }
    let secure = base.starts_with("https://");
    let local = base.starts_with("http://") && crate::anthropic::is_local_url(base);
    if !secure && !local {
        bail!("the server address must start with https://. Plain http:// is only allowed for a server on this computer (localhost)");
    }
    Ok(format!("{base}/chat/completions"))
}

pub struct OpenAiChat {
    settings: OpenAiSettings,
    agent: ureq::Agent,
    /// The conversation in API form, without the system message.
    messages: Vec<Value>,
}

/// A tool call as it arrives, in pieces.
#[derive(Default, Clone)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct Turn {
    text: String,
    calls: Vec<PartialCall>,
    finish: Option<String>,
}

impl OpenAiChat {
    pub fn new(settings: OpenAiSettings) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(30)))
            // A local model can take a while to load before its first token.
            .timeout_per_read(Some(Duration::from_secs(300)))
            .build()
            .into();
        Self { settings, agent, messages: Vec::new() }
    }

    /// Change server, model or key. A different server or model starts a new
    /// conversation, because tool call ids and formats don't carry over reliably.
    pub fn update_settings(&mut self, settings: OpenAiSettings) {
        if settings.base_url != self.settings.base_url || settings.model != self.settings.model {
            self.messages.clear();
        }
        self.settings = settings;
    }

    fn request_body(&self, tools: &[crate::ToolDef]) -> Value {
        let mut messages = vec![json!({ "role": "system", "content": self.settings.system })];
        messages.extend(self.messages.iter().cloned());
        let tools: Vec<Value> = tools
            .iter()
            // Without vision a picture is useless, so the tool isn't offered at all.
            .filter(|t| self.settings.vision || t.name != "view_model")
            .map(|t| {
                json!({
                    "type": "function",
                    "function": { "name": t.name, "description": t.description, "parameters": t.input_schema },
                })
            })
            .collect();
        json!({
            "model": self.settings.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages,
            "tools": tools,
        })
    }

    /// Send the request, retrying overloads and rate limits before any output arrives.
    fn open_stream(&self, url: &str, body: &Value, cancel: &AtomicBool) -> Result<ureq::http::Response<ureq::Body>> {
        let mut attempt = 0;
        loop {
            let mut request = self.agent.post(url).header("content-type", "application/json");
            if !self.settings.api_key.is_empty() {
                request = request.header("authorization", &format!("Bearer {}", self.settings.api_key));
            }
            let (retry_after, error) = match request.send_json(body) {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    let retry_after =
                        response.headers().get("retry-after").and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
                    let text = response.body_mut().read_to_string().unwrap_or_default();
                    let error = api_error(status, &text);
                    if !matches!(status, 429 | 500 | 502 | 503 | 504) {
                        return Err(error);
                    }
                    (retry_after, error)
                }
                Err(e) => (None, anyhow!(e).context(format!("could not reach {url}. Is the server running?"))),
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
        url: &str,
        body: &Value,
        on_event: &mut dyn FnMut(ChatEvent),
        cancel: &AtomicBool,
    ) -> Result<Turn> {
        let response = self.open_stream(url, body, cancel)?;
        let mut sse = SseReader::new(BufReader::new(response.into_body().into_reader()));
        let mut turn = Turn::default();
        let mut done = false;
        on_event(ChatEvent::ReplyStarted { model: self.settings.model.clone() });

        while let Some((_, data)) = sse.next_event().context("reading the reply stream")? {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            if data.trim() == "[DONE]" {
                done = true;
                break;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(&data) else { continue };
            if let Some(error) = chunk.get("error").filter(|e| !e.is_null()) {
                let message = error["message"].as_str().map_or_else(|| error.to_string(), str::to_owned);
                bail!("the server reported an error: {message}");
            }
            if let Some(usage) = chunk.get("usage").filter(|u| u.is_object()) {
                let cached = usage["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
                let tokens = Usage {
                    input_tokens: usage["prompt_tokens"].as_u64().unwrap_or(0).saturating_sub(cached),
                    cache_read_tokens: cached,
                    cache_write_tokens: 0,
                    output_tokens: usage["completion_tokens"].as_u64().unwrap_or(0),
                };
                on_event(ChatEvent::Usage(tokens));
                if self.settings.price_input > 0.0 || self.settings.price_output > 0.0 {
                    // Servers price cached input differently, so it is counted as plain input here.
                    let price = crate::Price {
                        input: self.settings.price_input,
                        cache_write: self.settings.price_input,
                        cache_read: self.settings.price_input,
                        output: self.settings.price_output,
                    };
                    on_event(ChatEvent::Cost(crate::pricing::cost(price, &tokens)));
                }
            }
            let choice = &chunk["choices"][0];
            if let Some(text) = choice["delta"]["content"].as_str().filter(|t| !t.is_empty()) {
                turn.text.push_str(text);
                on_event(ChatEvent::TextDelta(text.to_owned()));
            }
            for piece in choice["delta"]["tool_calls"].as_array().into_iter().flatten() {
                // Pieces of one call share an index. Servers that send a call whole may leave it out.
                let index = piece["index"].as_u64().map_or(turn.calls.len().saturating_sub(1), |i| i as usize);
                let index = if piece["index"].is_null() && piece["id"].is_string() { turn.calls.len() } else { index };
                if turn.calls.len() <= index {
                    turn.calls.resize(index + 1, PartialCall::default());
                }
                let call = &mut turn.calls[index];
                if let Some(id) = piece["id"].as_str().filter(|s| !s.is_empty()) {
                    call.id = id.to_owned();
                }
                if let Some(name) = piece["function"]["name"].as_str() {
                    call.name.push_str(name);
                }
                match &piece["function"]["arguments"] {
                    Value::String(part) => call.arguments.push_str(part),
                    Value::Null => {}
                    // Some local servers send the arguments as an object, not as text.
                    whole => call.arguments = whole.to_string(),
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str() {
                turn.finish = Some(reason.to_owned());
            }
        }
        // A reply that just stops is a dropped connection, not a finished reply.
        if !done && turn.finish.is_none() {
            bail!("the connection dropped before the reply finished. Nothing from it was kept, so you can send again");
        }
        Ok(turn)
    }
}

impl ChatSession for OpenAiChat {
    fn send_with_images(
        &mut self,
        text: &str,
        images: &[String],
        tools: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(ChatEvent),
        cancel: &AtomicBool,
    ) -> Result<()> {
        let url = completions_url(&self.settings.base_url)?;
        if self.settings.model.trim().is_empty() {
            bail!("choose a model in Settings");
        }
        let definitions = tools.definitions();
        if images.is_empty() {
            self.messages.push(json!({ "role": "user", "content": text }));
        } else if self.settings.vision {
            let mut content = vec![json!({ "type": "text", "text": text })];
            content.extend(images.iter().map(|data| {
                json!({ "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{data}") } })
            }));
            self.messages.push(json!({ "role": "user", "content": content }));
        } else {
            // The words still describe the sketch, so a text-only model isn't left guessing.
            let note = "(A picture was attached, but this model is set as unable to read images, so only the description above was sent.)";
            self.messages.push(json!({ "role": "user", "content": format!("{text}

{note}") }));
        }

        for round in 0..MAX_ROUNDS {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            let turn = self.stream_turn(&url, &self.request_body(&definitions), on_event, cancel)?;

            let mut calls = turn.calls;
            calls.retain(|c| !c.name.is_empty());
            for (i, call) in calls.iter_mut().enumerate() {
                if call.id.is_empty() {
                    call.id = format!("call_{round}_{i}");
                }
            }
            let mut message = json!({ "role": "assistant", "content": turn.text });
            if !calls.is_empty() {
                let list: Vec<Value> = calls
                    .iter()
                    .map(|c| json!({ "id": c.id, "type": "function", "function": { "name": c.name, "arguments": c.arguments } }))
                    .collect();
                message["tool_calls"] = list.into();
            }
            self.messages.push(message);

            if calls.is_empty() {
                match turn.finish.as_deref() {
                    Some("length") => on_event(ChatEvent::Notice("The reply was cut off at the model's output limit.".into())),
                    Some("content_filter") => on_event(ChatEvent::Notice("The server's content filter stopped the reply.".into())),
                    _ => {}
                }
                on_event(ChatEvent::Done);
                return Ok(());
            }

            let mut pictures = Vec::new();
            for call in &calls {
                let content = self.run_tool(call, tools, on_event, cancel, &mut pictures);
                self.messages.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            }
            // Tool messages can only hold text here, so pictures follow in a user message.
            if !pictures.is_empty() {
                let mut content = vec![json!({ "type": "text", "text": "Pictures from view_model, in the order they were asked for." })];
                content.extend(pictures.into_iter().map(|data| {
                    json!({ "type": "image_url", "image_url": { "url": format!("data:image/png;base64,{data}") } })
                }));
                self.messages.push(json!({ "role": "user", "content": content }));
            }
        }
        bail!("stopped after {MAX_ROUNDS} rounds of tool calls without a final answer")
    }

    fn reset(&mut self) {
        self.messages.clear();
    }
}

impl OpenAiChat {
    /// Run one tool call and return the text the model sees for it.
    fn run_tool(
        &self,
        call: &PartialCall,
        tools: &mut dyn ToolExecutor,
        on_event: &mut dyn FnMut(ChatEvent),
        cancel: &AtomicBool,
        pictures: &mut Vec<String>,
    ) -> String {
        // Every call needs an answer, even when it isn't run.
        if cancel.load(Ordering::Relaxed) {
            return "Not run: the user pressed Stop.".to_owned();
        }
        let arguments = if call.arguments.trim().is_empty() { "{}" } else { call.arguments.as_str() };
        let input = match serde_json::from_str::<Value>(arguments) {
            Ok(input) if input.is_object() => input,
            _ => {
                let message = "Error: the arguments were not a valid JSON object. Send the call again with valid JSON.".to_owned();
                on_event(ChatEvent::ToolStarted { id: call.id.clone(), name: call.name.clone(), input: json!({}) });
                on_event(ChatEvent::ToolFinished { id: call.id.clone(), ok: false, output: message.clone().into() });
                return message;
            }
        };
        on_event(ChatEvent::ToolStarted { id: call.id.clone(), name: call.name.clone(), input: input.clone() });
        let (ok, mut output) = match tools.call(&call.name, &input) {
            Ok(value) => (true, value),
            Err(e) => (false, Value::String(format!("{e:#}"))),
        };
        if let Some(Value::String(data)) = output.as_object_mut().and_then(|o| o.remove(crate::IMAGE_KEY)) {
            if self.settings.vision {
                pictures.push(data);
                output["image"] = "sent to the model".into();
            } else {
                output["image"] = "not sent: this model is set as unable to read images".into();
            }
        }
        on_event(ChatEvent::ToolFinished { id: call.id.clone(), ok, output: output.clone() });
        let text = match &output {
            Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).unwrap_or_default(),
        };
        if ok { text } else { format!("Error: {text}") }
    }
}

fn api_error(status: u16, body: &str) -> anyhow::Error {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().or(v["error"].as_str()).map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(300).collect());
    match status {
        401 => anyhow!("the server rejected the API key ({message})"),
        404 => anyhow!("the server doesn't know that address or model. Check both in Settings ({message})"),
        429 => anyhow!("rate limited, try again shortly ({message})"),
        _ => anyhow!("server error {status}: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_only_travel_over_https_or_to_this_computer() {
        assert_eq!(completions_url("https://api.openai.com/v1/").unwrap(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(completions_url(" http://localhost:11434/v1 ").unwrap(), "http://localhost:11434/v1/chat/completions");
        assert!(completions_url("http://127.0.0.1:1234/v1").is_ok());
        for bad in ["", "http://example.com/v1", "http://localhost.evil.com/v1", "ftp://x", "api.openai.com/v1", "http://192.168.1.5:11434/v1"] {
            assert!(completions_url(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn debug_never_prints_the_key() {
        let settings = OpenAiSettings { api_key: "sk-secret-123".into(), ..Default::default() };
        assert!(!format!("{settings:?}").contains("secret"));
    }
}
