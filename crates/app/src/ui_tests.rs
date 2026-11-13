// Headless UI tests. egui_kittest runs the real app in an off-screen window, clicks
// its widgets and renders frames to PNG. Nothing touches the real screen, mouse,
// keyboard or the user's settings (INTERCRAFT_CONFIG_DIR points at a temp folder).
//
// Screenshots go to %TEMP%/intercraft-ui/ for a person to look at.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use egui_kittest::Harness;

use super::*;

fn sandbox_dirs() -> PathBuf {
    let root = std::env::temp_dir().join(format!("intercraft-ui-{}", std::process::id()));
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("projects")).unwrap();
        std::env::set_var("INTERCRAFT_CONFIG_DIR", root.join("config"));
        std::env::set_var("INTERCRAFT_PROJECTS_DIR", root.join("projects"));
        std::env::remove_var("ANTHROPIC_API_KEY");
        // Tests start past the first-run question, in the advanced layout.
        std::fs::write(root.join("config/settings.json"), r#"{"onboarded": true}"#).unwrap();
    });
    root
}

/// One app at a time: each one starts a geometry worker.
fn one_at_a_time() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A fresh copy of the bracket example, so tests can change it.
fn example_project(name: &str) -> PathBuf {
    let dir = sandbox_dirs().join("projects").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/bracket");
    copy_dir(&src, &dir);
    dir
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        // Undo history and locks left by running the app on the example locally
        // would make tests behave differently here than on CI.
        if entry.file_name().to_string_lossy().starts_with(".intercraft") {
            continue;
        }
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn harness(project: &Path) -> Harness<'static, App> {
    sandbox_dirs();
    let project = project.to_path_buf();
    Harness::builder()
        .with_size(egui::vec2(1400.0, 860.0))
        .wgpu()
        .build_eframe(move |cc| App::new(cc, project).expect("app starts"))
}

