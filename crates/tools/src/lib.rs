// intercraft-tools: the tools an LLM can call, and what each one does to the project.
// Shared by the MCP server and the built-in chat panel, so both behave the same.
// serde_json 1, anyhow 1, dirs 7

pub mod motion;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use intercraft_core::{FeatureEdit, Joint, JointEdit, JointKind, LoadedProject, NewFeature, Params, Part, Project};
use intercraft_geo::{ExportFormat, FeatureInput, FeatureResult, PythonWorker, RebuildRequest};
use serde_json::{json, Value};

/// How to use the tools well. Sent as MCP server instructions and as the chat system prompt.
pub const GUIDE: &str = r#"InterCraft holds parametric CAD parts as an ordered list of features. The user watches the model update in a live viewer after every tool call, so make one small change per call.

Writing a feature:
- A feature is a short build123d snippet (algebra mode). `from build123d import *` and `math` are already imported.
- The snippet reads `part` (the shape so far, None for the first feature) and must reassign `part`.
- Parameters are in the dict `p`. Put every dimension a user may want to change in params, not in the code.
- All lengths are mm. Z is up.
- Examples:
    part = Box(p["width"], p["depth"], p["height"])
    part = part - Pos(0, 0, 5) * Cylinder(p["hole_d"] / 2, 50)
    part = fillet(part.edges().filter_by(Axis.Z), p["radius"])

Design brief:
- Every project has a brief (brief.md): what it is, how it will be made, key dimensions, decisions and open items. get_project includes it. Read it before designing.
- If the brief is empty or still the blank template, start by asking the user about it. Call ask_user 2 to 4 times in the same reply, one short question per call (what it is for, what it attaches to or holds, rough size, how it will be made), then end the reply. The user answers them all at once. Record the answers with update_brief.
- Keep it current as you work: add each decision with its reason, record measured versus assumed dimensions, and keep a short list of open items. Update one section at a time.

Working method:
- Call get_project first. It returns every part, feature, snippet and current build numbers.
- Every edit returns a build report. Check it before moving on: a cut must lower the volume, an addition must raise it, and the size must be what you intended.
- If a feature fails, the error is in the report and the change stays saved. Fix it with edit_feature, or remove it with delete_feature.
- Prefer set_param for dimension changes. Later features rebuild automatically.
- Pass the user's request in `prompt` when adding a feature. It is shown on their timeline.
- Use standard design terms where they fit (fillet, chamfer, boss, rib, counterbore, countersink, clearance hole, press fit, wall thickness, overhang). The app explains them to the user when they appear in a label or why.
- Give the params plain names in `labels` (e.g. {"bore_d": "Bore diameter"}), so beginners see words rather than code names.
- Give every feature a `why`: one plain sentence on the design reason (what it does for the part, or the rule behind a number), written for someone new to CAD. For example: "Sized at 8.2 mm so the 8 mm rod slides in with a little clearance." It is shown on the timeline and teaches the user.
- State which values you measured from a report and which you assumed.

Imported parts and measuring:
- A part can start from an imported STEP file (its first step is "import"). In the first feature after it, `part` is that imported shape, so cuts, holes and fillets work on it like any other.
- Before editing an imported part, call inspect_part to find its faces, holes and their exact positions instead of guessing.
- An imported assembly can be split into one part per body. They share a group, with the file's sub-assemblies as groups inside it (a path like `Printer/Extruder`). Each body is a normal part to place, joint, check or edit.
- In a big project get_project is an outline. Use find_parts to list parts by name or group, and get_part for the full detail of the ones you work on or against. Never guess a part's name.
- Groups only organise the parts list. Make and tidy them with set_group and rename_group; move_group moves everything in a group as one piece.
- Use measure to check clearances, hole spacing and wall thickness after a change.

Assemblies:
- Each part has a placement (position and rotation) in the assembly, set with place_part. Features never see it: snippets always work in the part's own frame.
- inspect_part answers in the part's own frame. Build reports and measure answer in assembly positions.
- view_model shows you the model. Look after a change that is easy to get wrong by position or side, and before telling the user it is done.
- check_fit finds clashes and clearances between parts. Parts that should mate must not overlap.

Joints and motion:
- add_joint makes a part move against another: a hinge turns about an axis, a slider moves along one. Give the axis in assembly mm with every joint at 0, and the range (degrees or mm).
- A moving part's placement is where it sits with its joint at 0, usually closed or at the start of its travel. Place it there first, then add the joint.
- `at` is where a joint is now. Build reports, measure, check_fit and view_model all show the parts at `at`. To check that a lid clears the box when open, edit_joint it to the open angle, run check_fit, then set it back.
- Joints can chain: a part moved by one joint can be the base of another, and moves everything hanging from it.
- Moving parts need clearance: about 0.3 mm or more around a printed hinge pin, and a gap where a part swings past another. Say which in the joint's why.
- After adding or changing a joint, or a part that moves, run check_motion. It sweeps the whole range and says where anything clashes. Fix a clash (more clearance, a smaller range, a chamfer on the swinging edge) and check again before calling a mechanism done.

Standard parts and fits:
- Never size a screw hole, nut trap, insert hole or bearing seat from memory. Call fastener_info for the size (e.g. "M4") and use its numbers.
- Clearance holes: normal fit for most parts; loose for 3D printed holes, which print a little small. Heat-set insert holes: use the insert's hole diameter and make the hole a little deeper than the insert.
- Printed fits: about 0.2 to 0.3 mm of clearance for parts that slide or turn, about 0.1 mm interference for a press fit in PLA. Say which rule you used in the feature's why.
- Before saying a printed part is done, run print_check and fix what it finds, or tell the user why it is fine (for example, an overhang that will face up when printed).
- add_standard_part adds a real screw, nut, washer, insert or bearing as its own part, so check_fit can test it against your design. In snippets, standard_part(kind, size, length) and clearance_hole(size, fit) give the same parts and hole sizes.

Undo:
- Every change is recorded. If an edit went wrong, or the user asks to go back, call undo instead of rebuilding by hand. redo steps forward again.
- delete_part removes a whole part. Reference parts can only be removed by the user."#;

/// A tool result carries a picture for the model under this key, as base64 PNG.
/// The MCP server and the chat clients turn it into a real image block.
pub const IMAGE_KEY: &str = "image_png_base64";

/// How long `view_model` waits for the viewer to draw the picture.
const VIEW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// A picture the model asked for, waiting in the project's `.intercraft/view` folder
/// for the viewer to draw it.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewRequest {
    pub id: String,
    /// iso, front, back, left, right, top, bottom or current.
    pub view: String,
    pub part: Option<String>,
    /// Cut the model open: (axis 0 to 2, position in mm, flip).
    pub section: Option<(usize, f64, bool)>,
    /// How long it has been waiting.
    pub age: std::time::Duration,
}

pub const VIEWS: [&str; 8] = ["iso", "front", "back", "left", "right", "top", "bottom", "current"];

fn view_dir(project: &Path) -> PathBuf {
    project.join(".intercraft").join("view")
}

/// Requests waiting for the viewer, oldest first. Anything malformed is deleted:
/// these files sit in the project folder, so they could have been put there by anyone.
pub fn view_requests(project: &Path) -> Vec<ViewRequest> {
    let Ok(entries) = std::fs::read_dir(view_dir(project)) else { return Vec::new() };
    let mut requests = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_prefix("request-").and_then(|n| n.strip_suffix(".json")) else { continue };
        let parsed = (|| {
            if id.is_empty() || id.len() > 40 || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
                return None;
            }
            if entry.metadata().ok()?.len() > 4096 {
                return None;
            }
            let value: Value = serde_json::from_str(&std::fs::read_to_string(entry.path()).ok()?).ok()?;
            let view = value["view"].as_str().filter(|v| VIEWS.contains(v))?.to_owned();
            let section = match &value["section"] {
                Value::Null => None,
                s => Some((
                    ["x", "y", "z"].iter().position(|a| s["axis"] == *a)?,
                    s["offset"].as_f64().filter(|v| v.is_finite())?,
                    s["flip"].as_bool().unwrap_or(false),
                )),
            };
            let age = entry.metadata().ok()?.modified().ok()?.elapsed().unwrap_or_default();
            Some(ViewRequest { id: id.to_owned(), view, part: value["part"].as_str().map(str::to_owned), section, age })
        })();
        match parsed {
            Some(request) => requests.push(request),
            None => {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    requests.sort_by_key(|r| std::cmp::Reverse(r.age));
    requests
}

/// The viewer's answer: PNG bytes, or why it couldn't draw the picture.
pub fn answer_view(project: &Path, id: &str, answer: std::result::Result<&[u8], &str>) {
    let dir = view_dir(project);
    let (ext, bytes) = match answer {
        Ok(png) => ("png", png),
        Err(error) => ("err", error.as_bytes()),
    };
    // Written under another name first, so the waiting tool never reads half a file.
    let tmp = dir.join(format!("{id}.tmp"));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(format!("{id}.{ext}")));
    }
    let _ = std::fs::remove_file(dir.join(format!("request-{id}.json")));
}

