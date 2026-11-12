// InterCraft: watch an LLM (or you) edit a parametric model, one feature at a time.
// eframe/egui 0.36.2 (wgpu 30), egui_dock 0.21.1, egui_commonmark 0.25, rfd 0.18, glam 0.34, bytemuck 1, png 0.18
// Worker: Python 3.11, build123d 0.13.0, numpy 2.4.6

mod caption;
mod chat;
mod fit;
mod measure;
mod menus;
mod params;
mod settings;
mod sketch;
#[cfg(test)]
mod ui_tests;
mod viewport;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use egui_dock::{DockArea, DockState, TabViewer};
use intercraft_core::{HistoryInfo, LoadedProject, Part, PartKind};
use intercraft_geo::{ExportFormat, FeatureResult, GeoEvent, GeometryBackend, Mesh, PythonWorker, Report};
use viewport::Viewport;

const POLL_INTERVAL: Duration = Duration::from_millis(400);

// One colour per part, cycled.
const PART_COLORS: [[f32; 3]; 4] = [
    [0.22, 0.42, 0.70],
    [0.80, 0.45, 0.13],
    [0.25, 0.58, 0.36],
    [0.55, 0.35, 0.65],
];
// Reference parts are grey so they read as "not ours to change".
const REFERENCE_COLOR: [f32; 3] = [0.45, 0.45, 0.47];

fn main() -> eframe::Result {
    // A folder on the command line wins, then the most recent project, then the example.
    let project_dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        settings::Settings::load()
            .recent_projects
            .iter()
            .map(PathBuf::from)
            .find(|p| p.join("project.json").exists())
            .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bracket")))
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 680.0])
            .with_maximized(true)
            .with_title("InterCraft"),
        ..Default::default()
    };
    eframe::run_native(
        "InterCraft",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, project_dir)?))),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Tab {
    Viewport,
    Parts,
    Inspector,
    Timeline,
    Chat,
    Sketch,
    Brief,
}

struct App {
    dock: DockState<Tab>,
    state: State,
    settings_open: bool,
    new_project: Option<menus::NewProjectDialog>,
    /// Window focus last frame, to notice when the user comes back to this window.
    was_focused: bool,
    /// The window title needs the new project's name.
    title_dirty: bool,
    /// The "new to CAD?" question hasn't been answered yet.
    first_run: bool,
}

/// Latest result for one feature. `stale` means a rebuild is running and
/// this result has not been confirmed by it yet.
struct Entry {
    result: FeatureResult,
    stale: bool,
}

struct State {
    project_dir: PathBuf,
    project: Option<LoadedProject>,
    project_error: Option<String>,
    last_mtime: Option<SystemTime>,
    last_poll: Instant,

    worker: Option<PythonWorker>,
    worker_status: String,
    building: HashSet<String>,

    /// (part, feature id) -> result
    results: HashMap<(String, String), Entry>,
    /// Part whose steps the timeline shows.
    selected_part: Option<String>,
    /// Step of the selected part being inspected. None follows the latest step.
    selected_step: Option<String>,
    /// Mesh file currently on screen for each part.
    shown: HashMap<String, PathBuf>,
    hidden: HashSet<String>,
    /// Undo history of the open project, refreshed on every reload.
    history: HistoryInfo,
    /// A parameter being dragged in the Inspector, previewed but not saved yet.
    param_drag: Option<params::ParamDrag>,
    /// The Rename window: what is being renamed, the name typed so far, and any error.
    rename: Option<(RenameTarget, String, Option<String>)>,
    /// A step's code being edited in the Inspector, not applied yet.
    code_draft: Option<CodeDraft>,
    /// The simple layout: code, file paths and raw parameter names hidden.
    simple: bool,
    /// The brief as typed in its tab, while editing.
    brief_draft: Option<String>,
    brief_markdown: egui_commonmark::CommonMarkCache,

    viewport: Viewport,
    chat: chat::ChatPanel,
    measure: measure::MeasureTool,
    /// The fit check panel, while it is open.
    fit: Option<fit::FitCheck>,
    sketch: sketch::Sketch,

    exports_pending: usize,
    exported: Vec<String>,
    /// Project name currently in the window title.
    shown_name: Option<String>,
    /// Folder of the last finished export, for the "Show" button.
    export_dir: Option<PathBuf>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, project_dir: PathBuf) -> anyhow::Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .clone()
            .ok_or_else(|| anyhow::anyhow!("InterCraft needs the wgpu renderer"))?;

        let ctx = cc.egui_ctx.clone();
        let (worker, worker_status) = match PythonWorker::spawn(move || ctx.request_repaint()) {
            Ok(w) => (Some(w), "Starting geometry worker...".to_owned()),
            Err(e) => (None, format!("Worker failed to start: {e:#}")),
        };

        let saved = settings::Settings::load();
        let simple = saved.layout == settings::Layout::Simple;
        let dock = menus::default_dock(simple);
        cc.egui_ctx.set_theme(saved.theme_preference());

        let mut state = State {
            project_dir: project_dir.clone(),
            project: None,
            project_error: None,
            last_mtime: None,
            last_poll: Instant::now(),
            worker,
            worker_status,
            building: HashSet::new(),
            results: HashMap::new(),
            selected_part: None,
            selected_step: None,
            shown: HashMap::new(),
            hidden: HashSet::new(),
            history: HistoryInfo::default(),
            param_drag: None,
            rename: None,
            code_draft: None,
            simple,
            brief_draft: None,
            brief_markdown: Default::default(),
            viewport: Viewport::new(render_state),
            chat: chat::ChatPanel::new(cc.egui_ctx.clone(), project_dir.clone()),
            measure: measure::MeasureTool::default(),
            fit: None,
            sketch: sketch::Sketch::default(),
            exports_pending: 0,
            exported: Vec::new(),
            shown_name: None,
            export_dir: None,
        };
        // Lets an MCP server started by another app (Claude Code, Cursor...) edit this project.
        if let Err(e) = intercraft_tools::record_active_project(&state.project_dir) {
            state.worker_status = format!("{} (couldn't record the open project: {e:#})", state.worker_status);
        }
        let _ = settings::Settings::remember_project(&state.project_dir);
        state.reload_project();
        let first_run = !saved.onboarded;
        Ok(Self { dock, state, settings_open: false, new_project: None, was_focused: true, title_dirty: true, first_run })
    }

    /// Switch to another project folder.
    pub(crate) fn open(&mut self, dir: PathBuf) {
        match self.state.open_project(dir) {
            Ok(()) => self.title_dirty = true,
            Err(e) => self.state.worker_status = format!("Couldn't open that project: {e:#}"),
        }
    }

    fn update_title(&self, ctx: &egui::Context) {
        let name = self.state.project.as_ref().map(|p| p.project.name.clone());
        let title = match name {
            Some(name) => format!("{name} - InterCraft"),
            None => "InterCraft".to_owned(),
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // The window the user last came back to decides which project an MCP client
        // (a Claude Code terminal) edits, not the window that happened to open last.
        let focused = ctx.input(|i| i.focused);
        if focused && !self.was_focused {
            let _ = intercraft_tools::record_active_project(&self.state.project_dir);
        }
        self.was_focused = focused;
        if std::mem::take(&mut self.state.chat.wants_settings) {
            self.settings_open = true;
        }
        // A screenshot only ever arrives because the sketch panel asked for one.
        let screenshot = ctx.input(|i| {
            i.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
        });
        if let Some(image) = screenshot {
            self.state.attach_sketch(&image, ctx.pixels_per_point());
        }
        self.state.drain_worker_events();
        // Follow the part the chat model is editing, so its steps show as they land.
        if let Some(part) = self.state.chat.poll() {
            self.state.selected_part = Some(part);
            self.state.selected_step = None;
        }
        self.state.poll_files();
        self.state.send_preview();
        self.state.fix_selection();
        self.state.sync_meshes();
        // Both of these need the meshes of the latest build, so they come after the sync.
        self.state.run_fit();
        self.state.serve_views();
        // The project name can also change on disk, so check it after each reload.
        if std::mem::take(&mut self.title_dirty) || self.state.project_renamed() {
            self.update_title(ctx);
        }
        // Keeps the file polling ticking while nothing else is happening.
        ctx.request_repaint_after(POLL_INTERVAL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.shortcuts(&ctx);
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if !self.state.building.is_empty() || self.state.exports_pending > 0 {
                    ui.spinner();
                }
                ui.label(&self.state.worker_status);
                if let Some(dir) = &self.state.export_dir {
                    if ui.small_button("Show files").clicked() {
                        menus::reveal(dir);
                    }
                }
                if let Some(err) = &self.state.project_error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
            });
        });
        self.settings_window(&ctx);
        self.first_run_window(&ctx);
        self.new_project_window(&ctx);
        DockArea::new(&mut self.dock)
            .show_close_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_inside(ui, &mut self.state);
    }
}

