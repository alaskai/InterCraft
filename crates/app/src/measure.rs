// The measuring tool. Picks come from the viewport; the numbers come from the
// geometry worker, which measures the real CAD shape rather than the mesh.

use eframe::egui;
use serde_json::{json, Value};

use crate::viewport::{Hit, Target};

pub struct Picked {
    pub hit: Hit,
    /// Worker cache key of the shape on screen when it was picked.
    pub mesh_key: String,
}

#[derive(Default)]
pub struct MeasureTool {
    pub active: bool,
    pub hover: Option<Hit>,
    pub picks: Vec<Picked>,
    pending: Option<u64>,
    outcome: Option<Result<Value, String>>,
}

impl MeasureTool {
    pub fn toggle(&mut self) {
        self.active = !self.active;
        if !self.active {
            self.clear();
        }
    }

    pub fn clear(&mut self) {
        self.picks.clear();
        self.hover = None;
        self.pending = None;
        self.outcome = None;
    }

    /// Add a pick. Returns the worker request to send: one selection describes it,
    /// two give the distance between them. A third click starts again.
    pub fn click(&mut self, hit: Hit, mesh_key: String) -> (Value, Option<Value>) {
        if self.picks.len() >= 2 {
            self.picks.clear();
        }
        self.picks.push(Picked { hit, mesh_key });
        self.outcome = None;
        let a = selection(&self.picks[0]);
        let b = self.picks.get(1).map(selection);
        (a, b)
    }

    pub fn sent(&mut self, id: u64) {
        self.pending = Some(id);
    }

    pub fn waiting_for(&self, id: u64) -> bool {
        self.pending == Some(id)
    }

    pub fn result(&mut self, id: u64, outcome: Result<Value, String>) {
        if self.pending == Some(id) {
            self.pending = None;
            self.outcome = Some(outcome);
        }
    }

    /// Closest points of a two-pick measurement, for drawing the dimension line.
    pub fn line(&self) -> Option<([f32; 3], [f32; 3])> {
        let data = self.outcome.as_ref()?.as_ref().ok()?;
        Some((point(&data["a_point"])?, point(&data["b_point"])?))
    }

    pub fn distance_text(&self) -> Option<String> {
        let data = self.outcome.as_ref()?.as_ref().ok()?;
        Some(format!("{} mm", mm(data["distance"].as_f64()?)))
    }

    /// The readout shown in the corner of the viewport.
    pub fn panel_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.strong("Measure");
            if !self.picks.is_empty() && ui.small_button("Clear").on_hover_text("Esc").clicked() {
                self.clear();
            }
        });
        match (&self.outcome, self.pending, self.picks.len()) {
            (_, Some(_), _) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Measuring...");
                });
            }
            (Some(Err(error)), _, _) => {
                ui.colored_label(ui.visuals().error_fg_color, error.lines().next().unwrap_or_default());
            }
            (Some(Ok(data)), _, 2) => pair_ui(ui, data, &self.picks),
            (Some(Ok(data)), _, 1) => {
                ui.label(describe(&data["a"], self.picks[0].hit.target));
                ui.weak("Click a second corner, edge or point for the distance.");
            }
            _ => {
                ui.weak("Click a corner, edge or face. Corners and edges snap.");
            }
        }
    }
}

fn pair_ui(ui: &mut egui::Ui, data: &Value, picks: &[Picked]) {
    if let Some(d) = data["distance"].as_f64() {
        ui.label(egui::RichText::new(format!("{} mm", mm(d))).size(18.0).strong());
    }
    if let Some(delta) = data["delta"].as_array() {
        let parts: Vec<String> = ["X", "Y", "Z"]
            .iter()
            .zip(delta)
            .map(|(axis, v)| format!("Δ{axis} {}", mm(v.as_f64().unwrap_or(0.0))))
            .collect();
        ui.monospace(parts.join("   "));
    }
    if let Some(angle) = data["angle_deg"].as_f64() {
        ui.label(format!("Angle {}°", mm(angle)));
    }
    ui.weak(format!("A: {}", describe(&data["a"], picks[0].hit.target)));
    ui.weak(format!("B: {}", describe(&data["b"], picks[1].hit.target)));
}

