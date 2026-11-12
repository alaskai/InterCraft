// intercraft-mcp: MCP server (stdio) that lets an LLM edit an InterCraft project.
// serde_json 1, anyhow 1. Speaks JSON-RPC 2.0, one message per line.
//
// Every edit is written to the project folder and built by the geometry worker.
// The desktop app watches the same folder, so the user sees each change land.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use intercraft_tools::{ToolHost, GUIDE};
use serde_json::{json, Value};

fn main() -> Result<()> {
    // A folder argument pins the server to one project. Without one it follows the viewer.
    let mut server = match std::env::args().nth(1).or_else(|| std::env::var("INTERCRAFT_PROJECT").ok()) {
        Some(dir) => ToolHost::new(PathBuf::from(dir)),
        None => ToolHost::follow_viewer(),
    };
    server.warm_up();

    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout();
    let mut raw = Vec::new();
    loop {
        // Read bytes, not lines: a line that isn't valid UTF-8 must be an error reply,
        // not the end of the server.
        raw.clear();
        if stdin.read_until(b'\n', &mut raw)? == 0 {
            break;
        }
        let line = String::from_utf8_lossy(&raw);
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&mut server, &msg),
            Err(e) => Some(error_reply(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

/// Returns None for notifications, which must not be answered.
fn handle(server: &mut ToolHost, msg: &Value) -> Option<Value> {
    let method = msg.get("method")?.as_str()?;
    let id = msg.get("id").cloned()?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    let result = match method {
        "initialize" => json!({
            // Echo the client's version. Only the basic tool calls are used, which every version has.
            "protocolVersion": params.get("protocolVersion").cloned().unwrap_or("2025-06-18".into()),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "intercraft", "version": env!("CARGO_PKG_VERSION") },
            "instructions": GUIDE,
        }),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": intercraft_tools::definitions() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            // Tool failures go back as a result the model can read, not a protocol error.
            match server.call(name, &args) {
                Ok(value) => json!({
                    "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
                    "isError": false,
                }),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": format!("{e:#}") }],
                    "isError": true,
                }),
            }
        }
        _ => return Some(error_reply(id, -32601, &format!("unknown method {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn error_reply(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
