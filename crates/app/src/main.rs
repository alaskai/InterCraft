// InterCraft: watch an LLM (or you) edit a parametric model, one feature at a time.
// eframe/egui 0.36.2 (wgpu 30), egui_dock 0.21.1, glam 0.34, bytemuck 1
// Worker: Python 3.11, build123d 0.13.0, numpy 2.4.6

mod viewport;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui;
use egui_dock::{DockArea, DockState, NodeIndex, TabViewer};
use intercraft_core::{LoadedProject, PartKind};
use intercraft_geo::{
    FeatureInput, FeatureResult, GeoEvent, GeometryBackend, Mesh, PythonWorker, RebuildRequest,
};
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
    let project_dir = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/bracket"))
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
    Hierarchy,
    Inspector,
    Timeline,
    Chat,
    Sketch,
}

struct App {
    dock: DockState<Tab>,
    state: State,
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
    /// (part, feature id) being inspected. None follows the latest step.
    selected: Option<(String, String)>,
    /// Mesh file currently on screen for each part.
    shown: HashMap<String, PathBuf>,
    hidden: HashSet<String>,

    viewport: Viewport,
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

        // Viewport in the middle, hierarchy on the left, chat on the right, timeline below.
        let mut dock = DockState::new(vec![Tab::Viewport]);
        let tree = dock.main_surface_mut();
        let [centre, _right] = tree.split_right(NodeIndex::root(), 0.76, vec![Tab::Chat, Tab::Sketch]);
        let [centre, left] = tree.split_left(centre, 0.24, vec![Tab::Hierarchy]);
        tree.split_below(left, 0.5, vec![Tab::Inspector]);
        tree.split_below(centre, 0.82, vec![Tab::Timeline]);

        let mut state = State {
            project_dir,
            project: None,
            project_error: None,
            last_mtime: None,
            last_poll: Instant::now(),
            worker,
            worker_status,
            building: HashSet::new(),
            results: HashMap::new(),
            selected: None,
            shown: HashMap::new(),
            hidden: HashSet::new(),
            viewport: Viewport::new(render_state),
        };
        state.reload_project();
        Ok(Self { dock, state })
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.state.drain_worker_events();
        self.state.poll_files();
        self.state.sync_meshes();
        // Keeps the file polling ticking while nothing else is happening.
        ctx.request_repaint_after(POLL_INTERVAL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if !self.state.building.is_empty() {
                    ui.spinner();
                }
                ui.label(&self.state.worker_status);
                if let Some(err) = &self.state.project_error {
                    ui.colored_label(ui.visuals().error_fg_color, err);
                }
            });
        });
        DockArea::new(&mut self.dock)
            .show_close_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_leaf_collapse_buttons(false)
            .show_inside(ui, &mut self.state);
    }
}

