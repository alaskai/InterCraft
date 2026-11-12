// Stress tests for the Anthropic client against a local fake of the Messages API.
// Every scenario is followed by a normal message, and the fake server rejects any
// request whose history is malformed, so a scenario that "breaks" the chat fails here.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use intercraft_llm::{AnthropicChat, AnthropicSettings, ChatEvent, ChatSession, ToolDef, ToolExecutor};
use serde_json::{json, Value};

// ---------- fake server ----------

struct Fake {
    requests: Mutex<Vec<Value>>,
    attempts: Mutex<HashMap<String, u32>>,
}

fn fake() -> &'static Fake {
    static FAKE: OnceLock<Fake> = OnceLock::new();
    FAKE.get_or_init(|| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::env::set_var("INTERCRAFT_ANTHROPIC_URL", format!("http://{addr}/v1/messages"));
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                std::thread::spawn(move || handle(stream));
            }
        });
        Fake { requests: Mutex::new(Vec::new()), attempts: Mutex::new(HashMap::new()) }
    })
}

/// Requests made for `token` (its newest scenario), in arrival order. The follow-up
/// message's requests belong to the follow-up's own token.
fn requests_for(token: &str) -> Vec<Value> {
    fake().requests.lock().unwrap().iter().filter(|r| scenario(&r["messages"]) == token).cloned().collect()
}

fn handle(mut stream: TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    fake().requests.lock().unwrap().push(body.clone());

    if let Err(problem) = check_history(&body["messages"]) {
        return respond_error(&mut stream, 400, &format!("invalid history: {problem}"));
    }
    let token = scenario(&body["messages"]);
    let round = body["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "assistant").count();
    let attempt = {
        let mut attempts = fake().attempts.lock().unwrap();
        let n = attempts.entry(format!("{token}/{round}")).or_insert(0);
        *n += 1;
        *n - 1
    };
    let name = token.split('#').next().unwrap_or_default().trim_start_matches("scenario:").to_owned();
    play(&mut stream, &name, round, attempt);
}

/// The same rules the real API enforces on history.
fn check_history(messages: &Value) -> Result<(), String> {
    let messages = messages.as_array().ok_or("messages is not a list")?;
    if messages.first().map(|m| &m["role"]) != Some(&json!("user")) {
        return Err("first message is not from the user".into());
    }
    for (i, pair) in messages.windows(2).enumerate() {
        if pair[0]["role"] == pair[1]["role"] {
            return Err(format!("messages {i} and {} have the same role", i + 1));
        }
    }
    for (i, message) in messages.iter().enumerate() {
        let content = message["content"].as_array().ok_or("content is not a list")?;
        if message["role"] == "assistant" {
            let calls: Vec<&str> =
                content.iter().filter(|b| b["type"] == "tool_use").filter_map(|b| b["id"].as_str()).collect();
            let Some(next) = messages.get(i + 1) else { continue };
            let results: Vec<&str> = next["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|b| b["type"] == "tool_result")
                .filter_map(|b| b["tool_use_id"].as_str())
                .collect();
            for id in &calls {
                if results.iter().filter(|r| *r == id).count() != 1 {
                    return Err(format!("tool call {id} needs exactly one result"));
                }
            }
            if let Some(stray) = results.iter().find(|r| !calls.contains(r)) {
                return Err(format!("result for unknown tool call {stray}"));
            }
        } else if i > 0 && messages[i - 1]["role"] != "assistant" {
            unreachable!()
        } else if i == 0 || messages[i - 1]["role"] == "user" {
            if content.iter().any(|b| b["type"] == "tool_result") {
                return Err("tool result without a preceding tool call".into());
            }
        }
    }
    Ok(())
}

/// The newest "scenario:" token in the user's text.
fn scenario(messages: &Value) -> String {
    let mut found = String::new();
    for message in messages.as_array().unwrap().iter().filter(|m| m["role"] == "user") {
        for block in message["content"].as_array().unwrap() {
            if let Some(text) = block["text"].as_str() {
                if let Some(at) = text.rfind("scenario:") {
                    found = text[at..].split_whitespace().next().unwrap_or_default().to_owned();
                }
            }
        }
    }
    found
}

fn respond_error(stream: &mut TcpStream, status: u16, message: &str) {
    let body = json!({ "type": "error", "error": { "type": "test_error", "message": message } }).to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Error\r\ncontent-type: application/json\r\nretry-after: 0\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
}

struct Sse<'a> {
    stream: &'a mut TcpStream,
}

impl Sse<'_> {
    fn open(stream: &mut TcpStream) -> Sse<'_> {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n"
        );
        Sse { stream }
    }
    fn raw(&mut self, text: &str) {
        let _ = write!(self.stream, "{:x}\r\n{text}\r\n", text.len());
        let _ = self.stream.flush();
    }
    fn event(&mut self, name: &str, data: Value) {
        self.raw(&format!("event: {name}\ndata: {data}\n\n"));
    }
    fn start(&mut self) {
        self.event("message_start", json!({"type": "message_start", "message": {
            "model": "fake-model", "usage": {"input_tokens": 10, "cache_read_input_tokens": 5, "output_tokens": 1}}}));
    }
    fn text(&mut self, index: u32, pieces: &[&str]) {
        self.event("content_block_start", json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}));
        for piece in pieces {
            self.event("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": piece}}));
        }
        self.event("content_block_stop", json!({"type": "content_block_stop", "index": index}));
    }
    fn tool(&mut self, index: u32, id: &str, name: &str, pieces: &[&str]) {
        self.event("content_block_start", json!({"type": "content_block_start", "index": index,
            "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}));
        for piece in pieces {
            self.event("content_block_delta", json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": piece}}));
        }
        self.event("content_block_stop", json!({"type": "content_block_stop", "index": index}));
    }
    fn block(&mut self, index: u32, block: Value) {
        self.event("content_block_start", json!({"type": "content_block_start", "index": index, "content_block": block}));
        self.event("content_block_stop", json!({"type": "content_block_stop", "index": index}));
    }
    fn end(&mut self, stop_reason: &str) {
        self.event("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": stop_reason}, "usage": {"output_tokens": 20}}));
        self.event("message_stop", json!({"type": "message_stop"}));
        self.raw_end();
    }
    fn raw_end(&mut self) {
        let _ = write!(self.stream, "0\r\n\r\n");
    }
}

