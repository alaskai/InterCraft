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
fn a_big_project_reads_as_an_outline_with_detail_on_request() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    let (mut host, dir) = project("outline");
    // More parts than get_project gives in full: a frame of rails, posts and a motor.
    for i in 0..8 {
        let name = format!("Rail_{i}");
        host.call("add_part", &json!({ "name": name })).unwrap();
        add(&mut host, &name, "Bar", "part = Box(200, 20, 20)").unwrap();
        host.call("place_part", &json!({ "part": name, "position": [0, i * 30, 10] })).unwrap();
    }
    for i in 0..5 {
        let name = format!("Post_{i}");
        host.call("add_part", &json!({ "name": name })).unwrap();
        add(&mut host, &name, "Bar", "part = Box(20, 20, 300)").unwrap();
    }
    host.call("add_part", &json!({ "name": "Z_Motor" })).unwrap();
    add(&mut host, "Z_Motor", "Body", "part = Box(42, 42, 34)").unwrap();

    // The model tidies them into nested groups.
    let rails: Vec<String> = (0..8).map(|i| format!("Rail_{i}")).collect();
    let grouped = host.call("set_group", &json!({ "parts": rails, "group": "Frame/Rails" })).unwrap();
    assert_eq!(grouped["groups"], json!(["Frame", "Frame/Rails"]));
    host.call("set_group", &json!({ "parts": ["Z_Motor"], "group": "Frame/Drive" })).unwrap();
    assert!(host.call("set_group", &json!({ "parts": ["Nope"], "group": "X" })).is_err());
    assert!(host.call("set_group", &json!({ "parts": "Rail_0" })).is_err());

    let outline = host.call("get_project", &json!({})).unwrap();
    let text = outline.to_string();
    assert!(text.len() < 4000, "the outline should stay small, it is {} characters", text.len());
    assert!(!text.contains("Box(200"), "no code in an outline");
    assert_eq!(outline["parts"].as_array().unwrap().len(), 14);
    assert_eq!(outline["parts"][1], json!({ "part": "Rail_1", "group": "Frame/Rails", "size": [200.0, 20.0, 20.0], "centre": [0.0, 30.0, 10.0], "steps": 1 }));
    // The outer group counts what's in its sub-groups and gives the box they fill.
    assert_eq!(outline["groups"][0], json!({ "group": "Frame", "parts": 9, "bbox_min": [-100.0, -21.0, -17.0], "bbox_max": [100.0, 220.0, 20.0] }));

    let found = host.call("find_parts", &json!({ "name": "MOTOR" })).unwrap();
    assert_eq!((found["found"].as_u64(), found["parts"][0]["part"].as_str()), (Some(1), Some("Z_Motor")));
    assert_eq!(host.call("find_parts", &json!({ "group": "Frame" })).unwrap()["found"], 9);
    assert_eq!(host.call("find_parts", &json!({ "group": "Frame/Rails", "name": "_3" })).unwrap()["found"], 1);
    let none = host.call("find_parts", &json!({ "group": "Frames" })).unwrap();
    assert_eq!((none["found"].as_u64(), &none["groups"]), (Some(0), &json!(["Frame", "Frame/Rails", "Frame/Drive"])));
    assert!(host.call("find_parts", &json!({})).is_err());

    let full = host.call("get_part", &json!({ "part": "Z_Motor" })).unwrap();
    assert_eq!(full["features"][0]["code"].as_str().map(str::trim), Some("part = Box(42, 42, 34)"));
    assert_eq!(full["group"], "Frame/Drive");

    // Moving a group with a path, then dissolving one.
    let moved = host.call("rename_group", &json!({ "group": "Frame/Drive", "new_path": "Motion/Z" })).unwrap();
    assert_eq!(moved["groups"], json!(["Frame", "Frame/Rails", "Motion", "Motion/Z"]));
    let gone = host.call("rename_group", &json!({ "group": "Frame/Rails" })).unwrap();
    assert_eq!(gone["groups"], json!(["Frame", "Motion", "Motion/Z"]));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn nearby_finds_neighbours_and_what_touches_what() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    let (mut host, dir) = project("nearby");
    // A plate, a bracket sitting on it, a bolt 3 mm above the bracket, and a lamp far off.
    for (name, code, at) in [
        ("Plate", "part = Box(100, 60, 5)", [0, 0, 0]),
        ("Bracket", "part = Box(20, 20, 30) - Pos(0, 0, 5) * Cylinder(2.1, 40)", [30, 0, 17.5 as i32]),
        ("Bolt", "part = Cylinder(2, 10)", [30, 0, 40]),
        ("Lamp", "part = Box(10, 10, 10)", [400, 0, 0]),
    ] {
        host.call("add_part", &json!({ "name": name })).unwrap();
        add(&mut host, name, "Body", code).unwrap();
        host.call("place_part", &json!({ "part": name, "position": at })).unwrap();
    }
    host.call("place_part", &json!({ "part": "Bracket", "position": [30, 0, 17.5] })).unwrap();

    let near = host.call("nearby", &json!({ "part": "Bracket" })).unwrap();
    let found: Vec<(&str, f64)> =
        near["near"].as_array().unwrap().iter().map(|n| (n["part"].as_str().unwrap(), n["clearance_mm"].as_f64().unwrap())).collect();
    // Nearest first; the lamp is far beyond 5 mm. The bolt sits over the 4.2 mm bore, so
    // its nearest point is the bore's rim: 2.5 mm up and 0.1 mm across.
    assert_eq!(found, [("Plate", 0.0), ("Bolt", 2.502)], "{near}");
    let wide = host.call("nearby", &json!({ "part": "Bracket", "within": 200 })).unwrap();
    assert_eq!(wide["near"].as_array().unwrap().len(), 2, "the lamp is over 300 mm away");

    // With no part: what touches what.
    let map = host.call("nearby", &json!({})).unwrap();
    assert_eq!(map["touching"], json!([{ "a": "Plate", "b": "Bracket", "clearance_mm": 0.0 }]), "{map}");
    // A group stands for its parts; the bolt is in it, so only outside parts are listed.
    host.call("set_group", &json!({ "parts": ["Bracket", "Bolt"], "group": "Mount" })).unwrap();
    let group = host.call("nearby", &json!({ "part": "Mount" })).unwrap();
    assert_eq!(group["near"][0]["part"], "Plate");
    assert_eq!(group["near"].as_array().unwrap().len(), 1, "{group}");
    assert!(host.call("nearby", &json!({ "part": "Nothing" })).is_err());
    assert!(host.call("nearby", &json!({ "part": "Plate", "within": 900 })).is_err());

    // The bracket's bore where it sits in the assembly, ready to line a new part up with.
    let own = host.call("inspect_part", &json!({ "part": "Bracket" })).unwrap();
    let placed = host.call("inspect_part", &json!({ "part": "Bracket", "in_assembly": true })).unwrap();
    let centre = |info: &Value| info["cylindrical_faces"][0]["centre"].as_array().unwrap()[0].as_f64().unwrap();
    assert!((centre(&placed) - centre(&own) - 30.0).abs() < 1e-6, "{placed}");
    assert!(placed["frame"].as_str().unwrap().contains("assembly"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn asking_a_busy_worker_never_blocks_the_caller() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    use intercraft_geo::{FeatureInput, GeometryBackend, RebuildRequest};
    // A whole assembly moving sends a rebuild per part. The viewer sends them from
    // its UI thread, so sending must not wait for the worker to catch up, or the
    // window freezes. This is far more than a pipe holds.
    let mut worker = intercraft_geo::PythonWorker::spawn(|| {}).unwrap();
    let code = "part = Box(10, 10, 10)";
    let started = Instant::now();
    for i in 0..3000 {
        let feature = FeatureInput::snippet("f010", code, &serde_json::Map::new());
        let req = RebuildRequest { part: format!("P{i}"), features: vec![feature], placement: Some([i as f64, 0.0, 0.0, 0.0, 0.0, 0.0]) };
        worker.rebuild(req).unwrap();
    }
    assert!(started.elapsed() < Duration::from_secs(2), "sending took {:?}", started.elapsed());
}

#[test]
fn an_assembly_splits_into_parts_where_they_sat() {
    let _one = one_at_a_time();
    if !worker_available() {
        return;
    }
    // kit.step: a base plate, and a hinge sub-assembly (two leaves and a pin) moved
    // 100 mm along Y. Made by fixtures/make_kit.py.
    let kit = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/kit.step");
    let mut worker = intercraft_geo::PythonWorker::spawn(|| {}).unwrap();
    let contents = worker.step_contents_blocking(&kit).unwrap();
    let bodies: Vec<String> =
        contents["bodies"].as_array().unwrap().iter().map(|b| b["name"].as_str().unwrap().to_owned()).collect();
    assert_eq!(bodies, ["Base", "Leaf", "Leaf", "Pin"]);

    let (mut host, dir) = project("split");
    let mut loaded = intercraft_core::LoadedProject::create(&dir, "Split").unwrap();
    // The hinge is a sub-assembly in the file, so its bodies say they sit in it.
    let nested: Vec<(String, Vec<String>)> = contents["bodies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| (b["name"].as_str().unwrap().to_owned(), b["path"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect()))
        .collect();
    let split = loaded.plan_split_import_nested(&kit, &nested).unwrap();
    assert_eq!(split.groups, ["kit", "kit/Hinge", "kit/Hinge", "kit/Hinge"]);
    std::fs::create_dir_all(&split.folder).unwrap();
    let files: Vec<PathBuf> = split.parts.iter().map(|(_, source)| dir.join(source)).collect();
    assert_eq!(worker.split_step_blocking(&kit, &files).unwrap()["written"], 4);
    loaded.add_split_import(&split).unwrap();
    loaded.save().unwrap();
    drop(worker);

    let project = host.call("get_project", &json!({})).unwrap();
    let parts = project["parts"].as_array().unwrap();
    let names: Vec<&str> = parts.iter().map(|p| p["part"].as_str().unwrap()).collect();
    assert_eq!(names, ["Base", "Leaf", "Leaf_2", "Pin"]);
    assert!(parts.iter().all(|p| p["ok"] == true), "{project}");
    assert_eq!((&parts[0]["group"], &parts[3]["group"]), (&json!("kit"), &json!("kit/Hinge")));
    // Untouched bodies are listed briefly, without code or every step.
    assert!(parts[0].get("features").is_none());
    // Each body is where it was in the assembly: the second leaf at x 2.5..17.5,
    // y 85..115 once the hinge's 100 mm move is added.
    assert_eq!(parts[2]["bbox_min"], json!([2.5, 85.0, -1.0]), "{}", parts[2]);
    assert_eq!(parts[2]["bbox_max"], json!([17.5, 115.0, 1.0]));
    assert_eq!(parts[0]["size_mm"], json!([60.0, 40.0, 5.0]));

    // A body is an ordinary part: it can be edited, and then it's reported in full.
    let report = add(&mut host, "Pin", "Shorter", "part = part & Box(10, 200, 10)").unwrap();
    assert_eq!(report["ok"], true, "{report}");
    let project = host.call("get_project", &json!({})).unwrap();
    assert!(project["parts"][3].get("features").is_some());
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
fn a_hinged_lid_opens_and_the_tools_see_it_open() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("hinge");
    host.call("add_part", &json!({ "name": "Box" })).unwrap();
    add(&mut host, "Box", "Body", "part = Box(40, 20, 10)").unwrap();
    host.call("add_part", &json!({ "name": "Lid" })).unwrap();
    add(&mut host, "Lid", "Plate", "part = Box(40, 20, 2)").unwrap();
    host.call("place_part", &json!({ "part": "Lid", "position": [0, 0, 6] })).unwrap();

    // Along the top back edge, turning so positive angles lift the front of the lid.
    let hinge = json!({
        "name": "Lid hinge", "kind": "hinge", "base": "Box", "part": "Lid",
        "origin": [0, 10, 5], "axis": [-1, 0, 0], "min": 0, "max": 110,
        "why": "Opens the lid upwards about its back edge.",
    });
    let added = host.call("add_joint", &hinge).unwrap();
    assert_eq!(added["summary"], "Lid hinge: hinge, 0 to 110°, now 0°");
    assert_eq!(added["moved_parts"][0]["bbox_min"], json!([-20.0, -10.0, 5.0]));
    assert!(host.call("add_joint", &hinge).is_err(), "the name is taken");

    let open = host.call("edit_joint", &json!({ "name": "Lid hinge", "at": 90 })).unwrap();
    let lid = &open["moved_parts"][0];
    assert_eq!((lid["bbox_min"].clone(), lid["bbox_max"].clone()), (json!([-20.0, 10.0, 5.0]), json!([20.0, 12.0, 25.0])), "{open}");
    assert!(host.call("edit_joint", &json!({ "name": "Lid hinge", "at": 200 })).is_err(), "outside the range");

    // Everything else sees the open pose, and says so.
    let fit = host.call("check_fit", &json!({})).unwrap();
    assert_eq!(fit["joints"][0], "Lid hinge: hinge, 0 to 110°, now 90°");
    let project = host.call("get_project", &json!({})).unwrap();
    assert_eq!(project["joints"][0]["at"], 90.0);
    let lid_report = project["parts"].as_array().unwrap().iter().find(|p| p["part"] == "Lid").unwrap().clone();
    assert_eq!(lid_report["moved_by"][0], "Lid hinge: hinge, 0 to 110°, now 90°");
    assert_eq!(lid_report["features"][0]["bbox_max"], json!([20.0, 12.0, 25.0]));

    // Exports stay at rest, wherever the hinge was left.
    let exported = host.call("export_part", &json!({ "part": "Lid", "format": "step" })).unwrap();
    assert!(exported["exported"][0].as_str().is_some());

    assert_eq!(host.call("undo", &json!({})).unwrap()["undone"], "Move 'Lid hinge' to 90°");
    let removed = host.call("delete_joint", &json!({ "name": "lid hinge" })).unwrap();
    assert_eq!(removed["back_at_rest"], json!(["Lid"]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_motion_finds_where_a_lid_hits_the_wall() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("sweep");
    host.call("add_part", &json!({ "name": "Box" })).unwrap();
    add(&mut host, "Box", "Body", "part = Box(40, 20, 10)").unwrap();
    host.call("add_part", &json!({ "name": "Lid" })).unwrap();
    add(&mut host, "Lid", "Plate", "part = Box(40, 20, 2)").unwrap();
    host.call("place_part", &json!({ "part": "Lid", "position": [0, 0, 6] })).unwrap();
    let hinge = json!({
        "name": "Lid hinge", "kind": "hinge", "base": "Box", "part": "Lid",
        "origin": [0, 10, 5], "axis": [-1, 0, 0], "min": 0, "max": 110,
    });
    host.call("add_joint", &hinge).unwrap();

    // Nothing else around: it swings freely, resting on the box when shut.
    let free = host.call("check_motion", &json!({ "joint": "Lid hinge", "steps": 12 })).unwrap();
    assert_eq!(free["clear"], true, "{free}");
    assert!(free["summary"].as_str().unwrap().starts_with("Lid hinge moves freely from 0° to 110°."), "{free}");
    // The hinge line is the box's top back edge, so the lid touches it the whole way.
    // That is a contact, reported apart, not a "0 mm" closest gap.
    assert_eq!(free["touching"], json!(["Lid touches Box all the way"]), "{free}");
    assert!(free.get("closest").is_none(), "{free}");

    // A wall 1 mm behind the open lid: fine at 90°, hit once it opens further.
    host.call("add_part", &json!({ "name": "Wall" })).unwrap();
    add(&mut host, "Wall", "Slab", "part = Box(40, 2, 30)").unwrap();
    host.call("place_part", &json!({ "part": "Wall", "position": [0, 14, 20] })).unwrap();
    let hit = host.call("check_motion", &json!({ "joint": "Lid hinge", "steps": 12 })).unwrap();
    assert_eq!(hit["clear"], false, "{hit}");
    let range = &hit["clash_ranges"][0];
    assert_eq!((range["from"].as_f64(), range["to"].as_f64()), (Some(100.0), Some(110.0)), "{hit}");
    assert_eq!(range["parts"], json!(["Lid and Wall"]));
    assert!(hit["note"].as_str().unwrap().contains("every 10°"));
    // The check moves nothing for real.
    assert_eq!(host.call("get_project", &json!({})).unwrap()["joints"][0]["at"], 0.0);
    assert!(host.call("check_motion", &json!({ "joint": "Nope" })).is_err());
    assert!(host.call("check_motion", &json!({ "joint": "Lid hinge", "steps": 1000 })).is_err());
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