impl State {
    /// Load another project, clearing everything from the current one. The current
    /// project stays open if the new one can't be loaded.
    fn open_project(&mut self, dir: PathBuf) -> anyhow::Result<()> {
        let loaded = LoadedProject::load(&dir)?;
        // Results from builds still running for the old project would land on the new one.
        if !self.building.is_empty() {
            if let Some(worker) = self.worker.as_mut() {
                worker.restart()?;
            }
        }
        self.project_dir = dir.clone();
        self.results.clear();
        self.building.clear();
        self.shown.clear();
        self.hidden.clear();
        self.selected_part = None;
        self.selected_step = None;
        self.export_dir = None;
        self.viewport.retain_meshes(|_| false);
        let _ = intercraft_tools::record_active_project(&dir);
        let _ = settings::Settings::remember_project(&dir);
        self.chat.set_project(dir, &loaded.project.name);
        self.worker_status = format!("Opened {}", loaded.project.name);
        self.last_mtime = loaded.latest_mtime();
        self.project_error = None;
        self.history = loaded.history();
        self.project = Some(loaded);
        self.rebuild_all();
        Ok(())
    }

    /// Pick a STEP file and add it to the project as a new part.
    fn import_step_dialog(&mut self) {
        let Some(file) = rfd::FileDialog::new()
            .set_title("Import a STEP file")
            .add_filter("STEP", &["step", "stp", "STEP", "STP"])
            .pick_file()
        else {
            return;
        };
        self.import_step(&file);
    }

    fn import_step(&mut self, file: &std::path::Path) {
        let result = self.edit_project(|p| {
            let name = p.import_step(file)?;
            p.save()?;
            Ok(name)
        });
        match result {
            Ok(name) => {
                self.worker_status = format!("Imported {} as the part '{name}'", file.display());
                self.selected_part = Some(name);
                self.selected_step = None;
            }
            Err(e) => self.worker_status = format!("Import failed: {e:#}"),
        }
    }

    fn set_part_kind(&mut self, name: &str, kind: PartKind) {
        if let Err(e) = self.edit_project(|p| p.set_kind(name, kind).and_then(|()| p.save())) {
            self.worker_status = format!("Couldn't change '{name}': {e:#}");
        }
    }

    pub(crate) fn delete_part(&mut self, name: &str) {
        match self.edit_project(|p| p.remove_part(name).and_then(|()| p.save())) {
            Ok(()) => {
                self.hidden.remove(name);
                self.worker_status = format!("Deleted '{name}'. Ctrl+Z brings it back.");
            }
            Err(e) => self.worker_status = format!("Couldn't delete '{name}': {e:#}"),
        }
    }

    fn delete_step(&mut self, part: &str, id: &str) {
        match self.edit_project(|p| p.delete_feature(part, id).and_then(|()| p.save())) {
            Ok(()) => self.worker_status = format!("Deleted a step from '{part}'. Ctrl+Z brings it back."),
            Err(e) => self.worker_status = format!("Couldn't delete that step: {e:#}"),
        }
    }

    pub(crate) fn undo(&mut self) {
        self.worker_status = match self.edit_project(|p| p.undo()) {
            Ok(label) => format!("Undid: {label}"),
            Err(e) => capitalise(&format!("{e:#}")),
        };
    }

    pub(crate) fn redo(&mut self) {
        self.worker_status = match self.edit_project(|p| p.redo()) {
            Ok(label) => format!("Redid: {label}"),
            Err(e) => capitalise(&format!("{e:#}")),
        };
    }

    /// Jump to a point in the history (Edit > History).
    pub(crate) fn go_to(&mut self, number: u64) {
        if let Err(e) = self.edit_project(|p| p.go_to(number)) {
            self.worker_status = format!("Couldn't go back: {e:#}");
        } else if let Some(entry) = self.history.entries.iter().find(|e| e.number == number) {
            self.worker_status = format!("Went to: {}", entry.label);
        }
    }

    /// Change the project the same way the tools do: under the project lock and
    /// starting from the files as they are now, so a change the model made a moment
    /// ago is never overwritten. Reloads afterwards.
    pub(crate) fn edit_project<T>(&mut self, change: impl FnOnce(&mut LoadedProject) -> anyhow::Result<T>) -> anyhow::Result<T> {
        let lock = intercraft_core::try_lock(&self.project_dir)?;
        if lock.is_none() {
            anyhow::bail!("the model is changing this project right now. Try again when it has finished");
        }
        let mut loaded = LoadedProject::load(&self.project_dir)?;
        let result = change(&mut loaded);
        drop(lock);
        self.reload_project();
        result
    }

    /// Save a parameter edited in the Inspector. One edit, one undo step.
    pub(crate) fn commit_param(&mut self, edit: params::ParamCommit) {
        let params::ParamCommit { part, field, value } = edit;
        let shown = value.as_f64().map_or_else(|| value.to_string(), measure::mm);
        let name = field.label();
        let result = self.edit_project(|p| {
            match &field {
                params::Field::Param { feature, name } => p.set_param(&part, feature, name, value)?,
                params::Field::Position(axis) | params::Field::Rotation(axis) => {
                    let current = p.part(&part)?;
                    let (mut position, mut rotation) = (current.position, current.rotation);
                    let number = value.as_f64().unwrap_or(0.0);
                    match field {
                        params::Field::Position(_) => position[*axis] = number,
                        _ => rotation[*axis] = number,
                    }
                    p.set_placement(&part, position, rotation)?;
                }
            }
            p.save()
        });
        match result {
            Ok(()) => self.worker_status = format!("Set {name} = {shown}. Ctrl+Z undoes it."),
            Err(e) => self.worker_status = format!("Couldn't change {name}: {e:#}"),
        }
    }

    fn rename_part(&mut self, old: &str, new: &str) -> anyhow::Result<()> {
        self.edit_project(|p| p.rename_part(old, new).and_then(|()| p.save()))?;
        if self.hidden.remove(old) {
            self.hidden.insert(new.to_owned());
        }
        if self.selected_part.as_deref() == Some(old) {
            self.selected_part = Some(new.to_owned());
        }
        self.worker_status = format!("Renamed '{old}' to '{new}'");
        Ok(())
    }

    pub(crate) fn duplicate_part(&mut self, name: &str) {
        let result = self.edit_project(|p| {
            let copy = p.duplicate_part(name, None)?;
            p.save()?;
            Ok(copy)
        });
        match result {
            Ok(copy) => {
                self.worker_status = format!("Copied '{name}' as '{copy}'. Move it with Placement in the Inspector.");
                self.selected_part = Some(copy);
                self.selected_step = None;
            }
            Err(e) => self.worker_status = format!("Couldn't copy '{name}': {e:#}"),
        }
    }

    fn rename_step(&mut self, part: &str, feature: &str, label: &str) -> anyhow::Result<()> {
        let edit = intercraft_core::FeatureEdit { label: Some(label.to_owned()), ..Default::default() };
        self.edit_project(|p| p.edit_feature(part, feature, edit).and_then(|()| p.save()))?;
        self.worker_status = format!("Renamed the step to '{label}'");
        Ok(())
    }

    /// Move a step one place earlier (-1) or later (+1) on the timeline.
    pub(crate) fn move_step(&mut self, part: &str, feature: &str, by: isize) {
        let result = self.edit_project(|p| {
            let features = &p.part(part)?.features;
            let from = features.iter().position(|f| f.id == feature).ok_or_else(|| anyhow::anyhow!("that step is gone"))?;
            let to = from.checked_add_signed(by).filter(|to| *to < features.len());
            let to = to.ok_or_else(|| anyhow::anyhow!("it is already at that end"))?;
            // After the step it swaps with when moving later, after the one before that when moving earlier.
            let after = if by > 0 { Some(features[to].id.clone()) } else { to.checked_sub(1).map(|i| features[i].id.clone()) };
            p.move_feature(part, feature, after.as_deref())?;
            p.save()
        });
        match result {
            Ok(()) => self.worker_status = "Moved the step. Ctrl+Z undoes it.".to_owned(),
            Err(e) => self.worker_status = format!("Couldn't move that step: {e:#}"),
        }
    }

