# InterCraft full test plan

A plan for testing every part of InterCraft, written for Claude to work through and for Ethan to check against. It covers what is already tested automatically, what needs a new test, what needs the real worker or a real model, and what only a person at the computer can judge.

> [!note] Status when written
> `main` at 3d254c4 (PR #34 merged). About 200 automated tests pass locally and in CI. The newest features (nested groups, outline view, chat history, `nearby`, labelled pictures) have only been tested automatically, never with a real model.

## How to run this plan

### Rules

1. **Never edit Ethan's real projects.** Copy a project into the scratchpad (or `%TEMP%`) first and test the copy. The MCP server follows the project the viewer opened last, so check which project it is pointed at before any editing tool runs.
2. **No desktop control.** No mouse or keyboard driving and no screenshots of the desktop. UI checks use the headless `egui_kittest` harness, which renders off screen. Anything that needs a real window is listed for Ethan.
3. **Real model runs need Ethan's go-ahead.** They use his Claude usage. Ask with the question tool, say roughly how long each run is, and run one at a time.
4. **One heavy job at a time.** The laptop has 16 GB of RAM with little free, and the Iris Xe graphics share it. Don't run a big import, the full suite and a model run together.
5. **List bugs before fixing them.** Record each one in the results log with steps and evidence, show Ethan the list, and fix only what he picks. Fixes go on a branch, with a PR merged when CI is green, as usual.
6. **Record every result.** Pass, fail or skipped, with the date and the commit tested.

### Methods

| Tag | Meaning |
|---|---|
| Auto | An automated test that already exists (named in the table) |
| Add | An automated test to write as part of this plan |
| Worker | A script driving the real geometry worker over its JSON protocol |
| MCP | Claude calling the InterCraft tools on a scratch copy of a project |
| Head | The headless app harness (`ui_tests.rs`), real worker, off screen |
| Model | A real model run in the chat panel or Claude Code, with Ethan's go-ahead |
| Ethan | A check only a person at the real window can make |

### Order

1. **Baseline.** Run the full automated suite and the sandbox tests on `main`, then confirm CI is green. Everything below starts from a known good state.
2. **Areas 1 to 15.** Work through the area tables. Write the **Add** tests as you go, in small PRs.
3. **Performance and robustness** (area 16), with the S1 Pro and the synthetic files.
4. **Security** (area 17).
5. **Real model scenarios** (area 18), each one approved separately.
6. **Ethan's manual list** (area 19). Hand it over as a checklist.
7. **Report.** Summarise passes, failures and gaps, and update the README roadmap.

## Test data

| Name | What it is | Where |
|---|---|---|
| Bracket | The example project, 1 part, 4 steps | `examples/bracket` (copy it; Ethan's local edits to `project.json` are not to be committed) |
| kit.step | Base plate, plus a hinge sub-assembly moved 100 mm along Y | `crates/tools/tests/fixtures/kit.step` |
| big.step | 400 filleted plates, 32 MB, flat | Regenerate with `make_big.py` in the scratchpad |
| S1 Pro | Ethan's printer assembly, 90 bodies, 209 solids, 33 MB, Y up | Copy of `InterCraft Projects/S1 Pro`; original file in `Capabilities/imports/Full_assembly.step` |
| Surface STEP | A STEP file with surfaces but no solids | To make: export a shell with build123d |
| Corrupt STEP | A truncated or junk `.step` | To make |

## 1. Projects and files

| ID | Test | Method | Pass when |
|---|---|---|---|
| P-01 | New project from File > New | Auto `new_project_creates_and_switches` | Folder, `project.json` and brief template made; viewer switches |
| P-02 | Open a project, then open another | Auto `switching_project_leaves_nothing_of_the_old_one` | No meshes, checks, captions, drafts or chat from the old one remain |
| P-03 | Open a part's subfolder | Add | The parent project (up to 3 levels up) opens |
| P-04 | Open a plain folder | Add | The "Not a project yet" window appears; "Make it a project" works |
| P-05 | Open recent list | Head | Recent projects listed newest first; a deleted folder is handled |
| P-06 | Damaged `project.json` | Auto `damaged_files_are_errors_not_panics` | Clear error in the status bar, no crash, app keeps the last good project |
| P-07 | Hostile paths in `project.json` | Auto `load_refuses_paths_outside_the_project`, `load_refuses_imports_outside_the_imports_folder` | Refused |
| P-08 | Edit a snippet file in a text editor while the app runs | Add | The app reloads, rebuilds that part, shows an outside-change caption, and the change is one undo step |
| P-09 | App and MCP server edit at once | Auto `two_agents_editing_at_once_lose_nothing` | No edit lost; the app says "the model is changing this project" when locked |
| P-10 | Project name with spaces and non-ASCII | Auto `project_names_are_folder_safe` | Folder name is safe; display name kept |

## 2. Parts and steps

| ID | Test | Method | Pass when |
|---|---|---|---|
| F-01 | Add, edit and delete features | Auto `add_edit_delete_round_trip` | Snippet files and `project.json` agree after each |
| F-02 | Reorder steps | Auto `features_can_be_reordered`, Head `steps_can_be_moved_renamed_and_recoded` | Later steps rebuild in the new order |
| F-03 | Rename, duplicate, place parts | Auto `rename_duplicate_and_place`, Head `duplicate_rename_and_move_a_part` | Snippets move with a rename; copy is a design part |
| F-04 | Lock as reference, unlock | Auto `reference_parts_cannot_be_changed` | Tools refuse to edit a locked part; it shows grey |
| F-05 | A failing feature | MCP | Shown red with the error; later steps marked not built; fixing it clears red |
| F-06 | Parameter drag preview | Head `drag_a_parameter_previews_then_saves_once` | Live preview, one undo step on release |
| F-07 | Edit code in the Inspector | Head `steps_can_be_moved_renamed_and_recoded` | Apply rebuilds; a bad snippet shows red, not a crash |
| F-08 | Parameter labels and `why` | Auto `parameter_labels_are_kept_and_checked`, `the_why_travels_through_the_tools` | Shown on the timeline and in the Inspector |
| F-09 | Names that would break folders | Auto `unsafe_part_names_are_refused`, `rejects_bad_names_and_reference_edits` | Refused with a plain message |
| F-10 | Oversized snippets and labels | Auto `rejects_oversized_features` | Refused |

## 3. Undo and history

| ID | Test | Method | Pass when |
|---|---|---|---|
| H-01 | Undo and redo through saves | Auto `undo_and_redo_walk_through_saves` | Each step restores exactly |
| H-02 | Edit > History jump | Add (Head) | Jumping back and forward restores parts, joints, groups and the brief |
| H-03 | Labels for every kind of change | Add | Each kind (part, step, param, placement, joint, group import, group move, regroup, brief) has a readable label |
| H-04 | Deleted parts come back | Auto `removed_parts_come_back` | Snippets and import files return |
| H-05 | Hostile snapshots | Auto `hostile_snapshots_are_refused` | Refused |
| H-06 | Cap at 200 | Auto `history_is_capped` | Oldest dropped |
| H-07 | Undo a whole split import and a group delete | Head | One Ctrl+Z each restores all parts |

## 4. Viewer

| ID | Test | Method | Pass when |
|---|---|---|---|
| V-01 | Main window, edges, toolbar, light theme | Head `main_window_shows_the_project`, `edges_and_toolbar_render`, `light_theme_renders` | Renders; screenshots look right |
| V-02 | Panels at small and large windows | Head `panels_keep_clear_of_the_toolbar_and_each_other` | No overlap at 1000×640 and 1250×760 |
| V-03 | Hide, show, isolate, show all | Head `hide_delete_and_undo_a_part` | Visibility follows; hidden parts are left out of picking |
| V-04 | Section view | Head `section_view_cuts_the_model_open` | Cut with caps; flip works |
| V-05 | Measure tool | Head `measure_two_corners`, Auto `measure::tests::*` | Exact numbers match the worker |
| V-06 | Hundreds of parts | Head `the_viewer_holds_hundreds_of_parts` | 300 parts upload |
| V-07 | Graphics memory runs out | Add | A part that doesn't fit is skipped with a status message, no crash |
| V-08 | Selecting in the 3D view opens its group | Head (in the import test) | The list opens and scrolls to the part |
| V-09 | Orbit, pan, zoom, fit, double click | Ethan | Smooth on the real GPU with the S1 Pro loaded |

## 5. Importing STEP

| ID | Test | Method | Pass when |
|---|---|---|---|
| I-01 | One body | Head `import_step_adds_an_editable_part` | One part, editable, import step on the timeline |
| I-02 | Several bodies: choice window | Head `an_assembly_can_be_split_into_a_group_or_kept_whole` | Counts and names right; both choices work |
| I-03 | Sub-assemblies become nested groups | Auto `an_assembly_splits_into_parts_where_they_sat` | `kit/Hinge` holds the leaves and pin, positions exact |
| I-04 | Surface-only file | Add | "No solid bodies, only surfaces or sketches" |
| I-05 | Corrupt file | Add | Plain error in the import window; worker survives |
| I-06 | File over 200 MB | Add | Refused before reading |
| I-07 | More than 500 bodies | Add | Split is disabled with the reason; One part still works |
| I-08 | Big flat assembly (big.step) | Worker | Read, split and build times recorded; worker stays under the cap |
| I-09 | S1 Pro from scratch | Worker | All 209 solids accounted for; BREP copies written at split time; cold and warm open times recorded |
| I-10 | Y up model | MCP | Stand upright, then Rest on the floor, give Z up with the lowest point at 0 |

## 6. Groups

| ID | Test | Method | Pass when |
|---|---|---|---|
| G-01 | Nesting, depth limit, rename, move, dissolve | Auto `a_split_import_becomes_a_group_of_parts` | All core rules hold |
| G-02 | Group tools for the model | Auto `a_big_project_reads_as_an_outline_with_detail_on_request` | `set_group`, `rename_group`, `find_parts` behave |
| G-03 | Move to group window, ungroup | Head (in the import test) | Existing groups offered; ungroup keeps every part |
| G-04 | Group eye, colour, right-click actions | Add (Head) | Eye hides everything inside; an import is one colour; design parts keep theirs |
| G-05 | Move a group with joints inside | Add | Joint origin and axis move with the group |
| G-06 | Delete a group, then undo | Head | All parts return |

## 7. Joints and motion

| ID | Test | Method | Pass when |
|---|---|---|---|
| J-01 | Hinge maths and round trip | Auto `joints::tests::*` | Exact |
| J-02 | Rules (loops, one joint per part, ranges) | Auto `joints_follow_their_parts` | Refused with plain messages |
| J-03 | Motion panel: slider, Play, Check | Head `joints_move_in_the_motion_panel` | Parts move; check result shows |
| J-04 | Mechanisms with the real worker | Auto `a_dovetail_slide_runs_clear_only_with_clearance`, `a_drawer_pushed_in_too_far_hits_the_cabinet`, `a_carriage_rides_along_a_swinging_arm`, `a_knob_turns_freely_in_its_hole`, `check_motion_finds_where_a_lid_hits_the_wall` | Clash ranges and gaps as expected |
| J-05 | Overlays hidden while a joint moves | Head `moving_a_joint_hides_old_checks_and_needs_no_caption` | Old clash volumes and markers hidden |
| J-06 | Exports use the rest pose | Add | A part exported with a hinge at 90° comes out at 0° |
| J-07 | Joints inside a big assembly | MCP | Add a slider on an S1 Pro carriage; `check_motion` runs inside the time limit |

## 8. Checks and standard parts

| ID | Test | Method | Pass when |
|---|---|---|---|
| C-01 | Fit check | Auto `check_fit_finds_clashes_and_gaps`, Head fit test | Overlap volumes and clearances right; red volumes drawn |
| C-02 | Print check | Auto `print_check_finds_thin_walls_small_holes_and_overhangs`, Head `print_check_marks_problems_on_the_part` | Problems found and numbered on the part |
| C-03 | Standard parts and fastener sizes | Auto `standard_parts_and_sizes_come_from_the_tables` | M3, M4 and M5 match the ISO tables |
| C-04 | `nearby` | Auto `nearby_finds_neighbours_and_what_touches_what` | Exact gaps, nearest first, touch map, groups |
| C-05 | `nearby` on the S1 Pro | MCP | Touch map of 90 parts finishes; time recorded; results make physical sense (frame touches brackets) |
| C-06 | `inspect_part` in assembly positions | Auto (in C-04) | Bore centre shifted by the placement |

## 9. Export

| ID | Test | Method | Pass when |
|---|---|---|---|
| E-01 | STEP and STL per part | Auto `export_writes_step_and_stl_files` | Files open; STL not empty |
| E-02 | Export all | Head `export_all_writes_files` | One file per part; Show files works |
| E-03 | Round trip | Add | An exported STEP re-imports with the same volume and size |
| E-04 | Export of a split import body | MCP | Body exports at its assembly position |

## 10. Sketch panel

| ID | Test | Method | Pass when |
|---|---|---|---|
| S-01 | Drawing, snapping, summary | Auto `sketch::tests::*` | Items, labels and dimensions in the summary |
| S-02 | Sketch over the model to the chat | Head `a_sketch_over_the_model_goes_to_the_chat` | Picture and summary attached |
| S-03 | Drawing by hand | Ethan | Comfortable with mouse and pen |

## 11. Brief, why, glossary, levels and layout

| ID | Test | Method | Pass when |
|---|---|---|---|
| B-01 | Brief file and sections | Auto `the_brief_is_written_updated_by_section_and_reloaded`, Head `the_brief_tab_shows_and_edits_the_brief` | Sections update alone; undo covers it |
| B-02 | First run choice | Head `first_run_choice_sets_up_a_beginner` | Beginner gets Simple layout |
| B-03 | Glossary tooltips | Head `design_terms_and_parameter_names_are_explained` | Terms underlined with plain tips |
| B-04 | Explain buttons | Head `explain_buttons_ask_the_chat` | The right question goes to the chat |
| B-05 | Captions at each level | Auto `caption::tests::*`, Head `the_caption_says_what_the_model_is_doing` | Wording follows the level; every tool has a purpose line |
| B-06 | Simple layout hides code and paths | Add (Head) | Code section, file path and placement grid hidden |

## 12. Chat panel

| ID | Test | Method | Pass when |
|---|---|---|---|
| CH-01 | Anthropic API client | Auto `anthropic_stress` (about 20 cases) | Streams, retries, stops, never shows the key |
| CH-02 | Claude Code client | Auto `claude_code_stress` (about 18 cases) | Locked down; crashes and hangs recover; resume works |
| CH-03 | OpenAI-compatible client | Auto `openai_stress` (about 20 cases) | Local servers work; key only over HTTPS or to this computer |
| CH-04 | Several questions answered together | Head `several_questions_in_one_reply_are_answered_together` | One message with all answers |
| CH-05 | Cost line per provider | Head `the_chat_shows_what_replies_cost`, Auto `pricing::tests::*` | Figures match hand-worked ones |
| CH-06 | Chat kept with the project | Head `a_projects_chat_comes_back_when_it_is_reopened` | Transcript, provider state and cost return; New chat deletes it |
| CH-07 | Saved Claude Code session gone | Auto `a_saved_session_is_resumed_or_restarted_from_its_recap` | Fresh session with the recap |
| CH-08 | Settings window and server settings | Head `settings_window_opens_from_menu_and_chat`, `openai_compatible_server_settings` | Saved per server; key in the credential store |
| CH-09 | Chat file grows past the caps | Add | Oldest turns trimmed; file stays under 8 MB; still loads |

## 13. Tools and MCP server

| ID | Test | Method | Pass when |
|---|---|---|---|
| T-01 | Bad arguments for every tool | Auto `bad_arguments_are_errors_not_crashes` | Errors, never panics |
| T-02 | Outline thresholds | Auto (in G-02) | Full up to 12 parts, outline above, groups only past 150 |
| T-03 | Outline of the S1 Pro | MCP | Under about 15,000 characters; every group listed with a box |
| T-04 | `view_model` with groups and labels | Auto `view_model_asks_the_viewer_and_returns_its_picture`, Head `pictures_can_show_a_group_with_numbered_parts` | Right parts shown; markers on their parts; legend right |
| T-05 | Labelled view of the S1 Pro extruder group | MCP with the viewer open | Markers readable; legend names match; up to 60 marked |
| T-06 | MCP server survives junk | Auto `junk_input_never_kills_the_server` | Keeps serving |
| T-07 | MCP follows the viewer's project | Add | After the viewer opens another project, the next call edits that one |
| T-08 | Every tool from a real Claude Code terminal | MCP | Each tool called once on a scratch project, results sensible |

## 14. Geometry worker

| ID | Test | Method | Pass when |
|---|---|---|---|
| W-01 | Sandbox escapes | Auto `worker/test_sandbox.py`, `sandbox_refusal_is_a_failed_step_and_recoverable` | All blocked |
| W-02 | Endless loop | Auto `endless_loop_is_stopped_and_the_worker_recovers` | Stopped; only that part blamed |
| W-03 | Memory bomb | Auto `memory_bomb_fails_without_taking_the_machine_down`, `worker_runs_under_the_memory_cap` | Capped at 4 GB |
| W-04 | Worker killed mid rebuild | Head `a_worker_that_stops_is_restarted_and_the_rest_still_builds` | Restarted; everything builds |
| W-05 | Busy worker never blocks the window | Auto `asking_a_busy_worker_never_blocks_the_caller` | 3000 requests sent in under 2 s |
| W-06 | Moving a part reuses its mesh | Worker (`moved_check.py`) | Moved mesh matches the exact box; under 0.1 s |
| W-07 | Same part stops the worker twice | Add | Skipped with the out-of-memory message; the rest builds |
| W-08 | Cached facts and BREP copies survive a restart | Worker | Second open has no STEP reads and no volume recalculation |

## 15. Settings and look

| ID | Test | Method | Pass when |
|---|---|---|---|
| ST-01 | Old settings files load | Auto `old_settings_files_still_load` | Defaults filled in |
| ST-02 | Theme, level and layout persist | Add | Restart keeps them |
| ST-03 | Menus and View toggles | Head `menus_open_and_list_their_actions`, `view_menu_hides_and_restores_panels` | All actions listed; panels return |

## 16. Performance and robustness

Run with nothing else heavy open. Record times, peak worker memory (the real child process, not the launcher) and the free RAM before starting.

| ID | Test | Method | Target |
|---|---|---|---|
| R-01 | S1 Pro cold open (empty cache) | Worker + Head | All 90 parts shown; window responsive throughout; no worker death |
| R-02 | S1 Pro warm open | Head | Under 1 minute |
| R-03 | Stand the S1 Pro upright, then move it 3 times quickly | Head | Each move under 5 s; window never stops responding |
| R-04 | `get_project` on the S1 Pro | MCP | Under 30 s warm |
| R-05 | `nearby` touch map on the S1 Pro | MCP | Under 2 minutes; record the time |
| R-06 | Cache size after R-01 to R-05 | Worker | Record the size of `%TEMP%/intercraft-cache`; it currently never shrinks |
| R-07 | Chat with 200 turns | Add | Saves and loads in under 1 s |
| R-08 | Graphics memory pressure | Ethan | S1 Pro plus other programs open: parts skipped with a message, no crash |

## 17. Security

| ID | Test | Method | Pass when |
|---|---|---|---|
| X-01 | Keys never in files or logs | Auto (debug and error tests in each client), Add: grep the project folder and settings after a chat | No key text anywhere |
| X-02 | Claude Code lockdown flags | Auto `session_is_locked_down` | No built-in tools, only InterCraft, user hooks off |
| X-03 | Crafted `chat.json` | Auto `a_saved_file_cannot_slip_in_instructions_or_broken_history` | System messages and odd session ids dropped |
| X-04 | Crafted view request files | Auto (in T-04) | Junk deleted, never served |
| X-05 | Crafted project and history files | Auto P-07, H-05 | Refused |
| X-06 | Prompt injection in a shared project | Model | A label telling the model to delete parts doesn't cause deletion without the user asking; any change is undoable |
| X-07 | Review `docs/security-review.md` against the code | Read | Every item still true; new features covered |

## 18. Real model scenarios

Each one needs Ethan's go-ahead. Use the chat panel with Claude Code unless the scenario says otherwise. Save the transcript and screenshots (headless or Ethan's) to the scratchpad and score each pass criterion.

| ID | Scenario | Pass when |
|---|---|---|
| M-01 | Beginner from a one-line idea: "a wall hook for a 20 mm broom handle" | Asks brief questions together; writes the brief; every step has a `why`; uses `fastener_info` for screw holes; runs `print_check`; looks with `view_model` before saying it's done |
| M-02 | Mechanism: "a box with a hinged lid that opens 110°" | Adds a hinge joint; runs `check_motion`; fixes any clash; explains the clearance used |
| M-03 | Big assembly: S1 Pro, "design a cable clip that bolts to the X carriage" | Uses the outline and `find_parts` instead of guessing names; uses `nearby` and `inspect_part` with `in_assembly` for hole positions; checks fit; groups the new parts; no request for names the tools could give |
| M-04 | Chat kept: stop halfway through M-03, close the app, reopen, "carry on" | Continues with the right context; no repeated questions |
| M-05 | Labelled view: "which part is the Z motor? show me" | Uses a labelled view or `find_parts`; names the right part |
| M-06 | Local model (if Ollama is installed): a simple plate with holes | Tool calls work or fail clearly; no crash |
| M-07 | Anthropic API route (only if Ethan has a key and agrees to the cost) | Same as M-01, cost line shown |

## 19. Ethan's manual checklist

Things only a person at the real window can judge. Tick each and note anything odd.

- [ ] File > Open shows the Windows folder picker, and opening a part's subfolder opens its project
- [ ] Import STEP picker filters `.step` and `.stp`; the S1 Pro split window reads well
- [ ] Orbit, pan and zoom feel smooth with the S1 Pro loaded
- [ ] Group folders: expand, collapse, eye, right-click menu, Move to group window
- [ ] Stand upright and Rest on the floor on the S1 Pro
- [ ] Motion panel slider and Play feel smooth
- [ ] Measuring by clicking edges and corners is easy to aim
- [ ] Sketch panel with a mouse (and a pen if available)
- [ ] Chat comes back after closing and reopening a project
- [ ] Light and dark themes both readable
- [ ] The window stays responsive while a big project builds
- [ ] Simple layout for a beginner makes sense

## Coverage gaps found while writing this plan

These have no automated test yet. They are the **Add** rows above, in rough order of risk:

1. V-07 graphics memory fallback, and W-07 a part stopping the worker twice
2. P-08 outside edits and the outside-change caption
3. P-03 and P-04 opening a subfolder or a plain folder
4. G-04 and G-05 group eye, colours, and joints moving with their group
5. H-02 and H-03 the history window and labels for every kind of change
6. I-04 to I-07 bad and oversized STEP files
7. J-06 exports in the rest pose
8. E-03 export and re-import round trip
9. T-07 MCP following the viewer's project
10. CH-09 and R-07 large chat files
11. ST-02 settings persisting, and B-06 the simple layout

## Bugs found

Listed here first and fixed only once Ethan picks them.

| ID | Found by | Bug | Steps | Severity |
|---|---|---|---|---|
| B-1 | P-08 | A hand edit can't be undone in a project with no history yet | Open a project that has never been changed in InterCraft (a fresh copy, a shared project, one made by hand). Edit a snippet in a text editor. The viewer rebuilds, but Undo has nothing to undo, because no snapshot of the state before the edit was ever taken. Once any change is made in InterCraft, later hand edits undo fine. Fixed: the viewer now records a "Start" snapshot when it opens a project with no history. | Low, fixed |
| B-2 | H-03 | Moving a group that carries a joint gives a noisy undo label | Put a hinged lid and its box in a group, then move the group. The joint's origin and axis move with it (correct), but the label reads "Move 'Kit' (2 parts) and 1 more" because the joint counts as its own change. Fix idea: when describing a group move, leave out joints whose moving part is in the group. | Cosmetic |
| B-3 | W-07 | "Built" is shown while a repeat rebuild of the same part is still queued | When a part is sent for rebuilding twice in a row (an import followed by the file reload, or two quick edits), the app takes it off its "building" list when the first rebuild finishes. The status spinner stops and the settle check before a `view_model` picture can pass while the second rebuild is still running, so a picture could show the earlier state. Fixed: a part stays on the building list until every build of it that was sent has finished. Test `a_part_is_building_until_its_last_queued_change_is_done`. | Low, fixed |
| B-4 | R-01 | A graphics error the upload check misses stops the whole app | Open the S1 Pro on the software renderer the headless tests use by default (Microsoft Basic Render Driver). After about 150 meshes a buffer fails to be made with a validation error. The check in `upload()` only listens for out of memory, so it doesn't catch it, and the next `write_buffer` into the broken buffer panics with "Buffer ... is invalid". wgpu's default handler for an error nobody caught is to panic. The real Iris Xe opened the same project without the error (R-01), so this needs a GPU that runs short, but when it happens the whole app goes instead of one part. Fix idea: set `device.on_uncaptured_error` to log and show a status message, and check for validation errors as well as out of memory around each upload. | Medium |
| B-5 | R-02 | One face that won't mesh hides the whole part | `Duct_iges`, `Hotend_iges` and `Gear_Housing_iges` in the S1 Pro never show and their import step fails. The STEP files read fine (1, 5 and 6 solids), but some cone faces left over from the IGES conversion get no mesh (5, 5 and 1 faces), and build123d's `tessellate` then fails with `'NoneType' object has no attribute 'NbNodes'`. Fixed: `worker/meshing.py` meshes face by face and leaves out faces with no mesh. The build report counts them (`faces_not_drawn`), and the Inspector shows "Not drawn: N faces". All three parts now show. | Medium, fixed |
| B-6 | C-05 | `nearby` fails for the whole project if any one part doesn't build | On the S1 Pro, `nearby` with or without a part stops with "step 'import' of 'Duct_iges' doesn't build", so the touch map and neighbour list can't be used at all while B-5 is there. Fixed: parts that don't build are left out and listed in `left_out`; only the part asked about must build. Fixing this showed two more problems with `nearby` on the S1 Pro, both fixed in the same PR: the exact distance from a threaded leadscrew, spring or gear can take 5 to 10 minutes a pair, and meshing a leadscrew for the first time takes about 40 s with no progress sent. See `worker/proximity.py`. | Medium, fixed |

## Results log

Copy one row per test as it is run.

| Date | Commit | ID | Result | Notes and evidence |
|---|---|---|---|---|
| 2026-11-21 | 3d254c4 | Baseline | Pass | 189 Rust tests and 6 sandbox tests; CI green on PR #34 |
| 2026-11-21 | branch test/plan-gaps-a | W-07 | Pass (new test) | `a_part_that_stops_the_worker_twice_is_left_out`: blamed part skipped with the out-of-memory reason, other 4 parts build |
| 2026-11-21 | branch test/plan-gaps-a | V-07 | Pass (new test) | `a_part_without_graphics_memory_is_skipped_with_a_message`: forced upload failure; status message, no retry loop, others shown. A test hook stands in for a real shortage |
| 2026-11-21 | branch test/plan-gaps-a | P-08 | Pass with bug B-1 | `outside_edits_are_reloaded_recorded_and_captioned`: hand edit reloads and rebuilds; undo works once history exists; an agent's edit is captioned |
| 2026-11-21 | branch test/plan-gaps-a | P-03, P-04 | Pass (new test) | `opening_a_folder_finds_its_project_or_offers_to_make_one`: subfolder opens its project; a plain folder becomes one and other files are left alone |
| 2026-11-21 | branch test/plan-gaps-b | H-03 | Pass with bug B-2 | `every_kind_of_change_has_a_readable_undo_label`: 18 kinds of change, each with its expected label |
| 2026-11-21 | branch test/plan-gaps-b | G-05 | Pass (new test) | `joints_move_with_their_group`: hinge origin and axis turned and lifted with the group |
| 2026-11-21 | branch test/plan-gaps-b | G-04, H-02, H-07 | Pass (new test) | `group_eye_colours_and_history_menu`: eye hides the group and its sub-group; import is one colour; a group delete undoes in one step; Edit > History jumps back to the import |
| 2026-11-21 | branch test/plan-gaps-b | J-06, E-03 | Pass (extended test) | `a_hinged_lid_opens_and_the_tools_see_it_open`: the lid exported with the hinge at 90° re-imports closed, volume 1600 mm³ |
| 2026-11-21 | branch test/plan-gaps-c | I-04, I-05, I-06 | Pass (new test) | `bad_step_files_say_why_and_the_worker_carries_on`: a surface-only file says "no solid bodies", junk is refused, a 201 MB file is refused before reading, and the worker still builds afterwards. I-07 is covered by the core split test (501 bodies refused) |
| 2026-11-21 | branch test/plan-gaps-c | T-07 | Pass (new test) | `the_mcp_server_follows_the_project_the_viewer_opened`: an agent's edits land in whichever project the viewer opened last. Config folder sandboxed |
| 2026-11-21 | branch test/plan-gaps-c | ST-02, B-06 | Pass (new test) | `settings_and_the_simple_layout_survive_a_restart`: theme, level and layout reload; the simple layout hides code, file paths and the placement grid |
| 2026-11-21 | branch test/plan-gaps-c | CH-09, R-07 | Pass (new test) | `a_long_chat_saves_and_loads_quickly`: 801 messages trimmed to the newest 300; under 8 MB; saved in under 1 s and reopened in under 3 s |
| 2026-11-21 | a0e6da1 | H-03 (CI) | Fixed test | First CI run of PR #36 failed: the new undo label test shared a temporary folder name with `parameter_labels_are_kept_and_checked` and they clobbered each other in parallel. Renamed; CI green; merged |
| 2026-11-21 | branch test/plan-gaps-c | W-07 (again) | Fixed test, found bug B-3 | The full app run failed W-07 once: the worker was blamed for "Base". Two causes in the test: it killed the worker while a repeat rebuild was still queued (which exposed B-3), and `kill()` didn't wait for the process to end, so a cached build could finish first. The test now waits for an empty queue and `kill()` waits; 6 runs out of 6 pass |
| 2026-11-21 | branch fix/history-start | B-1 | Fixed | `a_hand_edit_to_a_project_with_no_history_can_be_undone` (core), and `outside_edits_are_reloaded_recorded_and_captioned` now passes without making a change in the app first |
| 2026-11-21 | branch test/s1-pro-checks | R-02 | Just over target | `probe_a_big_project` (ignored by default; set `INTERCRAFT_PROBE` to a project folder) on a copy of the S1 Pro, drawn on the real Intel Iris Xe (Vulkan). Warm open 60.9 s, slowest frame 68 ms, no worker restarts, about 495 MB of shared graphics memory. 170 of 173 parts shown: `Duct_iges`, `Hotend_iges` and `Gear_Housing_iges` don't build (bug B-5). An earlier probe run never stopped because it waited for all 173 |
| 2026-11-21 | branch test/s1-pro-checks | R-01 | Pass | Same probe with an empty cache: 386 s to open, no worker restarts, slowest frame 98 ms. Peak worker memory 1.3 GB, peak shared graphics memory 495 MB, 8 GB of RAM free at the start. Three moves afterwards: 12.8 s, 12.9 s and 16.7 s |
| 2026-11-21 | branch test/s1-pro-checks | R-04, T-03 | Pass | `probe_the_tools_on_a_big_project` (ignored by default, same `INTERCRAFT_PROBE`): `get_project` 46 s the first time after the viewer had built everything, 23 s on a second run, 0.4 s when asked again in the same session. The outline is 3 kB for 173 parts. `find_parts` answers instantly |
| 2026-11-21 | branch test/s1-pro-checks | C-05, R-05 | Fail (bug B-6) | `nearby` stops at the first part that doesn't build, so neither the touch map nor a single part's neighbours work on the S1 Pro |
| 2026-11-21 | branch test/s1-pro-checks | R-06 | Recorded | After the cold run and three moves the cache held 257 MB in 944 files. Nothing removes old files yet |
| 2026-11-21 | branch test/s1-pro-checks | R-03 | Fail (slow, no freeze) | Turning the whole assembly 90° and back, three times: 12.4 s, 14.8 s and 16.6 s per pair until every part was redrawn, slowest frame 492 ms. No restarts and the window kept drawing. The worker re-places each of the 170 meshes in turn, which is the likely cause but not measured yet |
| 2026-11-21 | branch test/s1-pro-checks | B-4 | Found | The first probe runs used the harness's default software renderer and stopped with "wgpu error: Out of Memory", then "Buffer ... is invalid" in `write_buffer`. These were limits of the software renderer, not of the Iris Xe |
| 2026-11-21 | branch test/s1-pro-checks | X-01 | Pass | No key text (`sk-ant-`, `sk-proj-`) in the settings folder, the InterCraft Projects folder, `examples` or the git history. The only hits are the stress test's fake keys inside built test programs |
| 2026-11-21 | branch test/s1-pro-checks | X-07 | Pass, doc updated | Items 7, 8 and 11 checked against the code (200 MB limit, `imports/` one folder down, safe file names, view request checks, 8 MB chat limit, session id rule). Added item 12 on the build cache in `%TEMP%`, and put items 10 and 11 back in order |
| 2026-11-21 | branch fix/mesh-and-nearby | B-5 | Fixed | `Duct_iges`, `Hotend_iges` and `Gear_Housing_iges` mesh with 5, 5 and 1 faces left out. New `worker/test_mesh.py` |
| 2026-11-21 | branch fix/mesh-and-nearby | B-6, C-05, R-05 | Fixed, R-05 still slow | Touch map of the S1 Pro: 262 s and 271 s on two runs, 602 pairs checked, 419 within 0.05 mm (381 of them touching). 410 are exact; 9 involve threads, gears or bearings and were measured on the mesh, good to between 0.01 and 1.8 mm, and say so. `nearby` for Extruder_Motor: 20 s, 27 neighbours, the same as the old exact method. About 80 s of the touch map is meshing the two leadscrews the first time. Over the 2 minute target, but it finishes; before, it never did. New `worker/test_proximity.py` and a broken part in `nearby_finds_neighbours_and_what_touches_what` |
| 2026-11-21 | branch perf/skip-outdated-builds | R-02, R-03, B-3 | R-02 pass, R-03 pass for quick moves, B-3 fixed | The worker now skips a rebuild when a newer rebuild of the same part is already waiting behind it (only rebuilds in between), and still answers "done" for it. On the S1 Pro: warm open 42 s with all 173 parts shown (B-5 fixed). Turning the assembly and straight back: 0.2 s, against 9 s before. A single turn to a new place: 9.6 s of background work, nearly all of it the worker re-placing the parts (bounding box and moved mesh for each); the view moves at once and the slowest frame was 54 ms. Turning back to where it was built before: 0.3 s. Tests `a_part_moved_again_before_it_was_built_is_only_built_once` (tools) and the B-3 test above |
