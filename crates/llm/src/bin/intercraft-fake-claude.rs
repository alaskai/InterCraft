// Test double for Claude Code's headless mode, used by tests/claude_code_stress.rs.
// It speaks the same stream-json lines as `claude -p` and picks a behaviour from
// the words in each user message (crash, hang, tool, lockdown, ...).

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

fn out(value: Value) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{value}");
    let _ = stdout.flush();
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn stream_text(text: &str, parent: Value) {
    out(json!({"type": "stream_event", "parent_tool_use_id": parent, "event": {"type": "message_start"}}));
    out(json!({"type": "stream_event", "parent_tool_use_id": parent, "event": {"type": "content_block_delta",
        "delta": {"type": "text_delta", "text": text}}}));
    out(json!({"type": "assistant", "parent_tool_use_id": parent, "message": {"content": [{"type": "text", "text": text}]}}));
}

fn result(session: &str, ok: bool, text: &str) {
    // Like Claude Code, the cost is a running total for the process: 2 cents a reply.
    static REPLIES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let replies = REPLIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    out(json!({"type": "result", "subtype": if ok { "success" } else { "error_during_execution" },
        "is_error": !ok, "result": text, "session_id": session, "total_cost_usd": 0.02 * f64::from(replies),
        "usage": {"input_tokens": 12, "cache_read_input_tokens": 3000, "output_tokens": 40}}));
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("9.9.9 (Fake Claude Code)");
        return;
    }
    let resumed = arg_after(&args, "--resume").map(str::to_owned);
    // A session Claude Code no longer has: it says so and exits, as the real one does.
    if resumed.as_deref() == Some("gone-session-0000") {
        eprintln!("No conversation found with session ID: gone-session-0000");
        std::process::exit(1);
    }
    let session = resumed.clone().unwrap_or_else(|| format!("fake-session-{}", std::process::id()));
    let mcp_ok = arg_after(&args, "--mcp-config")
        .and_then(|c| serde_json::from_str::<Value>(c).ok())
        .is_some_and(|c| c["mcpServers"]["intercraft"]["command"].is_string());
    let lockdown = json!({
        "strict_mcp": args.iter().any(|a| a == "--strict-mcp-config"),
        "builtin_tools": arg_after(&args, "--tools"),
        "allowed": arg_after(&args, "--allowedTools"),
        "permission": arg_after(&args, "--permission-mode"),
        "setting_sources": arg_after(&args, "--setting-sources"),
        "api_key_in_env": std::env::var_os("ANTHROPIC_API_KEY").is_some() || std::env::var_os("ANTHROPIC_AUTH_TOKEN").is_some(),
        "mcp_config_ok": mcp_ok,
        "resumed": resumed,
    });

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                eprintln!("fake claude: stdin line was not JSON");
                std::process::exit(2);
            }
        };
        // Content is a plain string, or a list of blocks when pictures come with it.
        let content = &msg["message"]["content"];
        let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();
        let pictures = blocks.iter().filter(|b| b["type"] == "image" && b["source"]["media_type"] == "image/png").count();
        let text = match content.as_str() {
            Some(text) => text.to_owned(),
            None => blocks.iter().find_map(|b| b["text"].as_str()).unwrap_or_default().to_owned(),
        };
        out(json!({"type": "system", "subtype": "init", "session_id": session, "model": "fake-model"}));
        let word = text.split_whitespace().next().unwrap_or_default();

        match word {
            "lockdown" => {
                stream_text(&lockdown.to_string(), Value::Null);
                result(&session, true, "");
            }
            "pictures" => {
                stream_text(&format!("saw {pictures} picture(s)"), Value::Null);
                result(&session, true, "");
            }
            "echo" => {
                stream_text(&text, Value::Null);
                result(&session, true, "");
            }
            "tool" | "tool_error" => {
                out(json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "message_start"}}));
                out(json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "tool_use", "id": "tu1", "name": "mcp__intercraft__add_part", "input": {"name": "X"}}]}}));
                let (is_error, body) = if word == "tool" {
                    (false, "{\"added_part\": \"X\"}")
                } else {
                    (true, "a part named 'X' already exists")
                };
                out(json!({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "tu1",
                    "is_error": is_error, "content": [{"type": "text", "text": body}]}]}}));
                stream_text("done", Value::Null);
                result(&session, true, "done");
            }
            "crash" => {
                stream_text("half a rep", Value::Null);
                eprintln!("fatal: simulated crash");
                std::process::exit(3);
            }
            "error_result" => result(&session, false, "Not logged in. Please run /login"),
            "rate_limited" => {
                out(json!({"type": "rate_limit_event", "rate_limit_info": {"status": "rejected", "rateLimitType": "five_hour"}}));
                result(&session, false, "Claude usage limit reached");
            }
            "garbage" => {
                println!("this line is not json");
                println!("{{\"type\":\"mystery\",\"data\":[1,2,3]}}");
                println!("{{}}");
                println!("[1,2]");
                stream_text("survived", Value::Null);
                result(&session, true, "");
            }
            "subagent" => {
                stream_text("SUBAGENT TEXT", json!("parent-1"));
                out(json!({"type": "assistant", "parent_tool_use_id": "parent-1", "message": {"content": [
                    {"type": "tool_use", "id": "sub1", "name": "mcp__intercraft__delete_feature", "input": {}}]}}));
                stream_text("main text", Value::Null);
                result(&session, true, "");
            }
            "slow" => {
                out(json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "message_start"}}));
                for _ in 0..300 {
                    out(json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "content_block_delta",
                        "delta": {"type": "text_delta", "text": "."}}}));
                    std::thread::sleep(Duration::from_millis(100));
                }
                result(&session, true, "");
            }
            "hang" => std::thread::sleep(Duration::from_secs(600)),
            "huge" => {
                let text = "Ünïcödé 日本語 🔩 ".repeat(100_000);
                stream_text(&text, Value::Null);
                result(&session, true, "");
            }
            _ => {
                stream_text("ok", Value::Null);
                result(&session, true, "ok");
            }
        }
    }
}
