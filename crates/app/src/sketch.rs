// The sketch panel: a rough drawing pad for showing the model what you mean.
// Draw freehand or with simple shapes, add labels and dimensions, give each colour
// a meaning, and optionally draw over a flat view of the model. The sketch goes to
// the chat as a picture plus a written summary. It is never turned into geometry.

use eframe::egui;

use crate::measure::mm;

type Point = [f64; 2];

/// Pens. The name is what the model reads in the colour key.
pub const COLOURS: [(&str, egui::Color32); 6] = [
    ("red", egui::Color32::from_rgb(235, 80, 80)),
    ("orange", egui::Color32::from_rgb(240, 160, 50)),
    ("green", egui::Color32::from_rgb(90, 200, 110)),
    ("blue", egui::Color32::from_rgb(90, 160, 245)),
    ("purple", egui::Color32::from_rgb(190, 130, 240)),
    ("white", egui::Color32::from_rgb(235, 235, 235)),
];

#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// A freehand line.
    Stroke { points: Vec<Point>, colour: usize },
    Line { a: Point, b: Point, arrow: bool, colour: usize },
    Rect { a: Point, b: Point, colour: usize },
    Circle { centre: Point, radius: f64, colour: usize },
    Label { at: Point, text: String, colour: usize },
    /// A dimension line. `text` is what the user wrote on it. Empty shows the drawn length.
    Dimension { a: Point, b: Point, text: String, colour: usize },
}

impl Item {
    fn colour(&self) -> usize {
        match self {
            Item::Stroke { colour, .. }
            | Item::Line { colour, .. }
            | Item::Rect { colour, .. }
            | Item::Circle { colour, .. }
            | Item::Label { colour, .. }
            | Item::Dimension { colour, .. } => *colour,
        }
    }

    /// Distance in mm from a point to the item, for the eraser.
    fn distance_to(&self, p: Point) -> f64 {
        match self {
            Item::Stroke { points, .. } => points.windows(2).map(|w| to_segment(p, w[0], w[1])).fold(f64::MAX, f64::min),
            Item::Line { a, b, .. } | Item::Dimension { a, b, .. } => to_segment(p, *a, *b),
            Item::Rect { a, b, .. } => {
                let corners = [*a, [b[0], a[1]], *b, [a[0], b[1]]];
                (0..4).map(|i| to_segment(p, corners[i], corners[(i + 1) % 4])).fold(f64::MAX, f64::min)
            }
            Item::Circle { centre, radius, .. } => (distance(*centre, p) - radius).abs(),
            Item::Label { at, .. } => distance(*at, p),
        }
    }

    /// One line for the written summary.
    fn describe(&self) -> String {
        let at = |p: &Point| format!("({}, {})", mm(round1(p[0])), mm(round1(p[1])));
        let colour = COLOURS[self.colour()].0;
        match self {
            Item::Stroke { points, .. } => {
                let (lo, hi) = bounds(points);
                format!("{colour} freehand line from {} to {}, within {} to {}", at(&points[0]), at(&points[points.len() - 1]), at(&lo), at(&hi))
            }
            Item::Line { a, b, arrow: true, .. } => format!("{colour} arrow from {} pointing to {}", at(a), at(b)),
            Item::Line { a, b, .. } => format!("{colour} line from {} to {}", at(a), at(b)),
            Item::Rect { a, b, .. } => {
                let (lo, hi) = bounds(&[*a, *b]);
                format!("{colour} rectangle {} x {} mm, corners {} and {}", mm(round1(hi[0] - lo[0])), mm(round1(hi[1] - lo[1])), at(&lo), at(&hi))
            }
            Item::Circle { centre, radius, .. } => format!("{colour} circle, diameter {} mm, centre {}", mm(round1(radius * 2.0)), at(centre)),
            Item::Label { at: p, text, .. } => format!("{colour} label \"{text}\" at {}", at(p)),
            Item::Dimension { a, b, text, .. } => {
                let drawn = mm(round1(distance(*a, *b)));
                if text.trim().is_empty() {
                    format!("{colour} dimension of {drawn} mm between {} and {}", at(a), at(b))
                } else {
                    format!("{colour} dimension marked \"{}\" between {} and {} (drawn {drawn} mm long)", text.trim(), at(a), at(b))
                }
            }
        }
    }
}

fn distance(a: Point, b: Point) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn to_segment(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    distance(p, [a[0] + dx * t, a[1] + dy * t])
}