pub fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Which project the tools edit.
enum Target {
    Fixed(PathBuf),
    /// Whatever the InterCraft viewer opened most recently, checked on every call.
    FollowViewer,
}

pub struct ToolHost {
    target: Target,
    worker: Option<PythonWorker>,
    /// Offer `ask_user`. Only InterCraft's own chat panel can show the question.
    ask_user: bool,
}

/// The editing tools plus `ask_user`, for agents talking through InterCraft's chat panel.
pub fn chat_panel_definitions() -> Value {
    let mut tools = definitions();
    if let Some(list) = tools.as_array_mut() {
        list.push(json!({
            "name": "ask_user",
            "description": "Ask the user to choose when the decision is genuinely theirs: design intent, a trade-off, \
or a dimension you can't measure or sensibly assume. The options appear as buttons in InterCraft's chat. \
One question per call. To ask several things, call it once for each question in the same reply (up to 4), \
then end your reply without other tool calls; all the answers arrive together as the user's next message. \
Don't use it for things you can work out from the project or the build reports.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "One clear question, ending with a question mark." },
                    "options": {
                        "type": "array",
                        "minItems": 2,
                        "maxItems": 4,
                        "description": "2 to 4 distinct choices. The user can also type their own answer.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string", "description": "Short choice, 1 to 6 words." },
                                "description": { "type": "string", "description": "What picking it means, one sentence." }
                            },
                            "required": ["label"]
                        }
                    },
                    "multi_select": { "type": "boolean", "description": "Let the user pick more than one option." }
                },
                "required": ["question", "options"]
            },
        }));
    }
    tools
}

/// Checks an `ask_user` call so the chat panel never has to draw nonsense.
pub fn validate_question(args: &Value) -> Result<()> {
    let question = text(args, "question")?.trim();
    if question.is_empty() || question.chars().count() > 300 {
        bail!("the question must be 1 to 300 characters");
    }
    let options = args.get("options").and_then(Value::as_array).context("'options' must be a list")?;
    if !(2..=4).contains(&options.len()) {
        bail!("give 2 to 4 options");
    }
    let mut labels = std::collections::HashSet::new();
    for option in options {
        let label = option["label"].as_str().map(str::trim).unwrap_or_default();
        if label.is_empty() || label.chars().count() > 60 {
            bail!("each option needs a label of 1 to 60 characters");
        }
        if !labels.insert(label.to_lowercase()) {
            bail!("two options have the label '{label}'");
        }
        if option["description"].as_str().is_some_and(|d| d.chars().count() > 200) {
            bail!("option descriptions are limited to 200 characters");
        }
    }
    if args.get("multi_select").is_some_and(|m| !m.is_boolean()) {
        bail!("'multi_select' must be true or false");
    }
    Ok(())
}

/// Small file where the viewer records the project it has open, so an MCP server
/// started by another app (Claude Code, Cursor...) edits the same project.
/// InterCraft's folder for settings and the active project. INTERCRAFT_CONFIG_DIR
/// overrides it, so tests never touch the user's real settings.
pub fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("INTERCRAFT_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(dirs::config_dir()?.join("InterCraft")),
    }
}

fn active_project_file() -> Option<PathBuf> {
    Some(config_dir()?.join("active_project.txt"))
}

/// Called by the viewer when it opens a project.
pub fn record_active_project(dir: &Path) -> Result<()> {
    let file = active_project_file().context("no config folder on this system")?;
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dir = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    std::fs::write(file, dir.to_string_lossy().as_bytes())?;
    Ok(())
}

