// Checking a joint through its whole range: where the moving parts would be at each
// value, and a plain summary of what the worker found there. Shared by the
// check_motion tool and the viewer's Motion panel.

use intercraft_core::{Joint, Pose, Project};
use serde_json::{json, Value};

pub const DEFAULT_STEPS: usize = 19;
pub const MAX_STEPS: usize = 61;

/// Evenly spaced values from one end of the range to the other.
pub fn sweep_values(joint: &Joint, steps: usize) -> Vec<f64> {
    let steps = steps.clamp(2, MAX_STEPS);
    (0..steps).map(|i| joint.min + (joint.max - joint.min) * i as f64 / (steps - 1) as f64).collect()
}

/// What the worker needs: each built part's mesh key, and for the parts the joint
/// moves, one extra move per value from where their mesh was built (`built`: part,
/// mesh key, pose of that mesh) to where the joint would put them.
pub fn sweep_parts(project: &Project, joint: &str, values: &[f64], built: &[(String, String, Pose)]) -> Value {
    let moving = project.moved_by(joint);
    let mut parts = Vec::new();
    for (name, key, mesh_pose) in built {
        let Some(part) = project.parts.iter().find(|p| p.name == *name) else { continue };
        if !moving.contains(name) {
            parts.push(json!({ "name": name, "mesh": key }));
            continue;
        }
        let moves: Vec<[f64; 6]> = values
            .iter()
            .map(|v| {
                let mut posed = project.clone();
                if let Some(j) = posed.joints.iter_mut().find(|j| j.name.eq_ignore_ascii_case(joint)) {
                    j.at = *v;
                }
                mesh_pose.inverse().then(&posed.shown_pose(part)).to_placement()
            })
            .collect();
        parts.push(json!({ "name": name, "mesh": key, "moves": moves }));
    }
    json!(parts)
}

/// The worker's findings in a form a person (or a model) can act on.
pub fn summarise(joint: &Joint, data: &Value) -> Value {
    let unit = joint.kind.unit();
    let plain = |v: f64| {
        let r = (v * 10.0).round() / 10.0;
        if r.fract() == 0.0 { format!("{r:.0}") } else { format!("{r}") }
    };
    let num = |v: f64| format!("{}{unit}", plain(v));
    let steps = data["steps"].as_array().cloned().unwrap_or_default();
    let every = if steps.len() > 1 { (joint.max - joint.min) / (steps.len() - 1) as f64 } else { 0.0 };

    // Runs of neighbouring values that clash, each with who hits whom and the worst overlap.
    let mut ranges: Vec<Value> = Vec::new();
    let mut current: Option<(f64, f64, Vec<String>, f64, f64)> = None;
    for step in &steps {
        let at = step["at"].as_f64().unwrap_or(0.0);
        let clashes = step["clashes"].as_array().cloned().unwrap_or_default();
        if clashes.is_empty() {
            if let Some(range) = current.take() {
                ranges.push(range_json(range));
            }
            continue;
        }
        let (from, mut pairs, mut worst, mut worst_at) = match current.take() {
            Some((from, _, pairs, worst, worst_at)) => (from, pairs, worst, worst_at),
            None => (at, Vec::new(), 0.0, at),
        };
        for clash in &clashes {
            let pair = format!("{} and {}", clash["a"].as_str().unwrap_or("?"), clash["b"].as_str().unwrap_or("?"));
            if !pairs.contains(&pair) {
                pairs.push(pair);
            }
            let volume = clash["overlap_volume"].as_f64().unwrap_or(0.0);
            if volume > worst {
                worst = volume;
                worst_at = at;
            }
        }
        current = Some((from, at, pairs, worst, worst_at));
    }
    if let Some(range) = current {
        ranges.push(range_json(range));
    }

    // Every gap at every step. Parts that touch (a lid resting on its box) are listed
    // apart, so they don't hide the smallest real gap.
    let mut closest: Option<(f64, String, String, f64)> = None;
    let mut touching: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let at = step["at"].as_f64().unwrap_or(0.0);
        for gap in step["gaps"].as_array().into_iter().flatten() {
            let (a, b) = (gap["a"].as_str().unwrap_or("?"), gap["b"].as_str().unwrap_or("?"));
            let clearance = gap["clearance"].as_f64().unwrap_or(f64::MAX);
            if clearance < TOUCHING {
                let pair = format!("{a} touches {b}");
                match touching.iter_mut().find(|(p, _)| *p == pair) {
                    Some((_, at)) => at.push(i),
                    None => touching.push((pair, vec![i])),
                }
            } else if closest.as_ref().is_none_or(|c| clearance < c.3) {
                closest = Some((at, a.to_owned(), b.to_owned(), clearance));
            }
        }
    }
    let at_of = |i: usize| steps[i]["at"].as_f64().unwrap_or(0.0);
    let touch_text: Vec<String> = touching
        .iter()
        .map(|(pair, indices)| {
            let whole = indices.len() == steps.len();
            let span = if whole {
                "all the way".to_owned()
            } else if indices.len() == 1 {
                format!("at {}", num(at_of(indices[0])))
            } else {
                // Shown as one span from first to last; a gap in between is unusual.
                format!("from {} to {}", num(at_of(indices[0])), num(at_of(*indices.last().unwrap_or(&0))))
            };
            format!("{pair} {span}")
        })
        .collect();

    let mut summary = if ranges.is_empty() {
        let mut text = format!("{} moves freely from {} to {}.", joint.name, num(joint.min), num(joint.max));
        if let Some((at, a, b, clearance)) = &closest {
            text += &format!(" Closest gap {} mm, between {a} and {b} at {}.", plain_mm(*clearance), num(*at));
        }
        text
    } else {
        let parts: Vec<String> = ranges
            .iter()
            .map(|r| {
                let (from, to) = (r["from"].as_f64().unwrap_or(0.0), r["to"].as_f64().unwrap_or(0.0));
                let span = if from == to { format!("at {}", num(from)) } else { format!("from {} to {}", num(from), num(to)) };
                let pairs: Vec<&str> = r["parts"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                format!(
                    "{} clash {span} (up to {} mm³ overlap at {})",
                    pairs.join(", "),
                    plain(r["max_overlap"].as_f64().unwrap_or(0.0)),
                    num(r["max_overlap_at"].as_f64().unwrap_or(0.0))
                )
            })
            .collect();
        format!("{}: {}.", joint.name, parts.join("; "))
    };
    if !touch_text.is_empty() {
        summary += &format!(" {}.", touch_text.join("; "));
    }
    let mut result = json!({
        "joint": joint.summary(),
        "clear": ranges.is_empty(),
        "summary": summary,
        "clash_ranges": ranges,
        "checked": steps.len(),
        "note": format!("Checked every {}. A clash narrower than that can slip between checks.", num(every)),
    });
    if let Some((at, a, b, clearance)) = closest {
        result["closest"] = json!({ "at": at, "a": a, "b": b, "clearance": clearance });
    }
    if !touch_text.is_empty() {
        result["touching"] = json!(touch_text);
    }
    result
}

