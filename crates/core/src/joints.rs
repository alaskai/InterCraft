// Joints: how one part moves against another. A hinge turns about an axis, a slider
// moves along one. Each joint stores where it is now (`at`), and every part it moves
// is shown there, so measuring and fit checks see the same pose the user does.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::{Part, Project, MAX_WHY_CHARS};

pub const MAX_JOINTS: usize = 100;
const MAX_JOINT_NAME_CHARS: usize = 60;
/// Longest slider travel, in mm.
const MAX_TRAVEL: f64 = 10_000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JointKind {
    /// Turns about the axis. Values are degrees.
    Hinge,
    /// Moves along the axis. Values are mm.
    Slider,
}

impl JointKind {
    pub fn unit(self) -> &'static str {
        match self {
            JointKind::Hinge => "°",
            JointKind::Slider => " mm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Joint {
    pub name: String,
    pub kind: JointKind,
    /// The part the joint is fixed to.
    pub base: String,
    /// The part that moves.
    pub part: String,
    /// A point on the axis, in assembly mm, with every joint at zero.
    pub origin: [f64; 3],
    /// Which way the axis points. A hinge turns right-handed about it.
    pub axis: [f64; 3],
    /// The range of motion: degrees for a hinge, mm for a slider.
    pub min: f64,
    pub max: f64,
    /// Where the joint is now, within the range.
    #[serde(default)]
    pub at: f64,
    /// What the joint is for, in one plain sentence.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// Fields to change on a joint. None leaves a field as it is.
#[derive(Debug, Clone, Default)]
pub struct JointEdit {
    pub kind: Option<JointKind>,
    pub base: Option<String>,
    pub part: Option<String>,
    pub origin: Option<[f64; 3]>,
    pub axis: Option<[f64; 3]>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub at: Option<f64>,
    pub why: Option<String>,
}

impl Joint {
    /// The joint's own move at value `v`, in the assembly frame with every joint at zero.
    pub fn motion(&self, v: f64) -> Pose {
        let axis = normalise(self.axis);
        match self.kind {
            JointKind::Hinge => {
                let rot = axis_angle(axis, v.to_radians());
                let turned = mul_vec(&rot, self.origin);
                Pose { rot, pos: sub(self.origin, turned) }
            }
            JointKind::Slider => Pose { rot: IDENTITY, pos: axis.map(|a| a * v) },
        }
    }

    /// "Lid hinge: hinge, 0 to 110°, now 45°"
    pub fn summary(&self) -> String {
        let kind = match self.kind {
            JointKind::Hinge => "hinge",
            JointKind::Slider => "slider",
        };
        let u = self.kind.unit();
        format!("{}: {kind}, {} to {}{u}, now {}{u}", self.name, tidy(self.min), tidy(self.max), tidy(self.at))
    }
}

/// A whole number when it is one, otherwise up to two decimals.
pub(crate) fn tidy(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r.fract() == 0.0 { format!("{r:.0}") } else { format!("{r}") }
}

/// A rigid move: turn by `rot`, then shift by `pos`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub rot: [[f64; 3]; 3],
    pub pos: [f64; 3],
}

const IDENTITY: [[f64; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

impl Pose {
    pub const IDENTITY: Pose = Pose { rot: IDENTITY, pos: [0.0; 3] };

    /// A part's placement: rotation in degrees about X, then Y, then Z of the turned
    /// frame (the same order build123d's Location uses), then the move.
    pub fn from_placement(position: [f64; 3], rotation: [f64; 3]) -> Pose {
        let [x, y, z] = rotation.map(f64::to_radians);
        let rot = mat_mul(&mat_mul(&axis_angle([1.0, 0.0, 0.0], x), &axis_angle([0.0, 1.0, 0.0], y)), &axis_angle([0.0, 0.0, 1.0], z));
        Pose { rot, pos: position }
    }

    /// Back to position and rotation in degrees, rounded so the numbers stay readable.
    pub fn to_placement(&self) -> [f64; 6] {
        let m = &self.rot;
        let sy = m[0][2].clamp(-1.0, 1.0);
        let y = sy.asin();
        let (x, z) = if sy.abs() < 1.0 - 1e-9 {
            ((-m[1][2]).atan2(m[2][2]), (-m[0][1]).atan2(m[0][0]))
        } else {
            // Pointing straight along X: X and Z turn about the same line, so put it all in X.
            (m[2][1].atan2(m[1][1]), 0.0)
        };
        let round = |v: f64| {
            let r = (v * 1e6).round() / 1e6;
            if r == 0.0 { 0.0 } else { r }
        };
        let [px, py, pz] = self.pos.map(round);
        [px, py, pz, round(x.to_degrees()), round(y.to_degrees()), round(z.to_degrees())]
    }

    /// This move, then `next`.
    pub fn then(&self, next: &Pose) -> Pose {
        Pose { rot: mat_mul(&next.rot, &self.rot), pos: add(mul_vec(&next.rot, self.pos), next.pos) }
    }

    pub fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        add(mul_vec(&self.rot, p), self.pos)
    }
}

impl Project {
    pub fn joint(&self, name: &str) -> Option<&Joint> {
        self.joints.iter().find(|j| j.name.eq_ignore_ascii_case(name))
    }

    /// The joint that moves `part`, if any.
    pub fn joint_moving(&self, part: &str) -> Option<&Joint> {
        self.joints.iter().find(|j| j.part == part)
    }

    /// How the joints move a part from where its placement puts it.
    pub fn motion_of(&self, part: &str) -> Pose {
        let mut pose = Pose::IDENTITY;
        let mut current = part;
        // Validation rules out loops; the count is only a guard.
        for _ in 0..=self.joints.len() {
            let Some(joint) = self.joint_moving(current) else { break };
            pose = pose.then(&joint.motion(joint.at));
            current = &joint.base;
        }
        pose
    }

    /// Where a part is shown in the assembly: its placement, then the joints that move
    /// it. None when it sits at the origin unturned.
    pub fn shown_placement(&self, part: &Part) -> Option<[f64; 6]> {
        let moved = self.joint_chain(&part.name).iter().any(|j| j.at != 0.0);
        if !moved {
            // Exactly the stored numbers, so nothing changes until a joint moves.
            return part.placement();
        }
        let pose = Pose::from_placement(part.position, part.rotation).then(&self.motion_of(&part.name));
        let placement = pose.to_placement();
        placement.iter().any(|v| *v != 0.0).then_some(placement)
    }

    /// The joints between a part and the part it ultimately hangs from, nearest first.
    pub fn joint_chain(&self, part: &str) -> Vec<&Joint> {
        let mut chain = Vec::new();
        let mut current = part;
        for _ in 0..=self.joints.len() {
            let Some(joint) = self.joint_moving(current) else { break };
            chain.push(joint);
            current = &joint.base;
        }
        chain
    }

    /// The parts a joint moves: its own part, and everything hanging from that.
    pub fn moved_by(&self, joint: &str) -> Vec<String> {
        let Some(joint) = self.joint(joint) else { return Vec::new() };
        let mut moved = vec![joint.part.clone()];
        let mut i = 0;
        while i < moved.len() {
            for j in &self.joints {
                if j.base == moved[i] && !moved.contains(&j.part) {
                    moved.push(j.part.clone());
                }
            }
            i += 1;
        }
        moved
    }
}

/// Every joint must make sense on its own and together: real parts, one joint per
/// moving part, no loops, a usable axis and range.
pub(crate) fn check_joints(project: &Project) -> Result<()> {
    if project.joints.len() > MAX_JOINTS {
        bail!("a project can hold at most {MAX_JOINTS} joints");
    }
    let parts: HashSet<&str> = project.parts.iter().map(|p| p.name.as_str()).collect();
    let mut names = HashSet::new();
    let mut moving: HashMap<&str, &str> = HashMap::new();
    for joint in &project.joints {
        check_joint(joint)?;
        if !names.insert(joint.name.to_lowercase()) {
            bail!("two joints are called '{}'", joint.name);
        }
        for name in [&joint.base, &joint.part] {
            if !parts.contains(name.as_str()) {
                bail!("joint '{}' names a part '{name}' that doesn't exist", joint.name);
            }
        }
        if joint.base == joint.part {
            bail!("joint '{}' joins '{}' to itself; the base and the moving part must differ", joint.name, joint.part);
        }
        if let Some(other) = moving.insert(&joint.part, &joint.name) {
            bail!("'{}' is already moved by joint '{other}'; a part can only have one joint that moves it", joint.part);
        }
    }
    // Following each part to its base must end somewhere.
    for joint in &project.joints {
        let mut current = joint.base.as_str();
        for _ in 0..=project.joints.len() {
            if current == joint.part {
                bail!("joint '{}' makes a loop: '{}' ends up moving itself", joint.name, joint.part);
            }
            match project.joints.iter().find(|j| j.part == current) {
                Some(next) => current = &next.base,
                None => break,
            }
        }
    }
    Ok(())
}

fn check_joint(joint: &Joint) -> Result<()> {
    let name = joint.name.trim();
    if name.is_empty() || name.chars().count() > MAX_JOINT_NAME_CHARS || name.chars().any(char::is_control) {
        bail!("a joint name must be one line of 1 to {MAX_JOINT_NAME_CHARS} characters");
    }
    let numbers = joint.origin.iter().chain(&joint.axis).chain([&joint.min, &joint.max, &joint.at]);
    if numbers.clone().any(|v| !v.is_finite()) || joint.origin.iter().any(|v| v.abs() > 1e6) {
        bail!("joint '{}' has a number that isn't an ordinary number", joint.name);
    }
    if length(joint.axis) < 1e-9 {
        bail!("joint '{}' needs an axis that points somewhere, like [0, 0, 1]", joint.name);
    }
    if joint.min > joint.max {
        bail!("joint '{}' has min {} above max {}", joint.name, joint.min, joint.max);
    }
    match joint.kind {
        JointKind::Hinge => {
            if joint.min < -360.0 || joint.max > 360.0 || joint.max - joint.min > 360.0 {
                bail!("a hinge's range must sit within -360 to 360 degrees and span at most one turn");
            }
        }
        JointKind::Slider => {
            if joint.min < -MAX_TRAVEL || joint.max > MAX_TRAVEL {
                bail!("a slider can travel at most {MAX_TRAVEL} mm either way");
            }
        }
    }
    if joint.at < joint.min - 1e-9 || joint.at > joint.max + 1e-9 {
        bail!(
            "joint '{}' is at {}, outside its range of {} to {}",
            joint.name,
            tidy(joint.at),
            tidy(joint.min),
            tidy(joint.max)
        );
    }
    if joint.why.chars().count() > MAX_WHY_CHARS {
        bail!("keep the joint's why to one or two sentences ({MAX_WHY_CHARS} characters at most)");
    }
    Ok(())
}

impl JointEdit {
    pub(crate) fn apply(self, joint: &mut Joint) {
        if let Some(v) = self.kind {
            joint.kind = v;
        }
        if let Some(v) = self.base {
            joint.base = v;
        }
        if let Some(v) = self.part {
            joint.part = v;
        }
        if let Some(v) = self.origin {
            joint.origin = v;
        }
        if let Some(v) = self.axis {
            joint.axis = v;
        }
        if let Some(v) = self.min {
            joint.min = v;
        }
        if let Some(v) = self.max {
            joint.max = v;
        }
        if let Some(v) = self.at {
            joint.at = v;
        }
        if let Some(v) = self.why {
            joint.why = v.trim().to_owned();
        }
    }
}

fn length(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

fn normalise(v: [f64; 3]) -> [f64; 3] {
    let l = length(v);
    if l < 1e-12 { [0.0, 0.0, 1.0] } else { v.map(|x| x / l) }
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn mul_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn mat_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j]))
}