pub fn active_project() -> Option<PathBuf> {
    let text = std::fs::read_to_string(active_project_file()?).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

pub fn definitions() -> Value {
    let part = json!({ "type": "string", "description": "Part name" });
    let feature_id = json!({ "type": "string", "description": "Feature id, e.g. f020" });
    let params = json!({
        "type": "object",
        "description": "Named dimensions for the snippet, read as p[\"name\"]. Lengths in mm.",
    });
    let code = json!({
        "type": "string",
        "description": "build123d snippet that reads and reassigns `part`. Parameters are in `p`.",
    });

    json!([
        {
            "name": "get_project",
            "description": "Read the project: brief, joints and parts. A small project comes back whole: every part and feature with its snippet, parameters and build numbers (volume, size, errors). A big one comes back as an outline of groups and one line per part (size and centre in assembly mm); use get_part and find_parts for more. Call this first.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "get_part",
            "description": "One part in full: every step with its snippet, parameters and build numbers, its placement, group and joints. Use it for the parts you are about to work on or against.",
            "inputSchema": { "type": "object", "properties": { "part": part }, "required": ["part"] },
        },
        {
            "name": "find_parts",
            "description": "List parts by name, group or both, one short line each (size and centre in assembly mm). `name` matches any part whose name contains it, ignoring case, so 'motor' finds every motor. `group` takes a group path and includes the groups inside it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Text the part's name contains." },
                    "group": { "type": "string", "description": "A group path, such as 'Printer' or 'Printer/Extruder'." },
                },
            },
        },
        {
            "name": "set_group",
            "description": "Put parts in a group in the parts list, to keep a big project tidy. `group` is a path, so 'Printer/Extruder' is a group inside 'Printer'; it is made if it doesn't exist. Leave `group` out to take the parts out of any group. Grouping changes nothing about the parts themselves.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parts": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "group": { "type": "string", "description": "Group path. Omit for no group." },
                },
                "required": ["parts"],
            },
        },
        {
            "name": "rename_group",
            "description": "Rename a group or move it with everything inside. `new_path` is the whole new path: 'Printer/Extruder' to 'Printer/Sprite' renames it, to 'Extruder' lifts it out of 'Printer', to 'Machine/Extruder' moves it into 'Machine'. Leave `new_path` out to dissolve the group: its parts and groups move up one level.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "group": { "type": "string", "description": "The group's current path." },
                    "new_path": { "type": "string" },
                },
                "required": ["group"],
            },
        },
        {
            "name": "add_part",
            "description": "Create a new empty design part. Names may use letters, digits, '_' and '-'.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": part },
                "required": ["name"],
            },
        },
        {
            "name": "add_feature",
            "description": "Add one modelling step to a part and build it. Returns the build report for the part.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "label": { "type": "string", "description": "Short name shown in the hierarchy, e.g. 'Rod bore'" },
                    "code": code,
                    "params": params,
                    "prompt": { "type": "string", "description": "The user's request that led to this feature. Shown on their timeline." },
                    "why": { "type": "string", "description": "One plain sentence on the design reason behind this feature, for someone new to CAD." },
                    "labels": { "type": "object", "description": "Plain names for the params, shown to beginners, e.g. {\"bore_d\": \"Bore diameter\"}. Up to 40 characters each." },
                    "after": { "type": "string", "description": "Insert after this feature id. Omit to add at the end." },
                },
                "required": ["part", "label", "code"],
            },
        },
        {
            "name": "edit_feature",
            "description": "Change an existing feature and rebuild from it onwards. Only the fields you pass are changed. `params` replaces the whole parameter set.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "feature_id": feature_id,
                    "label": { "type": "string" },
                    "code": code,
                    "params": params,
                    "prompt": { "type": "string" },
                    "why": { "type": "string", "description": "One plain sentence on the design reason." },
                    "labels": { "type": "object", "description": "Plain names for the params. Replaces all of them." },
                },
                "required": ["part", "feature_id"],
            },
        },
        {
            "name": "set_param",
            "description": "Change one parameter of a feature and rebuild. The cheapest way to change a dimension.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "feature_id": feature_id,
                    "name": { "type": "string" },
                    "value": { "description": "New value. Usually a number in mm." },
                },
                "required": ["part", "feature_id", "name", "value"],
            },
        },
        {
            "name": "move_feature",
            "description": "Change the order of a part's features and rebuild. Order matters: a fillet added before a cut rounds different edges than one added after.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "feature_id": feature_id,
                    "after": { "type": "string", "description": "Put it straight after this feature id. Omit to make it the first feature." },
                },
                "required": ["part", "feature_id"],
            },
        },
        {
            "name": "delete_feature",
            "description": "Remove a feature and rebuild the features after it.",
            "inputSchema": {
                "type": "object",
                "properties": { "part": part, "feature_id": feature_id },
                "required": ["part", "feature_id"],
            },
        },
        {
            "name": "update_brief",
            "description": "Replace one section of the project's design brief, or add it if missing. Sections are '## ' headings: What it is, How it will be made, Key dimensions, Decisions, Open items, or your own. Pass the section's full new text without its heading.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "section": { "type": "string", "description": "Heading of the section, e.g. 'Decisions'." },
                    "text": { "type": "string", "description": "The section's new text, in Markdown. No '## ' headings inside it." },
                },
                "required": ["section", "text"],
            },
        },
        {
            "name": "place_part",
            "description": "Move or turn a design part in the assembly without changing its features. Position is [x, y, z] in mm and rotation is [rx, ry, rz] in degrees about X, Y and Z, both absolute. Pass only the one you want to change.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "position": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "rotation": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                },
                "required": ["part"],
            },
        },
        {
            "name": "move_group",
            "description": "Move a whole imported group as one rigid piece. Every part is turned by rotation [rx, ry, rz] (degrees about X, Y and Z through the assembly origin) and then shifted by position [x, y, z] mm, on top of where it is now. A model that came in Y-up stands upright with rotation [90, 0, 0].",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "group": { "type": "string", "description": "The group name, as get_project shows it." },
                    "position": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "rotation": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                },
                "required": ["group"],
            },
        },
        {
            "name": "duplicate_part",
            "description": "Copy a part with all its features. The copy is a design part, so copying a reference part gives you something you can change.",
            "inputSchema": {
                "type": "object",
                "properties": { "part": part, "new_name": { "type": "string", "description": "Name for the copy. Omit for '<name>_copy'." } },
                "required": ["part"],
            },
        },
        {
            "name": "rename_part",
            "description": "Give a design part a new name.",
            "inputSchema": {
                "type": "object",
                "properties": { "part": part, "new_name": { "type": "string" } },
                "required": ["part", "new_name"],
            },
        },
        {
            "name": "delete_part",
            "description": "Remove a whole design part and its features. It can be brought back with undo.",
            "inputSchema": {
                "type": "object",
                "properties": { "part": part },
                "required": ["part"],
            },
        },
        {
            "name": "undo",
            "description": "Undo the most recent change to the project, whoever made it (you, the user or another agent). Returns what was undone.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "redo",
            "description": "Redo the change most recently undone. Fails if the project changed since the undo.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "inspect_part",
            "description": "Describe a part's built shape precisely: volume, bounding box, cylindrical faces (holes and bosses) with diameter and axis, circular edge diameters, and the largest faces with type, area, centre and normal. Face and edge indices match measure. Use it before editing an imported part or when you need exact positions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "feature_id": { "type": "string", "description": "Inspect the shape as it was after this step. Omit for the latest." },
                },
                "required": ["part"],
            },
        },
        {
            "name": "measure",
            "description": "Measure exactly with the CAD kernel. With 'a' only: describe one face, edge or vertex (length, radius, diameter, area, normal). With 'a' and 'b': the minimum distance between them, the closest points, the X/Y/Z offset, and the angle when both are straight edges or flat faces. Each selection is {\"kind\": \"face\"|\"edge\"|\"vertex\", \"index\": n, \"part\": optional} using indices from inspect_part, or {\"point\": [x, y, z]}.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": { "type": "string", "description": "Default part for selections that don't name one." },
                    "a": { "type": "object", "description": "First selection." },
                    "b": { "type": "object", "description": "Second selection. Omit to describe 'a' alone." },
                },
                "required": ["a"],
            },
        },
        {
            "name": "view_model",
            "description": "Look at the model: returns a picture from InterCraft's viewer. Use it to check that a shape looks right (a hole on the correct face, a fillet on the intended edges), which numbers alone can miss. Each part has its own colour and reference parts are grey. Pass a section to see inside. Pictures are for checking shape; take dimensions from build reports, inspect_part and measure.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "view": { "type": "string", "enum": VIEWS, "description": "Camera direction. The six side views are flat like a drawing, iso has perspective, and 'current' is what the user is looking at. Default iso." },
                    "part": { "type": "string", "description": "Show only this part. Omit to see the whole assembly." },
                    "section": {
                        "type": "object",
                        "description": "Cut the model open with a plane across one axis.",
                        "properties": {
                            "axis": { "type": "string", "enum": ["x", "y", "z"] },
                            "offset": { "type": "number", "description": "Where the plane sits on that axis, in mm (assembly position)." },
                            "flip": { "type": "boolean", "description": "Cut away the other side. By default the side with larger coordinates is removed." },
                        },
                        "required": ["axis", "offset"],
                    },
                },
            },
        },
        {
            "name": "print_check",
            "description": "Check a part for 3D printing: walls thinner than the minimum (default 1.2 mm), round holes under 2 mm, and surfaces overhanging more than 45 degrees that would need support, each with its position, plus a rough PLA weight. Positions are in the part's own frame. Run it before calling a printed part done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "min_wall": { "type": "number", "description": "Thinnest wall allowed, in mm. About three times the nozzle width; 1.2 for a 0.4 mm nozzle." },
                    "up": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "Which way is up on the printer, in the part's frame. Default [0, 0, 1]." },
                },
                "required": ["part"],
            },
        },
        {
            "name": "fastener_info",
            "description": "Real sizes for a metric screw size from the ISO tables: clearance hole diameters (close, normal, loose), tap drill, socket head diameter and height, hex nut across flats and thickness, and the heat-set insert hole. Use it instead of remembering sizes.",
            "inputSchema": {
                "type": "object",
                "properties": { "size": { "type": "string", "description": "Metric size, e.g. 'M3', 'M4' or 'M5'." } },
                "required": ["size"],
            },
        },
        {
            "name": "add_standard_part",
            "description": "Add a real standard part (screw, nut, washer, heat-set insert or ball bearing) as its own part with ISO dimensions, to design around and check fits against. Screws hang down from the origin with the head on top; use position and rotation to put it in place.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": STANDARD_KINDS },
                    "size": { "type": "string", "description": "e.g. 'M4'. Bearings: bore-outside-width, e.g. 'M8-22-7'." },
                    "length": { "type": "number", "description": "Screw length in mm. Screws only." },
                    "name": { "type": "string", "description": "Part name. Omit for one made from the kind and size." },
                    "position": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "rotation": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                },
                "required": ["kind", "size"],
            },
        },
        {
            "name": "check_fit",
            "description": "Check how parts fit together in the assembly. For every pair it reports either an overlap (the clashing volume in mm3 and where it is) or the clearance (smallest gap in mm and the two closest points). Run it after placing parts or changing mating features.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "parts": { "type": "array", "items": { "type": "string" }, "description": "Part names to check. Omit for all parts." },
                },
            },
        },
        {
            "name": "add_joint",
            "description": "Let one part move against another. A hinge turns the moving part about an axis (lids, doors, levers); a slider moves it along one (drawers, carriages, plungers). Give the axis in assembly mm with every joint at 0. The moving part's placement is its position at 0. Returns where the moved parts now sit.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "A short plain name, e.g. 'Lid hinge'." },
                    "kind": { "type": "string", "enum": ["hinge", "slider"] },
                    "base": { "type": "string", "description": "The part the joint is fixed to." },
                    "part": { "type": "string", "description": "The part that moves. A part can have only one joint that moves it, but it can be the base of others, so joints can chain." },
                    "origin": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "A point on the axis, e.g. the centre of the hinge pin, in assembly mm." },
                    "axis": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3, "description": "Axis direction. A hinge turns right-handed about it: thumb along the axis, fingers show positive angles." },
                    "min": { "type": "number", "description": "Lower end of the range: degrees for a hinge, mm for a slider." },
                    "max": { "type": "number", "description": "Upper end of the range." },
                    "at": { "type": "number", "description": "Where to set it now. Default 0, or the nearest end of the range." },
                    "why": { "type": "string", "description": "One plain sentence on what the motion is for." },
                },
                "required": ["name", "kind", "base", "part", "origin", "axis", "min", "max"],
            },
        },
        {
            "name": "edit_joint",
            "description": "Change a joint. Most often used to move it: set `at` to open a hinge or slide a slider, then check_fit or view_model to see that pose. Pass only the fields to change.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "The joint to change." },
                    "at": { "type": "number", "description": "New position within the range: degrees or mm." },
                    "kind": { "type": "string", "enum": ["hinge", "slider"] },
                    "base": { "type": "string" },
                    "part": { "type": "string" },
                    "origin": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "axis": { "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 3 },
                    "min": { "type": "number" },
                    "max": { "type": "number" },
                    "why": { "type": "string" },
                },
                "required": ["name"],
            },
        },
        {
            "name": "check_motion",
            "description": "Move a joint through its whole range and check at each step whether the parts it moves hit anything. Reports the ranges that clash (which parts, and the worst overlap) or, if it moves freely, the closest gap and where. Run it after adding or changing a joint, and after changing a part that moves.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "joint": { "type": "string", "description": "The joint to check." },
                    "steps": { "type": "integer", "minimum": 2, "maximum": motion::MAX_STEPS, "description": "How many positions to check across the range. Default 19." },
                },
                "required": ["joint"],
            },
        },
        {
            "name": "delete_joint",
            "description": "Remove a joint. The parts stay where its value 0 puts them.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"],
            },
        },
        {
            "name": "export_part",
            "description": "Write the finished part to the project's exports folder. STEP is exact and editable, STL is for printing. Omit `format` to write both.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "part": part,
                    "format": { "type": "string", "enum": ["step", "stl"] },
                },
                "required": ["part"],
            },
        },
    ])
}

