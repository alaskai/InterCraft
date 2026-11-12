// Stress tests for the OpenAI-compatible client against a scripted local server.
// Each test gives the server the replies to send, in order, then checks both what
// the chat panel would hear and what the client sent back.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use intercraft_llm::{ChatEvent, ChatSession, OpenAiChat, OpenAiSettings, ToolDef, ToolExecutor, IMAGE_KEY};
use serde_json::{json, Value};

// ---------- scripted server ----------

enum Reply {
    /// Stream these chunks, then `[DONE]`.
    Stream(Vec<Value>),
    /// Stream these chunks, then close the connection without finishing.
    Dropped(Vec<Value>),
    Status(u16, &'static str),
}

struct Server {
    base_url: String,
    /// (authorization header, body) of every request, in order.
    requests: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Server {
    fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            let mut replies = replies.into_iter();
            for stream in listener.incoming().flatten() {
                let Some(request) = read_request(&stream) else { continue };
                seen.lock().unwrap().push(request);
                match replies.next() {
                    Some(reply) => answer(stream, reply),
                    None => answer(stream, Reply::Status(500, r#"{"error":{"message":"the test sent more requests than it scripted"}}"#)),
                }
            }
        });
        Self { base_url, requests }
    }

    fn body(&self, index: usize) -> Value {
        self.requests.lock().unwrap()[index].1.clone()
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

fn read_request(stream: &TcpStream) -> Option<(String, Value)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let (mut length, mut auth) = (0, String::new());
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            match key.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "authorization" => auth = value.trim().to_owned(),
                _ => {}
            }
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((auth, serde_json::from_slice(&body).ok()?))
}

fn answer(mut stream: TcpStream, reply: Reply) {
    let head = |status: u16, kind: &str| format!("HTTP/1.1 {status} X\r\nContent-Type: {kind}\r\nConnection: close\r\n\r\n");
    let _ = match reply {
        Reply::Status(status, body) => stream.write_all(format!("{}{body}", head(status, "application/json")).as_bytes()),
        Reply::Stream(chunks) => {
            let mut text = head(200, "text/event-stream");
            for chunk in chunks {
                text += &format!("data: {chunk}\n\n");
            }
            stream.write_all((text + "data: [DONE]\n\n").as_bytes())
        }
        Reply::Dropped(chunks) => {
            let mut text = head(200, "text/event-stream");
            for chunk in chunks {
                text += &format!("data: {chunk}\n\n");
            }
            stream.write_all(text.as_bytes())
        }
    };
}

// ---------- chunks the way servers send them ----------

fn text(part: &str) -> Value {
    json!({ "choices": [{ "index": 0, "delta": { "content": part } }] })
}

fn finish(reason: &str) -> Value {
    json!({ "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }] })
}

/// One piece of a streamed tool call. The id and name only come with the first piece.
fn call_piece(index: usize, id: Option<&str>, name: Option<&str>, arguments: &str) -> Value {
    let mut call = json!({ "index": index, "function": { "arguments": arguments } });
    if let Some(id) = id {
        call["id"] = id.into();
        call["type"] = "function".into();
    }
    if let Some(name) = name {
        call["function"]["name"] = name.into();
    }
    json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [call] } }] })
}

fn usage(prompt: u64, cached: u64, completion: u64) -> Value {
    json!({ "choices": [], "usage": { "prompt_tokens": prompt, "completion_tokens": completion, "prompt_tokens_details": { "cached_tokens": cached } } })
}

// ---------- the client under test ----------

#[derive(Default)]
struct Tools {
    calls: Vec<(String, Value)>,
}

impl ToolExecutor for Tools {
    fn definitions(&self) -> Vec<ToolDef> {
        ["add_part", "view_model"]
            .into_iter()
            .map(|name| ToolDef { name: name.into(), description: "test".into(), input_schema: json!({ "type": "object" }) })
            .collect()
    }

    fn call(&mut self, name: &str, input: &Value) -> anyhow::Result<Value> {
        self.calls.push((name.to_owned(), input.clone()));
        match name {
            "explode" => anyhow::bail!("the tool blew up"),
            "view_model" => Ok(json!({ "view": "iso", IMAGE_KEY: "QUJD" })),
            _ => Ok(json!({ "ok": true })),
        }
    }
}

struct Run {
    result: anyhow::Result<()>,
    events: Vec<ChatEvent>,
    tools: Tools,
}

impl Run {
    fn ok(&self) {
        assert!(self.result.is_ok(), "{:?}", self.result);
    }

