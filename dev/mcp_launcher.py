# Starts the InterCraft MCP server for an MCP client (Claude Code, Cursor, Codex CLI...).
# Python 3.11, standard library only.
#
# It builds the server first, then runs a copy of it. A running server would otherwise
# lock the .exe on Windows and the next build (from a second session, or after a code
# change) would fail, which the client only reports as "connection closed".
#
# Without arguments the server edits whatever project the InterCraft viewer has open.
# Pass a folder to pin it to one project:  python dev/mcp_launcher.py projects/demo

import glob
import json
import os
import shutil
import subprocess
import sys
import tempfile
import uuid

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE_NAME = "intercraft-mcp.exe" if os.name == "nt" else "intercraft-mcp"


def log(text):
    # stdout carries the MCP protocol, so everything human goes to stderr.
    print(f"intercraft launcher: {text}", file=sys.stderr, flush=True)


def build():
    """Build the server and return the path cargo reports for it, or None."""
    result = subprocess.run(
        ["cargo", "build", "-q", "-p", "intercraft-mcp", "--message-format=json"],
        cwd=REPO, stdout=subprocess.PIPE, stderr=sys.stderr, text=True,
    )
    exe = None
    for line in result.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("target", {}).get("name") == "intercraft-mcp":
            exe = msg.get("executable") or exe
    if result.returncode != 0:
        log("build failed, trying the last good build")
        return None
    return exe


def last_build():
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=REPO, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
    )
    try:
        target = json.loads(result.stdout)["target_directory"]
    except (ValueError, KeyError):
        return None
    exe = os.path.join(target, "debug", EXE_NAME)
    return exe if os.path.exists(exe) else None


def main():
    exe = build() or last_build()
    if not exe:
        log("could not build or find intercraft-mcp")
        return 1

    copies = os.path.join(tempfile.gettempdir(), "intercraft-mcp")
    os.makedirs(copies, exist_ok=True)
    # Old copies from servers that have exited. Ones still running are locked and stay.
    for old in glob.glob(os.path.join(copies, "*")):
        try:
            os.remove(old)
        except OSError:
            pass

    copy = os.path.join(copies, f"{uuid.uuid4().hex[:8]}-{EXE_NAME}")
    shutil.copy2(exe, copy)
    try:
        # stdin and stdout are inherited, so the client talks to the server directly.
        return subprocess.run([copy, *sys.argv[1:]]).returncode
    finally:
        try:
            os.remove(copy)
        except OSError:
            pass


if __name__ == "__main__":
    sys.exit(main())
