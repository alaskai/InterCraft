// Plain-language words for what the model is doing: what each tool is for, and the
// live caption shown over the 3D view while a reply runs. How much is said depends
// on the user's explanation level.

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::settings::Explain;

/// How long a caption stays up after the work it describes has finished.
pub const LINGER: Duration = Duration::from_secs(4);

/// The caption on screen: its text, and when the work it describes ended (None
/// while it is still going on).
#[derive(Debug, Clone, Default)]
pub struct Caption {
    text: Option<String>,
    ended: Option<Instant>,
}

impl Caption {
    pub fn set(&mut self, text: String) {
        self.text = Some(text);
        self.ended = None;
    }

    /// The work is done. The caption fades out over the next few seconds.
    pub fn finish(&mut self) {
        if self.text.is_some() && self.ended.is_none() {
            self.ended = Some(Instant::now());
        }
    }

    /// Text to draw and how opaque to draw it (1 while working, fading to 0).
    pub fn shown(&self) -> Option<(&str, f32)> {
        let text = self.text.as_deref()?;
        let alpha = match self.ended {
            None => 1.0,
            Some(ended) => {
                let left = LINGER.saturating_sub(ended.elapsed()).as_secs_f32() / LINGER.as_secs_f32();
                // Fully visible for most of the time, then a quick fade.
                (left * 3.0).min(1.0)
            }
        };
        (alpha > 0.0).then_some((text, alpha))
    }
}

/// What a tool does, in one sentence a newcomer can follow.
pub fn tool_purpose(name: &str) -> &'static str {
    match name {
        "get_project" => "Reads the whole project: every part, every step and their current measurements.",
        "add_part" => "Starts a new, empty part.",
        "add_feature" => "Adds one modelling step to a part, then rebuilds and checks it.",
        "edit_feature" => "Changes an existing step, then rebuilds from that step onwards.",
        "set_param" => "Changes one dimension of a step and rebuilds.",
        "move_feature" => "Changes the order of a part's steps. Order matters: a fillet before a cut rounds different edges.",
        "delete_feature" => "Removes a step and rebuilds what comes after it.",
        "delete_part" => "Removes a whole part. Undo brings it back.",
        "duplicate_part" => "Copies a part with all its steps.",
        "rename_part" => "Gives a part a new name.",
        "place_part" => "Moves or turns a part in the assembly without changing its shape.",
        "move_group" => "Moves or turns a whole imported assembly as one piece.",
        "get_part" => "Reads one part in full: its steps, code and numbers.",
        "find_parts" => "Looks up parts by name or group, with where each one sits.",
        "set_group" => "Tidies parts into a group in the parts list. The parts themselves don't change.",
        "rename_group" => "Renames a group, or moves it inside another one.",
        "inspect_part" => "Lists the part's faces and holes with exact sizes and positions, so nothing has to be guessed.",
        "measure" => "Measures a distance, angle or size exactly, using the CAD kernel rather than the picture.",
        "check_fit" => "Checks whether parts overlap, and how big the gap between them is.",
        "fastener_info" => "Looks up the real sizes for a screw size in the ISO tables: hole sizes, head, nut and insert.",
        "add_standard_part" => "Adds a real screw, nut, washer, insert or bearing with its true dimensions, to design around.",
        "print_check" => "Checks the part for 3D printing: walls too thin, holes too small and overhangs that would need support.",
        "view_model" => "Takes a picture of the model to check that it looks right.",
        "export_part" => "Writes the part to a file: STEP to keep editing it, STL to 3D print it.",
        "update_brief" => "Updates the project's design brief: what it is for, decisions made and open questions.",
        "add_joint" => "Lets one part move against another: a hinge turns it, a slider slides it.",
        "edit_joint" => "Changes a joint, or moves it to a new position to check that pose.",
        "delete_joint" => "Removes a joint, so the parts no longer move.",
        "check_motion" => "Moves a joint through its whole range and checks that nothing hits anything on the way.",
        "undo" => "Steps back one change.",
        "redo" => "Steps forward again after an undo.",
        "ask_user" => "Asks you to choose, because the decision is yours.",
        _ => "",
    }
}

