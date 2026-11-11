// Editing a feature's parameters from the Inspector. Dragging a number rebuilds the
// part live without touching the files; letting go (or typing a value) saves it once,
// so each edit is a single undo step.

use eframe::egui;
use serde_json::Value;

/// A number being dragged. The part is previewed at `value` until the drag ends.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDrag {
    pub part: String,
    pub feature: String,
    pub name: String,
    pub value: f64,
    /// Whole numbers stay whole while dragging (a width of 40 goes to 41, not 40.37).
    pub whole: bool,
    /// The value of the last preview sent to the worker.
    pub sent: Option<f64>,
}

impl ParamDrag {
    fn is(&self, part: &str, feature: &str, name: &str) -> bool {
        self.part == part && self.feature == feature && self.name == name
    }

    pub fn json(&self) -> Value {
        number(self.value, self.whole)
    }
}

/// A value to save.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamCommit {
    pub part: String,
    pub feature: String,
    pub name: String,
    pub value: Value,
}

/// One row of the parameter grid. Numbers can be dragged or typed and true/false
/// values ticked. Anything else is shown as it is.
pub fn param_row(
    ui: &mut egui::Ui,
    (part, feature): (&str, &str),
    name: &str,
    value: &Value,
    editable: bool,
    drag: &mut Option<ParamDrag>,
) -> Option<ParamCommit> {
    ui.label(name);
    let commit = |value: Value| Some(ParamCommit { part: part.into(), feature: feature.into(), name: name.into(), value });

    if let Some(mut on) = value.as_bool() {
        let changed = ui.add_enabled(editable, egui::Checkbox::without_text(&mut on)).changed();
        ui.end_row();
        return if changed { commit(Value::Bool(on)) } else { None };
    }
    let Some(saved) = value.as_f64() else {
        ui.monospace(value.to_string());
        ui.end_row();
        return None;
    };

    let whole = value.is_i64() || value.is_u64();
    let current = drag.as_ref().filter(|d| d.is(part, feature, name));
    let mut shown = current.map_or(saved, |d| d.value);
    let speed = if whole { 0.1 } else { (shown.abs() * 0.004).max(0.01) };
    let response = ui
        .add_enabled(
            editable,
            egui::DragValue::new(&mut shown)
                .speed(speed)
                .max_decimals(3)
                .custom_formatter(|v, _| crate::measure::mm(v))
                .update_while_editing(false),
        )
        .on_hover_text(if editable {
            "Drag to change it live, or click to type a value"
        } else {
            "Unlock the part to change it"
        });
    ui.end_row();

    if response.dragged() && response.changed() {
        let value = snap(shown, whole);
        let sent = current.and_then(|d| d.sent);
        *drag = Some(ParamDrag { part: part.into(), feature: feature.into(), name: name.into(), value, whole, sent });
        return None;
    }
    if response.drag_stopped() {
        let ended = drag.take_if(|d| d.is(part, feature, name))?;
        return (ended.value != saved).then(|| commit(ended.json())).flatten();
    }
    if response.changed() && shown != saved {
        // Typed: keep exactly what was typed, decimals included.
        return commit(number(shown, whole && shown.fract() == 0.0));
    }
    None
}

/// Drag steps: whole millimetres for whole numbers, tenths otherwise.
fn snap(value: f64, whole: bool) -> f64 {
    if whole { value.round() } else { (value * 10.0).round() / 10.0 }
}

fn number(value: f64, whole: bool) -> Value {
    if whole && value.fract() == 0.0 && value.abs() < 1e15 {
        Value::from(value as i64)
    } else {
        serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drag_steps_and_json_types() {
        assert_eq!(snap(40.37, true), 40.0);
        assert_eq!(snap(8.234, false), 8.2);
        assert_eq!(number(41.0, true), Value::from(41));
        assert_eq!(number(8.2, false), serde_json::json!(8.2));
        // A whole number typed as 2.5 becomes a decimal.
        assert_eq!(number(2.5, true), serde_json::json!(2.5));
    }
}