    fn text(&self) -> String {
        self.events.iter().filter_map(|e| if let ChatEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect()
    }

    fn notices(&self) -> Vec<&str> {
        self.events.iter().filter_map(|e| if let ChatEvent::Notice(t) = e { Some(t.as_str()) } else { None }).collect()
    }
}

fn chat(server: &Server, vision: bool) -> OpenAiChat {
    OpenAiChat::new(OpenAiSettings {
        base_url: server.base_url.clone(),
        api_key: "sk-test-key".into(),
        model: "test-model".into(),
        system: "You edit CAD parts.".into(),
        vision,
    })
}

fn send(chat: &mut OpenAiChat, message: &str) -> Run {
    send_with(chat, message, &AtomicBool::new(false))
}

fn send_with(chat: &mut OpenAiChat, message: &str, cancel: &AtomicBool) -> Run {
    let mut events = Vec::new();
    let mut tools = Tools::default();
    let result = chat.send(message, &mut tools, &mut |e| events.push(e), cancel);
    Run { result, events, tools }
}

/// Checks a request the way a strict server would: every tool call is answered by
/// exactly one tool message before anything else follows.
fn assert_history_is_valid(body: &Value) {
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    let mut waiting: Vec<String> = Vec::new();
    for message in &messages[1..] {
        match message["role"].as_str().unwrap() {
            "tool" => {
                let id = message["tool_call_id"].as_str().unwrap().to_owned();
                let at = waiting.iter().position(|w| *w == id).unwrap_or_else(|| panic!("tool message {id} answers nothing"));
                waiting.remove(at);
                assert!(message["content"].is_string(), "tool content must be text");
            }
            role => {
                assert!(waiting.is_empty(), "{role} message while calls {waiting:?} are unanswered");
                for call in message["tool_calls"].as_array().into_iter().flatten() {
                    assert!(call["function"]["arguments"].is_string(), "arguments go back as text");
                    waiting.push(call["id"].as_str().unwrap().to_owned());
                }
            }
        }
    }
    assert!(waiting.is_empty(), "calls {waiting:?} were never answered");
}

// ---------- scenarios ----------

#[test]
fn a_plain_reply_streams_through() {
    let server = Server::start(vec![Reply::Stream(vec![text("Hel"), text("lo"), finish("stop"), usage(120, 100, 7)])]);
    let mut chat = chat(&server, false);
    let run = send(&mut chat, "hi");
    run.ok();
    assert_eq!(run.text(), "Hello");
    assert!(matches!(run.events.last(), Some(ChatEvent::Done)));
    let usage = run.events.iter().find_map(|e| if let ChatEvent::Usage(u) = e { Some(*u) } else { None }).unwrap();
    assert_eq!((usage.input_tokens, usage.cache_read_tokens, usage.output_tokens), (20, 100, 7));

    let (auth, body) = server.requests.lock().unwrap()[0].clone();
    assert_eq!(auth, "Bearer sk-test-key");
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["messages"][0], json!({ "role": "system", "content": "You edit CAD parts." }));
    assert_eq!(body["messages"][1], json!({ "role": "user", "content": "hi" }));
    assert_eq!(body["tools"][0]["function"]["name"], "add_part");
}

#[test]
fn a_tool_call_split_across_chunks_is_run_and_answered() {
    let server = Server::start(vec![
        Reply::Stream(vec![
            text("Adding it."),
            call_piece(0, Some("call_a"), Some("add_part"), ""),
            call_piece(0, None, None, "{\"na"),
            call_piece(0, None, None, "me\": \"Plate\"}"),
            // A second call in the same reply.
            call_piece(1, Some("call_b"), Some("add_part"), "{\"name\": \"Lid\"}"),
            finish("tool_calls"),
        ]),
        Reply::Stream(vec![text("Both added."), finish("stop")]),
        Reply::Stream(vec![text("Still here."), finish("stop")]),
    ]);
    let mut chat = chat(&server, false);
    let run = send(&mut chat, "add a plate and a lid");
    run.ok();
    assert_eq!(run.tools.calls, [("add_part".into(), json!({ "name": "Plate" })), ("add_part".into(), json!({ "name": "Lid" }))]);
    assert_eq!(run.text(), "Adding it.Both added.");

    let second = server.body(1);
    assert_history_is_valid(&second);
    let assistant = &second["messages"][2];
    assert_eq!(assistant["tool_calls"][0]["function"]["arguments"], "{\"name\": \"Plate\"}");
    assert_eq!(second["messages"][3]["tool_call_id"], "call_a");
    assert_eq!(second["messages"][4]["tool_call_id"], "call_b");

    // The conversation carries on afterwards with a history the server accepts.
    send(&mut chat, "thanks").result.unwrap();
    assert_history_is_valid(&server.body(2));
}