impl ToolHost {
    /// The geometry worker starts on the first tool call. Call `warm_up` to start it sooner.
    pub fn new(dir: PathBuf) -> Self {
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        Self { target: Target::Fixed(dir), worker: None, ask_user: false }
    }

    /// Also accept `ask_user` calls (InterCraft's chat panel shows them).
    pub fn with_ask_user(mut self) -> Self {
        self.ask_user = true;
        self
    }

    /// Edit whichever project the viewer has open.
    pub fn follow_viewer() -> Self {
        Self { target: Target::FollowViewer, worker: None, ask_user: false }
    }

    /// Start the worker now so the first tool call doesn't wait for OpenCascade to load.
    pub fn warm_up(&mut self) {
        if self.worker.is_none() {
            self.worker = PythonWorker::spawn(|| {}).ok();
        }
    }

    pub fn project_dir(&self) -> Result<PathBuf> {
        match &self.target {
            Target::Fixed(dir) => Ok(dir.clone()),
            Target::FollowViewer => active_project().context(
                "no InterCraft project is open. Ask the user to start the viewer with `cargo run -- <project folder>` and try again",
            ),
        }
    }

    pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        if name == "ask_user" && self.ask_user {
            validate_question(args)?;
            return Ok(json!({
                "shown": true,
                "next": "The question is on the user's screen. Ask any other questions now with more ask_user calls, then end your reply without other tool calls. The answers arrive together as their next message.",
            }));
        }
        // One tool call at a time per project, across processes, so two agents
        // (say the chat panel and a Claude Code terminal) can't overwrite each other.
        let _lock = self.lock_project()?;
        let mut project = self.load()?;
        match name {
            "get_project" => {
                let all = &project.project.parts;
                let mut result = json!({
                    "name": project.project.name,
                    "units": project.project.units,
                    "folder": project.dir,
                    "brief": project.brief,
                    "joints": project.project.joints,
                });
                if all.len() <= FULL_PROJECT_PARTS {
                    // Small enough to give everything in one go.
                    let mut parts = Vec::new();
                    for part in all {
                        let report = self.build_report(&project, part, true)?;
                        let untouched = part.group.is_some() && part.features.is_empty();
                        parts.push(if untouched { brief_report(report) } else { report });
                    }
                    result["parts"] = json!(parts);
                    return Ok(result);
                }
                // A big project: an outline that always fits, with detail on request.
                let mut lines = Vec::new();
                for part in all {
                    lines.push((part, self.build_report(&project, part, false)?));
                }
                result["groups"] = json!(group_outline(&project.groups(), &lines));
                if all.len() <= LISTED_PARTS {
                    result["parts"] = json!(lines.iter().map(|(p, r)| part_line(p, r)).collect::<Vec<_>>());
                } else {
                    let loose: Vec<Value> = lines.iter().filter(|(p, _)| p.group.is_none()).map(|(p, r)| part_line(p, r)).collect();
                    result["parts_outside_groups"] = json!(loose);
                }
                result["note"] = format!(
                    "{} parts, so this is an outline: each part's size and centre in assembly mm. \
                     get_part gives one part in full (steps, code, numbers). find_parts lists parts by name or group.",
                    all.len()
                )
                .into();
                Ok(result)
            }
            "get_part" => {
                let part = project.part(text(args, "part")?)?.clone();
                self.build_report(&project, &part, true)
            }
            "find_parts" => {
                let name = optional_text(args, "name").map(str::to_ascii_lowercase);
                let group = optional_text(args, "group");
                if name.is_none() && group.is_none() {
                    bail!("give a name to look for, a group, or both");
                }
                let mut found = Vec::new();
                let mut more = 0;
                for part in &project.project.parts {
                    let named = name.as_ref().is_none_or(|n| part.name.to_ascii_lowercase().contains(n.as_str()));
                    let grouped = group.is_none_or(|g| intercraft_core::in_group(part.group.as_deref(), g));
                    if !(named && grouped) {
                        continue;
                    }
                    if found.len() == LISTED_PARTS {
                        more += 1;
                        continue;
                    }
                    let report = self.build_report(&project, part, false)?;
                    found.push(part_line(part, &report));
                }
                let mut result = json!({ "found": found.len() + more, "parts": found });
                if more > 0 {
                    result["note"] = format!("{more} more not shown. Narrow the name or group.").into();
                }
                // A near miss on a group name is the usual reason for nothing found.
                if found.is_empty() {
                    result["groups"] = json!(project.groups());
                }
                Ok(result)
            }
            "set_group" => {
                let parts: Vec<String> = args["parts"]
                    .as_array()
                    .context("`parts` must be a list of part names")?
                    .iter()
                    .map(|p| p.as_str().map(str::to_owned).context("`parts` must be a list of part names"))
                    .collect::<Result<_>>()?;
                let group = optional_text(args, "group").filter(|g| !g.trim().is_empty());
                project.set_group(&parts, group)?;
                project.save()?;
                Ok(json!({ "grouped": parts.len(), "group": group, "groups": project.groups() }))
            }
            "rename_group" => {
                let group = text(args, "group")?;
                let new = optional_text(args, "new_path").unwrap_or_default();
                project.rename_group(group, new)?;
                project.save()?;
                Ok(json!({ "was": group, "now": new, "groups": project.groups() }))
            }
            "add_part" => {
                let name = text(args, "name")?;
                project.add_part(name)?;
                project.save()?;
                Ok(json!({ "added_part": name }))
            }
            "add_feature" => {
                let part = text(args, "part")?;
                let id = project.add_feature(
                    part,
                    NewFeature {
                        label: text(args, "label")?.to_owned(),
                        code: text(args, "code")?.to_owned(),
                        params: optional_params(args)?.unwrap_or_default(),
                        prompt: optional_text(args, "prompt").unwrap_or_default().to_owned(),
                        why: optional_text(args, "why").unwrap_or_default().to_owned(),
                        labels: optional_labels(args)?.unwrap_or_default(),
                        after: optional_text(args, "after").map(str::to_owned),
                    },
                )?;
                project.save()?;
                let mut report = self.build_report(&project, project.part(part)?, false)?;
                report["added_feature"] = id.into();
                Ok(report)
            }
            "edit_feature" => {
                let part = text(args, "part")?;
                project.edit_feature(
                    part,
                    text(args, "feature_id")?,
                    FeatureEdit {
                        label: optional_text(args, "label").map(str::to_owned),
                        code: optional_text(args, "code").map(str::to_owned),
                        params: optional_params(args)?,
                        prompt: optional_text(args, "prompt").map(str::to_owned),
                        why: optional_text(args, "why").map(str::to_owned),
                        labels: optional_labels(args)?,
                    },
                )?;
                project.save()?;
                self.build_report(&project, project.part(part)?, false)
            }
            "set_param" => {
                let part = text(args, "part")?;
                let value = args.get("value").cloned().context("missing 'value'")?;
                project.set_param(part, text(args, "feature_id")?, text(args, "name")?, value)?;
                project.save()?;
                self.build_report(&project, project.part(part)?, false)
            }
            "move_feature" => {
                let part = text(args, "part")?;
                project.move_feature(part, text(args, "feature_id")?, optional_text(args, "after"))?;
                project.save()?;
                self.build_report(&project, project.part(part)?, false)
            }
            "delete_feature" => {
                let part = text(args, "part")?;
                project.delete_feature(part, text(args, "feature_id")?)?;
                project.save()?;
                self.build_report(&project, project.part(part)?, false)
            }
            "update_brief" => {
                project.update_brief_section(text(args, "section")?, text(args, "text")?)?;
                project.save()?;
                Ok(json!({ "brief": project.brief }))
            }
            "rename_part" => {
                let (old, new) = (text(args, "part")?, text(args, "new_name")?);
                self.refuse_reference(&project, old)?;
                project.rename_part(old, new)?;
                project.save()?;
                Ok(json!({ "renamed": old, "to": new }))
            }
            "duplicate_part" => {
                let name = project.duplicate_part(text(args, "part")?, optional_text(args, "new_name"))?;
                project.save()?;
                let mut report = self.build_report(&project, project.part(&name)?, false)?;
                report["duplicated_as"] = name.into();
                Ok(report)
            }
            "place_part" => {
                let name = text(args, "part")?;
                self.refuse_reference(&project, name)?;
                let part = project.part(name)?;
                let position = optional_triple(args, "position")?.unwrap_or(part.position);
                let rotation = optional_triple(args, "rotation")?.unwrap_or(part.rotation);
                project.set_placement(name, position, rotation)?;
                project.save()?;
                self.build_report(&project, project.part(name)?, false)
            }
            "move_group" => {
                let group = text(args, "group")?;
                let position = optional_triple(args, "position")?.unwrap_or([0.0; 3]);
                let rotation = optional_triple(args, "rotation")?.unwrap_or([0.0; 3]);
                project.move_group(group, position, rotation)?;
                project.save()?;
                let moved = project.project.parts.iter().filter(|p| intercraft_core::in_group(p.group.as_deref(), group)).count();
                Ok(json!({ "moved_group": group, "parts": moved, "note": "Call get_project to see where each part sits now." }))
            }
            "delete_part" => {
                let name = text(args, "part")?;
                self.refuse_reference(&project, name)?;
                project.remove_part(name)?;
                project.save()?;
                Ok(json!({ "deleted_part": name, "undo": "call undo to bring it back" }))
            }
            "undo" => Ok(json!({ "undone": project.undo()? })),
            "redo" => Ok(json!({ "redone": project.redo()? })),
            "inspect_part" => {
                let part = project.part(text(args, "part")?)?.clone();
                // In the part's own frame: that is where its features work.
                let key = self.built_key(&project, &part, optional_text(args, "feature_id"), false)?;
                let mut data = self.with_worker(|w| w.inspect_blocking(&key))?;
                if part.placement().is_some() {
                    data["frame"] = format!(
                        "Positions are in the part's own frame, the one its features use. In the assembly the part is moved to {:?} mm and rotated {:?} degrees, and measure works in assembly positions.",
                        part.position, part.rotation
                    )
                    .into();
                }
                data["part"] = part.name.into();
                Ok(data)
            }
            "measure" => {
                let default_part = optional_text(args, "part").map(str::to_owned);
                let mut key_for = |name: &str| -> Result<String> {
                    let part = project.part(name)?;
                    self.built_key(&project, part, None, true)
                };
                let a = selection(&mut key_for, default_part.as_deref(), args.get("a").context("missing 'a'")?)?;
                let b = match args.get("b") {
                    Some(b) if !b.is_null() => Some(selection(&mut key_for, default_part.as_deref(), b)?),
                    _ => None,
                };
                self.with_worker(|w| w.measure_blocking(a, b))
            }
            "view_model" => {
                let view = optional_text(args, "view").unwrap_or("iso");
                if !VIEWS.contains(&view) {
                    bail!("unknown view '{view}'. Use one of: {}", VIEWS.join(", "));
                }
                if let Some(part) = optional_text(args, "part") {
                    project.part(part)?;
                }
                let png = request_view(&project.dir, view, args)?;
                let facing = match view {
                    "iso" => "From the front right, above. Z is up.",
                    "front" => "From the front (the -Y side), looking along +Y. X runs to the right, Z is up.",
                    "back" => "From the back (the +Y side), looking along -Y. X runs to the left, Z is up.",
                    "right" => "From the right (the +X side), looking along -X. Y runs to the right, Z is up.",
                    "left" => "From the left (the -X side), looking along +X. Y runs to the left, Z is up.",
                    "top" => "From above, looking down -Z. X runs to the right, Y is up the image.",
                    "bottom" => "From below, looking up +Z. X runs to the right.",
                    _ => "The user's own view, with whatever they have hidden or cut.",
                };
                let mut result = json!({ "view": view, "facing": facing, IMAGE_KEY: base64(&png) });
                if !matches!(view, "iso" | "current") {
                    result["projection"] = "Flat, without perspective: shapes keep their true proportions and you can't see the walls of a hole that runs straight through.".into();
                }
                Ok(result)
            }
            "print_check" => {
                let part = project.part(text(args, "part")?)?.clone();
                let min_wall = args.get("min_wall").and_then(Value::as_f64).unwrap_or(1.2);
                if !(0.2..=20.0).contains(&min_wall) {
                    bail!("min_wall must be between 0.2 and 20 mm");
                }
                let up = optional_triple(args, "up")?.unwrap_or([0.0, 0.0, 1.0]);
                if up.iter().all(|v| *v == 0.0) {
                    bail!("'up' must point somewhere, like [0, 0, 1]");
                }
                let key = self.built_key(&project, &part, None, false)?;
                let mut data = self.with_worker(|w| w.print_check_blocking(&key, min_wall, up))?;
                data["part"] = part.name.into();
                Ok(data)
            }
            "fastener_info" => {
                let size = standard_size(text(args, "size")?)?;
                self.with_worker(|w| w.fastener_info_blocking(&size))
            }
            "add_standard_part" => {
                let kind = text(args, "kind")?;
                if !STANDARD_KINDS.contains(&kind) {
                    bail!("unknown kind '{kind}'. Use one of: {}", STANDARD_KINDS.join(", "));
                }
                let size = standard_size(text(args, "size")?)?;
                let screw = kind.ends_with("screw");
                let length = args.get("length").and_then(Value::as_f64);
                let code = match (screw, length) {
                    (true, Some(l)) if l > 0.0 && l <= 500.0 => format!("part = standard_part({kind:?}, {size:?}, {l})"),
                    (true, _) => bail!("a screw needs a length in mm, from 1 to 500"),
                    (false, _) => format!("part = standard_part({kind:?}, {size:?})"),
                };
                let label = match length {
                    Some(l) if screw => format!("{size} x {} {kind}", round(l)),
                    _ => format!("{size} {kind}"),
                };
                let name = match optional_text(args, "name") {
                    Some(name) => name.to_owned(),
                    None => {
                        let base: String = format!("{size}_{kind}").chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
                        let taken = |n: &str| project.project.parts.iter().any(|p| p.name.eq_ignore_ascii_case(n));
                        (1..).map(|i| if i == 1 { base.clone() } else { format!("{base}_{i}") }).find(|n| !taken(n)).unwrap_or(base)
                    }
                };
                project.add_part(&name)?;
                let new = NewFeature {
                    label: label.chars().take(80).collect(),
                    code,
                    why: "A standard part with its real ISO dimensions, to design around and check fits against.".to_owned(),
                    prompt: optional_text(args, "prompt").unwrap_or_default().to_owned(),
                    ..Default::default()
                };
                project.add_feature(&name, new)?;
                let position = optional_triple(args, "position")?.unwrap_or([0.0; 3]);
                let rotation = optional_triple(args, "rotation")?.unwrap_or([0.0; 3]);
                project.set_placement(&name, position, rotation)?;
                project.save()?;
                let mut report = self.build_report(&project, project.part(&name)?, false)?;
                report["added_part"] = name.into();
                Ok(report)
            }
            "check_fit" => {
                let wanted: Option<Vec<&str>> =
                    args.get("parts").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect());
                let parts: Vec<Part> = project
                    .project
                    .parts
                    .iter()
                    .filter(|p| wanted.as_ref().is_none_or(|w| w.contains(&p.name.as_str())))
                    .cloned()
                    .collect();
                if let Some(missing) = wanted.iter().flatten().find(|w| !parts.iter().any(|p| p.name == **w)) {
                    bail!("no part named '{missing}'");
                }
                if parts.len() < 2 {
                    bail!("a fit check needs at least two parts");
                }
                let mut keys = Vec::new();
                for part in &parts {
                    keys.push((part.name.clone(), self.built_key(&project, part, None, true)?));
                }
                let mut data = self.with_worker(|w| w.fit_blocking(&keys))?;
                // The mesh files are for the viewer.
                for pair in data["pairs"].as_array_mut().into_iter().flatten() {
                    if let Some(pair) = pair.as_object_mut() {
                        pair.remove("mesh");
                    }
                }
                let moved: Vec<String> = project.project.joints.iter().filter(|j| j.at != 0.0).map(Joint::summary).collect();
                if !moved.is_empty() {
                    data["joints"] = json!(moved);
                }
                Ok(data)
            }
            "add_joint" => {
                let (min, max) = (number(args, "min")?, number(args, "max")?);
                let at = match optional_number(args, "at")? {
                    Some(at) => at,
                    // 0 when the range allows it, otherwise its nearest end.
                    None if min <= max => 0f64.clamp(min, max),
                    None => 0.0,
                };
                let joint = Joint {
                    name: text(args, "name")?.trim().to_owned(),
                    kind: joint_kind(text(args, "kind")?)?,
                    base: text(args, "base")?.to_owned(),
                    part: text(args, "part")?.to_owned(),
                    origin: optional_triple(args, "origin")?.context("missing 'origin'")?,
                    axis: optional_triple(args, "axis")?.context("missing 'axis'")?,
                    min,
                    max,
                    at,
                    why: optional_text(args, "why").unwrap_or_default().to_owned(),
                };
                let name = joint.name.clone();
                project.add_joint(joint)?;
                project.save()?;
                self.joint_report(&project, &name)
            }
            "edit_joint" => {
                let name = text(args, "name")?;
                let edit = JointEdit {
                    kind: optional_text(args, "kind").map(joint_kind).transpose()?,
                    base: optional_text(args, "base").map(str::to_owned),
                    part: optional_text(args, "part").map(str::to_owned),
                    origin: optional_triple(args, "origin")?,
                    axis: optional_triple(args, "axis")?,
                    min: optional_number(args, "min")?,
                    max: optional_number(args, "max")?,
                    at: optional_number(args, "at")?,
                    why: optional_text(args, "why").map(str::to_owned),
                };
                project.edit_joint(name, edit)?;
                project.save()?;
                self.joint_report(&project, name)
            }
            "check_motion" => {
                let name = text(args, "joint")?;
                let joint = project.project.joint(name).with_context(|| format!("no joint called '{name}'"))?.clone();
                let steps = match args.get("steps") {
                    None | Some(Value::Null) => motion::DEFAULT_STEPS,
                    Some(v) => v.as_u64().filter(|n| (2..=motion::MAX_STEPS as u64).contains(n)).with_context(|| {
                        format!("'steps' must be a whole number from 2 to {}", motion::MAX_STEPS)
                    })? as usize,
                };
                let moving = project.project.moved_by(&joint.name);
                if moving.len() == project.project.parts.len() {
                    bail!("every part moves with '{}', so there is nothing for it to hit", joint.name);
                }
                let mut built = Vec::new();
                for part in &project.project.parts {
                    let key = self.built_key(&project, part, None, true)?;
                    built.push((part.name.clone(), key, project.project.shown_pose(part)));
                }
                let values = motion::sweep_values(&joint, steps);
                let parts = motion::sweep_parts(&project.project, &joint.name, &values, &built);
                let data = self.with_worker(|w| w.sweep_blocking(&parts, &values))?;
                Ok(motion::summarise(&joint, &data))
            }
            "delete_joint" => {
                let name = text(args, "name")?;
                let moved = project.project.moved_by(name);
                project.delete_joint(name)?;
                project.save()?;
                Ok(json!({ "deleted_joint": name, "back_at_rest": moved }))
            }
            "export_part" => {
                let part = project.part(text(args, "part")?)?;
                let formats = match optional_text(args, "format") {
                    Some("step") => vec![ExportFormat::Step],
                    Some("stl") => vec![ExportFormat::Stl],
                    None => vec![ExportFormat::Step, ExportFormat::Stl],
                    Some(other) => bail!("unknown format '{other}', use 'step' or 'stl'"),
                };
                let mut files = Vec::new();
                for format in formats {
                    let ext = if format == ExportFormat::Step { "step" } else { "stl" };
                    let path = project.dir.join("exports").join(format!("{}.{ext}", part.name));
                    let request = rest_request(&project.dir, part);
                    files.push(self.with_worker(|w| w.export_blocking(request, format, &path))?);
                }
                Ok(json!({ "exported": files }))
            }
            other => bail!("unknown tool '{other}'"),
        }
    }

    /// Reads the project fresh on every call, because the user may have edited the files.
    /// Held for the whole call; released when the returned file is dropped.
    /// Reference parts are the user's fixed surroundings: the model works around them.
    fn refuse_reference(&self, project: &LoadedProject, name: &str) -> Result<()> {
        if project.part(name)?.kind == intercraft_core::PartKind::Reference {
            bail!("'{name}' is a reference part. Only the user can move, rename or remove it");
        }
        Ok(())
    }

    fn lock_project(&self) -> Result<std::fs::File> {
        intercraft_core::lock(&self.project_dir()?)
    }

    /// A missing project is created, so a new folder opened in the viewer just works.
    fn load(&self) -> Result<LoadedProject> {
        let dir = self.project_dir()?;
        if dir.join("project.json").exists() {
            return LoadedProject::load(&dir);
        }
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("Untitled");
        LoadedProject::create(&dir, name)
    }

    /// Runs `job` on the worker, starting it if needed. A failed worker is dropped
    /// so the next call starts a clean one.
    fn with_worker<T>(&mut self, job: impl FnOnce(&mut PythonWorker) -> Result<T>) -> Result<T> {
        if self.worker.is_none() {
            self.worker = Some(PythonWorker::spawn(|| {}).context("starting the geometry worker")?);
        }
        let result = job(self.worker.as_mut().expect("worker was just set"));
        if result.is_err() {
            self.worker = None;
        }
        result
    }

    /// Builds a part and describes each feature's outcome in numbers.
    /// Build a part and return the cache key of its final shape (or of `feature_id`).
    fn built_key(&mut self, loaded: &LoadedProject, part: &Part, feature_id: Option<&str>, placed: bool) -> Result<String> {
        let dir = &loaded.dir;
        let request = if placed { request_for(dir, &loaded.project, part) } else { local_request(dir, part) };
        let results = self.with_worker(|w| w.build_blocking(request))?;
        let wanted = match feature_id {
            Some(id) => results.iter().find(|r| r.feature_id == id).with_context(|| format!("no step '{id}' in '{}'", part.name))?,
            None => results.last().with_context(|| format!("'{}' has nothing to measure yet", part.name))?,
        };
        if !wanted.ok {
            bail!("step '{}' of '{}' doesn't build, so it can't be inspected", wanted.feature_id, part.name);
        }
        wanted.mesh.as_deref().and_then(intercraft_geo::Mesh::key_of).context("the worker gave no shape")
    }

    /// Where a joint has put the parts it moves.
    fn joint_report(&mut self, loaded: &LoadedProject, name: &str) -> Result<Value> {
        let joint = loaded.project.joint(name).with_context(|| format!("no joint called '{name}'"))?.clone();
        let mut moved = Vec::new();
        for part_name in loaded.project.moved_by(&joint.name) {
            let part = loaded.part(&part_name)?;
            let request = request_for(&loaded.dir, &loaded.project, part);
            let results = self.with_worker(|w| w.build_blocking(request))?;
            let mut entry = json!({ "part": part_name });
            if let Some(r) = results.last().and_then(|r| r.report.as_ref()) {
                entry["bbox_min"] = json!(r.bbox_min.map(round));
                entry["bbox_max"] = json!(r.bbox_max.map(round));
            }
            moved.push(entry);
        }
        Ok(json!({
            "joint": joint,
            "summary": joint.summary(),
            "moved_parts": moved,
            "note": "Positions are in the assembly with the joint where it is now.",
        }))
    }

    fn build_report(&mut self, loaded: &LoadedProject, part: &Part, with_source: bool) -> Result<Value> {
        let request = request_for(&loaded.dir, &loaded.project, part);
        let results = self.with_worker(|w| w.build_blocking(request))?;
        let by_id: HashMap<&str, &FeatureResult> =
            results.iter().map(|r| (r.feature_id.as_str(), r)).collect();

        let mut all_ok = true;
        let mut features = Vec::new();
        // The imported file, if any, is step 0.
        let import_step = part.source.as_ref().map(|source| (intercraft_geo::IMPORT_STEP_ID.to_owned(), source.clone()));
        let steps = import_step
            .iter()
            .map(|(id, source)| (id.as_str(), format!("Imported {source}"), None))
            .chain(part.features.iter().map(|f| (f.id.as_str(), f.label.clone(), Some(f))));
        for (id, label, feature) in steps {
            let mut entry = json!({ "id": id, "label": label });
            if let (true, Some(feature)) = (with_source, feature) {
                entry["params"] = Value::Object(feature.params.clone());
                entry["code"] = feature.code.clone().into();
                entry["prompt"] = feature.prompt.clone().into();
                if !feature.why.is_empty() {
                    entry["why"] = feature.why.clone().into();
                }
                if !feature.labels.is_empty() {
                    entry["labels"] = json!(feature.labels);
                }
            }
            match by_id.get(id) {
                Some(result) if result.ok => {
                    entry["status"] = "built".into();
                    if let Some(r) = &result.report {
                        entry["volume_mm3"] = round(r.volume);
                        if let Some(before) = r.volume_before {
                            entry["volume_change_mm3"] = round(r.volume - before);
                        }
                        entry["size_mm"] = json!([
                            round(r.bbox_max[0] - r.bbox_min[0]),
                            round(r.bbox_max[1] - r.bbox_min[1]),
                            round(r.bbox_max[2] - r.bbox_min[2]),
                        ]);
                        entry["bbox_min"] = json!(r.bbox_min.map(round));
                        entry["bbox_max"] = json!(r.bbox_max.map(round));
                        entry["faces"] = r.faces.into();
                        entry["edges"] = r.edges.into();
                        entry["valid_solid"] = r.valid.into();
                        all_ok &= r.valid;
                    }
                }
                Some(result) => {
                    all_ok = false;
                    entry["status"] = "failed".into();
                    entry["error"] = result.error.clone().unwrap_or_default().into();
                }
                None => {
                    all_ok = false;
                    entry["status"] = "not built (an earlier feature failed)".into();
                }
            }
            features.push(entry);
        }

        let mut report = json!({ "part": part.name, "kind": part.kind, "ok": all_ok, "features": features });
        if part.placement().is_some() {
            report["position_mm"] = json!(part.position);
            report["rotation_deg"] = json!(part.rotation);
        }
        let chain = loaded.project.joint_chain(&part.name);
        if !chain.is_empty() {
            report["moved_by"] = json!(chain.iter().map(|j| j.summary()).collect::<Vec<_>>());
        }
        if let Some(source) = &part.source {
            report["imported_from"] = source.clone().into();
        }
        if let Some(group) = &part.group {
            report["group"] = group.clone().into();
        }
        if !all_ok {
            report["note"] =
                "The change is saved but the part does not build cleanly. Fix it with edit_feature or delete_feature.".into();
        }
        Ok(report)
    }
}

