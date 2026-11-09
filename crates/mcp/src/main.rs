// intercraft-mcp: MCP server (stdio) that lets an LLM edit an InterCraft project.
// serde_json 1, anyhow 1. Speaks JSON-RPC 2.0, one message per line.
//
// Every edit is written to the project folder and built by the geometry worker.
// The desktop app watches the same folder, so the user sees each change land.

mod tools;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use serde_json::{json, Value};

const GUIDE: &str = r#"InterCraft holds parametric CAD parts as an ordered list of features. The user watches the model update in a live viewer after every tool call, so make one small change per call.

Writing a feature:
- A feature is a short build123d snippet (algebra mode). `from build123d import *` and `math` are already imported.
- The snippet reads `part` (the shape so far, None for the first feature) and must reassign `part`.
- Parameters are in the dict `p`. Put every dimension a user may want to change in params, not in the code.
- All lengths are mm. Z is up.
- Examples:
    part = Box(p["width"], p["depth"], p["height"])
    part = part - Pos(0, 0, 5) * Cylinder(p["hole_d"] / 2, 50)
    part = fillet(part.edges().filter_by(Axis.Z), p["radius"])

Working method:
- Call get_project first. It returns every part, feature, snippet and current build numbers.
- Every edit returns a build report. Check it before moving on: a cut must lower the volume, an addition must raise it, and the size must be what you intended.
- If a feature fails, the error is in the report and the change stays saved. Fix it with edit_feature, or remove it with delete_feature.
- Prefer set_param for dimension changes. Later features rebuild automatically.
- Pass the user's request in `prompt` when adding a feature. It is shown on their timeline.
- State which values you measured from a report and which you assumed."#;

fn main() -> Result<()> {
    let dir = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("INTERCRAFT_PROJECT").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut server = tools::Server::new(dir);

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
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
fn handle(server: &mut tools::Server, msg: &Value) -> Option<Value> {
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
        "tools/list" => json!({ "tools": tools::definitions() }),
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
