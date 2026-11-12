# InterCraft security review

> [!info] Scope
> Reviewed on 11 November 2026 against the `feat/menus-projects` branch (PRs #1 to #3). It covers API key handling, the Claude Code route, snippet execution, project files, the chat clients, the MCP server and the chat display. Every finding below was checked against the code or reproduced by a test.

## Summary

InterCraft lets a language model write Python that runs on the user's machine, so the main risk is code execution rather than key theft. Before this review a model, or anyone who handed the user a project folder, could run any Python with the user's permissions. That issue is now contained by a sandbox, a memory cap and a build timeout. Key handling was already sound and has been tightened.

| | Grade |
|---|---|
| Before the review | **D** (one critical issue) |
| After the fixes in PRs #1 to #3 | **B+** |

Grading scale: **A** no known weakness worth fixing, **B** weaknesses with limited impact or that need local access, **C** a realistic way to cause harm, **D** a serious issue reachable in normal use, **F** trivially exploitable.

## Grades by area

| Area | Before | After | Main remaining risk |
|---|---|---|---|
| API key storage and use | B | A- | Key sits in memory as plain text while the app runs |
| Claude Code (subscription) route | B+ | A- | Relies on Claude Code honouring its lockdown flags |
| Snippet execution | F | B | In-process sandbox, no OS-level file or network isolation |
| Project files from others | D | A- | Prompt injection through labels, prompts and code |
| Chat clients (reliability) | C | A | None found in 34 stress scenarios |
| MCP server | C | A- | Trusts any local process that can start it |
| Chat display (Markdown) | B | A- | Links open in the browser when clicked |
| Repository and dependencies | B | B | No automated dependency audit |

## Findings

### 1. Snippets could run any Python (critical, fixed)

**Point.** Feature snippets ran through Python's `exec` with full builtins, so a snippet could open files, start processes or reach the network.

**Evidence.** The worker built its namespace with `from build123d import *` and ran the code as given. build123d also exports `export_stl`, `import_step` and 15 other functions that read or write files.

**Explain.** The code comes from a model, or from a project folder someone else wrote. A manipulated model, or a shared project with a hostile snippet, therefore had the same reach as the user. The Claude Code lockdown did not help, because `add_feature` itself was the way in.

**Fix.** `worker/sandbox.py` now checks each snippet before it runs and runs it with a short list of safe builtins:
- it refuses imports, `global`, double-underscore names, and attributes that reach frames, code objects or file I/O
- it removes all 17 build123d import and export functions
- it caps snippets at 20 000 characters

Each worker also runs in a Windows job object capped at 4 GB, which ends it when the app closes. A build step that makes no progress for 60 seconds is stopped and the worker restarted. `worker/test_sandbox.py` runs 32 escape attempts, including dunder chains, generator frames, format strings and Unicode lookalikes. All 32 are refused, and real projects still build.

**Link.** This moves snippet execution from F to B. It is not an F any more because the obvious and known routes out are closed. It is not an A because an in-process Python sandbox can never be complete (see [Remaining risks](#remaining-risks)).

### 2. Project files could reach outside the project (high, fixed)

**Point.** A crafted `project.json` could make InterCraft read or overwrite files anywhere the user can write.

**Evidence.** Snippet paths such as `../../x.py` or `C:/Windows/x.py` were joined to the project folder without checks, and saving wrote the snippet back to that path.

**Explain.** Opening a project someone shared was enough. Editing a feature in it would then overwrite the outside file with the snippet's text.

**Fix.** Loading now refuses absolute paths, `..`, files that aren't `.py`, unsafe or Windows-reserved part names (`CON`, `NUL`, `COM1` and the rest), duplicate ids, and oversized code, labels and prompts. Tests cover each case.

**Link.** Project files go from D to A-. The remaining risk is that their text reaches the model (see prompt injection below).

### 3. API key handling (tightened)

**Point.** The key was already stored and sent safely. Three smaller gaps have been closed.

**Evidence.**
- *Already sound.* The key lives in the operating system's credential store (Windows Credential Manager, macOS Keychain or Secret Service), never in `settings.json`, the project or a log. It is sent only in the `x-api-key` header to `api.anthropic.com` over TLS (rustls). The input field is masked and cleared after saving.
- *Gap one.* `AnthropicSettings` derived `Debug`, so printing the settings would have printed the key.
- *Gap two.* The test variable `INTERCRAFT_ANTHROPIC_URL` accepted any address, so anything able to set an environment variable could send the key to another server.
- *Gap three.* Claude Code inherited `ANTHROPIC_API_KEY`, so the "your subscription" route could silently bill an API account instead.

**Explain.** None of these leaked a key in normal use, but each was one mistake or one local change away from doing so.

**Fix.** `Debug` is written by hand and prints `<redacted>`. The URL override accepts only `127.0.0.1`, `localhost` and `::1`, and lookalikes such as `127.0.0.1.evil.com` are refused. The Claude Code process starts without `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`. Each of these has a test.

**Link.** Key handling goes from B to A-.

### 4. Claude Code route

**Point.** InterCraft never handles claude.ai credentials. It starts the user's own unmodified `claude` program, which signs in by itself.

**Evidence.** The background session runs with:
- `--tools ""`, so no shell and no file editing
- `--strict-mcp-config`, so InterCraft is the only MCP server
- `--allowedTools mcp__intercraft` with `--permission-mode dontAsk`
- `--setting-sources project`, which leaves the user's personal hooks out

A fake `claude` binary in `claude_code_stress.rs` checks every one of these flags and confirms that no API key reaches it.

**Explain.** Even a manipulated model in that session can only call InterCraft's tools, and those now run sandboxed.

**Link.** This route rates A-. Anthropic's terms allow users to run Claude Code with their own subscription, but check the current terms before InterCraft is published.

### 5. Chat reliability (fixed)

**Point.** Several ordinary failures could break a conversation or freeze the app.

**Evidence.** Stress tests reproduced each of these:
- a resumed `pause_turn` reply created two assistant messages in a row, which the API rejects
- a dropped connection was treated as a finished reply
- invalid UTF-8 on standard input stopped the MCP server
- an endless loop in a snippet froze the viewer
- two agents editing one project at once crashed a save or lost edits

**Explain.** These are availability problems rather than leaks, but a chat that can be broken by a network blip or a bad snippet is not usable.

**Fix.** Each case is fixed. Tool calls now lock the project (`.intercraft.lock`), so the chat panel and a Claude Code terminal can't overwrite each other. The suites now cover:
- 19 API scenarios, run against a fake server that rejects any malformed history
- 15 Claude Code scenarios, including crashes, hangs, junk output and Stop
- 10 tool tests that use the real geometry worker
- junk input to the MCP server

**Link.** Chat reliability goes from C to A.

### 6. Markdown in replies

**Point.** Replies are rendered as Markdown, which could have loaded images from file paths.

**Evidence.** egui_commonmark's default features include image loading from files.

**Fix.** The crate is used without default features, so images are never loaded. A reply can't display local files or make network requests. Links open in the browser only when clicked, and hovering shows the address first.

## Remaining risks

> [!warning] Not fixed yet
> These are known and ranked by how much they matter.

1. **The sandbox runs inside Python.** It blocks every escape tested, but CPython was not designed as a security boundary, and the worker still runs as the user with network access. The next step is OS-level isolation: run the worker under a restricted token or AppContainer on Windows (or a separate low-privilege account), and block its network access.
2. **The memory cap and the stuck-build watchdog are Windows only.** macOS and Linux need equivalents (`setrlimit`, cgroups) before release there.
3. **Prompt injection.** Labels, prompts and code from a shared project reach the model. The worst a manipulated model can do is edit or delete features and design parts in the open project. Since PR #6 every change is recorded in an undo history (the last 200 changes), so the damage can be undone from Edit > History. The model can't delete reference parts. History snapshots are checked like `project.json` before they are restored, so a crafted snapshot can't write outside the project.
4. **No dependency audit.** `cargo audit` and `pip-audit` have not been run, and there is no CI. Only build123d and numpy are pinned on the Python side, not their own dependencies. Adding CI with both audits and a Python lock file is the fix.
5. **Local files can be changed by local programs.** Any program running as the user can edit `active_project.txt` to point an MCP client at another folder, or read the credential store. This needs local access already, so it is rated low.
6. **The key stays in memory as plain text** while the app runs and is not cleared afterwards. Low impact.
7. **Imported STEP files are parsed by OpenCascade** in the geometry worker. A malformed file can crash the worker, which then restarts, and parser bugs in large C++ libraries are a known class of problem. Imports are limited to `.step` and `.stp` files under 200 MB that the user picks, are copied into the project's `imports` folder, and are checked again by the worker. Only import files from people you trust.

## Checks you can repeat

```powershell
cargo test --workspace
& "$env:LOCALAPPDATA\InterCraft\venv\Scripts\python.exe" -m unittest worker/test_sandbox.py
git log -p --all | Select-String "sk-ant-[A-Za-z0-9_-]{20,}"   # should find nothing real
```

The tests that run the real geometry worker skip themselves when the Python environment isn't set up.