/// Up to this many parts, get_project gives every part in full.
const FULL_PROJECT_PARTS: usize = 12;
/// Up to this many parts are listed one line each; beyond it, only groups are.
const LISTED_PARTS: usize = 150;

/// One short line about a part for an outline: where it is and how big, and only
/// what differs from the usual (steps on an import, a lock, a failed build).
fn part_line(part: &Part, report: &Value) -> Value {
    let mut line = json!({ "part": part.name });
    if let Some(group) = &part.group {
        line["group"] = group.clone().into();
    }
    let last = report["features"].as_array().and_then(|f| f.last());
    let tenth = |v: f64| (v * 10.0).round() / 10.0;
    if let Some((lo, hi)) = last.and_then(|s| Some((triple(&s["bbox_min"])?, triple(&s["bbox_max"])?))) {
        line["size"] = json!([tenth(hi[0] - lo[0]), tenth(hi[1] - lo[1]), tenth(hi[2] - lo[2])]);
        line["centre"] = json!([tenth((lo[0] + hi[0]) / 2.0), tenth((lo[1] + hi[1]) / 2.0), tenth((lo[2] + hi[2]) / 2.0)]);
    }
    if !part.features.is_empty() {
        line["steps"] = part.features.len().into();
    }
    if part.kind == intercraft_core::PartKind::Reference {
        line["locked"] = true.into();
    }
    if report["ok"] == false {
        line["ok"] = false.into();
    }
    line
}

