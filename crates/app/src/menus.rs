// Menu bar (File, Edit, Export, View, Settings), the Settings window and the project dialogs.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui::containers::menu::{MenuButton, MenuConfig};
use egui_dock::{DockState, NodeIndex};
use intercraft_geo::ExportFormat;

use crate::settings::{Explain, Layout, Settings};
use crate::{App, Tab};

/// Where new projects go unless the user picks somewhere else.
pub fn default_projects_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("INTERCRAFT_PROJECTS_DIR") {
        return PathBuf::from(dir); // tests keep new projects out of Documents
    }
    dirs::document_dir().unwrap_or_else(std::env::temp_dir).join("InterCraft Projects")
}

/// Viewport in the middle, parts and inspector on the left, chat on the right, timeline below.
/// The simple layout gives the 3D view and chat more room and the timeline less.
pub fn default_dock(simple: bool) -> DockState<Tab> {
    // The sketch canvas shares the middle with the 3D view, as a second tab.
    let mut dock = DockState::new(vec![Tab::Viewport, Tab::Sketch]);
    let tree = dock.main_surface_mut();
    let [centre, _right] = tree.split_right(NodeIndex::root(), 0.76, vec![Tab::Chat]);
    let [centre, left] = tree.split_left(centre, 0.22, vec![Tab::Parts]);
    tree.split_below(left, 0.5, vec![Tab::Inspector, Tab::Brief]);
    tree.split_below(centre, if simple { 0.86 } else { 0.80 }, vec![Tab::Timeline]);
    dock
}

/// Open a folder in the system file browser.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    let program = "explorer";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";
    let _ = std::process::Command::new(program).arg(path).spawn();
}

pub struct NewProjectDialog {
    name: String,
    location: PathBuf,
    error: Option<String>,
}

impl NewProjectDialog {
    pub fn new() -> Self {
        Self { name: String::new(), location: default_projects_dir(), error: None }
    }

    #[cfg(test)]
    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_owned();
    }
}

/// Project names become folder names, so keep them plain.
fn check_project_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the project a name.".into());
    }
    if name.chars().count() > 60 {
        return Err("Keep the name under 60 characters.".into());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_')) {
        return Err("Use letters, numbers, spaces, '-' and '_' only.".into());
    }
    Ok(())
}