    /// Save code edited in the Inspector and rebuild from that step.
    pub(crate) fn apply_code(&mut self, part: &str, feature: &str, code: String) {
        let edit = intercraft_core::FeatureEdit { code: Some(code), ..Default::default() };
        match self.edit_project(|p| p.edit_feature(part, feature, edit).and_then(|()| p.save())) {
            Ok(()) => {
                // The next frame starts a fresh draft from what was saved.
                self.code_draft = None;
                self.worker_status = "Applied the code. Ctrl+Z undoes it.".to_owned();
            }
            Err(e) => self.worker_status = format!("Couldn't apply the code: {e:#}"),
        }
    }

    /// The small window for typing a new name for a part or a step.
    fn rename_window(&mut self, ctx: &egui::Context) {
        let Some((target, mut name, mut error)) = self.rename.take() else { return };
        let (title, hint, current) = match &target {
            RenameTarget::Part(part) => ("Rename part", "Letters, digits, _ and -", part.clone()),
            RenameTarget::Step { label, .. } => ("Rename step", "A short name for the step", label.clone()),
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new(title)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let field = ui.add(egui::TextEdit::singleline(&mut name).hint_text(hint));
                if !field.has_focus() && !field.lost_focus() {
                    field.request_focus();
                }
                if let Some(error) = &error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Rename").clicked() || enter {
                    if name.trim() == current {
                        done = true;
                    } else {
                        let result = match &target {
                            RenameTarget::Part(part) => self.rename_part(part, name.trim()),
                            RenameTarget::Step { part, feature, .. } => self.rename_step(part, feature, name.trim()),
                        };
                        match result {
                            Ok(()) => done = true,
                            Err(e) => error = Some(capitalise(&format!("{e:#}"))),
                        }
                    }
                }
            });
        if open && !done {
            self.rename = Some((target, name, error));
        }
    }

    /// While a parameter is dragged, rebuild the part with the dragged value without
    /// saving it. A new preview goes out only when the last one has finished, so a
    /// fast drag never queues up a backlog of builds.
    fn send_preview(&mut self) {
        let Some(drag) = &mut self.param_drag else { return };
        if drag.sent == Some(drag.value) || self.building.contains(&drag.part) {
            return;
        }
        let (Some(loaded), Some(worker)) = (&self.project, &mut self.worker) else { return };
        let Ok(part) = loaded.part(&drag.part) else { return };
        let mut part = part.clone();
        if !drag.apply(&mut part) {
            return;
        }
        if worker.rebuild(intercraft_tools::request_for(&loaded.dir, &part)).is_ok() {
            for ((name, _), entry) in self.results.iter_mut() {
                if *name == part.name {
                    entry.stale = true;
                }
            }
            self.building.insert(part.name);
            drag.sent = Some(drag.value);
        }
    }

    fn set_visible(&mut self, name: &str, visible: bool) {
        self.viewport.set_visible(name, visible);
        if visible {
            self.hidden.remove(name);
        } else {
            self.hidden.insert(name.to_owned());
        }
    }

    fn show_all(&mut self) {
        for name in std::mem::take(&mut self.hidden) {
            self.viewport.set_visible(&name, true);
        }
    }

    /// Toolbar, keyboard shortcuts and the measuring tool, drawn over the 3D view.
    fn viewport_overlay(&mut self, ui: &mut egui::Ui, input: viewport::ViewportInput) {
        let rect = input.rect;
        let typing = ui.ctx().egui_wants_keyboard_input();
        if input.hover.is_some() && !typing {
            if ui.input(|i| i.key_pressed(egui::Key::M)) {
                self.measure.toggle();
            }
            if ui.input(|i| i.key_pressed(egui::Key::F)) {
                self.viewport.fit();
            }
            if ui.input(|i| i.key_pressed(egui::Key::E)) {
                self.viewport.show_edges = !self.viewport.show_edges;
            }
            if ui.input(|i| i.key_pressed(egui::Key::S)) {
                self.viewport.toggle_section();
            }
            if ui.input(|i| i.key_pressed(egui::Key::H)) {
                if let Some(name) = self.selected_part.clone() {
                    let hidden = self.hidden.contains(&name);
                    self.set_visible(&name, hidden);
                }
            }
        }
        if self.measure.active && !typing && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if self.measure.picks.is_empty() {
                self.measure.toggle();
            } else {
                self.measure.clear();
            }
        }

        let bar = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 8.0), egui::vec2(620.0, 30.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar), |ui| {
            egui::Frame::popup(ui.style()).inner_margin(egui::Margin::symmetric(6, 3)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("Fit").on_hover_text("Fit to view (F, or double click)").clicked() {
                        self.viewport.fit();
                    }
                    if ui.selectable_label(self.measure.active, "Measure").on_hover_text("Measure (M)").clicked() {
                        self.measure.toggle();
                    }
                    if ui.selectable_label(self.viewport.show_edges, "Edges").on_hover_text("Show edges (E)").clicked() {
                        self.viewport.show_edges = !self.viewport.show_edges;
                    }
                    if ui.selectable_label(self.fit.is_some(), "Fit check").on_hover_text("Find clashes and gaps between parts").clicked() {
                        self.toggle_fit();
                    }
                    let cutting = self.viewport.section.is_some();
                    if ui.selectable_label(cutting, "Section").on_hover_text("Cut the model open (S)").clicked() {
                        self.viewport.toggle_section();
                    }
                    if !self.hidden.is_empty() {
                        let text = format!("Show all ({} hidden)", self.hidden.len());
                        if ui.button(text).clicked() {
                            self.show_all();
                        }
                    }
                });
            });
        });

        self.section_panel(ui, rect);
        self.fit_panel(ui, rect);

        if !self.measure.active {
            return;
        }
        self.measure.hover = input.hover.and_then(|p| self.viewport.pick(p));
        if input.hover.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }
        if let Some(pos) = input.clicked {
            if let Some(hit) = self.viewport.pick(pos) {
                if let Some(key) = self.shown.get(&hit.part).and_then(|p| Mesh::key_of(p)) {
                    let (a, b) = self.measure.click(hit, key);
                    match self.worker.as_mut().map(|w| w.request_measure(a, b)) {
                        Some(Ok(id)) => self.measure.sent(id),
                        Some(Err(e)) => self.worker_status = format!("Measure failed: {e:#}"),
                        None => self.worker_status = "Measuring needs the geometry worker, which isn't running.".to_owned(),
                    }
                }
            }
        }

        let painter = ui.painter_at(rect);
        let first = egui::Color32::from_rgb(240, 150, 40);
        let second = egui::Color32::from_rgb(40, 190, 230);
        if let Some(hover) = &self.measure.hover {
            self.viewport.draw_hit(&painter, hover, egui::Color32::from_white_alpha(200));
        }
        for (i, picked) in self.measure.picks.iter().enumerate() {
            self.viewport.draw_hit(&painter, &picked.hit, if i == 0 { first } else { second });
        }
        if let Some((a, b)) = self.measure.line() {
            if let (Some(sa), Some(sb)) = (self.viewport.project(a), self.viewport.project(b)) {
                painter.line_segment([sa, sb], egui::Stroke::new(2.0, egui::Color32::WHITE));
                painter.circle_filled(sa, 3.0, first);
                painter.circle_filled(sb, 3.0, second);
                if let Some(text) = self.measure.distance_text() {
                    let galley = painter.layout_no_wrap(text, egui::FontId::proportional(14.0), egui::Color32::WHITE);
                    let mid = sa + (sb - sa) * 0.5;
                    let label = egui::Rect::from_center_size(mid, galley.size() + egui::vec2(10.0, 6.0));
                    painter.rect_filled(label, 4.0, egui::Color32::from_black_alpha(190));
                    painter.galley(label.min + egui::vec2(5.0, 3.0), galley, egui::Color32::WHITE);
                }
            }
        }

        let panel = egui::Rect::from_min_size(rect.left_top() + egui::vec2(8.0, 46.0), egui::vec2(320.0, 170.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(300.0);
                self.measure.panel_ui(ui);
            });
        });
    }

    /// Draw the pictures a model has asked for with the `view_model` tool. Requests
    /// arrive as files, so an MCP server in another process can ask too.
    fn serve_views(&mut self) {
        let Some(loaded) = &self.project else { return };
        let Some(request) = intercraft_tools::view_requests(&loaded.dir).into_iter().next() else { return };
        // The picture must show the model's latest edit, so wait until it is built.
        let settled = self.building.is_empty() && self.param_drag.is_none() && loaded.latest_mtime() == self.last_mtime;
        if !settled && request.age < Duration::from_secs(10) {
            return;
        }
        match self.take_view(&request) {
            Ok(png) => intercraft_tools::answer_view(&loaded.dir, &request.id, Ok(&png)),
            Err(e) => intercraft_tools::answer_view(&loaded.dir, &request.id, Err(&format!("{e:#}"))),
        }
    }

    fn take_view(&self, request: &intercraft_tools::ViewRequest) -> anyhow::Result<Vec<u8>> {
        let direction = view_direction(&request.view);
        let section = request.section.map(|(axis, offset, flip)| viewport::Section { axis, offset: offset as f32, flip });
        // Side views are drawn flat, like a drawing. The angled ones keep perspective.
        let flat = direction.is_some() && request.view != "iso";
        let shot = viewport::Shot { direction, only: request.part.clone(), section, flat };
        let pixels = self.viewport.snapshot(&shot)?;

        encode_png(&pixels, viewport::SHOT_SIZE, viewport::SHOT_SIZE)
    }

    /// The project's design brief: shown as formatted text, editable on request.
    fn brief_ui(&mut self, ui: &mut egui::Ui) {
        let Some(loaded) = &self.project else {
            ui.weak("No project loaded.");
            return;
        };
        let saved = loaded.brief.clone();
        let mut save = None;
        match &mut self.brief_draft {
            Some(draft) => {
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = Some(draft.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        save = Some(saved.clone());
                    }
                    ui.weak("Markdown: '## ' starts a section.");
                });
                let editor = egui::TextEdit::multiline(draft).code_editor().desired_width(f32::INFINITY).desired_rows(20);
                ui.add(editor).widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Brief text"));
            }
            None => {
                ui.horizontal(|ui| {
                    if ui.button("Edit brief").on_hover_text("Change the brief yourself. The model keeps it up to date too.").clicked() {
                        self.brief_draft = Some(saved.clone());
                    }
                    ui.weak("What the project is, how it will be made, key sizes, decisions and open items.");
                });
                ui.separator();
                let blank = saved.trim().is_empty() || saved.trim() == intercraft_core::BRIEF_TEMPLATE.trim();
                if blank {
                    ui.weak("The brief is empty. Describe what you want to make in the chat and the model will fill it in with you, or press Edit brief.");
                } else {
                    egui_commonmark::CommonMarkViewer::new().show(ui, &mut self.brief_markdown, &saved);
                }
            }
        }
        if let Some(text) = save {
            if text == saved {
                self.brief_draft = None;
                return;
            }
            match self.edit_project(|p| p.set_brief(text).and_then(|()| p.save())) {
                Ok(()) => {
                    self.brief_draft = None;
                    self.worker_status = "Saved the brief. Ctrl+Z undoes it.".to_owned();
                }
                Err(e) => self.worker_status = format!("Couldn't save the brief: {e:#}"),
            }
        }
    }

    /// Send a question to the chat for the user, and say so if it can't go yet.
    pub(crate) fn ask_chat(&mut self, question: &str) {
        if !self.chat.ask(question) {
            self.worker_status = "The chat can't take a question right now. Set it up in Settings, or wait for the reply.".to_owned();
        }
    }

    /// Put a flat view of the model behind the sketch, or take it away.
    pub(crate) fn set_sketch_backdrop(&mut self, ctx: &egui::Context, view: Option<String>) {
        let Some(view) = view else {
            self.sketch.backdrop = None;
            return;
        };
        let shot = viewport::Shot { direction: view_direction(&view), only: None, section: None, flat: true };
        match (self.viewport.flat_frame(), self.viewport.snapshot(&shot)) {
            (Some((centre, half)), Ok(pixels)) => {
                self.sketch.set_backdrop(ctx, &view, &pixels, viewport::SHOT_SIZE, centre, half);
                self.worker_status = format!("Drawing over the {view} view, at true scale.");
            }
            (_, Err(e)) => self.worker_status = format!("Couldn't draw over the {view} view: {e:#}"),
            (None, _) => self.worker_status = "There is no built part to draw over yet.".to_owned(),
        }
    }

    /// The screenshot asked for by Attach to chat has arrived: cut the sketch out of
    /// it and hand it to the chat with the written summary.
    pub(crate) fn attach_sketch(&mut self, image: &egui::ColorImage, pixels_per_point: f32) {
        let sketch::Capture::Waiting(area) = self.sketch.capture else { return };
        self.sketch.capture = sketch::Capture::Idle;
        let png = sketch::crop(image, area, pixels_per_point).and_then(|(pixels, width, height)| encode_png(&pixels, width, height).ok());
        match png {
            Some(png) => {
                self.chat.attach_sketch(intercraft_tools::base64(&png), self.sketch.describe());
                self.worker_status = "The sketch is attached. Type what you want in the chat and send.".to_owned();
            }
            None => self.worker_status = "Couldn't take a picture of the sketch.".to_owned(),
        }
    }

    pub(crate) fn toggle_fit(&mut self) {
        self.fit = match self.fit {
            None => Some(fit::FitCheck::default()),
            Some(_) => None,
        };
        self.viewport.set_overlays(&[]);
    }

    /// Send the fit check once everything it needs has been built.
    fn run_fit(&mut self) {
        let Some(fit) = &mut self.fit else { return };
        if !fit.wants_run() || !self.building.is_empty() || self.param_drag.is_some() {
            return;
        }
        // What is on screen: visible parts, at the step being shown.
        let mut parts: Vec<(String, String)> = self
            .shown
            .iter()
            .filter(|(name, _)| !self.hidden.contains(*name))
            .filter_map(|(name, path)| Some((name.clone(), Mesh::key_of(path)?)))
            .collect();
        parts.sort();
        if parts.len() < 2 {
            fit.result(Err("A fit check needs at least two visible parts.".to_owned()));
            return;
        }
        match self.worker.as_mut().map(|w| w.request_fit(&parts)) {
            Some(Ok(id)) => fit.sent(id),
            Some(Err(e)) => fit.result(Err(format!("{e:#}"))),
            None => fit.result(Err("The geometry worker isn't running.".to_owned())),
        }
    }

    /// Results of the fit check, with the gap of the
    /// row under the pointer drawn in the 3D view.
    fn fit_panel(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let Some(fit) = &mut self.fit else { return };
        // Top right, under the section controls when those are showing.
        let top = if self.viewport.section.is_some() { 78.0 } else { 8.0 };
        let panel = egui::Rect::from_min_size(rect.right_top() + egui::vec2(-328.0, top), egui::vec2(320.0, 220.0));
        let mut hovered = None;
        ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(300.0);
                hovered = fit.panel_ui(ui);
            });
        });
        if let Some((a, b)) = hovered {
            if let (Some(sa), Some(sb)) = (self.viewport.project(a), self.viewport.project(b)) {
                let painter = ui.painter_at(rect);
                painter.line_segment([sa, sb], egui::Stroke::new(2.0, egui::Color32::WHITE));
                painter.circle_filled(sa, 3.5, egui::Color32::from_rgb(240, 150, 40));
                painter.circle_filled(sb, 3.5, egui::Color32::from_rgb(40, 190, 230));
            }
        }
    }

    /// Controls for the section plane, in the top right corner of the viewport.
    fn section_panel(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let (Some(mut section), Some((lo, hi))) = (self.viewport.section, self.viewport.visible_bounds()) else {
            return;
        };
        let panel = egui::Rect::from_min_size(rect.right_top() + egui::vec2(-268.0, 8.0), egui::vec2(260.0, 90.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(panel), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Cut along");
                    for (axis, name) in ["X", "Y", "Z"].into_iter().enumerate() {
                        if ui.selectable_label(section.axis == axis, name).clicked() && section.axis != axis {
                            section = self.viewport.section_across(axis).unwrap_or(section);
                        }
                    }
                    if ui.selectable_label(section.flip, "Flip").on_hover_text("Cut away the other side").clicked() {
                        section.flip = !section.flip;
                    }
                });
                // A little past each end, so the plane can clear the model completely.
                let (a, b) = (lo[section.axis], hi[section.axis]);
                let margin = ((b - a) * 0.02).max(0.1);
                ui.spacing_mut().slider_width = 170.0;
                ui.add(
                    egui::Slider::new(&mut section.offset, (a - margin)..=(b + margin))
                        .suffix(" mm")
                        .max_decimals(1),
                );
            });
        });
        self.viewport.section = Some(section);
    }

    /// Write parts to the project's exports folder, using the viewer's own worker.
    fn export(&mut self, parts: &[String], format: ExportFormat) {
        let (Some(loaded), Some(worker)) = (&self.project, &mut self.worker) else {
            self.worker_status = "Export needs the geometry worker, which isn't running.".to_owned();
            return;
        };
        let dir = loaded.dir.join("exports");
        let ext = if format == ExportFormat::Step { "step" } else { "stl" };
        if self.exports_pending == 0 {
            self.exported.clear();
        }
        for name in parts {
            let Ok(part) = loaded.part(name) else { continue };
            let request = intercraft_tools::request_for(&loaded.dir, part);
            match worker.request_export(request, format, &dir.join(format!("{}.{ext}", part.name))) {
                Ok(()) => self.exports_pending += 1,
                Err(e) => self.worker_status = format!("Export failed: {e:#}"),
            }
        }
        if self.exports_pending > 0 {
            self.worker_status = format!("Exporting {}...", plural(self.exports_pending, "file"));
        }
    }

    /// True once after the loaded project's name changes.
    fn project_renamed(&mut self) -> bool {
        let name = self.project.as_ref().map(|p| p.project.name.clone());
        if name != self.shown_name {
            self.shown_name = name;
            return true;
        }
        false
    }

    fn reload_project(&mut self) {
        match LoadedProject::load(&self.project_dir) {
            Ok(loaded) => {
                self.last_mtime = loaded.latest_mtime();
                self.project_error = None;
                self.history = loaded.history();
                self.project = Some(loaded);
                // Picks point at shapes that are about to change.
                self.measure.clear();
                // An open fit check runs again once the rebuild has finished.
                if let Some(fit) = &mut self.fit {
                    fit.invalidate();
                    self.viewport.set_overlays(&[]);
                }
                self.rebuild_all();
            }
            // Keep showing the last good project. A half-saved file shouldn't blank the view.
            Err(e) => self.project_error = Some(format!("{e:#}")),
        }
    }

    fn rebuild_all(&mut self) {
        let (Some(loaded), Some(worker)) = (&self.project, &mut self.worker) else { return };

        for entry in self.results.values_mut() {
            entry.stale = true;
        }
        for part in &loaded.project.parts {
            match worker.rebuild(intercraft_tools::request_for(&loaded.dir, part)) {
                Ok(()) => {
                    self.building.insert(part.name.clone());
                }
                Err(e) => self.worker_status = format!("Worker error: {e:#}"),
            }
        }
    }

    fn poll_files(&mut self) {
        if self.last_poll.elapsed() < POLL_INTERVAL {
            return;
        }
        self.last_poll = Instant::now();

        let changed = match &self.project {
            Some(loaded) => loaded.latest_mtime() != self.last_mtime,
            // First load failed, so keep trying until the files are fixed.
            None => true,
        };
        if changed {
            self.reload_project();
        }
    }

    fn drain_worker_events(&mut self) {
        // A step that never finishes (usually an endless loop) would freeze the view forever.
        if self.worker.as_ref().is_some_and(|w| w.stuck()) {
            let parts: HashSet<String> = std::mem::take(&mut self.building);
            for ((part, _), entry) in self.results.iter_mut() {
                if entry.stale && parts.contains(part) {
                    entry.stale = false;
                    entry.result.ok = false;
                    entry.result.error = Some(format!(
                        "Stopped after {} s without finishing. An endless loop in a snippet is the usual cause.",
                        intercraft_geo::build_timeout().as_secs()
                    ));
                }
            }
            if let Some(worker) = self.worker.as_mut() {
                self.worker_status = match worker.restart() {
                    Ok(()) => "A build step took too long and was stopped. The geometry worker restarted.".to_owned(),
                    Err(e) => format!("Worker restart failed: {e:#}"),
                };
            }
        }
        loop {
            let Some(event) = self.worker.as_mut().and_then(|w| w.try_recv()) else { break };
            match event {
                GeoEvent::Ready { build123d } => {
                    self.worker_status = format!("Geometry worker ready (build123d {build123d})");
                }
                GeoEvent::Feature(result) => {
                    let key = (result.part.clone(), result.feature_id.clone());
                    self.results.insert(key, Entry { result, stale: false });
                }
                GeoEvent::Done { part } => {
                    self.building.remove(&part);
                    // Anything still stale was removed from the project or sits after a failure.
                    self.results.retain(|(p, _), e| p != &part || !e.stale);
                }
                GeoEvent::Measured { id, data } => self.measure.result(id, Ok(data)),
                GeoEvent::Inspected { .. } => {}
                GeoEvent::Fitted { id, data } => {
                    if let Some(fit) = self.fit.as_mut().filter(|f| f.waiting_for(id)) {
                        let meshes: Vec<Mesh> = fit::overlap_meshes(&data).iter().filter_map(|p| Mesh::load(p).ok()).collect();
                        self.viewport.set_overlays(&meshes);
                        fit.result(Ok(data));
                    }
                }
                GeoEvent::Error { id: Some(id), error } if self.fit.as_ref().is_some_and(|f| f.waiting_for(id)) => {
                    if let Some(fit) = &mut self.fit {
                        fit.result(Err(error));
                    }
                }
                GeoEvent::Error { id: Some(id), error } if self.measure.waiting_for(id) => {
                    self.measure.result(id, Err(error));
                }
                GeoEvent::Error { error, .. } if self.exports_pending > 0 => {
                    self.exports_pending -= 1;
                    let first = error.lines().next().unwrap_or_default();
                    self.worker_status = format!("Export failed: {first}");
                }
                GeoEvent::Error { error, .. } => self.worker_status = format!("Worker error: {error}"),
                GeoEvent::Exported { path } => {
                    self.exports_pending = self.exports_pending.saturating_sub(1);
                    if let Some(name) = path.file_name() {
                        self.exported.push(name.to_string_lossy().into_owned());
                    }
                    if self.exports_pending == 0 {
                        self.worker_status = format!("Exported {}", self.exported.join(", "));
                        self.export_dir = path.parent().map(PathBuf::from);
                    }
                }
                GeoEvent::Exited => {
                    self.worker_status = "Geometry worker stopped".to_owned();
                    self.building.clear();
                    self.worker = None;
                }
            }
        }
    }

    /// Work out which mesh each part should show and upload any that changed.
    fn sync_meshes(&mut self) {
        let Some(loaded) = &self.project else { return };

        for (i, part) in loaded.project.parts.iter().enumerate() {
            let mut wanted = None;
            for step in steps(part) {
                let key = (part.name.clone(), step.id.to_owned());
                match self.results.get(&key) {
                    Some(Entry { result, stale: false }) if result.ok => {
                        wanted = result.mesh.clone();
                    }
                    _ => break,
                }
                if self.is_selected_step(&part.name, step.id) {
                    break;
                }
            }

            // Nothing fresh yet: leave the old mesh up instead of flickering.
            let Some(path) = wanted else { continue };
            if self.shown.get(&part.name) == Some(&path) {
                continue;
            }
            match Mesh::load(&path) {
                Ok(mesh) => {
                    self.viewport.set_mesh(&part.name, mesh, part_color(part.kind, i));
                    if self.hidden.contains(&part.name) {
                        self.viewport.set_visible(&part.name, false);
                    }
                    self.shown.insert(part.name.clone(), path);
                }
                Err(e) => self.worker_status = format!("Mesh load failed: {e:#}"),
            }
        }

        let names: HashSet<&str> = loaded.project.parts.iter().map(|p| p.name.as_str()).collect();
        self.viewport.retain_meshes(|name| names.contains(name));
        self.shown.retain(|name, _| names.contains(name.as_str()));
        self.hidden.retain(|name| names.contains(name.as_str()));
    }

    /// Keep the selection pointing at things that still exist after a reload.
    fn fix_selection(&mut self) {
        let Some(loaded) = &self.project else { return };
        let parts = &loaded.project.parts;
        let part = self
            .selected_part
            .as_ref()
            .and_then(|name| parts.iter().find(|p| &p.name == name))
            .or(parts.first());
        let still_there = |step: &String| part.is_some_and(|p| steps(p).iter().any(|s| s.id == step));
        if !self.selected_step.as_ref().is_none_or(still_there) {
            self.selected_step = None;
        }
        self.selected_part = part.map(|p| p.name.clone());
    }

    fn is_selected_step(&self, part: &str, feature_id: &str) -> bool {
        self.selected_part.as_deref() == Some(part)
            && self.selected_step.as_deref() == Some(feature_id)
    }

    fn selected_part(&self) -> Option<(usize, &Part)> {
        let name = self.selected_part.as_ref()?;
        let parts = &self.project.as_ref()?.project.parts;
        parts.iter().enumerate().find(|(_, p)| &p.name == name)
    }

    /// Overall state of a part: failed beats rebuilding beats built.
    fn part_status(&self, ui: &egui::Ui, part: &Part) -> (egui::Color32, &'static str) {
        let steps = steps(part);
        if steps.is_empty() {
            return (ui.visuals().weak_text_color(), "No steps yet");
        }
        let mut status = (egui::Color32::from_rgb(70, 170, 90), "Built");
        for step in &steps {
            match self.results.get(&(part.name.clone(), step.id.to_owned())) {
                Some(e) if !e.stale && !e.result.ok => {
                    return (ui.visuals().error_fg_color, "A step failed");
                }
                Some(e) if !e.stale => {}
                _ => status = (ui.visuals().weak_text_color(), "Rebuilding"),
            }
        }
        status
    }

    fn latest_report(&self, part: &Part) -> Option<&Report> {
        let last = steps(part).pop()?;
        let entry = self.results.get(&(part.name.clone(), last.id.to_owned()))?;
        entry.result.report.as_ref()
    }

    fn parts_ui(&mut self, ui: &mut egui::Ui) {
        let Some(loaded) = &self.project else {
            ui.label("No project loaded.");
            return;
        };
        ui.heading(&loaded.project.name);
        ui.weak(format!("Units: {}", loaded.project.units));
        ui.separator();
        if loaded.project.parts.is_empty() {
            ui.weak("No parts yet. Ask the LLM to add one.");
        }

        let mut action = None;
        for (i, part) in loaded.project.parts.iter().enumerate() {
            let selected = self.selected_part.as_ref() == Some(&part.name);
            let hidden = self.hidden.contains(&part.name);
            let fill = if selected {
                ui.visuals().selection.bg_fill.gamma_multiply(0.35)
            } else {
                egui::Color32::TRANSPARENT
            };
            // The row senses clicks before its contents are added, so the eye button
            // sits on top of it and gets its own clicks.
            let row = ui
                .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(6, 4))
                        .corner_radius(4.0)
                        .fill(fill)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                if eye_button(ui, !hidden, &part.name).clicked() {
                                    action = Some(PartAction::Visible(part.name.clone(), hidden));
                                }
                                // Same colour as in the viewport, so parts are easy to match up.
                                let [r, g, b] = part_color(part.kind, i).map(|c| (c * 255.0) as u8);
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                                let swatch = egui::Color32::from_rgb(r, g, b);
                                let swatch = if hidden { swatch.gamma_multiply(0.35) } else { swatch };
                                ui.painter().rect_filled(rect, 2.0, swatch);
                                let name = egui::RichText::new(&part.name).strong();
                                ui.label(if hidden { name.color(ui.visuals().weak_text_color()) } else { name });
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let (color, hint) = self.part_status(ui, part);
                                    paint_dot(ui, color, hint);
                                });
                            });
                            let kind = match part.kind {
                                PartKind::Design => "design",
                                PartKind::Reference => "reference",
                            };
                            let mut detail = format!("{kind} · {}", plural(steps(part).len(), "step"));
                            if let Some(r) = self.latest_report(part) {
                                detail += &format!(" · {:.1} mm³", r.volume);
                            }
                            if hidden {
                                detail += " · hidden";
                            }
                            ui.weak(detail);
                        });
                })
                .response;
            if row.clicked() {
                action = Some(PartAction::Select(part.name.clone()));
            }
            row.context_menu(|ui| {
                let name = part.name.clone();
                if ui.button(if hidden { "Show" } else { "Hide" }).clicked() {
                    action = Some(PartAction::Visible(name.clone(), hidden));
                    ui.close();
                }
                if ui.button("Show only this").clicked() {
                    action = Some(PartAction::Isolate(name.clone()));
                    ui.close();
                }
                if ui.add_enabled(!self.hidden.is_empty(), egui::Button::new("Show all")).clicked() {
                    action = Some(PartAction::ShowAll);
                    ui.close();
                }
                ui.separator();
                if ui.button("Rename...").clicked() {
                    action = Some(PartAction::Rename(name.clone()));
                    ui.close();
                }
                let ready = self.chat.can_ask();
                if ui.add_enabled(ready, egui::Button::new("Explain this part")).on_disabled_hover_text("Set up a chat provider in Settings first, or wait for the current reply").clicked() {
                    action = Some(PartAction::Explain(name.clone()));
                    ui.close();
                }
                if ui.button("Duplicate").clicked() {
                    action = Some(PartAction::Duplicate(name.clone()));
                    ui.close();
                }
                let (label, kind) = match part.kind {
                    PartKind::Design => ("Lock as reference", PartKind::Reference),
                    PartKind::Reference => ("Unlock for editing", PartKind::Design),
                };
                if ui.button(label).clicked() {
                    action = Some(PartAction::Kind(name.clone(), kind));
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new("Delete part").shortcut_text("Del")).clicked() {
                    action = Some(PartAction::Delete(name));
                    ui.close();
                }
            });
        }
        ui.add_space(6.0);
        if ui.button("Import STEP...").on_hover_text("Add a part from a STEP file").clicked() {
            action = Some(PartAction::Import);
        }
        // Delete removes the selected part while the pointer is over this list.
        let typing = ui.ctx().egui_wants_keyboard_input();
        if ui.ui_contains_pointer() && !typing && ui.input(|i| i.key_pressed(egui::Key::Delete)) {
            if let Some(name) = self.selected_part.clone() {
                action = Some(PartAction::Delete(name));
            }
        }

        match action {
            Some(PartAction::Select(name)) => {
                if self.selected_part.as_ref() != Some(&name) {
                    self.selected_part = Some(name);
                    self.selected_step = None;
                }
            }
            Some(PartAction::Visible(name, visible)) => self.set_visible(&name, visible),
            Some(PartAction::Isolate(name)) => {
                let names: Vec<String> = self.project.iter().flat_map(|p| &p.project.parts).map(|p| p.name.clone()).collect();
                for other in names {
                    let visible = other == name;
                    self.set_visible(&other, visible);
                }
            }
            Some(PartAction::ShowAll) => self.show_all(),
            Some(PartAction::Kind(name, kind)) => self.set_part_kind(&name, kind),
            Some(PartAction::Delete(name)) => self.delete_part(&name),
            Some(PartAction::Rename(name)) => self.rename = Some((RenameTarget::Part(name.clone()), name, None)),
            Some(PartAction::Duplicate(name)) => self.duplicate_part(&name),
            Some(PartAction::Explain(name)) => self.ask_chat(&explain_part(&name)),
            Some(PartAction::Import) => self.import_step_dialog(),
            None => {}
        }
        self.rename_window(ui.ctx());
    }

    fn inspector_ui(&mut self, ui: &mut egui::Ui) {
        let mut drag = self.param_drag.take();
        let mut draft = self.code_draft.take();
        let action = self.inspector_contents(ui, &mut drag, &mut draft);
        self.param_drag = drag;
        self.code_draft = draft;
        match action {
            Some(InspectorAction::Param(commit)) => self.commit_param(commit),
            Some(InspectorAction::Code { part, feature, code }) => self.apply_code(&part, &feature, code),
            Some(InspectorAction::Ask(question)) => self.ask_chat(&question),
            None => {}
        }
    }

    /// Draws the Inspector. Returns an edit to save, if there was one.
    fn inspector_contents(
        &self,
        ui: &mut egui::Ui,
        drag: &mut Option<params::ParamDrag>,
        draft: &mut Option<CodeDraft>,
    ) -> Option<InspectorAction> {
        let Some((_, part)) = self.selected_part() else {
            ui.weak("Select a part to see its steps.");
            return None;
        };
        let mut commit = None;
        // Where the part sits in the assembly. Open by itself once the part has been moved.
        // In the simple layout moving a part is tucked away until asked for.
        let (title, open) = if self.simple { ("Move part", false) } else { ("Placement", part.placement().is_some()) };
        egui::CollapsingHeader::new(title)
            .id_salt(("placement", &part.name))
            .default_open(open)
            .show(ui, |ui| {
                egui::Grid::new("placement").num_columns(2).show(ui, |ui| {
                    commit = params::placement_rows(ui, part, drag).map(InspectorAction::Param);
                });
                ui.weak("X, Y, Z in mm, then degrees about each axis.");
            });

        // Follow the latest step unless one is picked on the timeline.
        let steps = steps(part);
        let index = match &self.selected_step {
            Some(id) => steps.iter().position(|s| s.id == id),
            None => steps.len().checked_sub(1),
        };
        let Some(index) = index else {
            ui.weak("This part has no steps yet.");
            return commit;
        };
        let step = &steps[index];

        ui.weak(format!(
            "{} · step {} of {}{}",
            part.name,
            index + 1,
            steps.len(),
            if self.selected_step.is_none() { " (latest)" } else { "" }
        ));
        ui.horizontal(|ui| {
            ui.heading(&step.label);
            let ready = self.chat.can_ask();
            let hint = if ready {
                "Ask the model what this step does and why it is designed this way"
            } else {
                "Set up a chat provider in Settings first, or wait for the current reply"
            };
            if ui.add_enabled(ready, egui::Button::new("Why is it like this?").small()).on_hover_text(hint).on_disabled_hover_text(hint).clicked() {
                commit = Some(InspectorAction::Ask(explain_step(&part.name, index + 1, &step.label)));
            }
        });
        if !self.simple {
            ui.weak(step.file);
        }
        if !step.why.is_empty() {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.strong("Why:");
                ui.label(step.why);
            });
        }
        if !step.prompt.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("Asked for: {}", step.prompt)).italics().weak());
        }

        if let Some(params) = step.params.filter(|p| !p.is_empty()) {
            ui.separator();
            let editable = part.kind == PartKind::Design;
            egui::Grid::new("params").num_columns(2).show(ui, |ui| {
                for (name, value) in params {
                    let key = (part.name.as_str(), step.id);
                    if let Some(edit) = params::param_row(ui, key, name, value, editable, drag, self.simple) {
                        commit = Some(InspectorAction::Param(edit));
                    }
                }
            });
            if drag.as_ref().is_some_and(|d| d.part == part.name) {
                ui.weak("Previewing. Let go to save.");
            }
        }

        if let Some(code) = step.code.filter(|_| !self.simple) {
            let editable = part.kind == PartKind::Design;
            egui::CollapsingHeader::new("Code").id_salt("step code").show(ui, |ui| {
                if let Some(code) = code_editor(ui, draft, (&part.name, step.id), code, editable) {
                    commit = Some(InspectorAction::Code { part: part.name.clone(), feature: step.id.to_owned(), code });
                }
            });
        }

        ui.separator();
        let Some(entry) = self.results.get(&(part.name.clone(), step.id.to_owned())) else {
            ui.weak("Not built yet.");
            return commit;
        };
        let result = &entry.result;
        if let Some(error) = &result.error {
            ui.colored_label(ui.visuals().error_fg_color, "Build failed");
            ui.monospace(error);
        }
        if let Some(r) = &result.report {
            egui::Grid::new("report").num_columns(2).show(ui, |ui| {
                ui.label("Volume");
                ui.monospace(format!("{:.1} mm³", r.volume));
                ui.end_row();
                if let Some(before) = r.volume_before {
                    ui.label("Change");
                    ui.monospace(format!("{:+.1} mm³", r.volume - before));
                    ui.end_row();
                }
                ui.label("Size (X Y Z)");
                ui.monospace(format!(
                    "{} x {} x {} mm",
                    trim_mm(r.bbox_max[0] - r.bbox_min[0]),
                    trim_mm(r.bbox_max[1] - r.bbox_min[1]),
                    trim_mm(r.bbox_max[2] - r.bbox_min[2])
                ));
                ui.end_row();
                ui.label("Faces / edges");
                ui.monospace(format!("{} / {}", r.faces, r.edges));
                ui.end_row();
                ui.label("Valid solid");
                ui.monospace(if r.valid { "yes" } else { "NO" });
                ui.end_row();
                ui.label("Build time");
                ui.monospace(if result.cached {
                    "cached".to_owned()
                } else {
                    format!("{:.0} ms", result.ms)
                });
                ui.end_row();
            });
        }
        commit
    }

    /// One card per step of the selected part, oldest on the left.
    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let Some((_, part)) = self.selected_part() else {
            ui.weak("Select a part to see its steps.");
            return;
        };
        ui.horizontal(|ui| {
            ui.strong(&part.name);
            ui.weak(format!(
                "{}. Click a step to see the part as it was then.",
                plural(steps(part).len(), "step")
            ));
        });
        ui.add_space(4.0);

        let part_name = part.name.clone();
        let editable = part.kind == PartKind::Design;
        let features = part.features.len();
        let first_feature = steps(part).len() - features;
        let mut clicked = None;
        let mut delete = None;
        let mut shift = None;
        let mut rename = None;
        egui::ScrollArea::horizontal().auto_shrink([false, true]).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for (i, step) in steps(part).iter().enumerate() {
                    let entry = self.results.get(&(part.name.clone(), step.id.to_owned()));
                    let selected = self.selected_step.as_deref() == Some(step.id);
                    let response = step_card(ui, selected, |ui| {
                        ui.horizontal(|ui| {
                            let (color, hint) = status_dot(ui, entry);
                            paint_dot(ui, color, hint);
                            ui.strong(format!("{}  {}", i + 1, step.label));
                        });
                        if let Some(text) = entry.and_then(|e| volume_delta(&e.result)) {
                            ui.weak(text);
                        }
                        // The reason teaches more than the request, so it wins when there is one.
                        let shown = [step.why, step.prompt].into_iter().find(|t| !t.is_empty()).unwrap_or("(no prompt recorded)");
                        let text = egui::RichText::new(shorten(shown, 90)).italics().weak();
                        ui.add(egui::Label::new(text).wrap());
                    });
                    let hover = [step.why, step.prompt].into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n\n");
                    let response = if hover.is_empty() { response } else { response.on_hover_text(hover) };
                    if response.clicked() {
                        clicked = Some(if selected { None } else { Some(step.id.to_owned()) });
                    }
                    response.context_menu(|ui| {
                        if step.id == intercraft_geo::IMPORT_STEP_ID {
                            ui.weak("This is the imported shape.\nDelete the part to remove it.");
                            return;
                        }
                        // Position among the features, which is what can be reordered.
                        let at = i - first_feature;
                        ui.add_enabled_ui(editable, |ui| {
                            if ui.add_enabled(at > 0, egui::Button::new("Move earlier")).clicked() {
                                shift = Some((step.id.to_owned(), -1));
                                ui.close();
                            }
                            if ui.add_enabled(at + 1 < features, egui::Button::new("Move later")).clicked() {
                                shift = Some((step.id.to_owned(), 1));
                                ui.close();
                            }
                            if ui.button("Rename...").clicked() {
                                rename = Some((step.id.to_owned(), step.label.clone()));
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Delete step").clicked() {
                                delete = Some(step.id.to_owned());
                                ui.close();
                            }
                        });
                    });
                }

                let latest = step_card(ui, self.selected_step.is_none(), |ui| {
                    ui.strong("Latest");
                    ui.weak("Show the current part");
                });
                if latest.clicked() {
                    clicked = Some(None);
                }
            });
        });
        if let Some(step) = clicked {
            self.selected_step = step;
        }
        if let Some(id) = delete {
            self.delete_step(&part_name, &id);
        }
        if let Some((id, by)) = shift {
            self.move_step(&part_name, &id, by);
        }
        if let Some((feature, label)) = rename {
            let target = RenameTarget::Step { part: part_name, feature, label: label.clone() };
            self.rename = Some((target, label, None));
        }
        self.rename_window(ui.ctx());
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RenameTarget {
    Part(String),
    Step { part: String, feature: String, label: String },
}

