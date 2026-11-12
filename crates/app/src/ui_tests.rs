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
    h.state_mut().state.rename = Some(("RodBracket_copy".into(), "bad name!".into(), None));
    h.step();
    click(&mut h, "Rename");
    assert_eq!(part_names(&h)[1], "RodBracket_copy", "a bad name changes nothing");
    h.state_mut().state.rename = Some(("RodBracket_copy".into(), "Twin".into(), None));
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