impl App {
    pub(crate) fn shortcuts(&mut self, ctx: &egui::Context) {
        let pressed = |key| {
            ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, key)))
        };
        if pressed(egui::Key::N) {
            self.new_project = Some(NewProjectDialog::new());
        }
        if pressed(egui::Key::O) {
            self.open_project_dialog();
        }
        if pressed(egui::Key::Comma) {
            self.settings_open = true;
        }
        // A focused text box has its own undo, so leave Ctrl+Z to it.
        if !ctx.egui_wants_keyboard_input() {
            let shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;
            // Ctrl+Shift+Z first, because Ctrl+Z also matches with Shift held.
            let redo = ctx.input_mut(|i| i.consume_shortcut(&egui::KeyboardShortcut::new(shift, egui::Key::Z)));
            if redo || pressed(egui::Key::Y) {
                self.state.redo();
            } else if pressed(egui::Key::Z) {
                self.state.undo();
            }
        }
    }

    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| self.file_menu(ui));
            ui.menu_button("Edit", |ui| self.edit_menu(ui));
            ui.menu_button("Export", |ui| self.export_menu(ui));
            // Stays open while ticking panels on and off.
            MenuButton::new("View")
                .config(MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, |ui| self.view_menu(ui));
            if ui.button("Settings").clicked() {
                self.settings_open = true;
            }
        });
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        if ui.add(egui::Button::new("New project...").shortcut_text("Ctrl+N")).clicked() {
            self.new_project = Some(NewProjectDialog::new());
            ui.close();
        }
        if ui.add(egui::Button::new("Open project...").shortcut_text("Ctrl+O")).clicked() {
            ui.close();
            self.open_project_dialog();
        }
        if ui.button("Import STEP...").on_hover_text("Add a part from a STEP file").clicked() {
            ui.close();
            self.state.import_step_dialog();
        }
        ui.menu_button("Open recent", |ui| {
            let recent = Settings::load().recent_projects;
            let current = std::path::absolute(&self.state.project_dir).unwrap_or_default();
            let mut shown = 0;
            for path in recent.iter().map(PathBuf::from) {
                if path == current || !path.join("project.json").exists() {
                    continue;
                }
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if ui.button(name).on_hover_text(path.display().to_string()).clicked() {
                    ui.close();
                    self.open(path);
                    return;
                }
                shown += 1;
            }
            if shown == 0 {
                ui.weak("No other recent projects");
            }
        });
        ui.separator();
        if ui.button("Show project folder").clicked() {
            reveal(&self.state.project_dir);
            ui.close();
        }
        ui.separator();
        if ui.button("Quit").clicked() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn edit_menu(&mut self, ui: &mut egui::Ui) {
        let history = &self.state.history;
        let undo = history.undo_label().map(|l| format!("Undo: {}", crate::shorten(l, 40)));
        let redo = history.redo_label().map(|l| format!("Redo: {}", crate::shorten(l, 40)));
        let button = |text: &Option<String>, plain: &str, keys: &str| {
            egui::Button::new(text.clone().unwrap_or_else(|| plain.to_owned())).shortcut_text(keys)
        };
        if ui.add_enabled(undo.is_some(), button(&undo, "Undo", "Ctrl+Z")).clicked() {
            ui.close();
            self.state.undo();
        }
        if ui.add_enabled(redo.is_some(), button(&redo, "Redo", "Ctrl+Y")).clicked() {
            ui.close();
            self.state.redo();
        }
        ui.separator();
        ui.menu_button("History", |ui| {
            let history = &self.state.history;
            if history.entries.is_empty() {
                ui.weak("No changes yet");
                return;
            }
            let head = if history.outside_changes { None } else { history.head };
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut jump = None;
            if history.outside_changes {
                ui.weak("Unrecorded edits from outside InterCraft");
            }
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                // Newest first, like the undo order.
                for entry in history.entries.iter().rev().take(60) {
                    let undone = head.is_some_and(|h| entry.number > h);
                    let text = format!("{}   {}", crate::shorten(&entry.label, 48), ago(now, entry.time));
                    let text = if undone { egui::RichText::new(text).weak() } else { egui::RichText::new(text) };
                    let hint = if undone { "Undone. Click to redo up to here." } else { "Go back to this point" };
                    if ui.selectable_label(head == Some(entry.number), text).on_hover_text(hint).clicked() {
                        jump = Some(entry.number);
                    }
                }
            });
            if let Some(number) = jump {
                ui.close();
                self.state.go_to(number);
            }
        });
    }

    fn export_menu(&mut self, ui: &mut egui::Ui) {
        let selected = self.state.selected_part.clone();
        let all: Vec<String> = self
            .state
            .project
            .as_ref()
            .map(|p| p.project.parts.iter().map(|p| p.name.clone()).collect())
            .unwrap_or_default();
        ui.add_enabled_ui(selected.is_some(), |ui| {
            let label = selected.as_deref().unwrap_or("selected part");
            ui.label(egui::RichText::new(label).weak());
            for (format, text) in [(ExportFormat::Step, "STEP (exact, editable)"), (ExportFormat::Stl, "STL (for printing)")] {
                if ui.button(text).clicked() {
                    self.state.export(&[selected.clone().unwrap_or_default()], format);
                    ui.close();
                }
            }
        });
        ui.separator();
        ui.add_enabled_ui(!all.is_empty(), |ui| {
            ui.label(egui::RichText::new("All parts").weak());
            for (format, text) in [(ExportFormat::Step, "All as STEP"), (ExportFormat::Stl, "All as STL")] {
                if ui.button(text).clicked() {
                    self.state.export(&all, format);
                    ui.close();
                }
            }
        });
        ui.separator();
        if ui.button("Open exports folder").clicked() {
            let dir = self.state.project_dir.join("exports");
            let _ = std::fs::create_dir_all(&dir);
            reveal(&dir);
            ui.close();
        }
    }

    fn view_menu(&mut self, ui: &mut egui::Ui) {
        if ui.add(egui::Button::new("Fit to view").shortcut_text("Double click")).clicked() {
            self.state.viewport.fit();
            ui.close();
        }
        ui.separator();
        for (tab, name) in [
            (Tab::Parts, "Parts"),
            (Tab::Inspector, "Inspector"),
            (Tab::Brief, "Brief"),
            (Tab::Timeline, "Timeline"),
            (Tab::Chat, "Chat"),
            (Tab::Sketch, "Sketch"),
        ] {
            let mut shown = self.dock.find_tab(&tab).is_some();
            if ui.checkbox(&mut shown, name).changed() {
                match self.dock.find_tab(&tab) {
                    Some(path) if !shown => {
                        self.dock.remove_tab(path);
                    }
                    None if shown => self.dock.push_to_first_leaf(tab),
                    _ => {}
                }
            }
        }
        let mut simple = self.state.simple;
        if ui.checkbox(&mut simple, "Simple layout").on_hover_text("Hide code, file paths and raw parameter names").changed() {
            self.set_experience(self.state.chat.explain(), simple);
        }
        if ui.button("Reset layout").clicked() {
            self.dock = default_dock(self.state.simple);
            ui.close();
        }
        ui.separator();
        self.theme_choice(ui);
    }

    fn theme_choice(&mut self, ui: &mut egui::Ui) {
        let mut theme = Settings::load().theme;
        let before = theme.clone();
        ui.horizontal(|ui| {
            ui.label("Theme");
            ui.selectable_value(&mut theme, "system".to_owned(), "System");
            ui.selectable_value(&mut theme, "dark".to_owned(), "Dark");
            ui.selectable_value(&mut theme, "light".to_owned(), "Light");
        });
        if theme != before {
            if let Ok(saved) = Settings::update(|s| s.theme = theme) {
                ui.ctx().set_theme(saved.theme_preference());
            }
        }
    }

    /// Explanation level and layout, in Settings.
    fn experience_choice(&mut self, ui: &mut egui::Ui) {
        let (mut explain, mut simple) = (self.state.chat.explain(), self.state.simple);
        egui::Grid::new("experience").num_columns(2).show(ui, |ui| {
            ui.label("Explanations");
            ui.horizontal(|ui| {
                for level in Explain::ALL {
                    ui.selectable_value(&mut explain, level, level.name()).on_hover_text(level.hint());
                }
            });
            ui.end_row();
            ui.label("Layout");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut simple, true, "Simple").on_hover_text("Chat and the 3D view first. Code, file paths and raw parameter names hidden.");
                ui.selectable_value(&mut simple, false, "Advanced").on_hover_text("Everything: code, timeline detail and placement");
            });
            ui.end_row();
        });
        if (explain, simple) != (self.state.chat.explain(), self.state.simple) {
            self.set_experience(explain, simple);
        }
    }

    /// Apply and save an explanation level and layout. A layout change resets the panels.
    pub(crate) fn set_experience(&mut self, explain: Explain, simple: bool) {
        let layout = if simple { Layout::Simple } else { Layout::Advanced };
        let _ = Settings::update(|s| {
            s.explain = explain;
            s.layout = layout;
            s.onboarded = true;
        });
        self.state.chat.set_explain(explain);
        if simple != self.state.simple {
            self.state.simple = simple;
            self.dock = default_dock(simple);
        }
    }

    /// The one-time question on first launch.
    pub(crate) fn first_run_window(&mut self, ctx: &egui::Context) {
        if !self.first_run {
            return;
        }
        let mut choice = None;
        egui::Window::new("Welcome to InterCraft")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Describe what you want to make and watch it being designed. How much should InterCraft explain?");
                ui.add_space(8.0);
                if ui.add_sized([360.0, 34.0], egui::Button::new("I'm new to CAD")).clicked() {
                    choice = Some((Explain::Beginner, true));
                }
                ui.weak("Plain-language explanations of every step, and a simpler layout.");
                ui.add_space(6.0);
                if ui.add_sized([360.0, 34.0], egui::Button::new("I know CAD")).clicked() {
                    choice = Some((Explain::Expert, false));
                }
                ui.weak("Terse answers, with code, parameters and the full timeline on show.");
                ui.add_space(6.0);
                ui.weak("You can change this any time in Settings.");
            });
        if let Some((explain, simple)) = choice {
            self.first_run = false;
            self.set_experience(explain, simple);
        }
    }

    pub(crate) fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(440.0)
            .show(ctx, |ui| {
                ui.heading("Chat");
                self.state.chat.settings_contents(ui);
                ui.add_space(8.0);
                ui.separator();
                ui.heading("Experience");
                self.experience_choice(ui);
                ui.add_space(8.0);
                ui.separator();
                ui.heading("Appearance");
                self.theme_choice(ui);
                ui.add_space(8.0);
                ui.separator();
                ui.heading("About");
                ui.weak(format!("InterCraft {}", env!("CARGO_PKG_VERSION")));
                ui.weak(&self.state.worker_status);
                ui.horizontal(|ui| {
                    ui.weak("Settings file:");
                    if ui.small_button("Show").clicked() {
                        if let Some(dir) = intercraft_tools::config_dir() {
                            reveal(&dir);
                        }
                    }
                });
            });
        self.settings_open = open;
    }

    pub(crate) fn new_project_window(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.new_project else { return };
        let mut open = true;
        let mut create = None;
        egui::Window::new("New project")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("Name");
                let field = ui.add(egui::TextEdit::singleline(&mut dialog.name).hint_text("Corner bracket"));
                if dialog.name.is_empty() {
                    field.request_focus();
                }
                ui.add_space(6.0);
                ui.label("Location");
                ui.horizontal(|ui| {
                    ui.weak(dialog.location.display().to_string());
                    if ui.small_button("Change...").clicked() {
                        if let Some(dir) = rfd::FileDialog::new().set_directory(&dialog.location).pick_folder() {
                            dialog.location = dir;
                        }
                    }
                });
                if let Some(error) = &dialog.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    if ui.button("Create").clicked() || enter {
                        match check_project_name(&dialog.name) {
                            Ok(()) => create = Some(dialog.location.join(dialog.name.trim())),
                            Err(e) => dialog.error = Some(e),
                        }
                    }
                });
            });
        if let Some(dir) = create {
            if dir.join("project.json").exists() {
                if let Some(d) = &mut self.new_project {
                    d.error = Some("A project with that name already exists there. Use File > Open instead.".into());
                }
                return;
            }
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match intercraft_core::LoadedProject::create(&dir, &name) {
                Ok(_) => {
                    self.new_project = None;
                    self.open(dir);
                }
                Err(e) => {
                    if let Some(d) = &mut self.new_project {
                        d.error = Some(format!("Couldn't create it: {e:#}"));
                    }
                }
            }
            return;
        }
        if !open {
            self.new_project = None;
        }
    }

    fn open_project_dialog(&mut self) {
        let start = self.state.project_dir.parent().map(Path::to_path_buf).unwrap_or_else(default_projects_dir);
        let Some(dir) = rfd::FileDialog::new().set_title("Open an InterCraft project folder").set_directory(start).pick_folder()
        else {
            return;
        };
        if !dir.join("project.json").exists() {
            self.state.worker_status = format!("{} isn't an InterCraft project (it has no project.json).", dir.display());
            return;
        }
        self.open(dir);
    }
}

/// "just now", "5 min ago", "3 h ago", "2 d ago".
fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "just now".to_owned(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ages_read_simply() {
        assert_eq!(ago(100, 90), "just now");
        assert_eq!(ago(1000, 400), "10 min ago");
        assert_eq!(ago(10_000, 0), "2 h ago");
        assert_eq!(ago(200_000, 0), "2 d ago");
        assert_eq!(ago(0, 50), "just now");
    }

    #[test]
    fn project_names_are_folder_safe() {
        assert!(check_project_name("Corner bracket 2").is_ok());
        assert!(check_project_name("Hülse_v2").is_ok());
        for bad in ["", "   ", "../up", "a/b", "C:", "x".repeat(61).as_str(), "name*"] {
            assert!(check_project_name(bad).is_err(), "accepted {bad:?}");
        }
    }
}