fn bounds(points: &[Point]) -> (Point, Point) {
    let fold = |pick: fn(f64, f64) -> f64, start: f64| [0, 1].map(|i| points.iter().map(|p| p[i]).fold(start, pick));
    (fold(f64::min, f64::MAX), fold(f64::max, f64::MIN))
}

fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// How a flat view of the model maps onto the sketch: which model axis runs along
/// sketch X and sketch Y, and whether it is reversed.
pub fn view_axes(view: &str) -> Option<[(usize, f64); 2]> {
    Some(match view {
        "top" => [(0, 1.0), (1, 1.0)],
        "bottom" => [(0, 1.0), (1, -1.0)],
        "front" => [(0, 1.0), (2, 1.0)],
        "back" => [(0, -1.0), (2, 1.0)],
        "right" => [(1, 1.0), (2, 1.0)],
        "left" => [(1, -1.0), (2, 1.0)],
        _ => return None,
    })
}

fn axis_name((axis, sign): (usize, f64)) -> String {
    format!("{}{}", if sign < 0.0 { "-" } else { "" }, ["X", "Y", "Z"][axis])
}

/// A flat picture of the model behind the sketch, at true scale.
pub struct Backdrop {
    pub view: String,
    texture: egui::TextureHandle,
    /// Middle of the picture in sketch coordinates, and half its side, in mm.
    centre: Point,
    half: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Pen,
    Line,
    Arrow,
    Rectangle,
    Circle,
    Label,
    Dimension,
    Erase,
}

/// Getting the finished sketch out as a picture takes a couple of frames.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Capture {
    Idle,
    /// The user pressed Attach. The next frame is drawn clean and photographed.
    Requested,
    /// Waiting for the screenshot of this area of the window.
    Waiting(egui::Rect),
}

pub enum SketchAction {
    /// Draw over this view of the model, or over nothing.
    Backdrop(Option<String>),
}

pub struct Sketch {
    pub items: Vec<Item>,
    /// What each colour stands for, as typed by the user.
    pub meanings: [String; COLOURS.len()],
    tool: Tool,
    colour: usize,
    snap: bool,
    /// Where the current drag started, and the freehand points collected so far.
    drag_start: Option<Point>,
    pen: Vec<Point>,
    /// The label or dimension whose text is being typed.
    editing: Option<usize>,
    /// Pixels per mm, and where the origin sits relative to the middle of the canvas.
    zoom: f32,
    pan: egui::Vec2,
    pub backdrop: Option<Backdrop>,
    pub capture: Capture,
}

impl Default for Sketch {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            meanings: Default::default(),
            tool: Tool::Pen,
            colour: 0,
            snap: true,
            drag_start: None,
            pen: Vec::new(),
            editing: None,
            zoom: 6.0,
            pan: egui::Vec2::ZERO,
            backdrop: None,
            capture: Capture::Idle,
        }
    }
}

impl Sketch {
    pub fn to_screen(&self, rect: egui::Rect, p: Point) -> egui::Pos2 {
        rect.center() + self.pan + egui::vec2(p[0] as f32, -p[1] as f32) * self.zoom
    }

    fn to_sketch(&self, rect: egui::Rect, pos: egui::Pos2, snap: bool) -> Point {
        let v = (pos - rect.center() - self.pan) / self.zoom;
        let fix = |x: f32| if snap { f64::from(x).round() } else { round1(f64::from(x)) };
        [fix(v.x), fix(-v.y)]
    }

    /// Put a flat picture of the model behind the sketch. `centre` is the middle of
    /// the picture in model coordinates and `half` is half its side in mm.
    pub fn set_backdrop(&mut self, ctx: &egui::Context, view: &str, pixels: &[u8], size: u32, centre: [f32; 3], half: f32) {
        let Some(axes) = view_axes(view) else { return };
        let image = egui::ColorImage::from_rgba_unmultiplied([size as usize; 2], pixels);
        let texture = ctx.load_texture("sketch backdrop", image, egui::TextureOptions::LINEAR);
        let along = |(axis, sign): (usize, f64)| f64::from(centre[axis]) * sign;
        self.backdrop = Some(Backdrop { view: view.to_owned(), texture, centre: [along(axes[0]), along(axes[1])], half: f64::from(half) });
        // Frame the picture.
        self.pan = egui::vec2(-along(axes[0]) as f32, along(axes[1]) as f32) * self.zoom;
    }