/// Closer than this counts as touching, not a gap.
const TOUCHING: f64 = 0.01;

/// A gap to two decimals, without trailing zeros.
fn plain_mm(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    format!("{r}")
}

fn range_json((from, to, pairs, worst, worst_at): (f64, f64, Vec<String>, f64, f64)) -> Value {
    json!({ "from": from, "to": to, "parts": pairs, "max_overlap": worst, "max_overlap_at": worst_at })
}

#[cfg(test)]
mod tests {
    use super::*;
    use intercraft_core::JointKind;

    fn joint() -> Joint {
        Joint {
            name: "Lid hinge".into(),
            kind: JointKind::Hinge,
            base: "Box".into(),
            part: "Lid".into(),
            origin: [0.0; 3],
            axis: [1.0, 0.0, 0.0],
            min: 0.0,
            max: 120.0,
            at: 0.0,
            why: String::new(),
        }
    }

    #[test]
    fn values_cover_the_range_evenly() {
        assert_eq!(sweep_values(&joint(), 5), [0.0, 30.0, 60.0, 90.0, 120.0]);
    }

    #[test]
    fn clashes_are_grouped_into_ranges() {
        let gap = |at: f64, clearance: f64| json!({ "at": at, "clashes": [], "gaps": [{ "a": "Lid", "b": "Box", "clearance": clearance }] });
        let hit = |at: f64, v: f64| json!({ "at": at, "clashes": [{ "a": "Lid", "b": "Wall", "overlap_volume": v }], "gaps": [] });
        let data = json!({ "steps": [gap(0.0, 0.5), gap(30.0, 2.0), hit(60.0, 10.0), hit(90.0, 80.0), gap(120.0, 1.0)] });
        let result = summarise(&joint(), &data);
        assert_eq!(result["clear"], false);
        assert_eq!(result["summary"], "Lid hinge: Lid and Wall clash from 60° to 90° (up to 80 mm³ overlap at 90°).");
        assert_eq!(result["closest"]["at"], 0.0);
        assert_eq!(result["note"], "Checked every 30°. A clash narrower than that can slip between checks.");

        let free = summarise(&joint(), &json!({ "steps": [gap(0.0, 0.5), gap(120.0, 0.25)] }));
        assert_eq!(free["summary"], "Lid hinge moves freely from 0° to 120°. Closest gap 0.25 mm, between Lid and Box at 120°.");
    }

    #[test]
    fn resting_contact_does_not_hide_a_tight_gap() {
        // Shut, the lid sits on the box; all the way round, it runs 0.15 mm from the pin.
        let step = |at: f64, on_box: f64| {
            json!({ "at": at, "clashes": [], "gaps": [
                { "a": "Lid", "b": "Box", "clearance": on_box },
                { "a": "Lid", "b": "Pin", "clearance": 0.15 },
            ] })
        };
        let result = summarise(&joint(), &json!({ "steps": [step(0.0, 0.0), step(60.0, 3.0), step(120.0, 0.4)] }));
        assert_eq!(
            result["summary"],
            "Lid hinge moves freely from 0° to 120°. Closest gap 0.15 mm, between Lid and Pin at 0°. Lid touches Box at 0°."
        );
        assert_eq!(result["touching"], json!(["Lid touches Box at 0°"]));
    }
}
