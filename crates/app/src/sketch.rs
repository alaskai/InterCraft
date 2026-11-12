// The sketch panel: draw a flat profile on a millimetre grid, then extrude it into
// a part or hand its exact coordinates to the chat. Shapes drawn inside another
// shape become holes. Everything snaps to the grid, so the numbers are exact.

use eframe::egui;
use serde_json::{json, Value};

use crate::measure::mm;

type Point = [f64; 2];

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// A closed outline, in drawing order.
    Polygon(Vec<Point>),
    /// Two opposite corners.
    Rect(Point, Point),
    Circle { centre: Point, radius: f64 },
}

impl Shape {
    /// A point on the shape's edge, used to tell which other shapes it sits inside.
    fn anchor(&self) -> Point {
        match self {
            Shape::Polygon(points) => points[0],
            Shape::Rect(a, _) => *a,
            Shape::Circle { centre, radius } => [centre[0] + radius, centre[1]],
        }
    }

    fn outline(&self) -> Vec<Point> {
        match self {
            Shape::Polygon(points) => points.clone(),
            Shape::Rect(a, b) => vec![*a, [b[0], a[1]], *b, [a[0], b[1]]],
            Shape::Circle { centre, radius } => (0..48)
                .map(|i| {
                    let angle = i as f64 / 48.0 * std::f64::consts::TAU;
                    [centre[0] + radius * angle.cos(), centre[1] + radius * angle.sin()]
                })
                .collect(),
        }
    }

    /// Strictly inside, so shapes that only share an edge don't count as nested.
    fn contains(&self, p: Point) -> bool {
        match self {
            Shape::Circle { centre, radius } => distance(*centre, p) < radius - 1e-9,
            Shape::Rect(a, b) => {
                let (lo, hi) = ([a[0].min(b[0]), a[1].min(b[1])], [a[0].max(b[0]), a[1].max(b[1])]);
                p[0] > lo[0] + 1e-9 && p[0] < hi[0] - 1e-9 && p[1] > lo[1] + 1e-9 && p[1] < hi[1] - 1e-9
            }
            Shape::Polygon(points) => {
                // Ray casting. A point on the outline itself is not inside.
                let mut inside = false;
                for (i, a) in points.iter().enumerate() {
                    let b = points[(i + 1) % points.len()];
                    if on_segment(*a, b, p) {
                        return false;
                    }
                    if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0] {
                        inside = !inside;
                    }
                }
                inside
            }
        }
    }

    fn area(&self) -> f64 {
        match self {
            Shape::Circle { radius, .. } => std::f64::consts::PI * radius * radius,
            Shape::Rect(a, b) => ((b[0] - a[0]) * (b[1] - a[1])).abs(),
            Shape::Polygon(points) => {
                let twice: f64 = (0..points.len())
                    .map(|i| {
                        let (a, b) = (points[i], points[(i + 1) % points.len()]);
                        a[0] * b[1] - b[0] * a[1]
                    })
                    .sum();
                twice.abs() / 2.0
            }
        }
    }

    /// One line with the exact numbers, for the chat.
    fn describe(&self) -> String {
        match self {
            Shape::Polygon(points) => {
                let list: Vec<String> = points.iter().map(|p| format!("({}, {})", mm(p[0]), mm(p[1]))).collect();
                format!("closed polygon through {}", list.join(", "))
            }
            Shape::Rect(a, b) => format!(
                "rectangle {} x {} mm, corners ({}, {}) and ({}, {})",
                mm((b[0] - a[0]).abs()),
                mm((b[1] - a[1]).abs()),
                mm(a[0].min(b[0])),
                mm(a[1].min(b[1])),
                mm(a[0].max(b[0])),
                mm(a[1].max(b[1]))
            ),
            Shape::Circle { centre, radius } => {
                format!("circle of diameter {} mm, centre ({}, {})", mm(radius * 2.0), mm(centre[0]), mm(centre[1]))
            }
        }
    }

    /// build123d code for the shape as a flat face.
    fn code(&self) -> String {
        match self {
            Shape::Polygon(points) => {
                let list: Vec<String> = points.iter().map(|p| format!("({}, {})", mm(p[0]), mm(p[1]))).collect();
                format!("Polygon({}, align=None)", list.join(", "))
            }
            Shape::Rect(a, b) => format!(
                "Pos({}, {}) * Rectangle({}, {})",
                mm((a[0] + b[0]) / 2.0),
                mm((a[1] + b[1]) / 2.0),
                mm((b[0] - a[0]).abs()),
                mm((b[1] - a[1]).abs())
            ),
            Shape::Circle { centre, radius } => format!("Pos({}, {}) * Circle({})", mm(centre[0]), mm(centre[1]), mm(*radius)),
        }
    }
}

