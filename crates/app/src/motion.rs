// Moving joints in the viewer. Dragging a joint's slider moves the parts straight
// away on the GPU without rebuilding them, letting go saves where it ended up, and
// Play sweeps the whole range back and forth without saving anything.

use std::time::{Duration, Instant};

use eframe::egui;
use intercraft_core::{JointKind, Project};

/// How long one sweep from one end of the range to the other takes.
const SWEEP: Duration = Duration::from_millis(2500);

#[derive(Default)]
pub struct Motion {
    /// The Motion panel is showing.
    pub open: bool,
    /// A joint being dragged, and the value under the pointer.
    drag: Option<(String, f64)>,
    playing: Option<(String, Instant)>,
    /// Range checks, by joint name.
    checks: std::collections::HashMap<String, Check>,
}

/// A check of a joint's whole range for clashes.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// Asked for, waiting for the parts to finish building.
    Wanted,
    Sent(u64),
    /// The summary, and whether it moves freely.
    Done(String, bool),
    Failed(String),
}

/// What the panel asks the app to do.
pub enum MotionAction {
    /// Save a joint at a new value.
    Set(String, f64),
}

impl Motion {
    /// The joint shown somewhere other than where it is saved, and where it is shown.
    pub fn preview(&self, project: &Project) -> Option<(String, f64)> {
        if let Some((name, value)) = &self.drag {
            return Some((name.clone(), *value));
        }
        let (name, started) = self.playing.as_ref()?;
        let joint = project.joint(name)?;
        Some((name.clone(), sweep(joint.min, joint.max, started.elapsed())))
    }

    pub fn animating(&self) -> bool {
        self.playing.is_some()
    }

    pub fn stop(&mut self) {
        self.drag = None;
        self.playing = None;
    }

    /// A joint whose range check is waiting to be sent.
    pub fn wanted_check(&self) -> Option<String> {
        self.checks.iter().find(|(_, c)| **c == Check::Wanted).map(|(name, _)| name.clone())
    }

    pub fn set_check(&mut self, joint: &str, check: Check) {
        self.checks.insert(joint.to_owned(), check);
    }

    #[cfg(test)]
    pub fn check(&self, joint: &str) -> Option<&Check> {
        self.checks.get(joint)
    }

    /// The joint whose check has this worker id.
    pub fn checking(&self, id: u64) -> Option<String> {
        self.checks.iter().find(|(_, c)| **c == Check::Sent(id)).map(|(name, _)| name.clone())
    }

    /// The project changed, so the answers no longer hold. A check still running
    /// is asked for again, so it ends up about the new shapes.
    pub fn invalidate(&mut self) {
        self.checks.retain(|_, c| matches!(c, Check::Wanted | Check::Sent(_)));
        for check in self.checks.values_mut() {
            *check = Check::Wanted;
        }
    }

    /// The panel: one row per joint with a slider and a Play button.
    pub fn panel_ui(&mut self, ui: &mut egui::Ui, project: &Project) -> Option<MotionAction> {
        // A joint that has gone (undo, or the model removed it) can't be shown any more.
        if self.playing.as_ref().is_some_and(|(name, _)| project.joint(name).is_none()) {
            self.playing = None;
        }
        if self.drag.as_ref().is_some_and(|(name, _)| project.joint(name).is_none()) {
            self.drag = None;
        }
        self.checks.retain(|name, _| project.joint(name).is_some());
        let mut action = None;
        ui.strong("Motion");
        if project.joints.is_empty() {
            ui.weak("No joints yet. Ask the model to add a hinge or a slider between two parts.");
            return None;
        }
        for joint in &project.joints {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let label = ui.label(&joint.name);
                let moves = format!("Moves {} on {}", joint.part, joint.base);
                if joint.why.is_empty() {
                    label.on_hover_text(moves);
                } else {
                    label.on_hover_text(format!("{moves}. {}", joint.why));
                }
                let playing = self.playing.as_ref().is_some_and(|(name, _)| *name == joint.name);
                let button = ui.small_button(if playing { "Stop" } else { "Play" });
                if button.on_hover_text("Sweep through the whole range without saving").clicked() {
                    self.playing = (!playing).then(|| (joint.name.clone(), Instant::now()));
                }
                let running = matches!(self.checks.get(&joint.name), Some(Check::Wanted | Check::Sent(_)));
                let check = ui.add_enabled(!running, egui::Button::new("Check").small());
                if check.on_hover_text("Check the whole range for parts hitting each other").clicked() {
                    self.checks.insert(joint.name.clone(), Check::Wanted);
                }
            });

            let shown = self.preview(project).filter(|(name, _)| *name == joint.name).map_or(joint.at, |(_, v)| v);
            let mut value = shown;
            let suffix = match joint.kind {
                JointKind::Hinge => "°",
                JointKind::Slider => " mm",
            };
            let slider = ui.add(
                egui::Slider::new(&mut value, joint.min..=joint.max)
                    .suffix(suffix)
                    .max_decimals(1)
                    .clamping(egui::SliderClamping::Always),
            );
            // The slider rounds what it shows, which counts as a change, so only a
            // drag, a click or the keyboard counts as the user moving it.
            let held = slider.dragged() || slider.is_pointer_button_down_on();
            let touched = held || slider.drag_stopped() || slider.clicked() || slider.has_focus();
            if touched && (slider.changed() || held) {
                // Taking hold of a playing joint stops it where it is.
                self.playing = None;
                self.drag = Some((joint.name.clone(), value));
            }
            if touched && !held {
                if let Some((name, value)) = self.drag.take() {
                    if value != joint.at {
                        action = Some(MotionAction::Set(name, value));
                    }
                }
            }
            match self.checks.get(&joint.name) {
                Some(Check::Wanted | Check::Sent(_)) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Checking the whole range...");
                    });
                }
                Some(Check::Done(text, clear)) => {
                    let colour = if *clear { egui::Color32::from_rgb(90, 190, 110) } else { egui::Color32::from_rgb(235, 90, 80) };
                    ui.label(egui::RichText::new(text).color(colour).small());
                }
                Some(Check::Failed(error)) => {
                    ui.weak(format!("Couldn't check: {error}"));
                }
                None => {}
            }
        }
        ui.add_space(2.0);
        ui.weak("Drag to move, let go to keep it there.");
        action
    }
}

/// Back and forth between `min` and `max`, starting from `min`.
fn sweep(min: f64, max: f64, elapsed: Duration) -> f64 {
    let t = elapsed.as_secs_f64() / SWEEP.as_secs_f64();
    let phase = t % 2.0;
    let along = if phase <= 1.0 { phase } else { 2.0 - phase };
    // Eased at the ends, like a real mechanism slowing to a stop.
    let eased = 0.5 - 0.5 * (along * std::f64::consts::PI).cos();
    min + (max - min) * eased
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sweep_goes_there_and_back() {
        let at = |ms: u64| sweep(0.0, 90.0, Duration::from_millis(ms));
        assert_eq!(at(0), 0.0);
        assert!((at(1250) - 45.0).abs() < 1e-9);
        assert!((at(2500) - 90.0).abs() < 1e-9);
        assert!((at(5000) - 0.0).abs() < 1e-9);
        assert!(at(3750) < 90.0 && at(3750) > 0.0);
    }
}