/// One line for the live caption, when a tool call starts. None for tools that are
/// only bookkeeping, so the caption doesn't flicker through them.
pub fn caption(name: &str, input: &Value, level: Explain) -> Option<String> {
    let text = |key: &str| input[key].as_str().map(str::trim).filter(|t| !t.is_empty());
    let part = text("part");
    let line = match name {
        "add_feature" | "edit_feature" => {
            let label = text("label").or(text("feature_id")).unwrap_or("a step");
            let verb = if name == "add_feature" { "Adding" } else { "Changing" };
            let base = match part {
                Some(part) => format!("{verb} {label} on {part}"),
                None => format!("{verb} {label}"),
            };
            match (level, text("why")) {
                (Explain::Expert, _) | (_, None) => base,
                (_, Some(why)) => format!("{base}: {why}"),
            }
        }
        "set_param" => {
            let name = text("name").unwrap_or("a dimension");
            let value = match &input["value"] {
                Value::Number(n) => format!("{} mm", crate::measure::mm(n.as_f64().unwrap_or(0.0))),
                other => other.to_string(),
            };
            format!("Changing {name} to {value}")
        }
        "add_part" => format!("Starting a new part, {}", text("name").unwrap_or("unnamed")),
        "move_feature" => "Reordering the steps".to_owned(),
        "delete_feature" => "Removing a step".to_owned(),
        "delete_part" => format!("Removing {}", part.unwrap_or("a part")),
        "duplicate_part" => format!("Copying {}", part.unwrap_or("a part")),
        "place_part" => format!("Moving {}", part.unwrap_or("a part")),
        "move_group" => format!("Moving the whole of {}", text("group").unwrap_or("a group")),
        "get_part" => format!("Reading {}", part.unwrap_or("a part")),
        "find_parts" => format!("Finding parts: {}", text("name").or(text("group")).unwrap_or("all")),
        "set_group" => format!("Grouping parts as {}", text("group").unwrap_or("ungrouped")),
        "rename_group" => format!("Tidying the group {}", text("group").unwrap_or("")),
        "inspect_part" => format!("Looking closely at {}", part.unwrap_or("the part")),
        "measure" => "Measuring".to_owned(),
        "check_fit" => "Checking how the parts fit together".to_owned(),
        "fastener_info" => format!("Looking up real {} sizes", text("size").unwrap_or("screw")),
        "add_standard_part" => format!("Adding a {} {}", text("size").unwrap_or(""), text("kind").unwrap_or("standard part")),
        "print_check" => format!("Checking {} will print well", part.unwrap_or("the part")),
        "view_model" => "Taking a look at the model".to_owned(),
        "export_part" => format!("Exporting {}", part.unwrap_or("the part")),
        "update_brief" => format!("Updating the brief: {}", text("section").unwrap_or("notes")),
        "undo" => "Undoing the last change".to_owned(),
        "redo" => "Redoing the change".to_owned(),
        "add_joint" => {
            let kind = text("kind").unwrap_or("joint");
            let base = format!("Adding a {kind} so {} can move", part.unwrap_or("the part"));
            match (level, text("why")) {
                (Explain::Expert, _) | (_, None) => base,
                (_, Some(why)) => format!("{base}: {why}"),
            }
        }
        "edit_joint" => match input["at"].as_f64() {
            Some(at) => format!("Moving {} to {}", text("name").unwrap_or("the joint"), crate::measure::mm(at)),
            None => format!("Changing {}", text("name").unwrap_or("a joint")),
        },
        "delete_joint" => format!("Removing {}", text("name").unwrap_or("a joint")),
        "check_motion" => format!("Checking {} through its whole range", text("joint").unwrap_or("the joint")),
        _ => return None,
    };
    // Checks get a word on why they matter, for someone new to this.
    let extra = match (level, name) {
        (Explain::Beginner, "measure") => " (exact numbers, not guesses from the picture)",
        (Explain::Beginner, "check_fit") => " (parts that should fit together must not overlap)",
        (Explain::Beginner, "inspect_part") => " (exact sizes before changing anything)",
        (Explain::Beginner, "fastener_info") => " (from the standard tables, not from memory)",
        (Explain::Beginner, "edit_joint") if input["at"].is_number() => " (to check that pose)",
        (Explain::Beginner, "check_motion") => " (a moving part must not hit anything on its way)",
        _ => "",
    };
    Some(format!("{line}{extra}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_caption_stays_while_working_then_fades() {
        let mut caption = Caption::default();
        assert!(caption.shown().is_none());
        caption.finish();
        assert!(caption.shown().is_none(), "finishing nothing shows nothing");
        caption.set("Adding a bore".into());
        assert_eq!(caption.shown(), Some(("Adding a bore", 1.0)));
        caption.finish();
        let (_, alpha) = caption.shown().unwrap();
        assert!(alpha > 0.9, "still clearly visible just after finishing");
        caption.ended = Some(Instant::now() - LINGER);
        assert!(caption.shown().is_none(), "gone once it has lingered");
    }

    #[test]
    fn every_tool_has_a_purpose() {
        for tool in intercraft_tools::chat_panel_definitions().as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            assert!(!tool_purpose(name).is_empty(), "no plain description for {name}");
        }
    }

    #[test]
    fn captions_follow_the_explanation_level() {
        let input = json!({ "part": "Bracket", "label": "Rod bore", "why": "Sized for an 8 mm rod to slide in." });
        assert_eq!(
            caption("add_feature", &input, Explain::Beginner).unwrap(),
            "Adding Rod bore on Bracket: Sized for an 8 mm rod to slide in."
        );
        assert_eq!(caption("add_feature", &input, Explain::Expert).unwrap(), "Adding Rod bore on Bracket");
        let value = json!({ "part": "Bracket", "feature_id": "f020", "name": "bore_d", "value": 9.0 });
        assert_eq!(caption("set_param", &value, Explain::Standard).unwrap(), "Changing bore_d to 9 mm");
        assert!(caption("measure", &json!({}), Explain::Beginner).unwrap().contains("exact numbers"));
        assert_eq!(caption("measure", &json!({}), Explain::Expert).unwrap(), "Measuring");
        assert!(caption("get_project", &json!({}), Explain::Beginner).is_none());
    }
}