impl State {
    fn reload_project(&mut self) {
        match LoadedProject::load(&self.project_dir) {
            Ok(loaded) => {
                self.last_mtime = loaded.latest_mtime();
                self.project_error = None;
                self.project = Some(loaded);
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
            let features = part
                .features
                .iter()
                .map(|f| FeatureInput {
                    id: f.id.clone(),
                    code: f.code.clone(),
                    params: f.params.clone(),
                })
                .collect();
            match worker.rebuild(RebuildRequest { part: part.name.clone(), features }) {
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
                GeoEvent::Error { error } => self.worker_status = format!("Worker error: {error}"),
                // Exports are only requested by the MCP server, never by the app.
                GeoEvent::Exported { .. } => {}
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
            for feature in &part.features {
                let key = (part.name.clone(), feature.id.clone());
                match self.results.get(&key) {
                    Some(Entry { result, stale: false }) if result.ok => {
                        wanted = result.mesh.clone();
                    }
                    _ => break,
                }
                if self.selected.as_ref() == Some(&key) {
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
                    let color = match part.kind {
                        PartKind::Reference => REFERENCE_COLOR,
                        PartKind::Design => PART_COLORS[i % PART_COLORS.len()],
                    };
                    self.viewport.set_mesh(&part.name, &mesh, color);
                    self.shown.insert(part.name.clone(), path);
                }
                Err(e) => self.worker_status = format!("Mesh load failed: {e:#}"),
            }
        }

        let names: HashSet<&str> = loaded.project.parts.iter().map(|p| p.name.as_str()).collect();
        self.viewport.retain_meshes(|name| names.contains(name));
        self.shown.retain(|name, _| names.contains(name.as_str()));
    }

    fn hierarchy_ui(&mut self, ui: &mut egui::Ui) {
        let Some(loaded) = &self.project else {
            ui.label("No project loaded.");
            return;
        };
        ui.heading(&loaded.project.name);
        ui.weak(format!("Units: {}", loaded.project.units));
        ui.separator();

        let mut clicked = None;
        for part in &loaded.project.parts {
            ui.horizontal(|ui| {
                let mut visible = !self.hidden.contains(&part.name);
                if ui.checkbox(&mut visible, "").on_hover_text("Show or hide").changed() {
                    if visible {
                        self.hidden.remove(&part.name);
                    } else {
                        self.hidden.insert(part.name.clone());
                    }
                    self.viewport.set_visible(&part.name, visible);
                }
                ui.strong(&part.name);
                ui.weak(match part.kind {
                    PartKind::Design => "design",
                    PartKind::Reference => "reference",
                });
            });

            ui.indent(&part.name, |ui| {
                for feature in &part.features {
                    let key = (part.name.clone(), feature.id.clone());
                    let entry = self.results.get(&key);
                    ui.horizontal(|ui| {
                        let (color, hint) = status_dot(ui, entry);
                        paint_dot(ui, color, hint);
                        let selected = self.selected.as_ref() == Some(&key);
                        if ui.selectable_label(selected, &feature.label).clicked() {
                            clicked = Some(if selected { None } else { Some(key.clone()) });
                        }
                        if let Some(text) = entry.and_then(|e| volume_delta(&e.result)) {
                            ui.weak(text);
                        }
                    });
                }
            });
            ui.add_space(6.0);
        }
        if let Some(selection) = clicked {
            self.selected = selection;
        }
    }

    fn inspector_ui(&mut self, ui: &mut egui::Ui) {
        let Some((part_name, feature_id)) = &self.selected else {
            ui.weak("Select a feature to see its prompt, parameters and numbers.");
            return;
        };
        let feature = self.project.as_ref().and_then(|l| {
            let part = l.project.parts.iter().find(|p| &p.name == part_name)?;
            part.features.iter().find(|f| &f.id == feature_id)
        });
        let Some(feature) = feature else {
            ui.weak("That feature no longer exists.");
            return;
        };

        ui.heading(&feature.label);
        ui.weak(format!("{part_name} / {}", feature.file));
        if !feature.prompt.is_empty() {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(&feature.prompt).italics());
        }

        if !feature.params.is_empty() {
            ui.separator();
            egui::Grid::new("params").num_columns(2).show(ui, |ui| {
                for (name, value) in &feature.params {
                    ui.label(name);
                    ui.monospace(value.to_string());
                    ui.end_row();
                }
            });
        }

        ui.separator();
        let Some(entry) = self.results.get(&(part_name.clone(), feature_id.clone())) else {
            ui.weak("Not built yet.");
            return;
        };
        let result = &entry.result;
        if let Some(error) = &result.error {
            ui.colored_label(ui.visuals().error_fg_color, "Build failed");
            ui.monospace(error);
        }
        if let Some(r) = &result.report {
            egui::Grid::new("report").num_columns(2).show(ui, |ui| {
                ui.label("Volume");
                ui.monospace(match r.volume_before {
                    Some(before) => format!("{before:.1} -> {:.1} mm³", r.volume),
                    None => format!("{:.1} mm³", r.volume),
                });
                ui.end_row();
                ui.label("Size (X Y Z)");
                ui.monospace(format!(
                    "{:.2} x {:.2} x {:.2} mm",
                    r.bbox_max[0] - r.bbox_min[0],
                    r.bbox_max[1] - r.bbox_min[1],
                    r.bbox_max[2] - r.bbox_min[2]
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
    }

    fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        let Some(loaded) = &self.project else { return };
        let mut clicked = None;
        for part in &loaded.project.parts {
            ui.horizontal(|ui| {
                ui.strong(&part.name);
                for (i, feature) in part.features.iter().enumerate() {
                    let key = (part.name.clone(), feature.id.clone());
                    let (dot, _) = status_dot(ui, self.results.get(&key));
                    let text = egui::RichText::new(format!("{}  {}", i + 1, feature.label));
                    let selected = self.selected.as_ref() == Some(&key);
                    let chip = ui
                        .selectable_label(selected, text.color(dot))
                        .on_hover_text(if feature.prompt.is_empty() {
                            "(no prompt recorded)"
                        } else {
                            &feature.prompt
                        });
                    if chip.clicked() {
                        clicked = Some(Some(key));
                    }
                }
                if ui.selectable_label(self.selected.is_none(), "Latest").clicked() {
                    clicked = Some(None);
                }
            });
        }
        if let Some(selection) = clicked {
            self.selected = selection;
        }
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
            Tab::Viewport => self.viewport.ui(ui),
            Tab::Hierarchy => self.hierarchy_ui(ui),
            Tab::Inspector => self.inspector_ui(ui),
            Tab::Timeline => self.timeline_ui(ui),
            Tab::Chat => {
                ui.weak("Chat with the LLM goes here (milestone 3).");
            }
            Tab::Sketch => {
                ui.weak("Quick sketch canvas goes here (milestone 4).");
            }
        }
    }

    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }

    // The viewport draws its own background, edge to edge.
    fn clear_background(&self, tab: &Tab) -> bool {
        *tab != Tab::Viewport
    }

    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        if *tab == Tab::Viewport { [false, false] } else { [true, true] }
    }
}