/// An edit made in the Inspector, saved once the panel is drawn.
enum InspectorAction {
    Param(params::ParamCommit),
    Code { part: String, feature: String, code: String },
    /// A question for the chat, from an Explain button.
    Ask(String),
}

/// What the Explain buttons ask. Plain and short, so the answer suits a beginner.
fn explain_step(part: &str, number: usize, label: &str) -> String {
    format!("Explain step {number}, '{label}', of {part}: what it does and why it is designed this way. Keep it short and plain.")
}

fn explain_part(part: &str) -> String {
    format!("Explain the part '{part}': what it is for, how its steps build it up, and the main design choices. Keep it short and plain.")
}

/// A step's snippet as typed in the Inspector, until it is applied.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodeDraft {
    part: String,
    feature: String,
    /// The saved code this draft started from.
    original: String,
    pub(crate) text: String,
}

/// The code box of the selected step. Returns the code to save when Apply is pressed.
fn code_editor(
    ui: &mut egui::Ui,
    draft: &mut Option<CodeDraft>,
    (part, feature): (&str, &str),
    saved: &str,
    editable: bool,
) -> Option<String> {
    // Start again from the saved code when another step is picked, or when the file
    // changed and nothing has been typed here.
    let stale = draft.as_ref().is_none_or(|d| {
        d.part != part || d.feature != feature || (d.text == d.original && d.original != saved)
    });
    if stale {
        *draft = Some(CodeDraft { part: part.into(), feature: feature.into(), original: saved.into(), text: saved.into() });
    }
    let d = draft.as_mut()?;
    let edited = d.text != d.original;

    let rows = d.text.lines().count().clamp(4, 18);
    let editor = egui::TextEdit::multiline(&mut d.text).code_editor().desired_rows(rows).desired_width(f32::INFINITY);
    let response = ui.add_enabled(editable, editor);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, editable, "Step code"));
    if !editable {
        ui.weak("Unlock the part to change its code.");
        return None;
    }
    if d.original != saved {
        ui.colored_label(ui.visuals().warn_fg_color, "This step changed while you were editing it.");
        if ui.small_button("Load the new version").clicked() {
            *draft = None;
            return None;
        }
    }
    let mut apply = false;
    ui.horizontal(|ui| {
        let shortcut = response.has_focus() && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Enter));
        apply = ui.add_enabled(edited, egui::Button::new("Apply")).on_hover_text("Save and rebuild (Ctrl+Enter)").clicked()
            || (edited && shortcut);
        if ui.add_enabled(edited, egui::Button::new("Revert")).clicked() {
            d.text = d.original.clone();
        }
        if edited {
            ui.weak("Not applied yet");
        }
    });
    ui.weak("build123d. Reads and reassigns `part`. Parameters are in `p`.");
    apply.then(|| d.text.clone())
}

