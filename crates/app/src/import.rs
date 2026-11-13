// Importing a STEP file: read it once to see how many bodies it holds, and if there
// are several, ask whether to keep it as one part or split it into a group of parts.

use std::path::PathBuf;

use eframe::egui;
use intercraft_core::{SplitImport, MAX_SPLIT_BODIES};

pub enum Stage {
    /// The worker is reading the file (request id).
    Reading(u64),
    /// Several bodies were found; waiting for the user to choose.
    Asking(Vec<String>),
    /// The worker is writing each body to its own file (request id, files written so far).
    Splitting(u64, SplitImport, usize),
    Failed(String),
}

pub struct Import {
    pub file: PathBuf,
    pub stage: Stage,
}

pub enum Choice {
    Split,
    Whole,
    Cancel,
}

impl Import {
    pub fn waiting_for(&self, id: u64) -> bool {
        matches!(self.stage, Stage::Reading(i) | Stage::Splitting(i, ..) if i == id)
    }

    /// Whether the worker is busy on this import (so a worker restart ends it).
    pub fn busy(&self) -> bool {
        matches!(self.stage, Stage::Reading(_) | Stage::Splitting(..))
    }

    /// One more body file written.
    pub fn progress(&mut self, id: u64) {
        if let Stage::Splitting(i, _, done) = &mut self.stage {
            if *i == id {
                *done += 1;
            }
        }
    }

    fn file_name(&self) -> String {
        self.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// The import window. Returns what the user picked, if anything this frame.
    pub fn window(&self, ctx: &egui::Context) -> Option<Choice> {
        let mut choice = None;
        let mut open = true;
        egui::Window::new("Import STEP")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.strong(self.file_name());
                ui.add_space(4.0);
                match &self.stage {
                    Stage::Reading(_) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Reading the file. A big assembly can take a minute or two.");
                        });
                    }
                    Stage::Splitting(_, split, done) => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(format!("Separating the parts: {done} of {}", split.parts.len()));
                        });
                    }
                    Stage::Failed(error) => {
                        ui.colored_label(ui.visuals().error_fg_color, format!("Couldn't import it: {error}"));
                        if ui.button("Close").clicked() {
                            choice = Some(Choice::Cancel);
                        }
                    }
                    Stage::Asking(bodies) => {
                        ui.label(format!("This file holds {} bodies:", bodies.len()));
                        ui.weak(preview(bodies));
                        ui.add_space(6.0);
                        let fits = bodies.len() <= MAX_SPLIT_BODIES;
                        let split = ui
                            .add_enabled(fits, egui::Button::new("Separate parts in a group"))
                            .on_hover_text("Each body becomes its own part, so you can move it, put it on a joint and check it")
                            .on_disabled_hover_text(format!("Up to {MAX_SPLIT_BODIES} bodies can be separated"));
                        if split.clicked() {
                            choice = Some(Choice::Split);
                        }
                        if ui.button("One part").on_hover_text("Keep the whole file as a single part").clicked() {
                            choice = Some(Choice::Whole);
                        }
                    }
                }
            });
        if !open {
            choice = Some(Choice::Cancel);
        }
        choice
    }
}

/// The first few body names, with repeats counted: "Base, Leaf ×2, Pin".
fn preview(bodies: &[String]) -> String {
    let mut counted: Vec<(&str, usize)> = Vec::new();
    for body in bodies {
        let name = if body.trim().is_empty() { "(no name)" } else { body.as_str() };
        match counted.iter_mut().find(|(n, _)| *n == name) {
            Some((_, count)) => *count += 1,
            None => counted.push((name, 1)),
        }
    }
    const SHOWN: usize = 8;
    let mut text: Vec<String> =
        counted.iter().take(SHOWN).map(|(n, c)| if *c > 1 { format!("{n} ×{c}") } else { (*n).to_owned() }).collect();
    if counted.len() > SHOWN {
        text.push(format!("and {} more", counted.len() - SHOWN));
    }
    text.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_are_counted_in_the_preview() {
        let bodies: Vec<String> = ["Base", "Leaf", "Leaf", "", "Pin"].map(String::from).to_vec();
        assert_eq!(preview(&bodies), "Base, Leaf ×2, (no name), Pin");
        let many: Vec<String> = (0..12).map(|i| format!("P{i}")).collect();
        assert!(preview(&many).ends_with("P7, and 4 more"));
    }
}