fn triple(value: &Value) -> Option<[f64; 3]> {
    let v = value.as_array()?;
    Some([v.first()?.as_f64()?, v.get(1)?.as_f64()?, v.get(2)?.as_f64()?])
}

/// Each group with how many parts it holds (sub-groups included) and the box they fill.
fn group_outline(groups: &[String], parts: &[(&Part, Value)]) -> Vec<Value> {
    groups
        .iter()
        .map(|group| {
            let (mut count, mut lo, mut hi) = (0, [f64::MAX; 3], [f64::MIN; 3]);
            for (_, report) in parts.iter().filter(|(p, _)| intercraft_core::in_group(p.group.as_deref(), group)) {
                count += 1;
                let last = report["features"].as_array().and_then(|f| f.last());
                if let Some((a, b)) = last.and_then(|s| Some((triple(&s["bbox_min"])?, triple(&s["bbox_max"])?))) {
                    for i in 0..3 {
                        lo[i] = lo[i].min(a[i]);
                        hi[i] = hi[i].max(b[i]);
                    }
                }
            }
            let mut entry = json!({ "group": group, "parts": count });
            if lo[0] <= hi[0] {
                entry["bbox_min"] = json!(lo.map(round));
                entry["bbox_max"] = json!(hi.map(round));
            }
            entry
        })
        .collect()
}