/// Rotation by `angle` radians about a unit axis (Rodrigues' formula).
fn axis_angle(k: [f64; 3], angle: f64) -> [[f64; 3]; 3] {
    let (s, c) = angle.sin_cos();
    let t = 1.0 - c;
    let [x, y, z] = k;
    [
        [c + x * x * t, x * y * t - z * s, x * z * t + y * s],
        [y * x * t + z * s, c + y * y * t, y * z * t - x * s],
        [z * x * t - y * s, z * y * t + x * s, c + z * z * t],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    #[test]
    fn placements_survive_the_round_trip() {
        for rotation in [[30.0, 40.0, 50.0], [0.0, 0.0, 90.0], [-120.0, 10.0, 170.0], [0.0, 90.0, 0.0]] {
            let pose = Pose::from_placement([1.0, 2.0, 3.0], rotation);
            let back = pose.to_placement();
            let again = Pose::from_placement([back[0], back[1], back[2]], [back[3], back[4], back[5]]);
            for p in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [3.0, -2.0, 5.0]] {
                assert!(close(pose.apply(p), again.apply(p)), "{rotation:?} came back as {back:?}");
            }
        }
        // The same order as build123d: X, then Y, then Z of the turned frame.
        let pose = Pose::from_placement([0.0; 3], [90.0, 0.0, 90.0]);
        assert!(close(pose.apply([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]));
    }

    #[test]
    fn a_hinge_turns_about_its_own_axis() {
        let joint = Joint {
            name: "Lid hinge".into(),
            kind: JointKind::Hinge,
            base: "Box".into(),
            part: "Lid".into(),
            origin: [0.0, 10.0, 20.0],
            axis: [1.0, 0.0, 0.0],
            min: 0.0,
            max: 110.0,
            at: 0.0,
            why: String::new(),
        };
        let open = joint.motion(90.0);
        // A point on the axis stays put; one 5 mm towards -Y turns down under it.
        assert!(close(open.apply([7.0, 10.0, 20.0]), [7.0, 10.0, 20.0]));
        assert!(close(open.apply([0.0, 5.0, 20.0]), [0.0, 10.0, 15.0]));
        let slider = Joint { kind: JointKind::Slider, axis: [0.0, 0.0, 2.0], ..joint };
        assert!(close(slider.motion(4.0).apply([1.0, 1.0, 1.0]), [1.0, 1.0, 5.0]));
    }
}