/// Camera direction of a named view, as (yaw, pitch) in radians. None for the user's own view.
fn view_direction(view: &str) -> Option<(f32, f32)> {
    let degrees = |yaw: f32, pitch: f32| Some((yaw.to_radians(), pitch.to_radians()));
    match view {
        "iso" => degrees(-50.0, 28.0),
        "front" => degrees(-90.0, 0.0),
        "back" => degrees(90.0, 0.0),
        "right" => degrees(0.0, 0.0),
        "left" => degrees(180.0, 0.0),
        // Just off straight down, where the camera's "up" would be undefined.
        "top" => degrees(-90.0, 89.9),
        "bottom" => degrees(-90.0, -89.9),
        _ => None,
    }
}

fn encode_png(pixels: &[u8], width: u32, height: u32) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(pixels)?;
    writer.finish()?;
    Ok(bytes)
}

/// What a click in the parts list asked for, applied once the list is drawn.
enum PartAction {
    Select(String),
    Visible(String, bool),
    Isolate(String),
    ShowAll,
    Kind(String, PartKind),
    Delete(String),
    Rename(String),
    Duplicate(String),
    Explain(String),
    Import,
}

/// An eye, crossed out when the part is hidden. Drawn by hand so it needs no icon font.
fn eye_button(ui: &mut egui::Ui, visible: bool, part: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 16.0), egui::Sense::click());
    let visuals = ui.visuals();
    let color = if response.hovered() {
        visuals.strong_text_color()
    } else if visible {
        visuals.text_color()
    } else {
        visuals.weak_text_color()
    };
    let stroke = egui::Stroke::new(1.3, color);
    let c = rect.center();
    let (w, h) = (7.0_f32, 4.0_f32);
    let lid = |x: f32| h * (1.0 - (x / w).powi(2));
    let mut points: Vec<egui::Pos2> = (0..=16).map(|i| -w + 2.0 * w * i as f32 / 16.0).map(|x| c + egui::vec2(x, -lid(x))).collect();
    points.extend((1..16).map(|i| w - 2.0 * w * i as f32 / 16.0).map(|x| c + egui::vec2(x, lid(x))));
    ui.painter().add(egui::Shape::closed_line(points, stroke));
    if visible {
        ui.painter().circle_filled(c, 2.2, color);
    } else {
        ui.painter().line_segment([c + egui::vec2(-w, h + 1.5), c + egui::vec2(w, -h - 1.5)], stroke);
    }
    let action = if visible { "Hide" } else { "Show" };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{action} {part}")));
    response.on_hover_text(format!("{action} (H)"))
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// One step of a part as the panels show it: the imported file, or a feature.
struct Step<'a> {
    id: &'a str,
    label: String,
    prompt: &'a str,
    /// The design reason, if the model gave one.
    why: &'a str,
    file: &'a str,
    params: Option<&'a intercraft_core::Params>,
    /// The snippet. An imported shape has none.
    code: Option<&'a str>,
}