fn distance(a: Point, b: Point) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn on_segment(a: Point, b: Point, p: Point) -> bool {
    let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
    let within = p[0] >= a[0].min(b[0]) - 1e-9 && p[0] <= a[0].max(b[0]) + 1e-9 && p[1] >= a[1].min(b[1]) - 1e-9 && p[1] <= a[1].max(b[1]) + 1e-9;
    cross.abs() < 1e-9 && within
}

/// Which shapes are holes: those sitting inside an odd number of other shapes.
pub fn holes(shapes: &[Shape]) -> Vec<bool> {
    shapes
        .iter()
        .enumerate()
        .map(|(i, shape)| {
            let anchor = shape.anchor();
            let depth = shapes.iter().enumerate().filter(|(j, other)| *j != i && other.contains(anchor)).count();
            depth % 2 == 1
        })
        .collect()
}

/// Area of material: outlines minus holes, in mm².
pub fn area(shapes: &[Shape]) -> f64 {
    shapes.iter().zip(holes(shapes)).map(|(s, hole)| if hole { -s.area() } else { s.area() }).sum()
}

/// The sketch in words and exact numbers, to put in a chat message.
pub fn describe(shapes: &[Shape]) -> String {
    let mut text = String::from("Sketch from the sketch panel (XY plane, mm, exact):\n");
    for (shape, hole) in shapes.iter().zip(holes(shapes)) {
        text += &format!("- {}: {}\n", if hole { "hole" } else { "outline" }, shape.describe());
    }
    text
}

/// What to do with the extruded sketch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Start a new part from it.
    NewPart,
    /// Add it to the selected part.
    Add,
    /// Cut it out of the selected part.
    Cut,
}

/// A feature made from the sketch: label, snippet and parameters.
pub struct SketchFeature {
    pub mode: Mode,
    pub label: String,
    pub code: String,
    pub params: serde_json::Map<String, Value>,
}

/// The build123d snippet that extrudes the sketch. Height and start level are
/// parameters, so they can be dragged in the Inspector afterwards.
pub fn feature(shapes: &[Shape], mode: Mode, height: f64) -> Option<SketchFeature> {
    let is_hole = holes(shapes);
    let join = |want_hole: bool| -> Vec<String> {
        shapes.iter().zip(&is_hole).filter(|(_, hole)| **hole == want_hole).map(|(s, _)| s.code()).collect()
    };
    let (outlines, cut_outs) = (join(false), join(true));
    if outlines.is_empty() {
        return None;
    }
    let mut code = String::from("# Drawn in the sketch panel: XY plane, mm.\n");
    code += &format!("profile = {}\n", outlines[0]);
    for outline in &outlines[1..] {
        code += &format!("profile += {outline}\n");
    }
    for hole in &cut_outs {
        code += &format!("profile -= {hole}\n");
    }
    code += "body = Pos(0, 0, p[\"z\"]) * extrude(profile, p[\"height\"])\n";
    code += match mode {
        Mode::NewPart => "part = body\n",
        Mode::Add => "part = body if part is None else part + body\n",
        Mode::Cut => "part = part - body\n",
    };
    let label = match mode {
        Mode::Cut => "Sketched cut",
        _ => "Sketched extrude",
    };
    let params = json!({ "height": height, "z": 0 }).as_object().cloned().unwrap_or_default();
    Some(SketchFeature { mode, label: label.to_owned(), code, params })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Line,
    Rectangle,
    Circle,
    Erase,
}

