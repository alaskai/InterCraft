// Feeds the MCP server malformed and hostile input. It must answer what it can,
// report errors properly, and keep running.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[test]
fn junk_input_never_kills_the_server() {
    let dir = std::env::temp_dir().join(format!("intercraft-mcp-protocol-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut child = Command::new(env!("CARGO_BIN_EXE_intercraft-mcp"))
        .arg(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut lines: Vec<Vec<u8>> = vec![
        b"this is not json".to_vec(),
        vec![0xff, 0xfe, 0x00, 0x80, b'{'],                // invalid UTF-8
        b"{}".to_vec(),                                    // no method, no id: ignored
        b"[1, 2, 3]".to_vec(),
        b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}".to_vec(),
        json!({"jsonrpc": "2.0", "id": 1, "method": "no/such/method"}).to_string().into_bytes(),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "add_part", "arguments": "not an object"}}).to_string().into_bytes(),
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "rm_rf", "arguments": {}}}).to_string().into_bytes(),
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "add_part", "arguments": {"name": "../../escape"}}}).to_string().into_bytes(),
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call"}).to_string().into_bytes(),
        format!("{{\"jsonrpc\":\"2.0\",\"id\":6,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}", "x".repeat(2_000_000)).into_bytes(),
        json!({"jsonrpc": "2.0", "id": {"weird": "id"}, "method": "ping"}).to_string().into_bytes(),
        json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "add_part", "arguments": {"name": "Good"}}}).to_string().into_bytes(),
        json!({"jsonrpc": "2.0", "id": 8, "method": "ping"}).to_string().into_bytes(),
    ];
    for line in &mut lines {
        line.push(b'\n');
        stdin.write_all(line).unwrap();
    }
    stdin.flush().unwrap();
    drop(stdin); // end of input: the server should finish and exit cleanly

    let mut replies: Vec<Value> = Vec::new();
    let mut raw = String::new();
    while stdout.read_line(&mut raw).unwrap() > 0 {
        replies.push(serde_json::from_str(raw.trim()).expect("server wrote a line that isn't JSON"));
        raw.clear();
    }
    let status = child.wait().unwrap();
    assert!(status.success(), "server exited with {status}");

    let by_id = |id: Value| replies.iter().find(|r| r["id"] == id).cloned();
    let errors = replies.iter().filter(|r| r["error"]["code"] == -32700).count();
    // The text line and the invalid UTF-8 line. `[1, 2, 3]` is valid JSON with no method, so it's ignored.
    assert_eq!(errors, 2, "expected parse errors for the two junk lines, got {replies:#?}");
    assert_eq!(by_id(json!(1)).unwrap()["error"]["code"], -32601);
    for id in [2, 3, 4] {
        assert_eq!(by_id(json!(id)).unwrap()["result"]["isError"], true, "id {id}");
    }
    assert!(by_id(json!(5)).is_some(), "a tools/call without params got no reply");
    assert!(by_id(json!(6)).unwrap()["result"].is_object(), "the 2 MB line wasn't handled");
    assert!(by_id(json!({"weird": "id"})).is_some());
    assert_eq!(by_id(json!(7)).unwrap()["result"]["isError"], false);
    assert!(by_id(json!(8)).unwrap()["result"].is_object(), "server stopped answering");
    assert!(!dir.join("..").join("..").join("escape").exists());
    std::fs::remove_dir_all(dir).unwrap();
}