fn steps(part: &Part) -> Vec<Step<'_>> {
    let mut list = Vec::with_capacity(part.features.len() + 1);
    if let Some(source) = &part.source {
        let name = source.rsplit('/').next().unwrap_or(source);
        list.push(Step {
            id: intercraft_geo::IMPORT_STEP_ID,
            label: format!("Imported {name}"),
            prompt: "Starting shape from an imported STEP file.",
            why: "",
            file: source,
            params: None,
            code: None,
        });
    }
    list.extend(part.features.iter().map(|f| Step {
        id: &f.id,
        label: f.label.clone(),
        prompt: &f.prompt,
        why: &f.why,
        file: &f.file,
        params: Some(&f.params),
        code: Some(&f.code),
    }));
    list
}

fn step_card(ui: &mut egui::Ui, selected: bool, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let visuals = ui.visuals();
    let stroke = if selected {
        egui::Stroke::new(1.5, visuals.selection.stroke.color)
    } else {
        visuals.widgets.noninteractive.bg_stroke
    };
    egui::Frame::new()
        .fill(visuals.faint_bg_color)
        .stroke(stroke)
        .corner_radius(6.0)
        .inner_margin(8)
        .show(ui, |ui| {
            ui.set_width(150.0);
            ui.set_min_height(70.0);
            // The timeline row is horizontal, so stack the card's own contents.
            ui.vertical(add);
        })
        .response
        .interact(egui::Sense::click())
}