pub enum SketchAction {
    /// Put this text in the chat box.
    ToChat(String),
    Feature(SketchFeature),
}

pub struct Sketch {
    pub shapes: Vec<Shape>,
    tool: Tool,
    /// Points placed so far for the shape being drawn.
    draft: Vec<Point>,
    /// Grid step in mm.
    snap: f64,
    /// Pixels per mm.
    zoom: f32,
    /// Where the sketch origin sits, as an offset from the centre of the canvas.
    pan: egui::Vec2,
    pub height: f64,
}

impl Default for Sketch {
    fn default() -> Self {
        Self { shapes: Vec::new(), tool: Tool::Line, draft: Vec::new(), snap: 1.0, zoom: 6.0, pan: egui::Vec2::ZERO, height: 5.0 }
    }
}

const CLOSE_RADIUS: f32 = 9.0;

impl Sketch {
    pub fn to_screen(&self, rect: egui::Rect, p: Point) -> egui::Pos2 {
        rect.center() + self.pan + egui::vec2(p[0] as f32, -p[1] as f32) * self.zoom
    }

    fn to_sketch(&self, rect: egui::Rect, pos: egui::Pos2) -> Point {
        let v = (pos - rect.center() - self.pan) / self.zoom;
        let snap = |x: f32| (f64::from(x) / self.snap).round() * self.snap;
        [snap(v.x), snap(-v.y)]
    }

    /// A click on the canvas at a snapped point. `closing` is true when the click
    /// landed on the first point of the outline being drawn.
    fn click(&mut self, p: Point, closing: bool) {
        match self.tool {
            Tool::Line => {
                if closing && self.draft.len() >= 3 {
                    self.shapes.push(Shape::Polygon(std::mem::take(&mut self.draft)));
                } else if self.draft.last() != Some(&p) && !closing {
                    self.draft.push(p);
                }
            }
            Tool::Rectangle | Tool::Circle => match self.draft.pop() {
                None => self.draft.push(p),
                Some(first) if first == p => self.draft.push(first),
                Some(first) if self.tool == Tool::Rectangle => {
                    // A rectangle needs both a width and a height.
                    if first[0] != p[0] && first[1] != p[1] {
                        self.shapes.push(Shape::Rect(first, p));
                    } else {
                        self.draft.push(first);
                    }
                }
                Some(first) => self.shapes.push(Shape::Circle { centre: first, radius: distance(first, p) }),
            },
            Tool::Erase => {
                // The smallest shape under the click, so a hole can be picked inside its outline.
                let hit = self
                    .shapes
                    .iter()
                    .enumerate()
                    .filter(|(_, s)| s.contains(p) || s.anchor() == p)
                    .min_by(|a, b| a.1.area().total_cmp(&b.1.area()))
                    .map(|(i, _)| i);
                if let Some(i) = hit {
                    self.shapes.remove(i);
                }
            }
        }
    }

    /// Finish the outline being drawn, if it has enough points.
    fn close(&mut self) {
        if self.tool == Tool::Line && self.draft.len() >= 3 {
            self.shapes.push(Shape::Polygon(std::mem::take(&mut self.draft)));
        }
    }

