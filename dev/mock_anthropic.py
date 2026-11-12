# Local stand-in for the Anthropic Messages API streaming endpoint, for testing the
# InterCraft chat loop without spending real tokens. Python 3.11, standard library only.
#
# Usage:
#   python dev/mock_anthropic.py requests.jsonl [port]
#   $env:INTERCRAFT_ANTHROPIC_URL = "http://127.0.0.1:8765/v1/messages"
#   $env:ANTHROPIC_API_KEY = "any-text"
#   cargo run -- projects/demo
#
# Any message gets a scripted reply: a progress note, then add_part "MockPlate" and
# add_feature (a 50 x 30 x 6 box). The next request (with the tool results) gets a
# short text answer. A message containing "question" gets a Markdown reply that asks
# which hole size to use (ask_user), and picking an answer gets a reply to it.
# Every request body is appended to requests.jsonl, minus the key.
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[1]
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 8765


def sse(event, data):
    return f"event: {event}\ndata: {json.dumps(data)}\n\n".encode()


def first_turn():
    yield sse("message_start", {"type": "message_start", "message": {
        "id": "msg_1", "type": "message", "role": "assistant", "model": "mock-opus",
        "content": [], "usage": {"input_tokens": 1200, "cache_read_input_tokens": 0,
                                 "cache_creation_input_tokens": 1100, "output_tokens": 1}}})
    yield b": keep-alive\n\n"
    yield sse("ping", {"type": "ping"})
    # progress note (thinking block, display "updates")
    yield sse("content_block_start", {"type": "content_block_start", "index": 0,
                                      "content_block": {"type": "thinking", "thinking": "", "signature": ""}})
    for piece in ["Creating a new part, ", "then the base plate."]:
        yield sse("content_block_delta", {"type": "content_block_delta", "index": 0,
                                          "delta": {"type": "thinking_delta", "thinking": piece}})
        time.sleep(0.3)
    yield sse("content_block_delta", {"type": "content_block_delta", "index": 0,
                                      "delta": {"type": "signature_delta", "signature": "SIG123"}})
    yield sse("content_block_stop", {"type": "content_block_stop", "index": 0})
    # tool call 1, input streamed in pieces
    yield sse("content_block_start", {"type": "content_block_start", "index": 1, "content_block": {
        "type": "tool_use", "id": "toolu_1", "name": "add_part", "input": {}}})
    for piece in ['{"na', 'me": "MockPl', 'ate"}']:
        yield sse("content_block_delta", {"type": "content_block_delta", "index": 1,
                                          "delta": {"type": "input_json_delta", "partial_json": piece}})
    yield sse("content_block_stop", {"type": "content_block_stop", "index": 1})
    # tool call 2
    inp = json.dumps({"part": "MockPlate", "label": "Base plate",
                      "code": 'part = Box(p["w"], p["d"], p["t"])',
                      "params": {"w": 50, "d": 30, "t": 6}, "prompt": "mock prompt"})
    yield sse("content_block_start", {"type": "content_block_start", "index": 2, "content_block": {
        "type": "tool_use", "id": "toolu_2", "name": "add_feature", "input": {}}})
    for i in range(0, len(inp), 25):
        yield sse("content_block_delta", {"type": "content_block_delta", "index": 2,
                                          "delta": {"type": "input_json_delta", "partial_json": inp[i:i + 25]}})
    yield sse("content_block_stop", {"type": "content_block_stop", "index": 2})
    yield sse("message_delta", {"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                                "usage": {"output_tokens": 180}})
    yield sse("message_stop", {"type": "message_stop"})


def second_turn():
    yield sse("message_start", {"type": "message_start", "message": {
        "id": "msg_2", "type": "message", "role": "assistant", "model": "mock-opus",
        "content": [], "usage": {"input_tokens": 300, "cache_read_input_tokens": 1100,
                                 "cache_creation_input_tokens": 0, "output_tokens": 1}}})
    yield sse("content_block_start", {"type": "content_block_start", "index": 0,
                                      "content_block": {"type": "text", "text": ""}})
    for piece in ["Done. MockPlate is a ", "50 x 30 x 6 mm block (9000 mm³)."]:
        yield sse("content_block_delta", {"type": "content_block_delta", "index": 0,
                                          "delta": {"type": "text_delta", "text": piece}})
        time.sleep(0.2)
    yield sse("content_block_stop", {"type": "content_block_stop", "index": 0})
    yield sse("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                                "usage": {"output_tokens": 20}})
    yield sse("message_stop", {"type": "message_stop"})


def text_turn(text, stop="end_turn", tool=None):
    yield sse("message_start", {"type": "message_start", "message": {
        "id": "msg_q", "type": "message", "role": "assistant", "model": "mock-opus", "content": [],
        "usage": {"input_tokens": 500, "cache_read_input_tokens": 1500, "cache_creation_input_tokens": 0, "output_tokens": 1}}})
    yield sse("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
    for i in range(0, len(text), 40):
        yield sse("content_block_delta", {"type": "content_block_delta", "index": 0,
                                          "delta": {"type": "text_delta", "text": text[i:i + 40]}})
        time.sleep(0.03)
    yield sse("content_block_stop", {"type": "content_block_stop", "index": 0})
    if tool:
        name, tool_input = tool
        yield sse("content_block_start", {"type": "content_block_start", "index": 1, "content_block": {
            "type": "tool_use", "id": "toolu_q", "name": name, "input": {}}})
        yield sse("content_block_delta", {"type": "content_block_delta", "index": 1,
                                          "delta": {"type": "input_json_delta", "partial_json": json.dumps(tool_input)}})
        yield sse("content_block_stop", {"type": "content_block_stop", "index": 1})
    yield sse("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 60}})
    yield sse("message_stop", {"type": "message_stop"})


QUESTION_TEXT = """## Mounting plate plan

I can make the plate **60 x 40 x 5 mm** with four holes. Before cutting them:

- Hole spacing: 44 x 28 mm, centred
- Edge distance: 8 mm

| Fastener | Clearance hole |
|---|---|
| M3 | 3.4 mm |
| M4 | 4.5 mm |

The holes will use `Cylinder(p["hole_d"] / 2, 20)`.
"""

QUESTION = ("ask_user", {
    "question": "Which fastener size should the holes fit?",
    "options": [
        {"label": "M3 clearance", "description": "3.4 mm holes, lighter brackets."},
        {"label": "M4 clearance", "description": "4.5 mm holes, matches 2020 extrusion T-nuts."},
        {"label": "Leave them out", "description": "Plain plate, holes added later."},
    ],
})


def pick_turn(body):
    messages = body["messages"]
    last = messages[-1]
    has_results = any(b.get("type") == "tool_result" for b in last["content"])
    user_text = [b.get("text", "") for m in messages if m["role"] == "user" for b in m["content"] if b.get("type") == "text"]
    latest = user_text[-1] if user_text else ""
    asked = any("question" in t for t in user_text)
    if asked and has_results and "toolu_q" in json.dumps(last):
        return text_turn("Waiting for your choice above.")
    if "Which fastener size" in latest:
        choice = latest.split("?", 1)[-1].strip()
        return text_turn(f"Going with **{choice}**.\n\n1. Cut the holes\n2. Check the volume drops\n3. Export STEP and STL")
    if "question" in latest:
        return text_turn(QUESTION_TEXT, stop="tool_use", tool=QUESTION)
    return second_turn() if has_results else first_turn()


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        headers = {k: v for k, v in self.headers.items() if k.lower() != "x-api-key"}
        headers["x-api-key-present"] = bool(self.headers.get("x-api-key"))
        with open(LOG, "a", encoding="utf-8") as f:
            f.write(json.dumps({"headers": headers, "body": body}) + "\n")
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        for chunk in pick_turn(body):
            self.wfile.write(chunk)
            self.wfile.flush()

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