/// A build report cut down to the part's name, group, state and where it sits.
fn brief_report(full: Value) -> Value {
    let mut brief = json!({});
    for key in ["part", "group", "kind", "ok", "imported_from", "position_mm", "rotation_deg", "moved_by"] {
        if let Some(v) = full.get(key) {
            brief[key] = v.clone();
        }
    }
    if let Some(step) = full["features"].as_array().and_then(|f| f.last()) {
        for key in ["size_mm", "bbox_min", "bbox_max", "error"] {
            if let Some(v) = step.get(key) {
                brief[key] = v.clone();
            }
        }
    }
    brief
}

/// Everything the worker needs to build a part: the imported file first, if any,
/// then each feature, placed where the assembly shows it (joints included). Shared
/// by the viewer and the tools so both build the same thing.
pub fn request_for(dir: &Path, project: &Project, part: &Part) -> RebuildRequest {
    RebuildRequest { placement: project.shown_placement(part), ..local_request(dir, part) }
}

/// The part where its placement puts it, with every joint at 0. Exports use this, so
/// a part's file doesn't depend on where a hinge was left.
pub fn rest_request(dir: &Path, part: &Part) -> RebuildRequest {
    RebuildRequest { placement: part.placement(), ..local_request(dir, part) }
}

/// The part in its own frame, ignoring where it sits in the assembly. Features are
/// written in this frame, so it's the one to inspect before editing.
fn local_request(dir: &Path, part: &Part) -> RebuildRequest {
    let mut features = Vec::new();
    if let Some(source) = &part.source {
        features.push(FeatureInput::import(dir.join(source)));
    }
    features.extend(part.features.iter().map(|f| FeatureInput::snippet(&f.id, &f.code, &f.params)));
    RebuildRequest { part: part.name.clone(), features, placement: None }
}