    fn undo(&mut self) {
        if self.draft.pop().is_none() {
            self.shapes.pop();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, selected_part: Option<&str>) -> Option<SketchAction> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            for (tool, name, hint) in [
                (Tool::Line, "Line", "Click points. Click the first point again, or press Enter, to close the outline."),
                (Tool::Rectangle, "Rectangle", "Click two opposite corners."),
                (Tool::Circle, "Circle", "Click the centre, then a point on the circle."),
                (Tool::Erase, "Erase", "Click inside a shape to remove it."),
            ] {
                if ui.selectable_label(self.tool == tool, name).on_hover_text(hint).clicked() {
                    self.tool = tool;
                    self.draft.clear();
                }
            }
            ui.separator();
            ui.label("Grid");
            for step in [0.5, 1.0, 5.0] {
                if ui.selectable_label(self.snap == step, format!("{} mm", mm(step))).clicked() {
                    self.snap = step;
                }
            }
            ui.separator();
            let anything = !self.shapes.is_empty() || !self.draft.is_empty();
            if ui.add_enabled(anything, egui::Button::new("Undo")).on_hover_text("Remove the last point or shape (Backspace)").clicked() {
                self.undo();
            }
            if ui.add_enabled(anything, egui::Button::new("Clear")).clicked() {
                self.shapes.clear();
                self.draft.clear();
            }
        });
        ui.horizontal_wrapped(|ui| {
            let ready = !self.shapes.is_empty();
            ui.label("Height");
            ui.add(egui::DragValue::new(&mut self.height).speed(0.1).range(0.1..=10_000.0).suffix(" mm"));
            let mut extrude = |ui: &mut egui::Ui, enabled: bool, text: String, hint: &str, mode: Mode| {
                if ui.add_enabled(enabled, egui::Button::new(text)).on_hover_text(hint).clicked() {
                    action = feature(&self.shapes, mode, self.height).map(SketchAction::Feature);
                }
            };
            extrude(ui, ready, "New part".to_owned(), "Extrude the sketch as a new part", Mode::NewPart);
            let name = selected_part.unwrap_or("part");
            let has_part = ready && selected_part.is_some();
            extrude(ui, has_part, format!("Add to {name}"), "Extrude the sketch and join it to the selected part", Mode::Add);
            extrude(ui, has_part, format!("Cut from {name}"), "Extrude the sketch and cut it out of the selected part", Mode::Cut);
            if ui.add_enabled(ready, egui::Button::new("Send to chat")).on_hover_text("Put the sketch's exact coordinates in the chat box").clicked() {
                action = Some(SketchAction::ToChat(describe(&self.shapes)));
            }
        });

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::CLICK | egui::Sense::DRAG);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Sketch canvas"));
        if rect.width() < 20.0 || rect.height() < 20.0 {
            return action;
        }

        if response.dragged_by(egui::PointerButton::Secondary) || response.dragged_by(egui::PointerButton::Middle) {
            self.pan += response.drag_delta();
        }
        let hover = response.hover_pos();
        if let Some(pos) = hover {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                // Zoom about the cursor, so the point under it stays put.
                let before = (pos - rect.center() - self.pan) / self.zoom;
                self.zoom = (self.zoom * (scroll * 0.002).exp()).clamp(0.5, 80.0);
                self.pan = pos - rect.center() - before * self.zoom;
            }
        }
        let cursor = hover.map(|pos| self.to_sketch(rect, pos));
        let over_first = hover.zip(self.draft.first()).is_some_and(|(pos, first)| {
            self.tool == Tool::Line && self.draft.len() >= 3 && self.to_screen(rect, *first).distance(pos) < CLOSE_RADIUS
        });
        if response.double_clicked() {
            self.close();
        } else if response.clicked() {
            if let Some(p) = response.interact_pointer_pos().map(|pos| self.to_sketch(rect, pos)) {
                self.click(p, over_first);
            }
        }
        if response.hovered() && !ui.ctx().egui_wants_keyboard_input() {
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                self.close();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.draft.clear();
            }
            if ui.input(|i| i.key_pressed(egui::Key::Backspace)) {
                self.undo();
            }
        }

        self.paint(ui, rect, cursor, over_first);
        action
    }

    fn paint(&self, ui: &egui::Ui, rect: egui::Rect, cursor: Option<Point>, over_first: bool) {
        let painter = ui.painter_at(rect);
        let visuals = ui.visuals();
        painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);

        // Grid: fine lines when they are far enough apart to read, bold every 10 mm.
        let faint = visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.45);
        for (step, color) in [(1.0_f32, faint), (10.0, visuals.widgets.noninteractive.bg_stroke.color)] {
            let gap = step * self.zoom;
            if gap < 7.0 {
                continue;
            }
            let origin = rect.center() + self.pan;
            let mut x = origin.x + ((rect.left() - origin.x) / gap).ceil() * gap;
            while x < rect.right() {
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], egui::Stroke::new(1.0, color));
                x += gap;
            }
            let mut y = origin.y + ((rect.top() - origin.y) / gap).ceil() * gap;
            while y < rect.bottom() {
                painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], egui::Stroke::new(1.0, color));
                y += gap;
            }
        }
        let origin = self.to_screen(rect, [0.0, 0.0]);
        let (red, green) = (egui::Color32::from_rgb(220, 70, 70), egui::Color32::from_rgb(70, 180, 80));
        painter.line_segment([egui::pos2(rect.left(), origin.y), egui::pos2(rect.right(), origin.y)], egui::Stroke::new(1.0, red.gamma_multiply(0.6)));
        painter.line_segment([egui::pos2(origin.x, rect.top()), egui::pos2(origin.x, rect.bottom())], egui::Stroke::new(1.0, green.gamma_multiply(0.6)));

        let solid = egui::Color32::from_rgb(70, 130, 215);
        let hole = egui::Color32::from_rgb(235, 150, 50);
        let text = egui::FontId::proportional(12.0);
        // Outlines first, so a hole's blank fill always ends up on top of its outline.
        let mut order: Vec<(&Shape, bool)> = self.shapes.iter().zip(holes(&self.shapes)).collect();
        order.sort_by_key(|(_, is_hole)| *is_hole);
        for (shape, is_hole) in order {
            let color = if is_hole { hole } else { solid };
            let points: Vec<egui::Pos2> = shape.outline().into_iter().map(|p| self.to_screen(rect, p)).collect();
            // Only rectangles and circles are filled: the fill assumes a convex shape.
            if !matches!(shape, Shape::Polygon(_)) {
                let fill = if is_hole { visuals.extreme_bg_color } else { solid.gamma_multiply(0.22) };
                painter.add(egui::Shape::convex_polygon(points.clone(), fill, egui::Stroke::NONE));
            }
            painter.add(egui::Shape::closed_line(points, egui::Stroke::new(2.0, color)));
            let (label, at) = match shape {
                Shape::Circle { centre, radius } => (format!("Ø {}", mm(radius * 2.0)), *centre),
                Shape::Rect(a, b) => (
                    format!("{} x {}", mm((b[0] - a[0]).abs()), mm((b[1] - a[1]).abs())),
                    [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0],
                ),
                Shape::Polygon(points) => {
                    let n = points.len() as f64;
                    (format!("{} mm²", mm(shape.area())), [points.iter().map(|p| p[0]).sum::<f64>() / n, points.iter().map(|p| p[1]).sum::<f64>() / n])
                }
            };
            // A rectangle's size sits in its top left corner, clear of anything drawn inside it.
            let (pos, align) = match shape {
                Shape::Rect(a, b) => {
                    let corner = self.to_screen(rect, [a[0].min(b[0]), a[1].max(b[1])]);
                    (corner + egui::vec2(5.0, 4.0), egui::Align2::LEFT_TOP)
                }
                _ => (self.to_screen(rect, at), egui::Align2::CENTER_CENTER),
            };
            painter.text(pos, align, label, text.clone(), color);
        }

        // The shape being drawn, following the cursor.
        let draft_stroke = egui::Stroke::new(1.5, visuals.strong_text_color());
        match (self.tool, self.draft.as_slice(), cursor) {
            (Tool::Line, points, _) if !points.is_empty() => {
                let mut line: Vec<egui::Pos2> = points.iter().map(|p| self.to_screen(rect, *p)).collect();
                if let Some(c) = cursor {
                    line.push(self.to_screen(rect, c));
                    let last = points[points.len() - 1];
                    let mid = self.to_screen(rect, [(last[0] + c[0]) / 2.0, (last[1] + c[1]) / 2.0]);
                    painter.text(mid + egui::vec2(0.0, -10.0), egui::Align2::CENTER_CENTER, mm(distance(last, c)), text.clone(), visuals.strong_text_color());
                }
                painter.add(egui::Shape::line(line, draft_stroke));
                for p in points {
                    painter.circle_filled(self.to_screen(rect, *p), 3.0, visuals.strong_text_color());
                }
                if over_first {
                    painter.circle_stroke(self.to_screen(rect, points[0]), CLOSE_RADIUS, egui::Stroke::new(2.0, green));
                }
            }
            (Tool::Rectangle, [first], Some(c)) => {
                let corners = egui::Rect::from_two_pos(self.to_screen(rect, *first), self.to_screen(rect, c));
                painter.rect_stroke(corners, 0.0, draft_stroke, egui::StrokeKind::Middle);
                let size = format!("{} x {}", mm((c[0] - first[0]).abs()), mm((c[1] - first[1]).abs()));
                painter.text(corners.center(), egui::Align2::CENTER_CENTER, size, text.clone(), visuals.strong_text_color());
            }
            (Tool::Circle, [centre], Some(c)) => {
                let radius = distance(*centre, c);
                painter.circle_stroke(self.to_screen(rect, *centre), radius as f32 * self.zoom, draft_stroke);
                painter.text(self.to_screen(rect, *centre), egui::Align2::CENTER_CENTER, format!("Ø {}", mm(radius * 2.0)), text.clone(), visuals.strong_text_color());
            }
            _ => {}
        }

        if let Some(c) = cursor {
            painter.circle_filled(self.to_screen(rect, c), 2.5, visuals.strong_text_color());
            let readout = format!("X {}  Y {} mm", mm(c[0]), mm(c[1]));
            painter.text(rect.left_bottom() + egui::vec2(8.0, -6.0), egui::Align2::LEFT_BOTTOM, readout, text.clone(), visuals.text_color());
        }
        if self.shapes.is_empty() && self.draft.is_empty() {
            let hint = "Draw an outline on the grid. A shape drawn inside another becomes a hole.\nRight drag pans, scroll zooms.";
            painter.text(rect.center_top() + egui::vec2(0.0, 14.0), egui::Align2::CENTER_TOP, hint, text, visuals.weak_text_color());
        } else {
            let summary = format!("{} mm² of material", mm(area(&self.shapes)));
            painter.text(rect.right_bottom() + egui::vec2(-8.0, -6.0), egui::Align2::RIGHT_BOTTOM, summary, text, visuals.text_color());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plate_with_holes() -> Vec<Shape> {
        vec![
            Shape::Rect([0.0, 0.0], [40.0, 20.0]),
            Shape::Circle { centre: [10.0, 10.0], radius: 4.0 },
            Shape::Polygon(vec![[26.0, 6.0], [34.0, 6.0], [30.0, 14.0]]),
            // Not inside anything: a second outline.
            Shape::Circle { centre: [60.0, 10.0], radius: 5.0 },
        ]
    }

    #[test]
    fn shapes_inside_another_are_holes() {
        let shapes = plate_with_holes();
        assert_eq!(holes(&shapes), [false, true, true, false]);
        let expected = 800.0 - std::f64::consts::PI * 16.0 - 32.0 + std::f64::consts::PI * 25.0;
        assert!((area(&shapes) - expected).abs() < 1e-9);
        // An island inside a hole is material again.
        let mut nested = shapes.clone();
        nested.push(Shape::Circle { centre: [10.0, 10.0], radius: 1.0 });
        assert!(!holes(&nested)[4]);
        // Shapes that only touch are not nested.
        let touching = [Shape::Rect([0.0, 0.0], [10.0, 10.0]), Shape::Rect([10.0, 0.0], [20.0, 10.0])];
        assert_eq!(holes(&touching), [false, false]);
    }

    #[test]
    fn the_snippet_extrudes_outlines_and_cuts_holes() {
        let made = feature(&plate_with_holes(), Mode::NewPart, 5.0).unwrap();
        assert_eq!(made.label, "Sketched extrude");
        assert_eq!(made.params["height"], 5.0);
        let lines: Vec<&str> = made.code.lines().collect();
        assert_eq!(lines[1], "profile = Pos(20, 10) * Rectangle(40, 20)");
        assert_eq!(lines[2], "profile += Pos(60, 10) * Circle(5)");
        assert_eq!(lines[3], "profile -= Pos(10, 10) * Circle(4)");
        assert_eq!(lines[4], "profile -= Polygon((26, 6), (34, 6), (30, 14), align=None)");
        assert_eq!(lines[6], "part = body");
        assert!(feature(&plate_with_holes(), Mode::Cut, 2.0).unwrap().code.ends_with("part = part - body\n"));
        assert!(feature(&[], Mode::NewPart, 5.0).is_none());
    }

    #[test]
    fn the_description_gives_exact_numbers() {
        let text = describe(&plate_with_holes());
        assert!(text.contains("- outline: rectangle 40 x 20 mm, corners (0, 0) and (40, 20)"));
        assert!(text.contains("- hole: circle of diameter 8 mm, centre (10, 10)"));
        assert!(text.contains("- hole: closed polygon through (26, 6), (34, 6), (30, 14)"));
    }

    #[test]
    fn clicks_build_shapes() {
        let mut sketch = Sketch::default();
        for p in [[0.0, 0.0], [30.0, 0.0], [30.0, 0.0], [30.0, 20.0]] {
            sketch.click(p, false);
        }
        assert_eq!(sketch.draft.len(), 3, "a repeated click adds no point");
        sketch.click([0.0, 0.0], true);
        assert_eq!(sketch.shapes, [Shape::Polygon(vec![[0.0, 0.0], [30.0, 0.0], [30.0, 20.0]])]);

        sketch.tool = Tool::Rectangle;
        sketch.click([2.0, 2.0], false);
        sketch.click([2.0, 9.0], false);
        assert_eq!(sketch.shapes.len(), 1, "a rectangle with no width isn't made");
        sketch.click([8.0, 6.0], false);
        assert_eq!(sketch.shapes[1], Shape::Rect([2.0, 2.0], [8.0, 6.0]));

        sketch.tool = Tool::Circle;
        sketch.click([20.0, 5.0], false);
        sketch.click([23.0, 5.0], false);
        assert_eq!(sketch.shapes[2], Shape::Circle { centre: [20.0, 5.0], radius: 3.0 });

        // Erase picks the smallest shape under the click, then undo removes the last one.
        sketch.tool = Tool::Erase;
        sketch.click([5.0, 4.0], false);
        assert_eq!(sketch.shapes.len(), 2);
        assert!(matches!(sketch.shapes[1], Shape::Circle { .. }));
        sketch.undo();
        assert_eq!(sketch.shapes.len(), 1);
    }

    #[test]
    fn the_grid_snaps_both_ways() {
        let mut sketch = Sketch::default();
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(600.0, 400.0));
        let p = [12.0, -7.0];
        assert_eq!(sketch.to_sketch(rect, sketch.to_screen(rect, p)), p);
        // A point between grid lines lands on the nearest one.
        let near = sketch.to_screen(rect, [12.3, -6.8]);
        assert_eq!(sketch.to_sketch(rect, near), [12.0, -7.0]);
        sketch.snap = 0.5;
        assert_eq!(sketch.to_sketch(rect, near), [12.5, -7.0]);
    }
}