#[test]
fn local_servers_that_send_calls_whole_and_without_ids_work() {
    // Ollama style: one chunk, arguments as an object, no id, no index.
    let whole = json!({ "choices": [{ "index": 0, "delta": { "tool_calls": [
        { "function": { "name": "add_part", "arguments": { "name": "Plate" } } }
    ] }, "finish_reason": "tool_calls" }] });
    let server = Server::start(vec![Reply::Stream(vec![whole]), Reply::Stream(vec![text("Done."), finish("stop")])]);
    let mut chat = chat(&server, false);
    let run = send(&mut chat, "add a plate");
    run.ok();
    assert_eq!(run.tools.calls, [("add_part".into(), json!({ "name": "Plate" }))]);
    let second = server.body(1);
    assert_history_is_valid(&second);
    assert!(!second["messages"][2]["tool_calls"][0]["id"].as_str().unwrap().is_empty(), "an id is made up");
}

#[test]
fn bad_arguments_and_failing_tools_go_back_to_the_model() {
    let server = Server::start(vec![
        Reply::Stream(vec![
            call_piece(0, Some("c1"), Some("add_part"), "{\"name\": "),
            call_piece(1, Some("c2"), Some("explode"), "{}"),
            call_piece(2, Some("c3"), Some("add_part"), "[1, 2]"),
            finish("tool_calls"),
        ]),
        Reply::Stream(vec![text("Sorry."), finish("stop")]),
    ]);
    let mut chat = chat(&server, false);
    let run = send(&mut chat, "go");
    run.ok();
    assert_eq!(run.tools.calls.len(), 1, "only the call with usable arguments ran");
    let second = server.body(1);
    assert_history_is_valid(&second);
    assert!(second["messages"][3]["content"].as_str().unwrap().contains("not a valid JSON object"));
    assert!(second["messages"][4]["content"].as_str().unwrap().contains("Error: the tool blew up"));
    assert!(second["messages"][5]["content"].as_str().unwrap().contains("not a valid JSON object"));
    let failed = run.events.iter().filter(|e| matches!(e, ChatEvent::ToolFinished { ok: false, .. })).count();
    assert_eq!(failed, 3);
}

#[test]
fn pictures_follow_as_a_user_message_when_the_model_can_see() {
    let look = || Reply::Stream(vec![call_piece(0, Some("c1"), Some("view_model"), "{}"), finish("tool_calls")]);
    let server = Server::start(vec![look(), Reply::Stream(vec![text("A block."), finish("stop")])]);
    let mut chat = chat(&server, true);
    let run = send(&mut chat, "look");
    run.ok();
    let first = server.body(0);
    assert!(first["tools"].as_array().unwrap().iter().any(|t| t["function"]["name"] == "view_model"));
    let second = server.body(1);
    assert_history_is_valid(&second);
    assert!(!second["messages"][3]["content"].as_str().unwrap().contains("QUJD"), "no base64 in the tool text");
    let picture = &second["messages"][4];
    assert_eq!(picture["role"], "user");
    assert_eq!(picture["content"][1]["image_url"]["url"], "data:image/png;base64,QUJD");
    assert!(!format!("{:?}", run.events).contains("QUJD"), "the chat panel never gets the base64");
}