/// One selection for `measure`, as the model writes it.
fn selection(dir_part: &mut dyn FnMut(&str) -> Result<String>, default_part: Option<&str>, value: &Value) -> Result<Value> {
    if let Some(point) = value.get("point") {
        let coords: Vec<f64> = point.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
        if coords.len() != 3 {
            bail!("'point' must be [x, y, z] in mm");
        }
        return Ok(json!({ "kind": "point", "point": coords }));
    }
    let kind = value.get("kind").and_then(Value::as_str).context("give 'kind' (face, edge or vertex) and 'index', or a 'point'")?;
    if !matches!(kind, "face" | "edge" | "vertex") {
        bail!("'kind' must be face, edge or vertex");
    }
    let index = value.get("index").and_then(Value::as_u64).context("'index' must be a whole number")?;
    let part = value.get("part").and_then(Value::as_str).or(default_part).context("say which 'part'")?;
    Ok(json!({ "mesh": dir_part(part)?, "kind": kind, "index": index }))
}

/// Three decimals is plenty for mm and keeps the reports short.
fn round(value: f64) -> Value {
    json!((value * 1000.0).round() / 1000.0)
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    optional_text(args, key).with_context(|| format!("missing '{key}'"))
}

fn optional_text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)?.as_str()
}

/// Ask the viewer for a picture and wait for it. The viewer watches the project's
/// `.intercraft/view` folder, so this works from another process (an MCP server) too.
fn request_view(project: &Path, view: &str, args: &Value) -> Result<Vec<u8>> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let id = format!("{:x}x{n:x}x{nanos:x}", std::process::id());

    let dir = view_dir(project);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let request = json!({ "view": view, "part": args.get("part"), "section": args.get("section") });
    if let Some(section) = args.get("section").filter(|s| !s.is_null()) {
        let axis_ok = ["x", "y", "z"].iter().any(|a| section["axis"] == *a);
        if !axis_ok || !section["offset"].as_f64().is_some_and(f64::is_finite) {
            bail!("'section' needs an axis (x, y or z) and an offset in mm");
        }
    }
    let file = dir.join(format!("request-{id}.json"));
    std::fs::write(&file, request.to_string()).context("writing the view request")?;

    let (png, err) = (dir.join(format!("{id}.png")), dir.join(format!("{id}.err")));
    let started = std::time::Instant::now();
    let result = loop {
        if let Ok(bytes) = std::fs::read(&png) {
            break Ok(bytes);
        }
        if let Ok(error) = std::fs::read_to_string(&err) {
            break Err(anyhow::anyhow!(error));
        }
        if started.elapsed() > VIEW_TIMEOUT {
            break Err(anyhow::anyhow!(
                "InterCraft's window didn't answer. A picture needs the viewer open on this project. Carry on with build reports, inspect_part and measure"
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    };
    for leftover in [&file, &png, &err] {
        let _ = std::fs::remove_file(leftover);
    }
    result
}

/// Parameter labels: an object of short strings.
fn optional_labels(args: &Value) -> Result<Option<std::collections::BTreeMap<String, String>>> {
    let Some(value) = args.get("labels").filter(|v| !v.is_null()) else { return Ok(None) };
    let object = value.as_object().context("'labels' must be an object like {\"bore_d\": \"Bore diameter\"}")?;
    let mut labels = std::collections::BTreeMap::new();
    for (name, label) in object {
        let label = label.as_str().with_context(|| format!("the label for '{name}' must be text"))?;
        labels.insert(name.clone(), label.trim().to_owned());
    }
    Ok(Some(labels))
}

/// Standard parts the worker can build. Matches `parts_library.KINDS`.
pub const STANDARD_KINDS: [&str; 7] =
    ["ball bearing", "button head screw", "countersunk screw", "heat-set insert", "hex nut", "socket head screw", "washer"];

/// A size like "M4", "M4-0.7" or "M8-22-7". It goes into a snippet, so only plain characters.
fn standard_size(size: &str) -> Result<String> {
    let size = size.trim();
    let plain = !size.is_empty() && size.len() <= 20 && size.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !plain || !size.starts_with(['M', 'm']) {
        bail!("give a metric size like 'M4' or 'M4-0.7'");
    }
    Ok(format!("M{}", &size[1..]))
}

/// Three numbers, e.g. a position.
fn optional_triple(args: &Value, key: &str) -> Result<Option<[f64; 3]>> {
    let Some(value) = args.get(key).filter(|v| !v.is_null()) else { return Ok(None) };
    let numbers: Option<Vec<f64>> = value.as_array().map(|a| a.iter().filter_map(Value::as_f64).collect());
    match numbers.as_deref() {
        Some(&[x, y, z]) if value.as_array().is_some_and(|a| a.len() == 3) => Ok(Some([x, y, z])),
        _ => bail!("'{key}' must be three numbers, like [10, 0, 5]"),
    }
}

fn number(args: &Value, key: &str) -> Result<f64> {
    optional_number(args, key)?.with_context(|| format!("missing '{key}'"))
}

fn optional_number(args: &Value, key: &str) -> Result<Option<f64>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_f64().map(Some).with_context(|| format!("'{key}' must be a number")),
    }
}

fn joint_kind(kind: &str) -> Result<JointKind> {
    match kind {
        "hinge" => Ok(JointKind::Hinge),
        "slider" => Ok(JointKind::Slider),
        other => bail!("unknown joint kind '{other}', use 'hinge' or 'slider'"),
    }
}

fn optional_params(args: &Value) -> Result<Option<Params>> {
    match args.get("params") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Object(map)) => Ok(Some(map.clone())),
        Some(_) => bail!("'params' must be an object"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn view_model_asks_the_viewer_and_returns_its_picture() {
        let dir = std::env::temp_dir().join(format!("intercraft-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut host = ToolHost::new(dir.clone());
        host.call("add_part", &json!({ "name": "P" })).unwrap();

        // Junk in the request folder is thrown away, not served.
        let folder = view_dir(&dir);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("request-..evil.json"), r#"{"view":"iso"}"#).unwrap();
        std::fs::write(folder.join("request-abc.json"), r#"{"view":"sideways"}"#).unwrap();
        assert!(view_requests(&dir).is_empty());
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0);

        // A stand-in for the viewer: answer the first request it sees.
        let project = dir.clone();
        let viewer = std::thread::spawn(move || loop {
            if let Some(request) = view_requests(&project).into_iter().next() {
                answer_view(&project, &request.id, Ok(b"PNG bytes"));
                return request;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        });
        let args = json!({ "view": "front", "part": "P", "section": { "axis": "z", "offset": 2.5 } });
        let result = host.call("view_model", &args).unwrap();
        let seen = viewer.join().unwrap();
        assert_eq!((seen.view.as_str(), seen.part.as_deref(), seen.section), ("front", Some("P"), Some((2, 2.5, false))));
        assert_eq!(result[IMAGE_KEY], base64(b"PNG bytes"));
        assert!(result["facing"].as_str().unwrap().contains("-Y side"));
        assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 0, "nothing is left behind");

        assert!(host.call("view_model", &json!({ "view": "upside-down" })).is_err());
        assert!(host.call("view_model", &json!({ "part": "Nope" })).is_err());
        assert!(host.call("view_model", &json!({ "section": { "axis": "w", "offset": 1 } })).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