    /// The sketch in words, sent with the picture so a model that reads images
    /// poorly, or not at all, still gets the labels, numbers and colour key.
    pub fn describe(&self) -> String {
        let mut text = String::from(
            "The user drew this sketch to show what they want. It is rough: treat positions and sizes as approximate unless a dimension or label says otherwise. Coordinates are in mm.\n",
        );
        match self.backdrop.as_ref().and_then(|b| Some((b, view_axes(&b.view)?))) {
            Some((backdrop, axes)) => {
                text += &format!(
                    "It is drawn over the {} view of the model at true scale: sketch X is model {}, sketch Y is model {}.\n",
                    backdrop.view,
                    axis_name(axes[0]),
                    axis_name(axes[1])
                );
            }
            None => text += "It is drawn on a blank grid, not over the model.\n",
        }
        let used: Vec<usize> = (0..COLOURS.len()).filter(|c| self.items.iter().any(|i| i.colour() == *c)).collect();
        let key: Vec<String> = used
            .iter()
            .filter(|c| !self.meanings[**c].trim().is_empty())
            .map(|c| format!("{} = {}", COLOURS[*c].0, self.meanings[*c].trim()))
            .collect();
        if !key.is_empty() {
            text += &format!("Colour key: {}.\n", key.join("; "));
        }
        for (heading, wanted) in [("Labels", 0), ("Dimensions", 1), ("Drawn", 2)] {
            let lines: Vec<String> = self
                .items
                .iter()
                .filter(|item| match item {
                    Item::Label { .. } => wanted == 0,
                    Item::Dimension { .. } => wanted == 1,
                    _ => wanted == 2,
                })
                .map(|item| format!("- {}\n", item.describe()))
                .collect();
            if !lines.is_empty() {
                text += &format!("{heading}:\n{}", lines.concat());
            }
        }
        text
    }

    /// Finish a drag from `a` to `b` with the current tool.
    fn finish_drag(&mut self, a: Point, b: Point) {
        let colour = self.colour;
        if a == b && self.tool != Tool::Pen {
            return;
        }
        match self.tool {
            Tool::Pen => {
                let points = std::mem::take(&mut self.pen);
                if points.len() >= 2 {
                    self.items.push(Item::Stroke { points, colour });
                }
            }
            Tool::Line => self.items.push(Item::Line { a, b, arrow: false, colour }),
            Tool::Arrow => self.items.push(Item::Line { a, b, arrow: true, colour }),
            Tool::Rectangle => self.items.push(Item::Rect { a, b, colour }),
            Tool::Circle => self.items.push(Item::Circle { centre: a, radius: distance(a, b), colour }),
            Tool::Dimension => {
                self.items.push(Item::Dimension { a, b, text: String::new(), colour });
                self.editing = Some(self.items.len() - 1);
            }
            Tool::Label | Tool::Erase => {}
        }
    }

    fn click(&mut self, p: Point) {
        match self.tool {
            Tool::Label => {
                self.items.push(Item::Label { at: p, text: String::new(), colour: self.colour });
                self.editing = Some(self.items.len() - 1);
            }
            Tool::Erase => {
                // Whatever is nearest, within a few pixels.
                let reach = f64::from(10.0 / self.zoom);
                let nearest = self
                    .items
                    .iter()
                    .enumerate()
                    .map(|(i, item)| (i, item.distance_to(p)))
                    .filter(|(_, d)| *d <= reach)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((i, _)) = nearest {
                    self.items.remove(i);
                }
            }
            _ => {}
        }
    }

