// Editing numbers from the Inspector: a feature's parameters and a part's placement.
// Dragging a number rebuilds the part live without touching the files; letting go
// (or typing a value) saves it once, so each edit is a single undo step.

use eframe::egui;
use intercraft_core::Part;
use serde_json::Value;

/// Which number is being edited.
#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Param { feature: String, name: String },
    /// Position along X, Y or Z (0, 1, 2), in mm.
    Position(usize),
    /// Rotation about X, Y or Z, in degrees.
    Rotation(usize),
}

impl Field {
    pub fn label(&self) -> String {
        const AXES: [&str; 3] = ["X", "Y", "Z"];
        match self {
            Field::Param { name, .. } => name.clone(),
            Field::Position(axis) => format!("position {}", AXES[*axis]),
            Field::Rotation(axis) => format!("rotation {}", AXES[*axis]),
        }
    }
}

/// A number being dragged. The part is previewed at `value` until the drag ends.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDrag {
    pub part: String,
    pub field: Field,
    pub value: f64,
    /// Whole numbers stay whole while dragging (a width of 40 goes to 41, not 40.37).
    pub whole: bool,
    /// The value of the last preview sent to the worker.
    pub sent: Option<f64>,
}

impl ParamDrag {
    /// Put the dragged value into a copy of the part, for a preview build.
    pub fn apply(&self, part: &mut Part) -> bool {
        match &self.field {
            Field::Param { feature, name } => {
                let Some(feature) = part.features.iter_mut().find(|f| &f.id == feature) else { return false };
                feature.params.insert(name.clone(), number(self.value, self.whole));
            }
            Field::Position(axis) => part.position[*axis] = self.value,
            Field::Rotation(axis) => part.rotation[*axis] = self.value,
        }
        true
    }
}

/// A value to save.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamCommit {
    pub part: String,
    pub field: Field,
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
    simple: bool,
    label: Option<&str>,
) -> Option<ParamCommit> {
    // Simple shows the plain name (the model's label, or the code name made readable).
    // Advanced shows the code name, with the plain name on hover.
    match (simple, label) {
        (true, Some(label)) => ui.label(label).on_hover_text(format!("Called {name} in the step's code")),
        (true, None) => ui.label(friendly(name)).on_hover_text(format!("Called {name} in the step's code")),
        (false, Some(label)) => ui.label(name).on_hover_text(label),
        (false, None) => ui.label(name),
    };
    let field = Field::Param { feature: feature.to_owned(), name: name.to_owned() };

    if let Some(mut on) = value.as_bool() {
        let changed = ui.add_enabled(editable, egui::Checkbox::without_text(&mut on)).changed();
        ui.end_row();
        return changed.then(|| ParamCommit { part: part.to_owned(), field, value: Value::Bool(on) });
    }
    let Some(saved) = value.as_f64() else {
        ui.monospace(value.to_string());
        ui.end_row();
        return None;
    };
    let whole = value.is_i64() || value.is_u64();
    let commit = number_cell(ui, part, field, saved, whole, editable, drag);
    ui.end_row();
    commit
}

/// Label, values, which field each axis edits, and whether it drags in whole steps.
type PlacementRow = (&'static str, [f64; 3], fn(usize) -> Field, bool);

/// Position and rotation of a part in the assembly, three numbers each.
pub fn placement_rows(ui: &mut egui::Ui, part: &Part, drag: &mut Option<ParamDrag>) -> Option<ParamCommit> {
    let mut commit = None;
    let rows: [PlacementRow; 2] = [
        ("Position", part.position, Field::Position, false),
        // Turning is easier in whole degrees. A typed value can still have decimals.
        ("Rotation", part.rotation, Field::Rotation, true),
    ];
    for (label, values, field, whole) in rows {
        ui.label(label);
        ui.horizontal(|ui| {
            for (axis, value) in values.into_iter().enumerate() {
                if let Some(edit) = number_cell(ui, &part.name, field(axis), value, whole, true, drag) {
                    commit = Some(edit);
                }
            }
        });
        ui.end_row();
    }
    commit
}

/// A number that can be dragged for a live preview or clicked to type a value.
fn number_cell(
    ui: &mut egui::Ui,
    part: &str,
    field: Field,
    saved: f64,
    whole: bool,
    editable: bool,
    drag: &mut Option<ParamDrag>,
) -> Option<ParamCommit> {
    let is_this = |d: &ParamDrag| d.part == part && d.field == field;
    let current = drag.as_ref().filter(|d| is_this(d));
    let mut shown = current.map_or(saved, |d| d.value);
    let speed = match &field {
        // A part moves a useful distance per drag, wherever it starts from.
        Field::Position(_) => 0.25,
        Field::Rotation(_) => 0.5,
        Field::Param { .. } if whole => 0.1,
        Field::Param { .. } => (shown.abs() * 0.004).max(0.02),
    };
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
    let label = field.label();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::DragValue, editable, &label));

    let commit = |value: Value, field: Field| Some(ParamCommit { part: part.to_owned(), field, value });
    if response.dragged() && response.changed() {
        let value = snap(shown, whole);
        let sent = current.and_then(|d| d.sent);
        *drag = Some(ParamDrag { part: part.to_owned(), field, value, whole, sent });
        return None;
    }
    if response.drag_stopped() {
        let ended = drag.take_if(|d| is_this(d))?;
        return (ended.value != saved).then(|| commit(number(ended.value, whole), field)).flatten();
    }
    if response.changed() && shown != saved {
        // Typed: keep exactly what was typed, decimals included.
        return commit(number(shown, whole && shown.fract() == 0.0), field);
    }
    None
}

/// "bore_d" reads as "Bore d": the raw name, made a little friendlier.
pub fn friendly(name: &str) -> String {
    let spaced = name.replace(['_', '-'], " ");
    let mut chars = spaced.trim().chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// Drag steps: whole units for whole numbers, tenths otherwise.
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
    fn names_read_as_words() {
        assert_eq!(friendly("bore_d"), "Bore d");
        assert_eq!(friendly("past_centre"), "Past centre");
        assert_eq!(friendly("radius"), "Radius");
    }

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