/// Step frames until `done` or the timeout. The worker runs in another process.
fn wait_until(h: &mut Harness<'static, App>, what: &str, done: impl Fn(&App) -> bool) {
    let started = Instant::now();
    while !done(h.state()) {
        assert!(started.elapsed() < Duration::from_secs(90), "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    // A few more frames so the result is on screen.
    for _ in 0..4 {
        h.step();
    }
}

fn built(app: &App) -> bool {
    let s = &app.state;
    s.building.is_empty() && !s.shown.is_empty() && s.project.is_some()
}

fn shot(h: &mut Harness<'static, App>, name: &str) {
    let dir = std::env::temp_dir().join("intercraft-ui");
    std::fs::create_dir_all(&dir).unwrap();
    let image = h.render().expect("render");
    image.save(dir.join(format!("{name}.png"))).unwrap();
}

fn click(h: &mut Harness<'static, App>, label: &str) {
    h.get_by_label(label).click();
    h.step();
    h.step();
}

#[test]
fn main_window_shows_the_project() {
    let _one = one_at_a_time();
    let project = example_project("main");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    // Shown in the parts list and as the timeline heading.
    assert!(h.get_all_by_label("RodBracket").count() >= 2);
    // A timeline card (dock tab titles aren't exposed as labels).
    assert!(h.get_all_by_label_contains("Edge fillets").count() >= 1);
    shot(&mut h, "main");
}

#[test]
fn menus_open_and_list_their_actions() {
    let _one = one_at_a_time();
    let project = example_project("menus");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    click(&mut h, "File");
    for item in ["New project...", "Open project...", "Open recent", "Show project folder", "Quit"] {
        h.get_by_label_contains(item);
    }
    shot(&mut h, "menu_file");
    h.key_press(egui::Key::Escape);
    h.step();

    click(&mut h, "Export");
    for item in ["STEP (exact, editable)", "STL (for printing)", "All as STEP", "All as STL", "Open exports folder"] {
        h.get_by_label_contains(item);
    }
    shot(&mut h, "menu_export");
    h.key_press(egui::Key::Escape);
    h.step();

    click(&mut h, "View");
    // Labels include shortcut text, and "Timeline" also names a tab.
    for item in ["Fit to view", "Parts", "Inspector", "Timeline", "Chat", "Reset layout", "Dark", "Light"] {
        assert!(h.get_all_by_label_contains(item).count() >= 1, "View menu is missing {item}");
    }
    shot(&mut h, "menu_view");
}

#[test]
fn export_all_writes_files() {
    let _one = one_at_a_time();
    let project = example_project("export");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    click(&mut h, "Export");
    click(&mut h, "All as STEP");
    wait_until(&mut h, "the export", |app| app.state.exports_pending == 0 && app.state.export_dir.is_some());
    let step = project.join("exports").join("RodBracket.step");
    assert!(step.exists(), "{} wasn't written", step.display());
    assert!(h.state().state.worker_status.contains("Exported RodBracket.step"), "{}", h.state().state.worker_status);
    h.get_by_label("Show files");
    shot(&mut h, "exported");
}

#[test]
fn settings_window_opens_from_menu_and_chat() {
    let _one = one_at_a_time();
    let project = example_project("settings");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    h.get_all_by_label("Settings").next().unwrap().click(); // the menu bar entry
    h.step();
    h.step();
    assert!(h.state().settings_open);
    h.get_by_label("Appearance");
    h.get_by_label("About");
    shot(&mut h, "settings");
}

#[test]
fn new_project_creates_and_switches() {
    let _one = one_at_a_time();
    let project = example_project("before-new");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::N);
    h.step();
    h.step();
    assert!(h.state().new_project.is_some(), "Ctrl+N didn't open the dialog");
    shot(&mut h, "new_project_dialog");
    h.get_by_label("Create").click(); // empty name
    h.step();
    h.step();
    h.get_by_label("Give the project a name.");

    let name = format!("Test plate {}", std::process::id());
    h.state_mut().new_project.as_mut().unwrap().set_name(&name);
    h.step();
    h.get_by_label("Create").click();
    for _ in 0..6 {
        h.step();
    }
    let app = h.state();
    assert!(app.new_project.is_none(), "the dialog stayed open");
    assert_eq!(app.state.project.as_ref().unwrap().project.name, name);
    assert!(sandbox_dirs().join("projects").join(&name).join("project.json").exists());
    shot(&mut h, "new_project_open");
}

#[test]
fn view_menu_hides_and_restores_panels() {
    let _one = one_at_a_time();
    let project = example_project("view");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    click(&mut h, "View");
    click(&mut h, "Inspector");
    assert!(h.state().dock.find_tab(&Tab::Inspector).is_none(), "Inspector still shown");
    click(&mut h, "Reset layout");
    assert!(h.state().dock.find_tab(&Tab::Inspector).is_some(), "Reset layout didn't bring it back");
}

#[test]
fn light_theme_renders() {
    let _one = one_at_a_time();
    let project = example_project("light");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    click(&mut h, "View");
    click(&mut h, "Light");
    for _ in 0..4 {
        h.step();
    }
    shot(&mut h, "light");
}

/// Move the virtual pointer to `pos` and click there (inside the test window only).
fn click_at(h: &mut Harness<'static, App>, pos: egui::Pos2) {
    h.event(egui::Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    h.step();
}

#[test]
fn edges_and_toolbar_render() {
    let _one = one_at_a_time();
    let project = example_project("edges");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    h.get_by_label("Fit");
    h.get_by_label("Measure");
    h.get_by_label("Edges");
    assert!(h.state().state.viewport.edge_points("RodBracket", 0).is_some(), "no edges loaded");
    shot(&mut h, "edges");
}

#[test]
fn measure_two_corners() {
    let _one = one_at_a_time();
    let project = example_project("measure");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    h.get_by_label("Measure").click();
    h.step();
    h.step();
    assert!(h.state().state.measure.active);

    // Two corners of the 40 x 30 x 20 block on the +X face, 30 mm apart in Y.
    let mut picked = 0;
    for corner in [[20.0, -15.0, 10.0], [20.0, 15.0, 10.0]] {
        let pos = h.state().state.viewport.project(corner).expect("corner on screen");
        click_at(&mut h, pos);
        let hit = &h.state().state.measure.picks.last().expect("a pick").hit;
        assert!(matches!(hit.target, viewport::Target::Corner(_)), "didn't snap to a corner: {hit:?}");
        let [x, y, z] = hit.pos;
        assert!((x - corner[0]).abs() < 0.01 && (y - corner[1]).abs() < 0.01 && (z - corner[2]).abs() < 0.01);
        picked += 1;
    }
    assert_eq!(picked, 2);
    wait_until(&mut h, "the measurement", |app| app.state.measure.distance_text().is_some());
    assert_eq!(h.state().state.measure.distance_text().unwrap(), "30 mm");
    shot(&mut h, "measure");
}

#[test]
fn import_step_adds_an_editable_part() {
    let _one = one_at_a_time();
    let project = example_project("import");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    click(&mut h, "Export");
    click(&mut h, "All as STEP");
    wait_until(&mut h, "the export", |app| app.state.exports_pending == 0 && app.state.export_dir.is_some());

    let step = project.join("exports").join("RodBracket.step");
    h.state_mut().state.import_step(&step);
    wait_until(&mut h, "the imported part to build", |app| built(app) && app.state.shown.contains_key("RodBracket_2"));
    let app = h.state();
    let part = app.state.project.as_ref().unwrap().part("RodBracket_2").unwrap();
    assert_eq!(part.source.as_deref(), Some("imports/RodBracket_2.step"));
    assert_eq!(app.state.selected_part.as_deref(), Some("RodBracket_2"));
    assert!(h.get_all_by_label_contains("Imported RodBracket_2.step").count() >= 1, "no import step on the timeline");
    shot(&mut h, "imported");
}

fn part_names(h: &Harness<'static, App>) -> Vec<String> {
    let project = h.state().state.project.as_ref().expect("a project is loaded");
    project.project.parts.iter().map(|p| p.name.clone()).collect()
}

#[test]
fn hide_delete_and_undo_a_part() {
    let _one = one_at_a_time();
    let project = example_project("undo");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    // The eye hides the part, and the toolbar offers to bring it back.
    click(&mut h, "Hide RodBracket");
    assert!(h.state().state.hidden.contains("RodBracket"));
    h.get_by_label_contains("Show all (1 hidden)");
    shot(&mut h, "part_hidden");
    click(&mut h, "Show RodBracket");
    assert!(h.state().state.hidden.is_empty());

    // Delete it, then bring it back from the Edit menu.
    h.state_mut().state.delete_part("RodBracket");
    h.step();
    assert!(part_names(&h).is_empty());
    click(&mut h, "Edit");
    shot(&mut h, "menu_edit");
    h.get_by_label_contains("Undo: Delete part 'RodBracket'").click();
    wait_until(&mut h, "the part to come back", built);
    assert_eq!(part_names(&h), ["RodBracket"]);

    // Ctrl+Y redoes the delete and Ctrl+Z undoes it again.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Y);
    h.step();
    h.step();
    assert!(part_names(&h).is_empty(), "{}", h.state().state.worker_status);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    wait_until(&mut h, "the part to come back again", built);
    assert_eq!(part_names(&h), ["RodBracket"]);

    click(&mut h, "Edit");
    shot(&mut h, "menu_edit_again");
    h.get_by_label_contains("History").hover();
    h.step();
    h.step();
    shot(&mut h, "menu_history");
    // In the history list, as "Redo: ..." in the Edit menu, and in the status bar.
    assert_eq!(h.get_all_by_label_contains("Delete part 'RodBracket'").count(), 3);
}

fn latest_volume(app: &App, part: &str, feature: &str) -> Option<f64> {
    let entry = app.state.results.get(&(part.to_owned(), feature.to_owned()))?;
    if entry.stale {
        return None;
    }
    Some(entry.result.report.as_ref()?.volume)
}

fn saved_param(project: &Path, feature: usize, name: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(project.join("project.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    json["parts"][0]["features"][feature]["params"][name].clone()
}

#[test]
fn drag_a_parameter_previews_then_saves_once() {
    let _one = one_at_a_time();
    let project = example_project("params");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    let before = latest_volume(h.state(), "RodBracket", "f040").unwrap();

    // The Inspector follows the latest step, the fillet, whose only parameter is radius = 3.
    let field = h.get_by_role(egui::accesskit::Role::SpinButton).rect();
    h.drag_at(field.center());
    h.step();
    h.hover_at(field.center() + egui::vec2(25.0, 0.0));
    h.step();
    let dragged = h.state().state.param_drag.clone().expect("dragging radius");
    assert_eq!(dragged.field.label(), "radius");
    assert!(dragged.value > 3.0 && dragged.value.fract() == 0.0, "whole steps: {}", dragged.value);

    // The part rebuilds with the dragged value, but nothing is saved yet.
    wait_until(&mut h, "the preview build", |app| {
        let s = &app.state;
        s.building.is_empty() && s.param_drag.as_ref().is_some_and(|d| d.sent == Some(d.value))
    });
    let preview = latest_volume(h.state(), "RodBracket", "f040").unwrap();
    assert!(preview < before, "a bigger fillet removes material ({preview} vs {before})");
    assert_eq!(saved_param(&project, 3, "radius"), 3);
    shot(&mut h, "param_drag");

    // Letting go saves it once, as one undo step.
    h.drop_at(field.center() + egui::vec2(25.0, 0.0));
    h.step();
    h.step();
    assert!(h.state().state.param_drag.is_none());
    assert_eq!(saved_param(&project, 3, "radius"), dragged.value as i64);
    let undo = h.state().state.history.undo_label().unwrap_or_default().to_owned();
    assert_eq!(undo, format!("Set radius = {} on 'Edge fillets'", dragged.value as i64));

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    h.step();
    h.step();
    assert_eq!(saved_param(&project, 3, "radius"), 3);
}

#[test]
fn section_view_cuts_the_model_open() {
    let _one = one_at_a_time();
    let project = example_project("section");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    click(&mut h, "Section");
    let section = h.state().state.viewport.section.expect("section is on");
    assert_eq!(section.axis, 1);
    // The bracket is 30 mm deep, centred on the origin.
    assert!(section.offset.abs() < 0.01, "starts through the middle: {}", section.offset);
    shot(&mut h, "section_y");

    click(&mut h, "X");
    let flipped = h.state().state.viewport.section.unwrap().flip;
    click(&mut h, "Flip");
    let section = h.state().state.viewport.section.unwrap();
    assert!(section.axis == 0 && section.flip != flipped);
    shot(&mut h, "section_x_flipped");

    // Nothing can be picked in the half that was cut away.
    let centre = h.get_by_label("Section").rect().center() + egui::vec2(300.0, 300.0);
    let state = &mut h.state_mut().state;
    state.viewport.section = Some(viewport::Section { axis: 2, offset: -1000.0, flip: false });
    assert!(state.viewport.pick(centre).is_none(), "everything is above the plane, so cut away");

    click(&mut h, "Section");
    assert!(h.state().state.viewport.section.is_none());
}

#[test]
fn duplicate_rename_and_move_a_part() {
    let _one = one_at_a_time();
    let project = example_project("assembly");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    h.state_mut().state.duplicate_part("RodBracket");
    wait_until(&mut h, "the copy to build", |app| built(app) && app.state.shown.len() == 2);
    assert_eq!(part_names(&h), ["RodBracket", "RodBracket_copy"]);
    assert_eq!(h.state().state.selected_part.as_deref(), Some("RodBracket_copy"));

    // Rename through the window, with a bad name refused first.
    h.state_mut().state.rename = Some((RenameTarget::Part("RodBracket_copy".into()), "bad name!".into(), None));
    h.step();
    click(&mut h, "Rename");
    assert_eq!(part_names(&h)[1], "RodBracket_copy", "a bad name changes nothing");
    h.state_mut().state.rename = Some((RenameTarget::Part("RodBracket_copy".into()), "Twin".into(), None));
    h.step();
    click(&mut h, "Rename");
    assert_eq!(part_names(&h), ["RodBracket", "Twin"]);
    assert!(h.state().state.rename.is_none());
    wait_until(&mut h, "the renamed part to build", |app| built(app) && app.state.shown.contains_key("Twin"));

    // Move it 60 mm along X with the Placement controls.
    click(&mut h, "Placement");
    let field = h.get_by_label("position X").rect();
    h.drag_at(field.center());
    h.step();
    h.hover_at(field.center() + egui::vec2(120.0, 0.0));
    h.step();
    let moving = h.state().state.param_drag.clone().expect("dragging the position");
    assert!(moving.value > 1.0);
    h.drop_at(field.center() + egui::vec2(120.0, 0.0));
    h.step();
    h.step();
    wait_until(&mut h, "the moved part", built);
    let twin = h.state().state.project.as_ref().unwrap().part("Twin").unwrap().clone();
    assert_eq!(twin.position[0], moving.value);
    let bbox = h.state().state.results[&("Twin".to_owned(), "f040".to_owned())].result.report.clone().unwrap();
    assert!((bbox.bbox_min[0] - (moving.value - 20.0)).abs() < 1e-6, "the mesh moved with it: {bbox:?}");
    assert_eq!(h.state().state.history.undo_label(), Some("Move 'Twin'"));
    h.state_mut().state.viewport.fit();
    h.step();
    shot(&mut h, "assembly_moved");

    // The copy now overlaps the original, and the fit check says so.
    click(&mut h, "Fit check");
    wait_until(&mut h, "the fit check", |app| app.state.fit.as_ref().is_some_and(|f| f.clashes().is_some()));
    assert_eq!(h.state().state.fit.as_ref().unwrap().clashes(), Some(1));
    h.get_by_label_contains("RodBracket / Twin: overlap");
    shot(&mut h, "fit_clash");

    // Moving it clear runs the check again by itself.
    h.state_mut().state.commit_param(params::ParamCommit {
        part: "Twin".into(),
        field: params::Field::Position(0),
        value: 60.into(),
    });
    wait_until(&mut h, "the second fit check", |app| built(app) && app.state.fit.as_ref().is_some_and(|f| f.clashes() == Some(0)));
    h.step();
    h.get_by_label_contains("RodBracket / Twin: 20 mm clear");
    h.state_mut().state.viewport.fit();
    h.step();
    shot(&mut h, "fit_clear");
}

/// Ask for a picture the way the model does, while the app keeps running.
fn view_through_the_tool(h: &mut Harness<'static, App>, project: &Path, args: serde_json::Value) -> Vec<u8> {
    let dir = project.to_path_buf();
    let call = std::thread::spawn(move || intercraft_tools::ToolHost::new(dir).call("view_model", &args));
    let started = Instant::now();
    while !call.is_finished() {
        assert!(started.elapsed() < Duration::from_secs(30), "the viewer never answered");
        h.step();
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = call.join().unwrap().expect("view_model works");
    let data = result[intercraft_tools::IMAGE_KEY].as_str().expect("a picture");
    // Decode the base64 by hand: the test has no decoder crate either.
    let value = |c: u8| b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".iter().position(|t| *t == c);
    let mut bytes = Vec::new();
    for chunk in data.as_bytes().chunks(4) {
        let n = chunk.iter().fold(0u32, |n, c| n << 6 | value(*c).unwrap_or(0) as u32);
        let keep = 3 - chunk.iter().filter(|c| **c == b'=').count();
        bytes.extend_from_slice(&n.to_be_bytes()[1..1 + keep]);
    }
    bytes
}

#[test]
fn the_model_can_look_at_the_part() {
    let _one = one_at_a_time();
    let project = example_project("view");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    assert_eq!(intercraft_tools::IMAGE_KEY, intercraft_llm::IMAGE_KEY);
    let camera_before = h.state().state.viewport.section;

    let out = std::env::temp_dir().join("intercraft-ui");
    std::fs::create_dir_all(&out).unwrap();
    let mut pictures = Vec::new();
    for (name, args) in [
        ("view_iso", serde_json::json!({})),
        ("view_front", serde_json::json!({ "view": "front", "part": "RodBracket" })),
        ("view_cut", serde_json::json!({ "view": "iso", "section": { "axis": "x", "offset": 0, "flip": false } })),
    ] {
        let png = view_through_the_tool(&mut h, &project, args);
        assert_eq!(&png[1..4], b"PNG");
        std::fs::write(out.join(format!("{name}.png")), &png).unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(&png)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (viewport::SHOT_SIZE, viewport::SHOT_SIZE));
        // The part is in the picture: not every pixel is background.
        let corner = pixels[..4].to_vec();
        let drawn = pixels.as_chunks::<4>().0.iter().filter(|p| p.as_slice() != corner.as_slice()).count();
        assert!(drawn > 20_000, "{name}: only {drawn} pixels differ from the background");
        pictures.push(png);
    }
    assert_ne!(pictures[0], pictures[1]);
    assert_ne!(pictures[0], pictures[2], "the section changes the picture");
    assert_eq!(h.state().state.viewport.section, camera_before, "the user's own view is untouched");
    assert!(intercraft_tools::view_requests(&project).is_empty());
}

fn step_labels(h: &Harness<'static, App>) -> Vec<String> {
    let project = h.state().state.project.as_ref().expect("a project is loaded");
    project.project.parts[0].features.iter().map(|f| f.label.clone()).collect()
}

#[test]
fn steps_can_be_moved_renamed_and_recoded() {
    let _one = one_at_a_time();
    let project = example_project("steps");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    // Reorder: the slit before the bore, then back with undo.
    h.state_mut().state.move_step("RodBracket", "f030", -1);
    assert_eq!(step_labels(&h), ["Base block", "Clamp slit", "Rod bore", "Edge fillets"]);
    assert_eq!(h.state().state.history.undo_label(), Some("Reorder RodBracket"));
    h.state_mut().state.move_step("RodBracket", "f010", -1);
    assert!(h.state().state.worker_status.contains("already at that end"));
    h.state_mut().state.undo();
    assert_eq!(step_labels(&h)[1], "Rod bore");

    // Rename a step through the window.
    let target = RenameTarget::Step { part: "RodBracket".into(), feature: "f020".into(), label: "Rod bore".into() };
    h.state_mut().state.rename = Some((target, "Shaft bore".into(), None));
    h.step();
    h.step();
    click(&mut h, "Rename");
    assert_eq!(step_labels(&h)[1], "Shaft bore");

    // Edit the code of the fillet step in the Inspector: double the radius.
    wait_until(&mut h, "the rebuild", built);
    let before = latest_volume(h.state(), "RodBracket", "f040").unwrap();
    click(&mut h, "Code");
    h.get_by_label("Step code");
    let draft = h.state_mut().state.code_draft.as_mut().expect("the code box is open");
    assert!(draft.text.contains("fillet"));
    draft.text = draft.text.replace(r#"p["radius"]"#, r#"p["radius"] * 2"#);
    h.step();
    h.get_by_label("Not applied yet");
    shot(&mut h, "step_code");
    click(&mut h, "Apply");
    wait_until(&mut h, "the edited step to build", |app| {
        built(app) && latest_volume(app, "RodBracket", "f040").is_some_and(|v| (v - before).abs() > 1.0)
    });
    assert!(latest_volume(h.state(), "RodBracket", "f040").unwrap() < before, "a bigger fillet removes more");
    let saved = std::fs::read_to_string(project.join("RodBracket/040_fillet.py")).unwrap();
    assert!(saved.contains(r#"p["radius"] * 2"#));
    assert_eq!(h.state().state.history.undo_label(), Some("Edit 'Edge fillets'"));

    // Code the sandbox refuses is saved like any edit and shows as a failed step.
    let draft = h.state_mut().state.code_draft.as_mut().unwrap();
    draft.text = "import os\npart = part\n".to_owned();
    h.step();
    click(&mut h, "Apply");
    wait_until(&mut h, "the refused step", |app| {
        app.state.building.is_empty()
            && app.state.results.get(&("RodBracket".to_owned(), "f040".to_owned())).is_some_and(|e| !e.stale && !e.result.ok)
    });
    h.state_mut().state.undo();
    wait_until(&mut h, "the undo to rebuild", built);
}

#[test]
fn openai_compatible_server_settings() {
    let _one = one_at_a_time();
    let project = example_project("server");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    // No model yet: the chat says what is missing instead of offering to send.
    h.state_mut().state.chat.use_server("http://localhost:11434/v1", "");
    h.step();
    h.get_by_label_contains("Ollama (this computer) · no model chosen");
    h.get_by_label_contains("set the server address and a model");

    h.state_mut().state.chat.use_server("http://localhost:11434/v1", "qwen3:14b");
    h.step();
    h.get_by_label_contains("qwen3:14b · Ollama (this computer)");
    h.state_mut().settings_open = true;
    h.step();
    h.step();
    for label in ["Server", "Images", "This model can read images", "None saved for this server"] {
        assert!(h.get_all_by_label_contains(label).count() >= 1, "settings are missing {label}");
    }
    shot(&mut h, "settings_server");

    // An address that would send a key in the clear is called out.
    h.state_mut().state.chat.use_server("http://example.com/v1", "m");
    h.step();
    h.step();
    h.get_by_label_contains("must start with https://");
}

fn drag(h: &mut Harness<'static, App>, from: egui::Pos2, to: egui::Pos2) {
    h.hover_at(from);
    h.step();
    h.drag_at(from);
    h.step();
    // Two moves, so the pen has something between the ends.
    h.hover_at(from + (to - from) * 0.5);
    h.step();
    h.hover_at(to);
    h.step();
    h.drop_at(to);
    h.step();
    h.step();
}

#[test]
fn a_sketch_over_the_model_goes_to_the_chat() {
    let _one = one_at_a_time();
    let project = example_project("sketch");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    let tab = h.state().dock.find_tab(&Tab::Sketch).expect("the sketch tab is in the layout");
    h.state_mut().dock.set_active_tab(tab).expect("the tab can be shown");
    h.step();
    h.step();

    // Draw over the top view. The bracket is 40 x 30, centred on the origin.
    let ctx = h.ctx.clone();
    h.state_mut().state.set_sketch_backdrop(&ctx, Some("top".into()));
    h.step();
    h.step();
    let canvas = h.get_by_label("Sketch canvas").rect();
    let at = |h: &Harness<'static, App>, p: [f64; 2]| {
        // Drawing must not move the canvas, or the sketch would jump under the pen.
        assert_eq!(h.get_by_label("Sketch canvas").rect(), canvas, "the canvas moved");
        h.state().state.sketch.to_screen(canvas, p)
    };

    // A red circle where a hole should go, a blue freehand line, and an arrow.
    click(&mut h, "Circle");
    let (a, b) = (at(&h, [-10.0, 5.0]), at(&h, [-7.0, 5.0]));
    drag(&mut h, a, b);
    click(&mut h, "blue pen");
    click(&mut h, "Pen");
    let (a, b) = (at(&h, [5.0, -10.0]), at(&h, [15.0, -4.0]));
    drag(&mut h, a, b);
    click(&mut h, "red pen");
    click(&mut h, "Arrow");
    let (a, b) = (at(&h, [-25.0, 22.0]), at(&h, [-11.0, 7.0]));
    drag(&mut h, a, b);
    // A dimension across the part: drawn 40 long, marked as the 50 the user wants.
    click(&mut h, "Dimension");
    let (a, b) = (at(&h, [-20.0, -18.0]), at(&h, [20.0, -18.0]));
    drag(&mut h, a, b);
    h.get_by_label("Sketch text").type_text("50");
    h.step();
    h.key_press(egui::Key::Enter);
    h.step();
    h.step();

    let items = h.state().state.sketch.items.clone();
    assert_eq!(items.len(), 4, "{items:?}");
    assert_eq!(items[0], sketch::Item::Circle { centre: [-10.0, 5.0], radius: 3.0, colour: 0 });
    assert!(matches!(&items[1], sketch::Item::Stroke { points, colour: 3 } if points.len() >= 3));
    assert!(matches!(&items[2], sketch::Item::Line { arrow: true, colour: 0, .. }));
    assert_eq!(items[3], sketch::Item::Dimension { a: [-20.0, -18.0], b: [20.0, -18.0], text: "50".into(), colour: 0 });

    // Colours get a meaning, which also shows as a legend on the picture.
    h.get_by_label("red meaning").click();
    h.step();
    h.get_by_label("red meaning").type_text("new M4 hole");
    h.step();
    h.step();
    h.state_mut().state.sketch.meanings[3] = "keep this area clear".into();
    h.step();
    shot(&mut h, "sketch_annotated");

    // Attach: the app asks the window for a screenshot and cuts the canvas out of it.
    // If this harness doesn't deliver one by itself, hand it over the way a window would.
    click(&mut h, "Attach to chat");
    let frame = h.render().expect("render");
    let size = [frame.width() as usize, frame.height() as usize];
    if h.state().state.sketch.capture != sketch::Capture::Idle {
        let image = std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied(size, frame.as_raw()));
        h.event(egui::Event::Screenshot { viewport_id: egui::ViewportId::ROOT, user_data: Default::default(), image });
        h.step();
        h.step();
    }
    assert_eq!(h.state().state.sketch.capture, sketch::Capture::Idle);
    let area = canvas;

    let (png, summary) = h.state().state.chat.attached_sketch().cloned().expect("the sketch is attached");
    assert!(summary.contains("drawn over the top view of the model at true scale: sketch X is model X, sketch Y is model Y"), "{summary}");
    assert!(summary.contains("Colour key: red = new M4 hole; blue = keep this area clear."), "{summary}");
    assert!(summary.contains("red circle, diameter 6 mm, centre (-10, 5)"), "{summary}");
    assert!(summary.contains("red dimension marked \"50\" between (-20, -18) and (20, -18) (drawn 40 mm long)"), "{summary}");
    h.get_by_label("Sketch attached");

    // The picture is the canvas, cut out of the window, with the drawing in it.
    assert!(png.starts_with("iVBORw0KGgo"), "base64 of a PNG header");
    let (pixels, width, height) = sketch::crop(&egui::ColorImage::from_rgba_unmultiplied(size, frame.as_raw()), area, 1.0).unwrap();
    assert_eq!((width, height), (area.width().round() as u32, area.height().round() as u32));
    let red = pixels.as_chunks::<4>().0.iter().filter(|p| p[0] > 200 && p[1] < 110 && p[2] < 110).count();
    assert!(red > 150, "the red drawing is in the picture ({red} red pixels)");
    std::fs::write(std::env::temp_dir().join("intercraft-ui").join("sketch_sent.rgba"), &pixels).unwrap();

    // Remove takes it off again.
    shot(&mut h, "sketch_attached");
    // The Send button is still on screen with the attachment showing.
    let window = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 860.0));
    assert!(window.contains_rect(h.get_by_label("Send").rect()), "Send was pushed out of view");
    click(&mut h, "Remove");
    assert!(h.state().state.chat.attached_sketch().is_none());
}

#[test]
fn the_why_shows_on_the_timeline_and_in_the_inspector() {
    let _one = one_at_a_time();
    let project = example_project("why");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    let why = "Leaves 3 mm of material round the bore so the clamp stays stiff.";
    let edit = intercraft_core::FeatureEdit { why: Some(why.into()), ..Default::default() };
    h.state_mut().state.edit_project(|p| p.edit_feature("RodBracket", "f040", edit).and_then(|()| p.save())).unwrap();
    h.step();
    h.step();
    // Once on the step card and once in the Inspector, which follows the latest step.
    assert_eq!(h.get_all_by_label_contains("so the clamp stays stiff").count(), 2);
    h.get_by_label("Why:");
    h.get_by_label_contains("Asked for: Round the vertical edges.");
    shot(&mut h, "step_why");
}

#[test]
fn explain_buttons_ask_the_chat() {
    let _one = one_at_a_time();
    let project = example_project("explain");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    // No provider set up: the button is there but does nothing.
    h.state_mut().state.chat.use_server("", "");
    h.step();
    h.get_by_label("Why is it like this?").click();
    h.step();
    assert!(h.state().state.chat.last_question().is_none());

    // With a provider (a local address nothing answers on), it asks about the shown step.
    h.state_mut().state.chat.use_server("http://127.0.0.1:9/v1", "test-model");
    h.step();
    click(&mut h, "Why is it like this?");
    let asked = h.state().state.chat.last_question().unwrap_or_default().to_owned();
    assert_eq!(asked, "Explain step 4, 'Edge fillets', of RodBracket: what it does and why it is designed this way. Keep it short and plain.");
    shot(&mut h, "explain_asked");
}

#[test]
fn the_brief_tab_shows_and_edits_the_brief() {
    let _one = one_at_a_time();
    let project = example_project("brief");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    let tab = h.state().dock.find_tab(&Tab::Brief).expect("the brief tab is in the layout");
    h.state_mut().dock.set_active_tab(tab).expect("the tab can be shown");
    h.step();
    h.step();
    // The example has no brief file yet.
    h.get_by_label_contains("The brief is empty");

    click(&mut h, "Edit brief");
    h.state_mut().state.brief_draft = Some("# Brief

## What it is

A clamp for an 8 mm rod.
".into());
    h.step();
    h.get_by_label("Brief text");
    click(&mut h, "Save");
    assert!(h.state().state.brief_draft.is_none());
    let saved = std::fs::read_to_string(project.join("brief.md")).unwrap();
    assert!(saved.contains("A clamp for an 8 mm rod."));
    h.step();
    h.get_by_label_contains("A clamp for an 8 mm rod.");
    assert_eq!(h.state().state.history.undo_label(), Some("Update the brief"));
    shot(&mut h, "brief_tab");
}

/// Puts the explanation level and layout back when a test that changes them ends,
/// even if it fails, so the tests after it see the usual advanced layout.
struct RestoreExperience;

impl Drop for RestoreExperience {
    fn drop(&mut self) {
        let _ = settings::Settings::update(|s| {
            s.explain = settings::Explain::Standard;
            s.layout = settings::Layout::Advanced;
            s.onboarded = true;
        });
    }
}

#[test]
fn first_run_choice_sets_up_a_beginner() {
    let _one = one_at_a_time();
    let _restore = RestoreExperience;
    let project = example_project("first-run");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    h.state_mut().first_run = true;
    // A new window takes a few frames to size itself and settle.
    for _ in 0..4 {
        h.step();
    }
    shot(&mut h, "first_run");
    click(&mut h, "I'm new to CAD");
    assert!(!h.state().first_run);
    assert!(h.state().state.simple);
    assert_eq!(h.state().state.chat.explain(), settings::Explain::Beginner);
    let saved = settings::Settings::load();
    assert_eq!((saved.explain, saved.layout, saved.onboarded), (settings::Explain::Beginner, settings::Layout::Simple, true));
    assert!(h.state().state.chat.system_prompt().contains("The user is new to CAD"));

    // The simple layout hides the code and the file path, and names parameters in words.
    h.step();
    h.step();
    assert_eq!(h.query_all_by_label("Code").count(), 0, "no Code section");
    assert_eq!(h.query_all_by_label_contains("040_fillet.py").count(), 0, "no file path");
    h.get_by_label("Radius");
    h.get_by_label("Move part");
    shot(&mut h, "simple_layout");

    // Back to expert and advanced from Settings.
    h.state_mut().set_experience(settings::Explain::Expert, false);
    h.step();
    h.step();
    h.get_by_label("Code");
    assert!(h.state().state.chat.system_prompt().contains("The user knows CAD"));
}

#[test]
fn the_caption_says_what_the_model_is_doing() {
    let _one = one_at_a_time();
    let project = example_project("caption");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    let input = serde_json::json!({ "part": "RodBracket", "label": "Rod bore", "why": "Sized for the 8 mm rod." });
    let started = intercraft_llm::ChatEvent::ToolStarted { id: "t1".into(), name: "add_feature".into(), input };
    h.state_mut().state.chat.inject(started);
    h.step();
    let (text, alpha) = h.state().state.chat.caption().map(|(t, a)| (t.to_owned(), a)).expect("a caption");
    assert_eq!((text.as_str(), alpha), ("Adding Rod bore on RodBracket: Sized for the 8 mm rod.", 1.0));
    shot(&mut h, "caption");

    // After the reply it lingers, then goes.
    h.state_mut().state.chat.inject(intercraft_llm::ChatEvent::Done);
    h.step();
    assert!(h.state().state.chat.caption().is_some());

    // A change from outside the chat (say a Claude Code terminal) gets a caption too.
    let edit = intercraft_core::FeatureEdit { label: Some("Rounded edges".into()), ..Default::default() };
    let mut loaded = intercraft_core::LoadedProject::load(&project).unwrap();
    loaded.edit_feature("RodBracket", "f040", edit).unwrap();
    loaded.save().unwrap();
    wait_until(&mut h, "the outside change", |app| app.state.outside_caption.shown().is_some());
    assert_eq!(h.state().state.outside_caption.shown().unwrap().0, "Edit 'Rounded edges'");
}

#[test]
fn the_chat_shows_what_replies_cost() {
    let _one = one_at_a_time();
    let project = example_project("cost");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    let chat = &mut h.state_mut().state.chat;

    // A local model is free, and says so.
    chat.use_server("http://localhost:11434/v1", "qwen3");
    assert_eq!(chat.cost_line().unwrap().0, "Free: the model runs on this computer");
    // A hosted server without prices asks for them.
    chat.use_server("https://openrouter.ai/api/v1", "some-model");
    assert!(chat.cost_line().unwrap().0.contains("Enter the server's prices"));

    // The API: costs add up per reply and per conversation.
    chat.use_provider(settings::Provider::Anthropic);
    assert!(chat.cost_line().is_none(), "nothing to say before a reply");
    chat.inject(intercraft_llm::ChatEvent::Cost(0.0125));
    chat.inject(intercraft_llm::ChatEvent::Cost(0.0225));
    chat.inject(intercraft_llm::ChatEvent::Done);
    assert_eq!(chat.cost_line().unwrap().0, "Reply ~$0.035 · chat ~$0.035");
    chat.use_provider(settings::Provider::ClaudeCode);
    assert!(chat.cost_line().unwrap().0.ends_with("(subscription)"));
    // The panel sizes itself a frame after the line first appears.
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label_contains("chat ~$0.035");
    shot(&mut h, "cost_line");
}

#[test]
fn a_part_on_a_joint_is_shown_where_the_joint_puts_it() {
    let _one = one_at_a_time();
    let project = example_project("joint");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);

    // From outside, the way a Claude Code terminal would: a copy turned 90° on a hinge.
    let mut loaded = intercraft_core::LoadedProject::load(&project).unwrap();
    loaded.duplicate_part("RodBracket", Some("Arm")).unwrap();
    loaded.save().unwrap();
    loaded
        .add_joint(intercraft_core::Joint {
            name: "Arm pivot".into(),
            kind: intercraft_core::JointKind::Hinge,
            base: "RodBracket".into(),
            part: "Arm".into(),
            origin: [0.0, 0.0, 0.0],
            axis: [0.0, 0.0, 1.0],
            min: 0.0,
            max: 90.0,
            at: 90.0,
            why: "Swings the arm round.".into(),
        })
        .unwrap();
    loaded.save().unwrap();
    wait_until(&mut h, "the turned copy", |app| built(app) && app.state.shown.contains_key("Arm"));

    // The bracket is 40 x 30 in X and Y; turned a quarter about Z the copy is 30 x 40.
    let (lo, hi) = h.state().state.viewport.visible_bounds().unwrap();
    assert!((hi[0] - lo[0] - 40.0).abs() < 0.01 && (hi[1] - lo[1] - 40.0).abs() < 0.01, "{lo:?} {hi:?}");
    h.state_mut().state.selected_part = Some("Arm".into());
    h.step();
    h.step();
    h.get_by_label("Moves on Arm pivot: hinge, 0 to 90°, now 90°");
    assert_eq!(h.state().state.outside_caption.shown().unwrap().0, "Add joint 'Arm pivot'");
}

#[test]
fn joints_move_in_the_motion_panel() {
    let _one = one_at_a_time();
    let project = example_project("motion");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    let mut loaded = intercraft_core::LoadedProject::load(&project).unwrap();
    loaded.duplicate_part("RodBracket", Some("Arm")).unwrap();
    loaded
        .add_joint(intercraft_core::Joint {
            name: "Arm pivot".into(),
            kind: intercraft_core::JointKind::Hinge,
            base: "RodBracket".into(),
            part: "Arm".into(),
            origin: [0.0, 0.0, 0.0],
            axis: [0.0, 0.0, 1.0],
            min: 0.0,
            max: 90.0,
            at: 0.0,
            why: String::new(),
        })
        .unwrap();
    loaded.save().unwrap();
    wait_until(&mut h, "the copy", |app| built(app) && app.state.shown.contains_key("Arm"));
    // Both parts sit on top of each other: 40 x 30 from above.
    let depth = |h: &Harness<'static, App>| {
        let (lo, hi) = h.state().state.viewport.visible_bounds().unwrap();
        hi[1] - lo[1]
    };
    let saved = || intercraft_core::LoadedProject::load(&project).unwrap().project.joints[0].at;
    assert!((depth(&h) - 30.0).abs() < 0.01);

    click(&mut h, "Motion");
    h.get_by_label_contains("Drag to move");

    // Play swings the arm round without saving anything.
    click(&mut h, "Play");
    for _ in 0..30 {
        h.step();
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(depth(&h) > 31.0, "the arm has turned: {}", depth(&h));
    shot(&mut h, "motion_playing");
    click(&mut h, "Stop");
    assert!((depth(&h) - 30.0).abs() < 0.01, "back where it is saved");
    assert_eq!(saved(), 0.0);

    // Dragging the slider previews; letting go saves.
    let rail = h.get_by_role(egui::accesskit::Role::Slider).rect();
    let press = |h: &mut Harness<'static, App>, pos: egui::Pos2, pressed: bool| {
        h.event(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    };
    let start = rail.left_center() + egui::vec2(4.0, 0.0);
    h.event(egui::Event::PointerMoved(start));
    h.step();
    press(&mut h, start, true);
    let end = rail.right_center() + egui::vec2(30.0, 0.0);
    h.event(egui::Event::PointerMoved(end));
    h.step();
    h.step();
    assert!((depth(&h) - 40.0).abs() < 0.01, "turned a quarter while held: {}", depth(&h));
    assert_eq!(saved(), 0.0, "nothing saved while dragging");
    press(&mut h, end, false);
    h.step();
    assert_eq!(saved(), 90.0);
    assert_eq!(h.state().state.history.undo_label(), Some("Move 'Arm pivot' to 90°"));
    // Still shown turned while the copy rebuilds, and after.
    assert!((depth(&h) - 40.0).abs() < 0.01);
    wait_until(&mut h, "the rebuild", |app| built(app) && app.state.building.is_empty());
    h.step();
    assert!((depth(&h) - 40.0).abs() < 0.01);
    shot(&mut h, "motion_panel");
}

#[test]
fn several_questions_in_one_reply_are_answered_together() {
    let _one = one_at_a_time();
    let project = example_project("questions");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    let chat = &mut h.state_mut().state.chat;
    chat.use_server("http://127.0.0.1:9/v1", "test-model");

    let ask = |id: &str, question: &str, a: &str, b: &str| {
        let input = serde_json::json!({ "question": question, "options": [{ "label": a }, { "label": b }] });
        intercraft_llm::ChatEvent::ToolStarted { id: id.into(), name: "ask_user".into(), input }
    };
    let shown = |id: &str| intercraft_llm::ChatEvent::ToolFinished {
        id: id.into(),
        ok: true,
        output: serde_json::json!({ "shown": true }),
    };
    chat.inject(ask("q1", "What will hold it to the wall?", "Two screws", "Adhesive strip"));
    chat.inject(shown("q1"));
    chat.inject(ask("q2", "How will it be made?", "3D printed", "Cut from wood"));
    chat.inject(shown("q2"));
    chat.inject(intercraft_llm::ChatEvent::Done);
    h.step();
    h.step();

    // One answer is not enough to send.
    click(&mut h, "Two screws");
    assert!(h.state().state.chat.last_question().is_none());
    shot(&mut h, "two_questions");

    click(&mut h, "3D printed");
    let sent = h.state().state.chat.last_question().unwrap_or_default().to_owned();
    assert_eq!(sent, "What will hold it to the wall? Two screws\nHow will it be made? 3D printed");
}

#[test]
fn design_terms_and_parameter_names_are_explained() {
    let _one = one_at_a_time();
    let _restore = RestoreExperience;
    let project = example_project("glossary");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    let labels = [("radius".to_owned(), "Corner rounding".to_owned())].into_iter().collect();
    let edit = intercraft_core::FeatureEdit {
        why: Some("A fillet takes the sharp corner off so it is nicer to hold.".into()),
        labels: Some(labels),
        ..Default::default()
    };
    h.state_mut().state.edit_project(|p| p.edit_feature("RodBracket", "f040", edit).and_then(|()| p.save())).unwrap();
    h.step();

    // The step's terms come with a plain explanation, unless the user chose Expert.
    let tip = h.state().state.glossary_tip("Edge fillets").expect("fillet is explained");
    assert!(tip.starts_with("Fillet: A rounded edge."));
    h.state_mut().set_experience(settings::Explain::Expert, false);
    assert!(h.state().state.glossary_tip("Edge fillets").is_none());

    // The simple layout uses the model's plain name for the parameter.
    h.state_mut().set_experience(settings::Explain::Beginner, true);
    h.step();
    h.step();
    h.get_by_label("Corner rounding");
    // Not the code name made readable ("Radius"): the model's label wins.
    assert_eq!(h.query_all_by_label("Radius").count(), 0);
}

#[test]
fn print_check_marks_problems_on_the_part() {
    let _one = one_at_a_time();
    let project = example_project("print");
    let mut h = harness(&project);
    wait_until(&mut h, "the first build", built);
    click(&mut h, "Print check");
    wait_until(&mut h, "the print check", |app| app.state.print_check.as_ref().is_some_and(|c| !c.markers().is_empty()));
    // The bracket's 1 mm clamp slit cuts a thin bridge above the bore.
    let markers = h.state().state.print_check.as_ref().unwrap().markers();
    assert!(markers.iter().any(|(_, _, kind)| kind == "thin wall" || kind == "overhang"), "{markers:?}");
    h.get_by_label("Print check: RodBracket");
    shot(&mut h, "print_check");
    click(&mut h, "Print check");
    assert!(h.state().state.print_check.is_none());
}
