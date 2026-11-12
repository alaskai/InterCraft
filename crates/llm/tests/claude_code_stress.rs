// Stress tests for the Claude Code client, against a fake `claude` that crashes,
// hangs, prints junk and so on. After each failure the chat must still work.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use intercraft_llm::{
    claude_code_version, ChatEvent, ChatSession, ClaudeCodeChat, ClaudeCodeSettings, ToolDef, ToolExecutor,
};
use serde_json::{json, Value};

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        std::env::set_var("INTERCRAFT_CLAUDE_BIN", env!("CARGO_BIN_EXE_intercraft-fake-claude"));
        // Must not reach the Claude Code process, or it would bill this key.
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-should-not-leak");
    });
}

struct NoTools;

impl ToolExecutor for NoTools {
    fn definitions(&self) -> Vec<ToolDef> {
        Vec::new()
    }
    fn call(&mut self, _: &str, _: &Value) -> anyhow::Result<Value> {
        panic!("Claude Code runs the tools itself; the executor must never be called");
    }
}

fn new_chat() -> ClaudeCodeChat {
    setup();
    ClaudeCodeChat::new(ClaudeCodeSettings {
        project_dir: std::env::temp_dir(),
        mcp_command: "python".into(),
        mcp_args: vec!["launcher.py".into(), "project".into()],
        model: String::new(),
        extra_prompt: "test prompt with \"quotes\" and\nnewlines".into(),
    })
}

fn send(chat: &mut ClaudeCodeChat, text: &str) -> (anyhow::Result<()>, Vec<ChatEvent>) {
    let mut events = Vec::new();
    let cancel = AtomicBool::new(false);
    let result = chat.send(text, &mut NoTools, &mut |e| events.push(e), &cancel);
    (result, events)
}

fn text_of(events: &[ChatEvent]) -> String {
    events.iter().filter_map(|e| if let ChatEvent::TextDelta(t) = e { Some(t.as_str()) } else { None }).collect()
}

fn lockdown(chat: &mut ClaudeCodeChat) -> Value {
    let (result, events) = send(chat, "lockdown");
    assert!(result.is_ok(), "{result:?}");
    serde_json::from_str(&text_of(&events)).unwrap()
}

#[test]
fn version_is_detected() {
    setup();
    assert!(claude_code_version().unwrap().contains("Fake Claude Code"));
}

#[test]
fn session_is_locked_down() {
    let mut chat = new_chat();
    let flags = lockdown(&mut chat);
    assert_eq!(flags["strict_mcp"], true);
    assert_eq!(flags["builtin_tools"], "", "built-in tools must be off");
    assert_eq!(flags["allowed"], "mcp__intercraft");
    assert_eq!(flags["permission"], "dontAsk");
    assert_eq!(flags["setting_sources"], "project");
    assert_eq!(flags["mcp_config_ok"], true);
    assert_eq!(flags["api_key_in_env"], false, "the API key leaked into Claude Code's environment");
}

#[test]
fn tool_calls_are_reported() {
    let mut chat = new_chat();
    let (result, events) = send(&mut chat, "tool");
    assert!(result.is_ok());
    let started = events.iter().find_map(|e| match e {
        ChatEvent::ToolStarted { name, input, .. } => Some((name.clone(), input.clone())),
        _ => None,
    });
    assert_eq!(started, Some(("add_part".to_owned(), json!({"name": "X"}))));
    let finished = events.iter().find_map(|e| match e {
        ChatEvent::ToolFinished { ok, output, .. } => Some((*ok, output.clone())),
        _ => None,
    });
    assert_eq!(finished, Some((true, json!({"added_part": "X"}))));
}

#[test]
fn tool_errors_are_reported() {
    let mut chat = new_chat();
    let (_, events) = send(&mut chat, "tool_error");
    assert!(events.iter().any(|e| matches!(e, ChatEvent::ToolFinished { ok: false, .. })));
}

#[test]
fn crash_is_reported_and_the_session_resumes() {
    let mut chat = new_chat();
    let first = lockdown(&mut chat)["resumed"].clone();
    assert!(first.is_null());
    let (result, _) = send(&mut chat, "crash");
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("simulated crash"), "{error}");
    // The next message starts a new process that carries on the same conversation.
    let after = lockdown(&mut chat);
    assert!(after["resumed"].as_str().unwrap().starts_with("fake-session-"));
}

#[test]
fn error_results_become_errors() {
    let mut chat = new_chat();
    let (result, _) = send(&mut chat, "error_result");
    assert!(format!("{:#}", result.unwrap_err()).contains("Not logged in"));
    assert!(send(&mut chat, "ok").0.is_ok());
}

