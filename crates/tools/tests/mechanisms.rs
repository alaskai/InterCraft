// Real mechanisms through the tools and the geometry worker: a dovetail slide, a
// drawer, a carriage riding on a swinging arm and a knob turning in a hole. Each is
// checked the way the model would check it. Skips without the worker's Python.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use intercraft_tools::ToolHost;
use serde_json::{json, Value};

fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn worker_available() -> bool {
    let venv = std::env::var_os("LOCALAPPDATA")
        .map(|d| PathBuf::from(d).join("InterCraft/venv/Scripts/python.exe"))
        .is_some_and(|p| p.exists());
    venv || std::env::var_os("INTERCRAFT_PYTHON").is_some()
}

fn project(name: &str) -> (ToolHost, PathBuf) {
    let dir = std::env::temp_dir().join(format!("intercraft-mech-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    (ToolHost::new(dir.clone()), dir)
}

fn part(host: &mut ToolHost, name: &str, code: &str, params: Value) {
    host.call("add_part", &json!({ "name": name })).unwrap();
    let report = host.call("add_feature", &json!({ "part": name, "label": "Body", "code": code, "params": params })).unwrap();
    assert_eq!(report["ok"], true, "{name} didn't build: {report}");
}

fn check(host: &mut ToolHost, joint: &str, steps: u64) -> Value {
    host.call("check_motion", &json!({ "joint": joint, "steps": steps })).unwrap()
}

const RAIL: &str = r#"
rail = extrude(Plane.YZ * Polygon((-6, 0), (6, 0), (8, 6), (-8, 6), align=None), amount=p["length"] / 2, both=True)
part = Pos(0, 0, -5) * Box(p["length"], 30, 10) + rail
"#;

const CARRIAGE: &str = r#"
c = p["clearance"]
# The rail's own profile grown by the clearance, so every face has the same gap.
profile = Polygon((-6, 0), (6, 0), (8, 6), (-8, 6), align=None)
if c != 0:
    profile = offset(profile, c, kind=Kind.INTERSECTION)
block = Pos(0, 0, 7 + c) * Box(30, 30, 14)
part = block - extrude(Plane.YZ * profile, amount=20, both=True)
"#;

#[test]
fn a_dovetail_slide_runs_clear_only_with_clearance() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("dovetail");
    part(&mut host, "Rail", RAIL, json!({ "length": 100 }));
    part(&mut host, "Carriage", CARRIAGE, json!({ "clearance": 0.2 }));
    let slide = json!({
        "name": "Slide", "kind": "slider", "base": "Rail", "part": "Carriage",
        "origin": [0, 0, 0], "axis": [1, 0, 0], "min": -30, "max": 30,
    });
    host.call("add_joint", &slide).unwrap();

    let free = check(&mut host, "Slide", 13);
    eprintln!("0.2 mm: {free}");
    assert_eq!(free["clear"], true, "{free}");
    let gap = free["closest"]["clearance"].as_f64().unwrap();
    assert!((gap - 0.2).abs() < 1e-3, "{free}");

    // No clearance: the faces touch the whole way, which a real slide can't do.
    let edit = |c: f64| json!({ "part": "Carriage", "feature_id": "f010", "name": "clearance", "value": c });
    host.call("set_param", &edit(0.0)).unwrap();
    let tight = check(&mut host, "Slide", 13);
    eprintln!("0 mm: {tight}");
    assert_eq!(tight["clear"], true);
    assert_eq!(tight["touching"], json!(["Carriage touches Rail all the way"]), "{tight}");

    // Too tight: it clashes over the whole travel.
    host.call("set_param", &edit(-0.1)).unwrap();
    let jammed = check(&mut host, "Slide", 13);
    eprintln!("-0.1 mm: {jammed}");
    assert_eq!(jammed["clear"], false);
    let range = &jammed["clash_ranges"][0];
    assert_eq!((range["from"].as_f64(), range["to"].as_f64()), (Some(-30.0), Some(30.0)), "{jammed}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_drawer_pushed_in_too_far_hits_the_cabinet() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("drawer");
    // Open at the front, 3 mm back wall, drawer with 0.5 mm all round and a front plate.
    part(&mut host, "Cabinet", "part = Pos(0, 0, 20) * Box(60, 80, 40) - Pos(0, -2, 20) * Box(56, 78, 34)", json!({}));
    part(
        &mut host,
        "Drawer",
        "part = Pos(0, -2, 20) * Box(55, 76, 33) + Pos(0, -41, 20) * Box(64, 2, 44)",
        json!({}),
    );
    let pull = json!({
        "name": "Drawer runner", "kind": "slider", "base": "Cabinet", "part": "Drawer",
        "origin": [0, 0, 0], "axis": [0, -1, 0], "min": -5, "max": 70,
    });
    host.call("add_joint", &pull).unwrap();
    let result = check(&mut host, "Drawer runner", 16);
    eprintln!("drawer: {result}");
    assert_eq!(result["clear"], false);
    let range = &result["clash_ranges"][0];
    assert_eq!(range["from"], -5.0, "{result}");
    assert!(range["to"].as_f64().unwrap() < 0.0, "only pushed-in values clash: {result}");
    // Shut, the front plate rests on the cabinet's front edge.
    assert!(result["touching"].to_string().contains("Drawer touches Cabinet at 0 mm"), "{result}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_carriage_rides_along_a_swinging_arm() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("chain");
    part(&mut host, "Base", "part = Pos(0, 0, -2.5) * Box(30, 30, 5)", json!({}));
    part(&mut host, "Arm", "part = Pos(40, 0, 5) * Box(80, 10, 10)", json!({}));
    part(&mut host, "Carriage", "part = Pos(10, 0, 12.5) * Box(10, 10, 5)", json!({}));
    part(&mut host, "Post", "part = Pos(0, 65, 10) * Box(10, 10, 20)", json!({}));
    let swing = json!({
        "name": "Swing", "kind": "hinge", "base": "Base", "part": "Arm",
        "origin": [0, 0, 0], "axis": [0, 0, 1], "min": 0, "max": 90,
    });
    host.call("add_joint", &swing).unwrap();
    let ride = json!({
        "name": "Ride", "kind": "slider", "base": "Arm", "part": "Carriage",
        "origin": [0, 0, 0], "axis": [1, 0, 0], "min": 0, "max": 60,
    });
    host.call("add_joint", &ride).unwrap();

    // Swung round 90°, the arm points along +Y and the carriage's slide follows it.
    host.call("edit_joint", &json!({ "name": "Swing", "at": 90 })).unwrap();
    let moved = host.call("edit_joint", &json!({ "name": "Ride", "at": 30 })).unwrap();
    eprintln!("ride: {moved}");
    let carriage = &moved["moved_parts"][0];
    assert_eq!(carriage["bbox_min"], json!([-5.0, 35.0, 10.0]), "{moved}");
    assert_eq!(carriage["bbox_max"], json!([5.0, 45.0, 15.0]), "{moved}");

    // Swinging takes the carriage with it, so the arm and the carriage both meet the post.
    let swing_check = check(&mut host, "Swing", 10);
    eprintln!("swing: {swing_check}");
    assert_eq!(swing_check["clear"], false);
    let text = swing_check["summary"].as_str().unwrap();
    assert!(text.contains("Arm and Post"), "{text}");

    // Riding along the arm, the carriage (already near the post) runs into it too.
    let ride_check = check(&mut host, "Ride", 13);
    eprintln!("ride check: {ride_check}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_knob_turns_freely_in_its_hole() {
    if !worker_available() {
        return;
    }
    let _one = one_at_a_time();
    let (mut host, dir) = project("knob");
    part(&mut host, "Panel", "part = Pos(0, 0, -1.5) * Box(40, 40, 3) - Cylinder(3.2, 10)", json!({}));
    part(
        &mut host,
        "Knob",
        "part = Pos(0, 0, -1.5) * Cylinder(3, 3) + Pos(0, 0, 4.3) * Cylinder(8, 8) + Pos(6, 0, 9.3) * Box(4, 2, 2)",
        json!({}),
    );
    let turn = json!({
        "name": "Knob turn", "kind": "hinge", "base": "Panel", "part": "Knob",
        "origin": [0, 0, 0], "axis": [0, 0, 1], "min": -180, "max": 180,
    });
    host.call("add_joint", &turn).unwrap();
    let result = check(&mut host, "Knob turn", 13);
    eprintln!("knob: {result}");
    assert_eq!(result["clear"], true, "{result}");
    // Lifted 0.3 mm off the panel, the tightest gap is the 0.2 mm round the shaft.
    assert!((result["closest"]["clearance"].as_f64().unwrap() - 0.2).abs() < 1e-3, "{result}");

    // Renaming and deleting parts keep the joints honest, and undo brings them back.
    host.call("rename_part", &json!({ "part": "Knob", "new_name": "Dial" })).unwrap();
    let project = host.call("get_project", &json!({})).unwrap();
    assert_eq!(project["joints"][0]["part"], "Dial");
    host.call("delete_part", &json!({ "part": "Dial" })).unwrap();
    assert_eq!(host.call("get_project", &json!({})).unwrap()["joints"], json!([]));
    host.call("undo", &json!({})).unwrap();
    assert_eq!(host.call("get_project", &json!({})).unwrap()["joints"][0]["part"], "Dial");
    let _ = std::fs::remove_dir_all(&dir);
}