    /// Stop typing. A label left empty is dropped.
    fn finish_editing(&mut self) {
        if let Some(i) = self.editing.take() {
            if matches!(self.items.get(i), Some(Item::Label { text, .. }) if text.trim().is_empty()) {
                self.items.remove(i);
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, can_attach: bool) -> Option<SketchAction> {
        let mut action = None;
        ui.horizontal_wrapped(|ui| {
            for (tool, name, hint) in [
                (Tool::Pen, "Pen", "Drag to draw freehand"),
                (Tool::Line, "Line", "Drag from one end to the other"),
                (Tool::Arrow, "Arrow", "Drag from the tail to the thing it points at"),
                (Tool::Rectangle, "Rectangle", "Drag from corner to corner"),
                (Tool::Circle, "Circle", "Drag from the centre outwards"),
                (Tool::Label, "Label", "Click where the text should go, then type"),
                (Tool::Dimension, "Dimension", "Drag between two points, then type the size you want (or leave it as drawn)"),
                (Tool::Erase, "Erase", "Click something to remove it"),
            ] {
                if ui.selectable_label(self.tool == tool, name).on_hover_text(hint).clicked() {
                    self.finish_editing();
                    self.tool = tool;
                }
            }
            ui.separator();
            for (i, (name, colour)) in COLOURS.iter().enumerate() {
                let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
                ui.painter().rect_filled(rect.shrink(2.0), 3.0, *colour);
                if self.colour == i {
                    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(2.0, ui.visuals().strong_text_color()), egui::StrokeKind::Inside);
                }
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{name} pen")));
                if response.on_hover_text(*name).clicked() {
                    self.colour = i;
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.snap, "Snap to 1 mm").on_hover_text("Shapes, labels and dimensions land on the grid. The pen never snaps.");
            ui.separator();
            ui.label("Draw over");
            let current = self.backdrop.as_ref().map_or("nothing", |b| b.view.as_str()).to_owned();
            egui::ComboBox::from_id_salt("sketch backdrop")
                .selected_text(current.as_str())
                .show_ui(ui, |ui| {
                    if ui.selectable_label(self.backdrop.is_none(), "nothing").clicked() {
                        action = Some(SketchAction::Backdrop(None));
                    }
                    for view in ["top", "front", "right", "back", "left", "bottom"] {
                        if ui.selectable_label(current == view, view).clicked() {
                            action = Some(SketchAction::Backdrop(Some(view.to_owned())));
                        }
                    }
                })
                .response
                .on_hover_text("Put a flat view of the model behind the sketch, at true scale, to draw on one of its faces");
            if self.backdrop.is_some() && ui.button("Refresh").on_hover_text("Take the picture again after the model changed").clicked() {
                action = Some(SketchAction::Backdrop(Some(current)));
            }
            ui.separator();
            let anything = !self.items.is_empty();
            if ui.add_enabled(anything, egui::Button::new("Undo")).clicked() {
                self.editing = None;
                self.items.pop();
            }
            if ui.add_enabled(anything, egui::Button::new("Clear")).clicked() {
                self.editing = None;
                self.items.clear();
            }
            let hint = if can_attach {
                "Attach the sketch to your next chat message, as a picture with a written summary"
            } else {
                "Wait for the current reply to finish"
            };
            if ui.add_enabled(anything && can_attach, egui::Button::new("Attach to chat")).on_hover_text(hint).clicked() {
                self.finish_editing();
                self.capture = Capture::Requested;
            }
        });
        self.colour_key_ui(ui);

        let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::CLICK | egui::Sense::DRAG);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Sketch canvas"));
        if rect.width() < 20.0 || rect.height() < 20.0 {
            return action;
        }
        let clean = self.capture != Capture::Idle;
        if self.capture == Capture::Requested {
            // This frame is drawn without the cursor and then photographed.
            self.capture = Capture::Waiting(rect);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
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

        let snap = self.snap && self.tool != Tool::Pen;
        let pointer = response.interact_pointer_pos().or(hover);
        if response.drag_started_by(egui::PointerButton::Primary) {
            self.finish_editing();
            // The press position, not where the pointer has got to since.
            let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
            self.drag_start = origin.map(|pos| self.to_sketch(rect, pos, snap));
            self.pen.clear();
            if let (Tool::Pen, Some(pos)) = (self.tool, origin) {
                self.pen.push(self.to_sketch(rect, pos, false));
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) && self.tool == Tool::Pen {
            if let Some(pos) = pointer {
                let p = self.to_sketch(rect, pos, false);
                // Skip points that barely moved, so a stroke stays light.
                let moved = self.pen.last().is_none_or(|last| distance(*last, p) * f64::from(self.zoom) > 2.0);
                if moved {
                    self.pen.push(p);
                }
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            if let (Some(a), Some(pos)) = (self.drag_start.take(), pointer) {
                let b = self.to_sketch(rect, pos, snap);
                self.finish_drag(a, b);
            }
        } else if response.clicked() {
            let was_editing = self.editing.is_some();
            self.finish_editing();
            // A click that only ends typing doesn't also start a new label.
            if let (Some(pos), false) = (pointer, was_editing && self.tool == Tool::Label) {
                let p = self.to_sketch(rect, pos, snap);
                self.click(p);
            }
        }
        if response.hovered() && !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.key_pressed(egui::Key::Backspace)) {
            self.items.pop();
        }

        let cursor = hover.filter(|_| !clean).map(|pos| self.to_sketch(rect, pos, snap));
        let preview = self.drag_start.zip(pointer.map(|pos| self.to_sketch(rect, pos, snap)));
        self.paint(ui, rect, cursor, preview);
        self.text_box(ui, rect);
        action
    }

    /// One box per colour in use, for saying what it stands for.
    fn colour_key_ui(&mut self, ui: &mut egui::Ui) {
        let used: Vec<usize> = (0..COLOURS.len()).filter(|c| self.items.iter().any(|i| i.colour() == *c)).collect();
        // Always there, so the canvas doesn't jump when the first line is drawn.
        ui.horizontal_wrapped(|ui| {
            // The same height with or without text boxes in it.
            ui.set_min_height(24.0);
            ui.label("Colour key").on_hover_text("Say what each colour means, for example \"cut away\" or \"keep clear\"");
            if used.is_empty() {
                ui.weak("Each colour you draw with can be given a meaning here.");
            }
            for c in used {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, COLOURS[c].1);
                let field = egui::TextEdit::singleline(&mut self.meanings[c]).hint_text(format!("what {} means", COLOURS[c].0)).desired_width(130.0);
                let response = ui.add(field);
                response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, format!("{} meaning", COLOURS[c].0)));
            }
        });
    }