/// One line about a picked item, from the worker's description.
fn describe(item: &Value, target: Target) -> String {
    match item["kind"].as_str() {
        Some("vertex") => {
            // Free points come back as vertices too; the pick says which it was.
            let name = if target == Target::Surface { "Point on face" } else { "Corner" };
            match point(&item["position"]) {
                Some(p) => format!("{name} ({}, {}, {})", mm(p[0] as f64), mm(p[1] as f64), mm(p[2] as f64)),
                None => name.to_owned(),
            }
        }
        Some("edge") => {
            let length = item["length"].as_f64().map(mm).unwrap_or_default();
            match item["type"].as_str() {
                Some("circle") => format!(
                    "Circular edge, Ø {} mm (r {}), length {length} mm",
                    item["diameter"].as_f64().map(mm).unwrap_or_default(),
                    item["radius"].as_f64().map(mm).unwrap_or_default()
                ),
                Some("line") => format!("Straight edge, length {length} mm"),
                Some(other) => format!("{other} edge, length {length} mm"),
                None => format!("Edge, length {length} mm"),
            }
        }
        Some("face") => format!("{} face, area {} mm²", item["type"].as_str().unwrap_or("A"), item["area"].as_f64().map(mm).unwrap_or_default()),
        _ => "Selection".to_owned(),
    }
}

fn selection(picked: &Picked) -> Value {
    match picked.hit.target {
        Target::Corner(i) => json!({ "mesh": picked.mesh_key, "kind": "vertex", "index": i }),
        Target::Edge(i) => json!({ "mesh": picked.mesh_key, "kind": "edge", "index": i }),
        // A surface point comes from the mesh, so it's exact on flat faces and within
        // the mesh tolerance (0.05 mm) on curved ones.
        Target::Surface => json!({ "kind": "point", "point": picked.hit.pos }),
    }
}

fn point(value: &Value) -> Option<[f32; 3]> {
    let a = value.as_array()?;
    Some([a.first()?.as_f64()? as f32, a.get(1)?.as_f64()? as f32, a.get(2)?.as_f64()? as f32])
}

/// Up to three decimals, without trailing zeros: 12.5, 8.2, 40.
pub fn mm(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" { "0".to_owned() } else { text.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(target: Target) -> Hit {
        Hit { part: "P".into(), target, pos: [1.0, 2.0, 3.0] }
    }

    #[test]
    fn clicks_build_requests_and_restart_after_two() {
        let mut tool = MeasureTool { active: true, ..Default::default() };
        let (a, b) = tool.click(hit(Target::Edge(4)), "k1".into());
        assert_eq!(a, json!({"mesh": "k1", "kind": "edge", "index": 4}));
        assert!(b.is_none());
        let (_, b) = tool.click(hit(Target::Surface), "k2".into());
        assert_eq!(b.unwrap(), json!({"kind": "point", "point": [1.0, 2.0, 3.0]}));
        let (a, b) = tool.click(hit(Target::Corner(0)), "k3".into());
        assert_eq!(a["kind"], "vertex");
        assert!(b.is_none(), "a third click should start a new measurement");
    }

    #[test]
    fn stale_results_are_ignored() {
        let mut tool = MeasureTool::default();
        tool.sent(7);
        tool.result(6, Ok(json!({"distance": 1.0})));
        assert!(tool.waiting_for(7) && tool.outcome.is_none());
        tool.result(7, Ok(json!({"distance": 12.5, "a_point": [0, 0, 0], "b_point": [12.5, 0, 0]})));
        assert_eq!(tool.distance_text().unwrap(), "12.5 mm");
        assert!(tool.line().is_some());
    }

    #[test]
    fn numbers_read_cleanly() {
        assert_eq!(mm(40.0), "40");
        assert_eq!(mm(8.2), "8.2");
        assert_eq!(mm(12.3456), "12.346");
        assert_eq!(mm(-0.0001), "0");
    }
}
