// The print check panel: thin walls, small holes and overhangs on the selected part,
// each marked in the 3D view, with a button that asks the model why it matters.

use eframe::egui;
use serde_json::Value;

pub struct PrintCheck {
    /// The part being checked.
    pub part: String,
    pending: Option<u64>,
    outcome: Option<Result<Value, String>>,
    /// The part changed since the last answer, so it needs checking again.
    dirty: bool,
}

/// What a click in the panel asked for.
pub enum PrintAction {
    /// Put this question to the chat.
    Ask(String),
}

impl PrintCheck {
    pub fn new(part: String) -> Self {
        Self { part, pending: None, outcome: None, dirty: true }
    }

    pub fn invalidate(&mut self) {
        self.pending = None;
        self.dirty = true;
    }

    pub fn wants_run(&self) -> bool {
        self.dirty && self.pending.is_none()
    }

    pub fn sent(&mut self, id: u64) {
        self.pending = Some(id);
        self.dirty = false;
    }

    pub fn waiting_for(&self, id: u64) -> bool {
        self.pending == Some(id)
    }

    pub fn result(&mut self, outcome: Result<Value, String>) {
        self.pending = None;
        self.dirty = false;
        self.outcome = Some(outcome);
    }

    fn data(&self) -> Option<&Value> {
        if self.pending.is_some() || self.dirty {
            return None;
        }
        self.outcome.as_ref()?.as_ref().ok()
    }

    /// The problems found, for drawing markers: (number shown, position, kind).
    pub fn markers(&self) -> Vec<(usize, [f32; 3], String)> {
        let Some(problems) = self.data().and_then(|d| d["problems"].as_array()) else { return Vec::new() };
        problems
            .iter()
            .enumerate()
            .filter_map(|(i, p)| Some((i + 1, point(&p["at"])?, p["kind"].as_str()?.to_owned())))
            .collect()
    }

    pub fn panel_ui(&mut self, ui: &mut egui::Ui, can_ask: bool) -> Option<PrintAction> {
        ui.strong(format!("Print check: {}", self.part));
        if let Some(Err(error)) = &self.outcome {
            if self.pending.is_none() && !self.dirty {
                ui.colored_label(ui.visuals().error_fg_color, error.lines().next().unwrap_or_default());
                return None;
            }
        }
        let Some(data) = self.data() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Checking...");
            });
            return None;
        };
        let mut action = None;
        let problems = data["problems"].as_array().map(Vec::as_slice).unwrap_or_default();
        if problems.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(70, 170, 90), "Ready to print: no thin walls, tiny holes or unsupported overhangs.");
        }
        egui::ScrollArea::vertical().id_salt("print results").max_height(150.0).show(ui, |ui| {
            for (i, problem) in problems.iter().enumerate() {
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("{}.", i + 1));
                    ui.label(problem["message"].as_str().unwrap_or_default());
                    let ask = ui.add_enabled(can_ask, egui::Button::new("Ask why").small());
                    if ask.on_disabled_hover_text("Set up a chat provider in Settings first").clicked() {
                        let question = format!(
                            "The print check on {} found: {} Why does this matter for 3D printing, and what is the best fix here?",
                            self.part,
                            problem["message"].as_str().unwrap_or_default()
                        );
                        action = Some(PrintAction::Ask(question));
                    }
                });
            }
        });
        if let Some(grams) = data["material"]["estimated_grams"].as_f64() {
            ui.weak(format!("About {grams} g of PLA. Printing with +Z up, as shown."));
        }
        action
    }
}

fn point(value: &Value) -> Option<[f32; 3]> {
    let a = value.as_array()?;
    Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn markers_follow_the_answer_and_a_change_clears_them() {
        let mut check = PrintCheck::new("Plate".into());
        assert!(check.wants_run());
        check.sent(3);
        assert!(check.markers().is_empty(), "nothing until the answer arrives");
        check.result(Ok(json!({ "problems": [{ "kind": "thin wall", "at": [1, 2, 3], "message": "Thin." }] })));
        assert_eq!(check.markers(), [(1, [1.0, 2.0, 3.0], "thin wall".to_owned())]);
        check.invalidate();
        assert!(check.markers().is_empty() && check.wants_run());
    }
}