    /// The text box for the label or dimension being typed, placed on the canvas.
    fn text_box(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        let Some(i) = self.editing else { return };
        let (at, drawn) = match self.items.get(i) {
            Some(Item::Label { at, .. }) => (*at, None),
            Some(Item::Dimension { a, b, .. }) => ([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0], Some(distance(*a, *b))),
            _ => {
                self.editing = None;
                return;
            }
        };
        let pos = self.to_screen(rect, at);
        let Some(Item::Label { text, .. } | Item::Dimension { text, .. }) = self.items.get_mut(i) else { return };
        let hint = match drawn {
            Some(length) => format!("{} (as drawn)", mm(round1(length))),
            None => "label".to_owned(),
        };
        let area = egui::Rect::from_min_size(pos + egui::vec2(-4.0, -12.0), egui::vec2(150.0, 22.0));
        let response = ui.put(area, egui::TextEdit::singleline(text).hint_text(hint).char_limit(80));
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Sketch text"));
        if response.lost_focus() {
            self.finish_editing();
        } else if !response.has_focus() {
            response.request_focus();
        }
    }

    fn paint(&self, ui: &egui::Ui, rect: egui::Rect, cursor: Option<Point>, preview: Option<(Point, Point)>) {
        let painter = ui.painter_at(rect);
        // Always dark, like the model pictures, so the pen colours read the same in both themes.
        let background = egui::Color32::from_gray(28);
        let ink = egui::Color32::from_gray(225);
        painter.rect_filled(rect, 0.0, background);

        // Grid: fine lines when they are far enough apart to read, bolder every 10 mm.
        for (step, shade) in [(1.0_f32, 38), (10.0, 58)] {
            let gap = step * self.zoom;
            if gap < 7.0 {
                continue;
            }
            let stroke = egui::Stroke::new(1.0, egui::Color32::from_gray(shade));
            let origin = rect.center() + self.pan;
            let mut x = origin.x + ((rect.left() - origin.x) / gap).ceil() * gap;
            while x < rect.right() {
                painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], stroke);
                x += gap;
            }
            let mut y = origin.y + ((rect.top() - origin.y) / gap).ceil() * gap;
            while y < rect.bottom() {
                painter.line_segment([egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)], stroke);
                y += gap;
            }
        }
        if let Some(backdrop) = &self.backdrop {
            let (c, h) = (backdrop.centre, backdrop.half);
            let area = egui::Rect::from_two_pos(self.to_screen(rect, [c[0] - h, c[1] + h]), self.to_screen(rect, [c[0] + h, c[1] - h]));
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(backdrop.texture.id(), area, uv, egui::Color32::WHITE);
        }

        let font = egui::FontId::proportional(13.0);
        for item in &self.items {
            self.paint_item(&painter, rect, item, &font);
        }
        // What the current drag would make.
        if let Some((a, b)) = preview {
            let colour = self.colour;
            let ghost = match self.tool {
                Tool::Pen => Some(Item::Stroke { points: self.pen.clone(), colour }),
                Tool::Line => Some(Item::Line { a, b, arrow: false, colour }),
                Tool::Arrow => Some(Item::Line { a, b, arrow: true, colour }),
                Tool::Rectangle => Some(Item::Rect { a, b, colour }),
                Tool::Circle => Some(Item::Circle { centre: a, radius: distance(a, b), colour }),
                Tool::Dimension => Some(Item::Dimension { a, b, text: String::new(), colour }),
                Tool::Label | Tool::Erase => None,
            };
            if let Some(ghost) = ghost.filter(|g| !matches!(g, Item::Stroke { points, .. } if points.len() < 2)) {
                self.paint_item(&painter, rect, &ghost, &font);
            }
        }

        // Legend, scale and axes are part of the picture the model gets.
        let mut line = rect.left_top() + egui::vec2(8.0, 6.0);
        for (c, (name, colour)) in COLOURS.iter().enumerate() {
            let meaning = self.meanings[c].trim();
            if meaning.is_empty() || !self.items.iter().any(|i| i.colour() == c) {
                continue;
            }
            painter.rect_filled(egui::Rect::from_min_size(line + egui::vec2(0.0, 3.0), egui::vec2(10.0, 10.0)), 2.0, *colour);
            painter.text(line + egui::vec2(16.0, 0.0), egui::Align2::LEFT_TOP, format!("{name}: {meaning}"), font.clone(), ink);
            line.y += 17.0;
        }
        let bar_mm = [1.0_f32, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0, 500.0].into_iter().find(|m| m * self.zoom >= 50.0).unwrap_or(1000.0);
        let start = rect.left_bottom() + egui::vec2(10.0, -12.0);
        let end = start + egui::vec2(bar_mm * self.zoom, 0.0);
        painter.line_segment([start, end], egui::Stroke::new(2.0, ink));
        for tick in [start, end] {
            painter.line_segment([tick + egui::vec2(0.0, -4.0), tick + egui::vec2(0.0, 4.0)], egui::Stroke::new(2.0, ink));
        }
        painter.text(end + egui::vec2(6.0, 0.0), egui::Align2::LEFT_CENTER, format!("{bar_mm} mm"), font.clone(), ink);
        let axes = self.backdrop.as_ref().and_then(|b| view_axes(&b.view)).map_or(["X".to_owned(), "Y".to_owned()], |a| a.map(axis_name));
        let view = self.backdrop.as_ref().map_or(String::new(), |b| format!("{} view   ", b.view));
        let caption = format!("{view}right: {}   up: {}", axes[0], axes[1]);
        painter.text(rect.right_bottom() + egui::vec2(-8.0, -6.0), egui::Align2::RIGHT_BOTTOM, caption, font.clone(), ink);

        if let Some(c) = cursor {
            painter.circle_filled(self.to_screen(rect, c), 2.5, ink);
            let readout = format!("{}, {}", mm(c[0]), mm(c[1]));
            painter.text(rect.right_top() + egui::vec2(-8.0, 6.0), egui::Align2::RIGHT_TOP, readout, font.clone(), egui::Color32::from_gray(150));
        }
        if self.items.is_empty() && self.drag_start.is_none() {
            let hint = "Draw what you mean, roughly. Add labels and dimensions, give colours a meaning,\nthen attach it to a chat message. Right drag pans, scroll zooms.";
            painter.text(rect.center_top() + egui::vec2(0.0, 30.0), egui::Align2::CENTER_TOP, hint, font, egui::Color32::from_gray(150));
        }
    }

    fn paint_item(&self, painter: &egui::Painter, rect: egui::Rect, item: &Item, font: &egui::FontId) {
        let colour = COLOURS[item.colour()].1;
        let stroke = egui::Stroke::new(2.5, colour);
        let at = |p: &Point| self.to_screen(rect, *p);
        match item {
            Item::Stroke { points, .. } => {
                painter.add(egui::Shape::line(points.iter().map(at).collect(), stroke));
            }
            Item::Line { a, b, arrow, .. } => {
                painter.line_segment([at(a), at(b)], stroke);
                if *arrow {
                    arrow_head(painter, at(a), at(b), stroke);
                }
            }
            Item::Rect { a, b, .. } => {
                painter.rect_stroke(egui::Rect::from_two_pos(at(a), at(b)), 0.0, stroke, egui::StrokeKind::Middle);
            }
            Item::Circle { centre, radius, .. } => {
                painter.circle_stroke(at(centre), *radius as f32 * self.zoom, stroke);
            }
            Item::Label { at: p, text, .. } => {
                let shown = if text.is_empty() { " " } else { text.as_str() };
                let galley = painter.layout_no_wrap(shown.to_owned(), font.clone(), colour);
                let pos = at(p) + egui::vec2(0.0, -galley.size().y / 2.0);
                // A dark plate keeps the words readable over the model picture.
                let plate = egui::Rect::from_min_size(pos, galley.size()).expand2(egui::vec2(4.0, 2.0));
                painter.rect_filled(plate, 3.0, egui::Color32::from_black_alpha(190));
                painter.galley(pos, galley, colour);
            }
            Item::Dimension { a, b, text, .. } => {
                let (sa, sb) = (at(a), at(b));
                let thin = egui::Stroke::new(1.5, colour);
                painter.line_segment([sa, sb], thin);
                arrow_head(painter, sa, sb, thin);
                arrow_head(painter, sb, sa, thin);
                let shown = if text.trim().is_empty() { format!("{} mm", mm(round1(distance(*a, *b)))) } else { text.trim().to_owned() };
                let galley = painter.layout_no_wrap(shown, font.clone(), colour);
                let pos = sa + (sb - sa) * 0.5 - galley.size() / 2.0;
                let plate = egui::Rect::from_min_size(pos, galley.size()).expand2(egui::vec2(4.0, 2.0));
                painter.rect_filled(plate, 3.0, egui::Color32::from_black_alpha(210));
                painter.galley(pos, galley, colour);
            }
        }
    }
}

