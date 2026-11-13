// The fit check: every pair of visible parts is either overlapping (a clash, shown
// in red in the viewport) or clear by some gap. The geometry worker does the maths
// on the real CAD shapes.

use std::path::PathBuf;

use eframe::egui;
use serde_json::Value;

use crate::measure::mm;

pub struct FitCheck {
    pending: Option<u64>,
    outcome: Option<Result<Value, String>>,
    /// The parts changed since the last answer, so it needs asking again.
    dirty: bool,
}

impl Default for FitCheck {
    fn default() -> Self {
        Self { pending: None, outcome: None, dirty: true }
    }
}

impl FitCheck {
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

    /// How many pairs clash, once the answer is in.
    #[cfg(test)]
    pub fn clashes(&self) -> Option<usize> {
        let data = self.outcome.as_ref()?.as_ref().ok()?;
        Some(data["pairs"].as_array()?.iter().filter(|p| p["status"] == "overlap").count())
    }

    /// Draws the results. Returns the closest points of the gap under the pointer.
    pub fn panel_ui(&mut self, ui: &mut egui::Ui) -> Option<([f32; 3], [f32; 3])> {
        ui.strong("Fit check");
        let data = match &self.outcome {
            Some(Ok(data)) if self.pending.is_none() && !self.dirty => data,
            Some(Err(error)) if self.pending.is_none() && !self.dirty => {
                ui.colored_label(ui.visuals().error_fg_color, error.lines().next().unwrap_or_default());
                return None;
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Checking...");
                });
                return None;
            }
        };
        let pairs = data["pairs"].as_array().map(Vec::as_slice).unwrap_or_default();
        let clashes = pairs.iter().filter(|p| p["status"] == "overlap").count();
        if clashes == 0 {
            ui.colored_label(egui::Color32::from_rgb(70, 170, 90), "No clashes");
        } else {
            let text = if clashes == 1 { "1 clash".to_owned() } else { format!("{clashes} clashes") };
            ui.colored_label(ui.visuals().error_fg_color, format!("{text}, shown in red"));
        }

        let mut hovered = None;
        egui::ScrollArea::vertical().id_salt("fit results").max_height(130.0).show(ui, |ui| {
            // Clashes first, then the tightest gaps.
            let mut sorted: Vec<&Value> = pairs.iter().collect();
            sorted.sort_by(|a, b| {
                let rank = |p: &Value| (p["status"] != "overlap", p["clearance"].as_f64().unwrap_or(0.0));
                rank(a).partial_cmp(&rank(b)).unwrap_or(std::cmp::Ordering::Equal)
            });
            for pair in sorted {
                let names = format!("{} / {}", pair["a"].as_str().unwrap_or("?"), pair["b"].as_str().unwrap_or("?"));
                let response = match pair["status"].as_str() {
                    Some("overlap") => {
                        let volume = pair["overlap_volume"].as_f64().unwrap_or(0.0);
                        ui.colored_label(ui.visuals().error_fg_color, format!("{names}: overlap {volume:.1} mm³"))
                    }
                    Some("touching") => ui.label(format!("{names}: touching")),
                    _ => ui.label(format!("{names}: {} mm clear", mm(pair["clearance"].as_f64().unwrap_or(0.0)))),
                };
                if response.hovered() {
                    hovered = point(&pair["a_point"]).zip(point(&pair["b_point"]));
                }
            }
        });
        hovered
    }
}

/// Mesh files of the clash volumes in a fit result.
pub fn overlap_meshes(data: &Value) -> Vec<PathBuf> {
    let pairs = data["pairs"].as_array().map(Vec::as_slice).unwrap_or_default();
    pairs.iter().filter_map(|p| p["mesh"].as_str()).map(PathBuf::from).collect()
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
    fn a_change_asks_again_and_stale_answers_are_ignored() {
        let mut fit = FitCheck::default();
        assert!(fit.wants_run());
        fit.sent(4);
        assert!(!fit.wants_run() && fit.waiting_for(4));
        fit.invalidate();
        assert!(!fit.waiting_for(4), "the old answer no longer matches the parts");
        fit.sent(5);
        fit.result(Ok(json!({ "pairs": [
            { "a": "A", "b": "B", "status": "overlap", "overlap_volume": 200.0, "mesh": "x.icm" },
            { "a": "A", "b": "C", "status": "clear", "clearance": 2.5 },
        ]})));
        assert_eq!(fit.clashes(), Some(1));
        let data = fit.outcome.clone().unwrap().unwrap();
        assert_eq!(overlap_meshes(&data), [PathBuf::from("x.icm")]);
    }
}
