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
    assert_eq!(dragged.name, "radius");
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