fn shorten(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}...", text[..cut].trim_end()),
        None => text.to_owned(),
    }
}

/// 8.2 stays 8.2, 60.00 becomes 60. Keeps sizes short in narrow panels.
fn trim_mm(value: f64) -> String {
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 { format!("1 {word}") } else { format!("{n} {word}s") }
}

fn part_color(kind: PartKind, index: usize) -> [f32; 3] {
    match kind {
        PartKind::Reference => REFERENCE_COLOR,
        PartKind::Design => PART_COLORS[index % PART_COLORS.len()],
    }
}

fn paint_dot(ui: &mut egui::Ui, color: egui::Color32, hint: &str) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
    response.on_hover_text(hint);
}

fn status_dot(ui: &egui::Ui, entry: Option<&Entry>) -> (egui::Color32, &'static str) {
    match entry {
        Some(e) if e.stale => (ui.visuals().weak_text_color(), "Rebuilding"),
        Some(e) if e.result.ok => (egui::Color32::from_rgb(70, 170, 90), "Built"),
        Some(_) => (ui.visuals().error_fg_color, "Failed"),
        None => (ui.visuals().weak_text_color(), "Not built"),
    }
}

/// Volume change caused by one feature, e.g. "-813.3 mm³".
fn volume_delta(result: &FeatureResult) -> Option<String> {
    let report = result.report.as_ref()?;
    Some(match report.volume_before {
        Some(before) => format!("{:+.1} mm³", report.volume - before),
        None => format!("{:.1} mm³", report.volume),
    })
}

