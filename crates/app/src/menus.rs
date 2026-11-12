// Menu bar (File, Export, View, Settings), the Settings window and the project dialogs.

use std::path::{Path, PathBuf};

use eframe::egui;
use egui_dock::{DockState, NodeIndex};
use intercraft_geo::ExportFormat;

use crate::settings::Settings;
use crate::{App, Tab};

/// Where new projects go unless the user picks somewhere else.
pub fn default_projects_dir() -> PathBuf {
    dirs::document_dir().unwrap_or_else(std::env::temp_dir).join("InterCraft Projects")
}

/// Viewport in the middle, parts and inspector on the left, chat on the right, timeline below.
pub fn default_dock() -> DockState<Tab> {
    let mut dock = DockState::new(vec![Tab::Viewport]);
    let tree = dock.main_surface_mut();
    let [centre, _right] = tree.split_right(NodeIndex::root(), 0.76, vec![Tab::Chat, Tab::Sketch]);
    let [centre, left] = tree.split_left(centre, 0.22, vec![Tab::Parts]);
    tree.split_below(left, 0.5, vec![Tab::Inspector]);
    tree.split_below(centre, 0.80, vec![Tab::Timeline]);
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
    }

    pub(crate) fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| self.file_menu(ui));
            ui.menu_button("Export", |ui| self.export_menu(ui));
            ui.menu_button("View", |ui| self.view_menu(ui));
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
        if ui.button("Reset layout").clicked() {
            self.dock = default_dock();
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
                        if let Some(dir) = dirs::config_dir() {
                            reveal(&dir.join("InterCraft"));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_folder_safe() {
        assert!(check_project_name("Corner bracket 2").is_ok());
        assert!(check_project_name("Hülse_v2").is_ok());
        for bad in ["", "   ", "../up", "a/b", "C:", "x".repeat(61).as_str(), "name*"] {
            assert!(check_project_name(bad).is_err(), "accepted {bad:?}");
        }
    }
}