#[test]
fn a_model_without_vision_is_not_offered_pictures() {
    let server = Server::start(vec![Reply::Stream(vec![text("ok"), finish("stop")])]);
    let mut chat = chat(&server, false);
    send(&mut chat, "hi").result.unwrap();
    let body = server.body(0);
    let names: Vec<&str> = body["tools"].as_array().unwrap().iter().map(|t| t["function"]["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["add_part"]);
}

#[test]
fn a_dropped_connection_is_an_error_and_the_chat_recovers() {
    let server = Server::start(vec![
        Reply::Dropped(vec![text("I was about to")]),
        Reply::Stream(vec![text("Here now."), finish("stop")]),
    ]);
    let mut chat = chat(&server, false);
    let run = send(&mut chat, "first");
    assert!(run.result.unwrap_err().to_string().contains("connection dropped"));
    let run = send(&mut chat, "second");
    run.ok();
    let body = server.body(1);
    assert_history_is_valid(&body);
    assert!(!body.to_string().contains("I was about to"), "the half reply was not kept");
}

#[test]
fn errors_are_readable_and_never_show_the_key() {
    let server = Server::start(vec![
        Reply::Status(401, r#"{"error":{"message":"Incorrect API key provided"}}"#),
        Reply::Status(404, r#"{"error":"model 'nope' not found"}"#),
        Reply::Stream(vec![json!({ "error": { "message": "upstream provider failed" } })]),
    ]);
    let mut chat = chat(&server, false);
    let messages: Vec<String> = (0..3).map(|_| format!("{:#}", send(&mut chat, "hi").result.unwrap_err())).collect();
    assert!(messages[0].contains("rejected the API key") && messages[0].contains("Incorrect API key"));
    assert!(messages[1].contains("model 'nope' not found"));
    assert!(messages[2].contains("upstream provider failed"));
    assert!(messages.iter().all(|m| !m.contains("sk-test-key")));
}

#[test]
fn rate_limits_are_retried() {
    let server = Server::start(vec![
        Reply::Status(429, r#"{"error":{"message":"slow down"}}"#),
        Reply::Stream(vec![text("ok"), finish("stop")]),
    ]);
    let mut chat = chat(&server, false);
    let run = send(&mut chat, "hi");
    run.ok();
    assert_eq!(server.count(), 2);
}

#[test]
fn stop_ends_the_reply_and_skipped_calls_are_still_answered() {
    // Stop is already pressed when the tools would run.
    struct StopOnCall<'a>(&'a AtomicBool, usize);
    impl ToolExecutor for StopOnCall<'_> {
        fn definitions(&self) -> Vec<ToolDef> {
            Vec::new()
        }
        fn call(&mut self, _name: &str, _input: &Value) -> anyhow::Result<Value> {
            self.1 += 1;
            self.0.store(true, Ordering::Relaxed);
            Ok(json!({ "ok": true }))
        }
    }
    let server = Server::start(vec![
        Reply::Stream(vec![
            call_piece(0, Some("c1"), Some("add_part"), "{}"),
            call_piece(1, Some("c2"), Some("add_part"), "{}"),
            finish("tool_calls"),
        ]),
        Reply::Stream(vec![text("Carrying on."), finish("stop")]),
    ]);
    let mut chat = chat(&server, false);
    let cancel = AtomicBool::new(false);
    let mut tools = StopOnCall(&cancel, 0);
    let result = chat.send("go", &mut tools, &mut |_| {}, &cancel);
    assert!(result.unwrap_err().is::<intercraft_llm::Cancelled>());
    assert_eq!(tools.1, 1, "the second call was not run");

    send(&mut chat, "carry on").result.unwrap();
    let body = server.body(1);
    assert_history_is_valid(&body);
    assert!(body["messages"][4]["content"].as_str().unwrap().contains("pressed Stop"));
}

#[test]
fn a_cut_off_reply_says_so() {
    let server = Server::start(vec![Reply::Stream(vec![text("Long answer"), finish("length")])]);
    let mut chat = chat(&server, false);
    let run = send(&mut chat, "hi");
    run.ok();
    assert!(run.notices()[0].contains("cut off"));
}

#[test]
fn nothing_is_sent_to_an_unsafe_address_or_without_a_model() {
    let server = Server::start(vec![]);
    let mut chat = OpenAiChat::new(OpenAiSettings {
        base_url: "http://example.com/v1".into(),
        api_key: "sk-test-key".into(),
        model: "m".into(),
        ..Default::default()
    });
    assert!(send(&mut chat, "hi").result.unwrap_err().to_string().contains("https://"));
    chat.update_settings(OpenAiSettings { base_url: server.base_url.clone(), ..Default::default() });
    assert!(send(&mut chat, "hi").result.unwrap_err().to_string().contains("choose a model"));
    assert_eq!(server.count(), 0);
}

#[test]
fn a_users_picture_is_sent_only_to_a_model_that_can_see() {
    let reply = || Reply::Stream(vec![text("ok"), finish("stop")]);
    let images = ["QUJD".to_owned()];
    for vision in [true, false] {
        let server = Server::start(vec![reply(), reply()]);
        let mut chat = chat(&server, vision);
        chat.send_with_images("my sketch", &images, &mut Tools::default(), &mut |_| {}, &AtomicBool::new(false)).unwrap();
        let message = server.body(0)["messages"][1].clone();
        if vision {
            assert_eq!(message["content"][0]["text"], "my sketch");
            assert_eq!(message["content"][1]["image_url"]["url"], "data:image/png;base64,QUJD");
        } else {
            let text = message["content"].as_str().expect("plain text for a text-only model");
            assert!(text.starts_with("my sketch") && text.contains("unable to read images"));
            assert!(!text.contains("QUJD"));
        }
        send(&mut chat, "and then?").ok();
        assert_history_is_valid(&server.body(1));
    }
}