#[test]
fn usage_limit_is_explained() {
    let mut chat = new_chat();
    let (result, events) = send(&mut chat, "rate_limited");
    assert!(result.is_err());
    assert!(events.iter().any(|e| matches!(e, ChatEvent::Notice(t) if t.contains("usage limit"))));
}

#[test]
fn junk_output_is_ignored() {
    let mut chat = new_chat();
    let (result, events) = send(&mut chat, "garbage");
    assert!(result.is_ok());
    assert_eq!(text_of(&events), "survived");
}

#[test]
fn subagent_output_is_not_shown_as_the_reply() {
    let mut chat = new_chat();
    let (result, events) = send(&mut chat, "subagent");
    assert!(result.is_ok());
    let text = text_of(&events);
    assert!(!text.contains("SUBAGENT") && text.contains("main text"), "{text}");
    assert!(!events.iter().any(|e| matches!(e, ChatEvent::ToolStarted { .. })));
}

#[test]
fn stop_kills_a_slow_reply_and_the_chat_carries_on() {
    for word in ["slow", "hang"] {
        let mut chat = new_chat();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(700));
            flag.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let result = chat.send(word, &mut NoTools, &mut |_| {}, &cancel);
        assert!(result.unwrap_err().is::<intercraft_llm::Cancelled>(), "{word}");
        assert!(started.elapsed() < Duration::from_secs(3), "{word}: Stop took {:?}", started.elapsed());
        assert!(send(&mut chat, "ok").0.is_ok(), "{word}: chat broken after Stop");
    }
}

#[test]
fn hostile_text_arrives_intact() {
    let mut chat = new_chat();
    let nasty = "echo line one\nline two \"quoted\" {\"type\":\"result\"} \u{0} \\ \u{202e} 🔩";
    let (result, events) = send(&mut chat, nasty);
    assert!(result.is_ok());
    assert_eq!(text_of(&events), nasty, "the message was split or altered on the way");
}

#[test]
fn huge_reply_is_handled() {
    let mut chat = new_chat();
    let (result, events) = send(&mut chat, "huge");
    assert!(result.is_ok());
    assert!(text_of(&events).len() > 2_000_000);
}

#[test]
fn many_messages_reuse_one_process() {
    let mut chat = new_chat();
    let first = lockdown(&mut chat);
    for i in 0..30 {
        assert!(send(&mut chat, &format!("message {i}")).0.is_ok(), "message {i}");
    }
    // Still the first process: it was never restarted, so nothing was resumed.
    assert!(lockdown(&mut chat)["resumed"].is_null());
    assert!(first["resumed"].is_null());
}

#[test]
fn new_chat_forgets_the_session() {
    let mut chat = new_chat();
    let _ = send(&mut chat, "crash");
    chat.reset();
    assert!(lockdown(&mut chat)["resumed"].is_null(), "a new chat resumed the old conversation");
}

#[test]
fn missing_claude_gives_a_clear_error() {
    setup();
    // Point at a program that doesn't exist, in a separate child to keep the env var global safe.
    let mut chat = ClaudeCodeChat::new(ClaudeCodeSettings {
        project_dir: std::path::PathBuf::from("Z:/definitely/not/a/folder"),
        mcp_command: "python".into(),
        mcp_args: vec![],
        model: String::new(),
        extra_prompt: String::new(),
    });
    let (result, _) = send(&mut chat, "ok");
    assert!(format!("{:#}", result.unwrap_err()).contains("couldn't start Claude Code"));
}

#[test]
fn pictures_travel_with_the_message() {
    setup();
    let mut chat = new_chat();
    let mut events = Vec::new();
    let images = ["QUJD".to_owned(), "REVG".to_owned()];
    let result = chat.send_with_images("pictures please", &images, &mut NoTools, &mut |e| events.push(e), &AtomicBool::new(false));
    result.unwrap();
    assert_eq!(text_of(&events), "saw 2 picture(s)");
    // A message without pictures is still sent as plain text.
    let (result, events) = send(&mut chat, "pictures again");
    result.unwrap();
    assert_eq!(text_of(&events), "saw 0 picture(s)");
}

#[test]
fn each_reply_reports_its_own_cost() {
    setup();
    let mut chat = new_chat();
    let costs = |events: &[ChatEvent]| -> Vec<f64> {
        events.iter().filter_map(|e| if let ChatEvent::Cost(c) = e { Some(*c) } else { None }).collect()
    };
    // The fake's running total goes 0.02, 0.04: each reply is 2 cents, not the running total.
    let (result, first) = send(&mut chat, "echo one");
    result.unwrap();
    let (result, second) = send(&mut chat, "echo two");
    result.unwrap();
    assert_eq!(costs(&first).len(), 1);
    assert!((costs(&first)[0] - 0.02).abs() < 1e-9);
    assert!((costs(&second)[0] - 0.02).abs() < 1e-9, "{:?}", costs(&second));
}