impl TabViewer for State {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        format!("{tab:?}").into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Viewport => {
                let input = self.viewport.ui(ui);
                self.viewport_overlay(ui, input);
            }
            Tab::Parts => self.parts_ui(ui),
            Tab::Inspector => self.inspector_ui(ui),
            Tab::Brief => self.brief_ui(ui),
            Tab::Timeline => self.timeline_ui(ui),
            Tab::Chat => {
                let context = self
                    .selected_part
                    .as_ref()
                    .map(|part| format!("Viewer note: the user has part '{part}' selected"));
                self.chat.ui(ui, context);
            }
            Tab::Sketch => {
                let can_attach = !self.chat.busy();
                if let Some(sketch::SketchAction::Backdrop(view)) = self.sketch.ui(ui, can_attach) {
                    let ctx = ui.ctx().clone();
                    self.set_sketch_backdrop(&ctx, view);
                }
            }
        }
    }

    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }

    // The viewport draws its own background, edge to edge.
    // Every tab, the viewport included, gets the panel background, so the dock's
    // margin around the 3D view matches the rest of the window.
    fn clear_background(&self, _tab: &Tab) -> bool {
        true
    }

    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        // These two manage their own scrolling.
        if matches!(tab, Tab::Viewport | Tab::Chat | Tab::Sketch) { [false, false] } else { [true, true] }
    }
}