fn play(stream: &mut TcpStream, name: &str, round: usize, attempt: u32) {
    match name {
        "http401" => return respond_error(stream, 401, "invalid x-api-key"),
        "always500" => return respond_error(stream, 500, "internal error"),
        "overload" if attempt == 0 => return respond_error(stream, 529, "overloaded"),
        _ => {}
    }
    let mut sse = Sse::open(stream);
    sse.start();
    match (name, round) {
        ("tools", 0) => {
            sse.tool(0, "t1", "add_part", &["{\"na", "me\": \"A\"}"]);
            sse.end("tool_use");
        }
        ("parallel", 0) => {
            sse.tool(0, "t1", "add_part", &["{\"name\": \"A\"}"]);
            sse.tool(1, "t2", "add_part", &["{\"name\": \"B\"}"]);
            sse.end("tool_use");
        }
        ("bad_json", 0) => {
            sse.tool(0, "t1", "add_part", &["{\"name\": \"A"]);
            sse.end("tool_use");
        }
        ("tool_error", 0) => {
            sse.tool(0, "t1", "explode", &["{}"]);
            sse.end("tool_use");
        }
        ("loop", r) => {
            sse.tool(0, &format!("t{r}"), "add_part", &["{\"name\": \"L\"}"]);
            sse.end("tool_use");
        }
        ("drop", _) => {
            sse.text(0, &["partial answ"]);
            // connection closes here with no message_stop
        }
        ("sse_error", _) => {
            sse.text(0, &["start"]);
            sse.event("error", json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}));
            sse.raw_end();
        }
        ("refusal", _) => {
            sse.text(0, &["I'll start"]);
            sse.end("refusal");
        }
        ("max_tokens_tool", 0) => {
            sse.tool(0, "t1", "add_part", &["{\"name\": \"Cut"]);
            sse.end("max_tokens");
        }
        ("pause", 0) => {
            sse.text(0, &["part one"]);
            sse.end("pause_turn");
        }
        ("fallback", 0) => {
            sse.block(0, json!({"type": "thinking", "thinking": "", "signature": "s1"}));
            sse.tool(1, "t_old", "add_part", &["{\"name\": \"Old\"}"]);
            sse.block(2, json!({"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}}));
            sse.tool(3, "t_new", "add_part", &["{\"name\": \"New\"}"]);
            sse.end("tool_use");
        }
        ("slow", _) => {
            sse.event("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}));
            for _ in 0..100 {
                sse.event("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "."}}));
                std::thread::sleep(Duration::from_millis(100));
            }
            sse.event("content_block_stop", json!({"type": "content_block_stop", "index": 0}));
            sse.end("end_turn");
        }
        ("garbage", _) => {
            sse.raw("data: this is not json\n\n");
            sse.raw(": a comment line\n\n");
            sse.raw("event: mystery_event\ndata: {\"type\":\"mystery\"}\n\n");
            sse.event("content_block_delta", json!({"type": "content_block_delta", "index": 99, "delta": {"type": "text_delta", "text": "orphan"}}));
            sse.event("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "brand_new_delta"}}));
            sse.text(0, &["survived"]);
            sse.end("end_turn");
        }
        ("huge", _) => {
            let chunk = "Wörter mit Ümlauten, 日本語, emoji 🔩🛠️ and plain text. ".repeat(500);
            let pieces: Vec<&str> = std::iter::repeat_n(chunk.as_str(), 60).collect();
            sse.text(0, &pieces);
            sse.end("end_turn");
        }
        _ => {
            sse.text(0, &["done"]);
            sse.end("end_turn");
        }
    }
}

// ---------- client side ----------

struct Tools {
    calls: Vec<(String, Value)>,
}

impl ToolExecutor for Tools {
    fn definitions(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "add_part".into(),
            description: "test".into(),
            input_schema: json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        }]
    }
    fn call(&mut self, name: &str, input: &Value) -> anyhow::Result<Value> {
        self.calls.push((name.to_owned(), input.clone()));
        if name == "explode" {
            anyhow::bail!("the tool blew up");
        }
        Ok(json!({"ok": true}))
    }
}

struct Run {
    result: anyhow::Result<()>,
    events: Vec<ChatEvent>,
    tools: Tools,
    chat: AnthropicChat,
}

fn new_chat() -> AnthropicChat {
    fake();
    AnthropicChat::new(AnthropicSettings {
        api_key: "test-key".into(),
        model: "claude-opus-5-5".into(),
        effort: "high".into(),
        system: "test".into(),
    })
}

fn send(chat: &mut AnthropicChat, text: &str) -> (anyhow::Result<()>, Vec<ChatEvent>, Tools) {
    let mut tools = Tools { calls: Vec::new() };
    let mut events = Vec::new();
    let cancel = AtomicBool::new(false);
    let result = chat.send(text, &mut tools, &mut |e| events.push(e), &cancel);
    (result, events, tools)
}

/// Runs a scenario, then a normal follow-up message that must succeed.
fn run(token: &str) -> Run {
    let mut chat = new_chat();
    let (result, events, tools) = send(&mut chat, token);
    let follow_up = format!("scenario:text#after-{}", &token["scenario:".len()..]);
    let (after, _, _) = send(&mut chat, &follow_up);
    assert!(after.is_ok(), "{token}: the chat was broken afterwards: {after:?}");
    Run { result, events, tools, chat }
}

fn notices(events: &[ChatEvent]) -> Vec<String> {
    events.iter().filter_map(|e| if let ChatEvent::Notice(t) = e { Some(t.clone()) } else { None }).collect()
}

fn done(events: &[ChatEvent]) -> bool {
    events.iter().any(|e| matches!(e, ChatEvent::Done))
}

#[test]
fn normal_tool_round_trip() {
    let r = run("scenario:tools#a");
    assert!(r.result.is_ok());
    assert_eq!(r.tools.calls, vec![("add_part".to_owned(), json!({"name": "A"}))]);
    assert!(done(&r.events));
}

#[test]
fn parallel_calls_get_results_in_one_message() {
    let r = run("scenario:parallel#a");
    assert!(r.result.is_ok());
    assert_eq!(r.tools.calls.len(), 2);
    let second = &requests_for("scenario:parallel#a")[1];
    let last = second["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["content"].as_array().unwrap().len(), 2);
}

#[test]
fn invalid_tool_json_is_never_executed() {
    let r = run("scenario:bad_json#a");
    assert!(r.result.is_ok());
    assert!(r.tools.calls.is_empty(), "ran a tool with broken input");
    let second = requests_for("scenario:bad_json#a")[1].to_string();
    assert!(second.contains("INVALID_JSON") && second.contains("\"is_error\":true"));
}

#[test]
fn tool_errors_go_back_to_the_model() {
    let r = run("scenario:tool_error#a");
    assert!(r.result.is_ok());
    let second = requests_for("scenario:tool_error#a")[1].to_string();
    assert!(second.contains("the tool blew up") && second.contains("\"is_error\":true"));
}

#[test]
fn bad_key_fails_fast_without_retrying() {
    let r = run("scenario:http401#a");
    let error = format!("{:#}", r.result.unwrap_err());
    assert!(error.contains("rejected the API key"), "{error}");
    assert!(!error.contains("test-key"), "error message leaked the key: {error}");
    assert_eq!(requests_for("scenario:http401#a").len(), 1);
}

#[test]
fn overload_is_retried() {
    let r = run("scenario:overload#a");
    assert!(r.result.is_ok(), "{:?}", r.result);
    assert_eq!(requests_for("scenario:overload#a").len(), 2);
}

#[test]
fn server_errors_give_up_after_retries() {
    let r = run("scenario:always500#a");
    assert!(r.result.is_err());
    assert_eq!(requests_for("scenario:always500#a").len(), 4); // first try + 3 retries
}

#[test]
fn dropped_connection_is_an_error_not_a_reply() {
    let r = run("scenario:drop#a");
    let error = format!("{:#}", r.result.as_ref().unwrap_err());
    assert!(error.contains("closed before the reply finished") || error.contains("reading"), "{error}");
    let history = r.chat_history_text();
    assert!(!history.contains("partial answ"), "half a reply was kept in the history");
}

#[test]
fn stream_error_event_is_reported() {
    let r = run("scenario:sse_error#a");
    assert!(format!("{:#}", r.result.unwrap_err()).contains("Overloaded"));
}

#[test]
fn refusal_is_shown_and_discarded() {
    let r = run("scenario:refusal#a");
    assert!(r.result.is_ok());
    assert!(notices(&r.events).iter().any(|n| n.contains("declined")));
    assert!(!r.chat_history_text().contains("I'll start"), "refused partial reply was kept");
}

#[test]
fn cut_off_tool_call_is_not_run() {
    let r = run("scenario:max_tokens_tool#a");
    assert!(r.result.is_ok());
    assert!(r.tools.calls.is_empty());
    assert!(notices(&r.events).iter().any(|n| n.contains("length limit")));
}

#[test]
fn endless_tool_loop_is_capped() {
    let r = run("scenario:loop#a");
    assert!(r.result.is_ok());
    assert_eq!(r.tools.calls.len(), 40);
    assert!(notices(&r.events).iter().any(|n| n.contains("Stopped after 40")));
}

#[test]
fn paused_turn_resumes() {
    let r = run("scenario:pause#a");
    assert!(r.result.is_ok());
    assert_eq!(requests_for("scenario:pause#a").len(), 2); // paused, then resumed
}

#[test]
fn fallback_drops_the_declined_tool_call() {
    let r = run("scenario:fallback#a");
    assert!(r.result.is_ok());
    assert_eq!(r.tools.calls, vec![("add_part".to_owned(), json!({"name": "New"}))]);
    let second = requests_for("scenario:fallback#a")[1].to_string();
    assert!(!second.contains("t_old") && !second.contains("\"signature\":\"s1\""));
}

#[test]
fn stop_ends_a_slow_reply_quickly() {
    let mut chat = new_chat();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(500));
        flag.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let mut tools = Tools { calls: Vec::new() };
    let result = chat.send("scenario:slow#a", &mut tools, &mut |_| {}, &cancel);
    assert!(result.unwrap_err().is::<intercraft_llm::Cancelled>());
    assert!(started.elapsed() < Duration::from_secs(3), "Stop took {:?}", started.elapsed());
    let (after, _, _) = send(&mut chat, "scenario:text#after-slow");
    assert!(after.is_ok(), "{after:?}");
}

#[test]
fn junk_in_the_stream_is_ignored() {
    let r = run("scenario:garbage#a");
    assert!(r.result.is_ok());
    let text: String = r.events.iter().filter_map(|e| if let ChatEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect();
    assert!(text.contains("survived"));
}

#[test]
fn huge_unicode_reply_is_handled() {
    let r = run("scenario:huge#a");
    assert!(r.result.is_ok());
    let total: usize = r.events.iter().map(|e| if let ChatEvent::TextDelta(t) = e { t.len() } else { 0 }).sum();
    assert!(total > 1_000_000, "only {total} bytes arrived");
}

#[test]
fn hostile_user_text_is_just_text() {
    let nasty = "scenario:text#nasty \u{0} \"}]} </s> \r\n\r\nHTTP/1.1 200 OK\r\n {{{{ \u{202e}reversed \u{1F4A5}";
    let r = run(nasty);
    assert!(r.result.is_ok());
}

#[test]
fn many_messages_in_one_chat() {
    let mut chat = new_chat();
    for i in 0..25 {
        let (result, _, _) = send(&mut chat, &format!("scenario:tools#many{i}"));
        assert!(result.is_ok(), "message {i}: {result:?}");
    }
}

impl Run {
    /// Everything the chat would send next time, as text.
    fn chat_history_text(&self) -> String {
        let last = fake().requests.lock().unwrap().last().cloned().unwrap_or_default();
        let _ = &self.chat;
        last["messages"].to_string()
    }
}

#[test]
fn pictures_travel_with_the_message() {
    let mut chat = new_chat();
    let mut tools = Tools { calls: Vec::new() };
    let images = ["QUJD".to_owned()];
    let token = "scenario:text#picture";
    chat.send_with_images(token, &images, &mut tools, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let request = &requests_for(token)[0];
    let content = request["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["source"], json!({ "type": "base64", "media_type": "image/png", "data": "QUJD" }));
    assert_eq!(content[1]["text"], token);
    // The picture stays in the history, and the next message is accepted.
    let (after, _, _) = send(&mut chat, "scenario:text#after-picture");
    after.unwrap();
}

#[test]
fn each_request_reports_its_cost() {
    let r = run("scenario:text#cost");
    r.result.unwrap();
    let usage = r.events.iter().find_map(|e| if let ChatEvent::Usage(u) = e { Some(*u) } else { None }).expect("usage");
    let cost = r.events.iter().find_map(|e| if let ChatEvent::Cost(c) = e { Some(*c) } else { None }).expect("a cost");
    let price = intercraft_llm::anthropic_price("claude-opus-5-5", &usage).unwrap();
    assert!((cost - intercraft_llm::cost(price, &usage)).abs() < 1e-12);
}