fn arrow_head(painter: &egui::Painter, from: egui::Pos2, tip: egui::Pos2, stroke: egui::Stroke) {
    let back = (from - tip).normalized();
    if !back.is_finite() {
        return;
    }
    for turn in [0.45_f32, -0.45] {
        let (sin, cos) = turn.sin_cos();
        let wing = egui::vec2(back.x * cos - back.y * sin, back.x * sin + back.y * cos) * 10.0;
        painter.line_segment([tip, tip + wing], stroke);
    }
}

/// Cut the sketch out of a screenshot of the whole window. Returns RGBA pixels with
/// their width and height, halved if large so the picture stays a sensible size.
pub fn crop(image: &egui::ColorImage, area: egui::Rect, pixels_per_point: f32) -> Option<(Vec<u8>, u32, u32)> {
    let [width, height] = image.size;
    let clamp = |v: f32, max: usize| (v * pixels_per_point).round().clamp(0.0, max as f32) as usize;
    let (x0, x1) = (clamp(area.left(), width), clamp(area.right(), width));
    let (y0, y1) = (clamp(area.top(), height), clamp(area.bottom(), height));
    if x1 <= x0 + 8 || y1 <= y0 + 8 {
        return None;
    }
    let step = if x1 - x0 > 1600 || y1 - y0 > 1600 { 2 } else { 1 };
    let mut pixels = Vec::new();
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            let [r, g, b, _] = image.pixels[y * width + x].to_array();
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Some((pixels, (x1 - x0).div_ceil(step) as u32, (y1 - y0).div_ceil(step) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn annotated() -> Sketch {
        let items = vec![
            Item::Rect { a: [0.0, 0.0], b: [40.0, 20.0], colour: 3 },
            Item::Circle { centre: [10.0, 10.0], radius: 4.0, colour: 0 },
            Item::Line { a: [30.0, 30.0], b: [10.0, 14.0], arrow: true, colour: 0 },
            Item::Label { at: [31.0, 31.0], text: "M4 hole here".into(), colour: 0 },
            Item::Dimension { a: [0.0, -5.0], b: [40.0, -5.0], text: "60".into(), colour: 5 },
            Item::Dimension { a: [45.0, 0.0], b: [45.0, 20.0], text: String::new(), colour: 5 },
            Item::Stroke { points: vec![[1.04, 2.0], [3.0, 5.0], [8.0, 4.0]], colour: 2 },
        ];
        let mut sketch = Sketch { items, ..Default::default() };
        sketch.meanings[0] = "cut away".into();
        sketch.meanings[3] = " the existing plate ".into();
        // A meaning for a colour that isn't used is left out.
        sketch.meanings[4] = "unused".into();
        sketch
    }

    #[test]
    fn the_summary_carries_the_key_labels_and_dimensions() {
        let text = annotated().describe();
        assert!(text.contains("drawn on a blank grid"));
        assert!(text.contains("Colour key: red = cut away; blue = the existing plate.\n"), "{text}");
        assert!(!text.contains("unused"));
        assert!(text.contains("Labels:\n- red label \"M4 hole here\" at (31, 31)\n"));
        assert!(text.contains("- white dimension marked \"60\" between (0, -5) and (40, -5) (drawn 40 mm long)\n"));
        assert!(text.contains("- white dimension of 20 mm between (45, 0) and (45, 20)\n"));
        assert!(text.contains("- blue rectangle 40 x 20 mm, corners (0, 0) and (40, 20)\n"));
        assert!(text.contains("- red circle, diameter 8 mm, centre (10, 10)\n"));
        assert!(text.contains("- red arrow from (30, 30) pointing to (10, 14)\n"));
        assert!(text.contains("- green freehand line from (1, 2) to (8, 4), within (1, 2) to (8, 5)\n"));
        assert!(text.contains("approximate unless a dimension or label says otherwise"));
    }

    #[test]
    fn drags_and_clicks_make_items() {
        let mut sketch = Sketch { tool: Tool::Rectangle, ..Default::default() };
        sketch.finish_drag([0.0, 0.0], [0.0, 0.0]);
        assert!(sketch.items.is_empty(), "a click with a shape tool draws nothing");
        sketch.finish_drag([0.0, 0.0], [30.0, 20.0]);
        sketch.tool = Tool::Arrow;
        sketch.colour = 1;
        sketch.finish_drag([5.0, 5.0], [9.0, 9.0]);
        assert_eq!(sketch.items[1], Item::Line { a: [5.0, 5.0], b: [9.0, 9.0], arrow: true, colour: 1 });

        // A dimension opens its text box, and an empty label is dropped.
        sketch.tool = Tool::Dimension;
        sketch.finish_drag([0.0, 0.0], [30.0, 0.0]);
        assert_eq!(sketch.editing, Some(2));
        sketch.finish_editing();
        assert_eq!(sketch.items.len(), 3, "an untyped dimension stays, showing its drawn length");
        sketch.tool = Tool::Label;
        sketch.click([4.0, 4.0]);
        sketch.finish_editing();
        assert_eq!(sketch.items.len(), 3);

        // The eraser takes the nearest thing within reach, and nothing when far away.
        sketch.tool = Tool::Erase;
        sketch.click([100.0, 100.0]);
        assert_eq!(sketch.items.len(), 3);
        sketch.click([30.0, 10.0]);
        assert!(!sketch.items.iter().any(|i| matches!(i, Item::Rect { .. })), "the rectangle's edge was clicked");
    }

    #[test]
    fn views_map_onto_the_sketch_axes() {
        assert_eq!(view_axes("front"), Some([(0, 1.0), (2, 1.0)]));
        assert_eq!(view_axes("left").map(|a| a.map(axis_name)), Some(["-Y".to_owned(), "Z".to_owned()]));
        assert_eq!(view_axes("iso"), None);
    }

    #[test]
    fn the_sketch_is_cut_out_of_the_screenshot() {
        let mut image = egui::ColorImage::filled([40, 30], egui::Color32::BLACK);
        image.pixels[12 * 40 + 20] = egui::Color32::from_rgb(9, 8, 7);
        let area = egui::Rect::from_min_max(egui::pos2(10.0, 5.0), egui::pos2(15.0, 10.0));
        // At 2 pixels per point the area covers pixels 20..30 by 10..20.
        let (pixels, width, height) = crop(&image, area, 2.0).unwrap();
        assert_eq!((width, height), (10, 10));
        assert_eq!(&pixels[(2 * 10) * 4..(2 * 10) * 4 + 4], &[9, 8, 7, 255]);
        assert!(crop(&image, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), 2.0).is_none());
    }

    #[test]
    fn positions_round_trip_and_snap() {
        let sketch = Sketch::default();
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(600.0, 400.0));
        let near = sketch.to_screen(rect, [12.3, -6.8]);
        assert_eq!(sketch.to_sketch(rect, near, true), [12.0, -7.0]);
        assert_eq!(sketch.to_sketch(rect, near, false), [12.3, -6.8]);
    }
}
