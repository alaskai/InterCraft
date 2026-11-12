// Robustness tests for the tools the model calls, with the real geometry worker.
// They need the worker's Python environment (see the README); without it they skip.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use intercraft_tools::ToolHost;
use serde_json::{json, Value};

/// Each worker loads OpenCascade (about 500 MB), so only one worker test runs at a time.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn worker_available() -> bool {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| std::env::set_var("INTERCRAFT_BUILD_TIMEOUT", "5"));
    let venv = std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("InterCraft/venv/Scripts/python.exe"))
        .is_some_and(|p| p.exists());
    if !venv && std::env::var_os("INTERCRAFT_PYTHON").is_none() {
        eprintln!("skipping: the geometry worker's Python environment isn't set up");
        return false;
    }
    true
}

fn project(name: &str) -> (ToolHost, PathBuf) {
    let dir = std::env::temp_dir().join(format!("intercraft-tools-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (ToolHost::new(dir.clone()), dir)
}

fn add(host: &mut ToolHost, part: &str, label: &str, code: &str) -> anyhow::Result<Value> {
    host.call("add_feature", &json!({ "part": part, "label": label, "code": code }))
}

#[test]
fn bad_arguments_are_errors_not_crashes() {
    let (mut host, dir) = project("args");
    host.call("add_part", &json!({ "name": "P" })).unwrap();
    let cases = [
        ("add_part", json!({})),
        ("add_part", json!({ "name": 5 })),
        ("add_part", json!({ "name": ["P"] })),
        ("add_feature", json!({ "part": "P" })),
        ("add_feature", json!({ "part": "P", "label": "x", "code": 42 })),
        ("add_feature", json!({ "part": "P", "label": "x", "code": "part = Box(1,1,1)", "params": "not an object" })),
        ("add_feature", json!({ "part": "Nope", "label": "x", "code": "part = Box(1,1,1)" })),
        ("add_feature", json!({ "part": "P", "label": "x", "code": "part = Box(1,1,1)", "after": "f999" })),
        ("edit_feature", json!({ "part": "P", "feature_id": "f999" })),
        ("set_param", json!({ "part": "P", "feature_id": "f010", "name": "w" })),
        ("delete_feature", json!({ "part": "P", "feature_id": "../../etc" })),
        ("export_part", json!({ "part": "P", "format": "obj" })),
        ("no_such_tool", json!({})),
        ("add_part", Value::Null),
        ("add_part", json!("a string")),
    ];
    for (tool, args) in cases {
        assert!(host.call(tool, &args).is_err(), "{tool} accepted {args}");
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn unsafe_part_names_are_refused() {
    let (mut host, dir) = project("names");
    for name in ["..", "../escape", "a/b", "a\\b", "C:", "", " ", "CON", "nul", "Com1", "LPT9", "x".repeat(65).as_str()] {
        assert!(host.call("add_part", &json!({ "name": name })).is_err(), "accepted part name {name:?}");
    }
    assert!(!dir.join("..").join("escape").exists());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sandbox_refusal_is_a_failed_step_and_recoverable() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    let (mut host, dir) = project("sandbox");
    host.call("add_part", &json!({ "name": "P" })).unwrap();
    add(&mut host, "P", "Base", "part = Box(10, 10, 10)").unwrap();
    let report = add(&mut host, "P", "Sneaky", "import os\npart = Box(1, 1, 1)").unwrap();
    assert_eq!(report["ok"], false);
    let error = report["features"][1]["error"].as_str().unwrap();
    assert!(error.contains("imports aren't allowed"), "{error}");
    // The model can fix it.
    let fixed = host.call("delete_feature", &json!({ "part": "P", "feature_id": "f020" })).unwrap();
    assert_eq!(fixed["ok"], true);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn endless_loop_is_stopped_and_the_worker_recovers() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    let (mut host, dir) = project("loop");
    host.call("add_part", &json!({ "name": "P" })).unwrap();
    add(&mut host, "P", "Base", "part = Box(10, 10, 10)").unwrap();
    let started = Instant::now();
    let result = add(&mut host, "P", "Spin", "while True:\n    pass");
    let error = format!("{:#}", result.unwrap_err());
    assert!(error.contains("endless loop"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(30), "took {:?}", started.elapsed());
    // Remove the loop and carry on with a fresh worker.
    let report = host.call("delete_feature", &json!({ "part": "P", "feature_id": "f020" })).unwrap();
    assert_eq!(report["ok"], true);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn memory_bomb_fails_without_taking_the_machine_down() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    let (mut host, dir) = project("memory");
    host.call("add_part", &json!({ "name": "P" })).unwrap();
    // 8 GB of list on its own; the worker is capped at 4 GB.
    let result = add(&mut host, "P", "Hog", "x = [0] * (10**9)\npart = Box(1, 1, 1)");
    let failed = match &result {
        Ok(report) => report["ok"] == false,
        Err(_) => true,
    };
    assert!(failed, "an 8 GB allocation succeeded: {result:?}");
    host.call("delete_feature", &json!({ "part": "P", "feature_id": "f010" })).unwrap();
    let report = add(&mut host, "P", "Base", "part = Box(10, 10, 10)").unwrap();
    assert_eq!(report["ok"], true, "the worker didn't recover");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn worker_runs_under_the_memory_cap() {
    let _one = one_at_a_time();
    if !worker_available() || !cfg!(windows) {
        return;
    }
    let worker = intercraft_geo::PythonWorker::spawn(|| {}).unwrap();
    assert!(worker.memory_capped(), "the worker isn't inside the memory-capped job");
}

#[test]
fn reference_parts_cannot_be_changed() {
    let (mut host, dir) = project("reference");
    host.call("add_part", &json!({ "name": "Rail" })).unwrap();
    let manifest = dir.join("project.json");
    let text = std::fs::read_to_string(&manifest).unwrap().replace("\"design\"", "\"reference\"");
    std::fs::write(&manifest, text).unwrap();
    assert!(add(&mut host, "Rail", "Cut", "part = Box(1, 1, 1)").is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn two_agents_editing_at_once_lose_nothing() {
    // The chat panel and a Claude Code terminal can edit one project at the same time.
    let dir = std::env::temp_dir().join(format!("intercraft-tools-concurrent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut setup = ToolHost::new(dir.clone());
    setup.call("add_part", &json!({ "name": "A" })).unwrap();
    setup.call("add_part", &json!({ "name": "B" })).unwrap();

    let threads: Vec<_> = ["A", "B"]
        .into_iter()
        .map(|part| {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let mut host = ToolHost::new(dir);
                for i in 0..15 {
                    // Labels only: building isn't needed to test the bookkeeping.
                    host.call("add_part", &json!({ "name": format!("{part}{i}") })).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let project: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("project.json")).unwrap()).unwrap();
    let parts = project["parts"].as_array().unwrap().len();
    assert_eq!(parts, 32, "edits were lost: {parts} parts instead of 32");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ask_user_is_checked_and_only_offered_to_the_chat_panel() {
    let (mut plain, dir) = project("ask");
    let good = json!({ "question": "Which side?", "options": [{ "label": "Left" }, { "label": "Right" }] });
    assert!(plain.call("ask_user", &good).is_err(), "a plain MCP client got ask_user");
    let mut chat = ToolHost::new(dir.clone()).with_ask_user();
    assert_eq!(chat.call("ask_user", &good).unwrap()["shown"], true);
    let bad = [
        json!({ "question": "Pick", "options": [{ "label": "Only one" }] }),
        json!({ "question": "Pick", "options": [{ "label": "A" }, { "label": "a" }] }),
        json!({ "question": "", "options": [{ "label": "A" }, { "label": "B" }] }),
        json!({ "question": "Pick", "options": [{ "label": "" }, { "label": "B" }] }),
        json!({ "question": "Pick", "options": "A, B" }),
        json!({ "question": "Pick", "options": [{ "label": "A" }, { "label": "B" }, { "label": "C" }, { "label": "D" }, { "label": "E" }] }),
        json!({ "question": "Q".repeat(301), "options": [{ "label": "A" }, { "label": "B" }] }),
        json!({ "question": "Pick", "options": [{ "label": "A" }, { "label": "B" }], "multi_select": "yes" }),
    ];
    for args in bad {
        assert!(chat.call("ask_user", &args).is_err(), "accepted {args}");
    }
    let names: Vec<String> = intercraft_tools::chat_panel_definitions().as_array().unwrap().iter()
        .map(|t| t["name"].as_str().unwrap().to_owned()).collect();
    assert!(names.contains(&"ask_user".to_owned()));
    assert!(!intercraft_tools::definitions().to_string().contains("ask_user"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn export_writes_step_and_stl_files() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    let (mut host, dir) = project("export");
    host.call("add_part", &json!({ "name": "Block" })).unwrap();
    add(&mut host, "Block", "Base", "part = Box(20, 10, 5)").unwrap();
    let result = host.call("export_part", &json!({ "part": "Block" })).unwrap();
    assert_eq!(result["exported"].as_array().unwrap().len(), 2);
    let step = std::fs::read_to_string(dir.join("exports/Block.step")).unwrap();
    assert!(step.starts_with("ISO-10303-21"), "not a STEP file");
    let stl = std::fs::metadata(dir.join("exports/Block.stl")).unwrap();
    assert!(stl.len() > 84, "STL is empty");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn imported_part_can_be_inspected_measured_and_edited() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    // Make a STEP file to import: a 40 x 30 x 20 block with an 8.2 mm bore along Y.
    let (mut maker, maker_dir) = project("import-src");
    maker.call("add_part", &json!({ "name": "Block" })).unwrap();
    add(&mut maker, "Block", "Body", "part = Box(40, 30, 20) - Rot(90, 0, 0) * Cylinder(4.1, 100)").unwrap();
    maker.call("export_part", &json!({ "part": "Block", "format": "step" })).unwrap();

    let (mut host, dir) = project("import-dst");
    let mut loaded = intercraft_core::LoadedProject::create(&dir, "Imports").unwrap();
    let name = loaded.import_step(&maker_dir.join("exports/Block.step")).unwrap();
    loaded.save().unwrap();

    let info = host.call("inspect_part", &json!({ "part": name })).unwrap();
    let holes = info["cylindrical_faces"].as_array().unwrap();
    assert_eq!(holes.len(), 1, "{info}");
    assert!((holes[0]["diameter"].as_f64().unwrap() - 8.2).abs() < 1e-6);

    // The bore's circular edges are 30 mm apart (the block's depth).
    let circles: Vec<u64> = info["circle_edge_diameters"]["8.2"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    let gap = host
        .call("measure", &json!({ "part": name, "a": { "kind": "edge", "index": circles[0] }, "b": { "kind": "edge", "index": circles[1] } }))
        .unwrap();
    assert!((gap["distance"].as_f64().unwrap() - 30.0).abs() < 1e-6, "{gap}");
    let single = host.call("measure", &json!({ "part": name, "a": { "kind": "edge", "index": circles[0] } })).unwrap();
    assert_eq!(single["a"]["diameter"].as_f64().unwrap(), 8.2);
    assert!(host.call("measure", &json!({ "part": name, "a": { "kind": "edge", "index": 9999 } })).is_err());
    assert!(host.call("measure", &json!({ "part": name, "a": { "kind": "solid", "index": 0 } })).is_err());

    // The model edits the imported shape like any other part.
    let report = add(&mut host, &name, "Pocket", "part = part - Pos(0, 0, 10) * Box(10, 10, 4)").unwrap();
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["features"][0]["id"], "import");
    let change = report["features"][1]["volume_change_mm3"].as_f64().unwrap();
    assert!((change + 200.0).abs() < 1e-3, "the pocket should remove 200 mm³, removed {change}");

    std::fs::remove_dir_all(maker_dir).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn undo_redo_and_delete_part_through_the_tools() {
    // None of these need the worker.
    let (mut host, dir) = project("undo");
    host.call("add_part", &json!({ "name": "A" })).unwrap();
    host.call("add_part", &json!({ "name": "B" })).unwrap();
    let names = |host: &mut ToolHost| -> Vec<String> {
        let p = intercraft_core::LoadedProject::load(&host.project_dir().unwrap()).unwrap();
        p.project.parts.iter().map(|p| p.name.clone()).collect()
    };

    let deleted = host.call("delete_part", &json!({ "part": "A" })).unwrap();
    assert_eq!(deleted["deleted_part"], "A");
    assert_eq!(names(&mut host), ["B"]);
    let undone = host.call("undo", &json!({})).unwrap();
    assert_eq!(undone["undone"], "Delete part 'A'");
    assert_eq!(names(&mut host), ["A", "B"], "the part comes back in its old place");
    host.call("redo", &json!({})).unwrap();
    assert_eq!(names(&mut host), ["B"]);

    // Back to the empty project, then nothing more to undo.
    host.call("undo", &json!({})).unwrap();
    host.call("undo", &json!({})).unwrap();
    host.call("undo", &json!({})).unwrap();
    assert!(names(&mut host).is_empty());
    assert!(host.call("undo", &json!({})).is_err());

    // The model can't remove a part the user locked.
    host.call("redo", &json!({})).unwrap();
    let mut p = intercraft_core::LoadedProject::load(&dir).unwrap();
    p.set_kind("A", intercraft_core::PartKind::Reference).unwrap();
    p.save().unwrap();
    assert!(host.call("delete_part", &json!({ "part": "A" })).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn placed_parts_report_assembly_positions() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("place");
    host.call("add_part", &json!({ "name": "Block" })).unwrap();
    add(&mut host, "Block", "Body", "part = Box(10, 10, 10)").unwrap();
    let copy = host.call("duplicate_part", &json!({ "part": "Block", "new_name": "Twin" })).unwrap();
    assert_eq!(copy["duplicated_as"], "Twin");

    // Moving the copy changes where it is, not what it is.
    let moved = host.call("place_part", &json!({ "part": "Twin", "position": [30, 0, 0] })).unwrap();
    let body = &moved["features"][0];
    assert_eq!(body["bbox_min"][0], 25.0, "{moved}");
    assert_eq!(body["volume_mm3"], 1000.0);
    assert_eq!(moved["position_mm"], json!([30.0, 0.0, 0.0]));

    // Measuring works in assembly positions: the blocks are 20 mm apart.
    let gap = host
        .call("measure", &json!({ "a": { "part": "Block", "kind": "face", "index": 0 }, "b": { "point": [25, 0, 0] } }))
        .unwrap();
    assert!(gap["distance"].as_f64().is_some(), "{gap}");
    // Inspecting stays in the part's own frame, where its features work.
    let own = host.call("inspect_part", &json!({ "part": "Twin" })).unwrap();
    assert_eq!(own["bbox_min"][0], -5.0, "{own}");
    assert!(own["frame"].as_str().unwrap().contains("own frame"));

    // A quarter turn about Z swaps a 20 x 10 footprint.
    host.call("add_part", &json!({ "name": "Bar" })).unwrap();
    add(&mut host, "Bar", "Body", "part = Box(20, 10, 4)").unwrap();
    let turned = host.call("place_part", &json!({ "part": "Bar", "rotation": [0, 0, 90] })).unwrap();
    assert_eq!(turned["features"][0]["size_mm"], json!([10.0, 20.0, 4.0]), "{turned}");

    assert!(host.call("place_part", &json!({ "part": "Bar", "position": [1, 2] })).is_err());
    host.call("rename_part", &json!({ "part": "Bar", "new_name": "Rail" })).unwrap();
    assert!(dir.join("Rail").exists() && !dir.join("Bar").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_fit_finds_clashes_and_gaps() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("fit");
    for name in ["A", "B", "C"] {
        host.call("add_part", &json!({ "name": name })).unwrap();
        add(&mut host, name, "Body", "part = Box(10, 10, 10)").unwrap();
    }
    assert!(host.call("check_fit", &json!({ "parts": ["A"] })).is_err());
    assert!(host.call("check_fit", &json!({ "parts": ["A", "Nope"] })).is_err());

    // B overlaps A by 2 mm, C sits 20 mm clear of A.
    host.call("place_part", &json!({ "part": "B", "position": [8, 0, 0] })).unwrap();
    host.call("place_part", &json!({ "part": "C", "position": [-30, 0, 0] })).unwrap();
    let fit = host.call("check_fit", &json!({})).unwrap();
    let pair = |a: &str, b: &str| -> Value {
        fit["pairs"].as_array().unwrap().iter().find(|p| p["a"] == a && p["b"] == b).cloned().unwrap()
    };
    let clash = pair("A", "B");
    assert_eq!(clash["status"], "overlap", "{fit}");
    assert!((clash["overlap_volume"].as_f64().unwrap() - 200.0).abs() < 0.01);
    assert_eq!(clash["bbox_min"][0], 3.0);
    assert!(clash.get("mesh").is_none(), "mesh paths are for the viewer only");
    let gap = pair("A", "C");
    assert_eq!(gap["status"], "clear");
    assert_eq!(gap["clearance"], 20.0);

    // Slide B until it just touches A.
    host.call("place_part", &json!({ "part": "B", "position": [10, 0, 0] })).unwrap();
    let fit = host.call("check_fit", &json!({ "parts": ["A", "B"] })).unwrap();
    assert_eq!(fit["pairs"][0]["status"], "touching", "{fit}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_why_travels_through_the_tools() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("why");
    host.call("add_part", &json!({ "name": "P" })).unwrap();
    let why = "Thick enough to print strong walls.";
    host.call("add_feature", &json!({ "part": "P", "label": "Body", "code": "part = Box(10, 10, 4)", "why": why })).unwrap();
    let project = host.call("get_project", &json!({})).unwrap();
    assert_eq!(project["parts"][0]["features"][0]["why"], why);
    let long = "x".repeat(700);
    assert!(host.call("edit_feature", &json!({ "part": "P", "feature_id": "f010", "why": long })).is_err());
    host.call("edit_feature", &json!({ "part": "P", "feature_id": "f010", "why": "Changed." })).unwrap();
    let project = host.call("get_project", &json!({})).unwrap();
    assert_eq!(project["parts"][0]["features"][0]["why"], "Changed.");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_brief_is_read_and_updated_through_the_tools() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("brief");
    host.call("add_part", &json!({ "name": "P" })).unwrap();
    let project = host.call("get_project", &json!({})).unwrap();
    assert!(project["brief"].as_str().unwrap().contains("## Open items"), "a new project has the template");
    let updated = host.call("update_brief", &json!({ "section": "Decisions", "text": "- 3 mm walls, for stiffness." })).unwrap();
    assert!(updated["brief"].as_str().unwrap().contains("## Decisions\n\n- 3 mm walls, for stiffness."));
    assert!(host.call("update_brief", &json!({ "section": "Decisions" })).is_err());
    assert!(host.call("update_brief", &json!({ "section": "X", "text": "## Y" })).is_err());
    let undone = host.call("undo", &json!({})).unwrap();
    assert_eq!(undone["undone"], "Update the brief");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn standard_parts_and_sizes_come_from_the_tables() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("standard");
    host.call("add_part", &json!({ "name": "Plate" })).unwrap();

    // ISO 273 clearance holes for M4 are 4.3, 4.5 and 4.8 mm.
    let info = host.call("fastener_info", &json!({ "size": "M4" })).unwrap();
    assert_eq!(info["size"], "M4-0.7");
    assert_eq!(info["clearance_hole"], json!({ "close": 4.3, "normal": 4.5, "loose": 4.8 }), "{info}");
    assert_eq!(info["hex_nut"]["across_flats"], 7.0, "ISO 4032 M4 nut is 7 mm across flats");
    assert_eq!(info["heat_set_insert"]["hole_diameter"], 5.5, "McMaster 94459A170 hole");
    assert!(host.call("fastener_info", &json!({ "size": "M4\") ; import os #" })).is_err());
    assert!(host.call("fastener_info", &json!({ "size": "banana" })).is_err());

    // A real M4 x 16 screw, as its own part, standing in a plate's hole.
    let screw = host.call("add_standard_part", &json!({ "kind": "socket head screw", "size": "M4", "length": 16, "position": [0, 0, 5] })).unwrap();
    assert_eq!(screw["added_part"], "M4_socket_head_screw", "{screw}");
    assert_eq!(screw["features"][0]["status"], "built", "{screw}");
    let size = &screw["features"][0]["size_mm"];
    assert_eq!(size[0], 7.22, "head diameter: {screw}");
    assert!(size[2].as_f64().unwrap() > 16.0, "shank plus head: {screw}");
    assert!(host.call("add_standard_part", &json!({ "kind": "socket head screw", "size": "M4" })).is_err(), "a screw needs a length");
    assert!(host.call("add_standard_part", &json!({ "kind": "rivet", "size": "M4" })).is_err());

    // In a snippet, clearance_hole gives the same table value.
    add(&mut host, "Plate", "Body", "part = Box(30, 30, 5) - Cylinder(clearance_hole('M4') / 2, 10)").unwrap();
    let hole = host.call("inspect_part", &json!({ "part": "Plate" })).unwrap();
    assert!(hole["circle_edge_diameters"].as_object().unwrap().contains_key("4.5"), "{hole}");
    let nut = host.call("add_standard_part", &json!({ "kind": "hex nut", "size": "M4" })).unwrap();
    assert_eq!(nut["features"][0]["status"], "built", "{nut}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn print_check_finds_thin_walls_small_holes_and_overhangs() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("print");
    host.call("add_part", &json!({ "name": "Tray" })).unwrap();
    // A 20 x 20 x 10 block, hollowed to leave a 0.8 mm wall on one side, a 1.5 mm pin
    // hole, and a ledge sticking out 10 mm above the bed.
    let code = "part = Box(20, 20, 10) - Pos(0, 0.6, 1) * Box(18.4, 17.2, 10) - Pos(8, -8, 0) * Cylinder(0.75, 30) + Pos(15, 0, 4) * Box(10, 6, 2)";
    add(&mut host, "Tray", "Body", code).unwrap();
    let check = host.call("print_check", &json!({ "part": "Tray" })).unwrap();
    let kinds: Vec<&str> = check["problems"].as_array().unwrap().iter().map(|p| p["kind"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"thin wall"), "{check}");
    assert!(kinds.contains(&"small hole"), "{check}");
    assert!(kinds.contains(&"overhang"), "{check}");
    let thinnest = check["problems"].as_array().unwrap().iter().filter_map(|p| p["thickness"].as_f64()).fold(f64::MAX, f64::min);
    assert!((thinnest - 0.8).abs() < 0.01, "{check}");
    assert_eq!(check["ok"], false);

    // Printed upside down, the ledge rests on nothing above it: no overhang from it.
    host.call("add_part", &json!({ "name": "Cube" })).unwrap();
    add(&mut host, "Cube", "Body", "part = Box(20, 20, 20)").unwrap();
    let fine = host.call("print_check", &json!({ "part": "Cube" })).unwrap();
    assert_eq!(fine["ok"], true, "{fine}");
    assert!(host.call("print_check", &json!({ "part": "Cube", "up": [0, 0, 0] })).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
