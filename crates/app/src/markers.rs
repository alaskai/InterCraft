// Numbered markers painted onto a view_model picture, so a model can tell parts apart
// by name instead of guessing from colours. Drawn on the pixels with a tiny built-in
// digit font, since the snapshot has no text renderer.

use serde_json::{json, Value};

/// More markers than this make a picture unreadable; the rest are listed as unmarked.
pub const MAX_MARKERS: usize = 60;

/// Digits 0 to 9, 3 pixels wide and 5 high, one row per entry, high bit on the left.
const DIGITS: [[u8; 5]; 10] = [
    [0b111, 0b101, 0b101, 0b101, 0b111],
    [0b010, 0b110, 0b010, 0b010, 0b111],
    [0b111, 0b001, 0b111, 0b100, 0b111],
    [0b111, 0b001, 0b111, 0b001, 0b111],
    [0b101, 0b101, 0b111, 0b001, 0b001],
    [0b111, 0b100, 0b111, 0b001, 0b111],
    [0b111, 0b100, 0b111, 0b101, 0b111],
    [0b111, 0b001, 0b001, 0b001, 0b001],
    [0b111, 0b101, 0b111, 0b101, 0b111],
    [0b111, 0b101, 0b111, 0b001, 0b111],
];
const SCALE: i32 = 4;
const PAD: i32 = 4;
const FILL: [u8; 3] = [255, 214, 64];
const INK: [u8; 3] = [0, 0, 0];

/// Paint a marker for each part that has a place in the picture, numbered in the
/// order given. Returns the key: {"markers": {"1": name, ...}, "unmarked": [names]}.
pub fn draw(pixels: &mut [u8], size: u32, marks: &[(String, Option<[f32; 2]>)]) -> Value {
    let mut canvas = Canvas { pixels, size: size as i32 };
    let mut key = serde_json::Map::new();
    let mut unmarked = Vec::new();
    let mut taken: Vec<[i32; 4]> = Vec::new();
    for (name, at) in marks {
        let Some([x, y]) = at.filter(|_| key.len() < MAX_MARKERS) else {
            unmarked.push(name.clone());
            continue;
        };
        let number = (key.len() + 1).to_string();
        let (x, y) = (x as i32, y as i32);
        let w = number.len() as i32 * (3 * SCALE + SCALE) - SCALE + PAD * 2;
        let h = 5 * SCALE + PAD * 2;
        // Up and to the right of the point; moved down until it clears the others.
        let mut bx = (x + 6).clamp(0, canvas.size - w);
        let mut by = (y - h - 4).clamp(0, canvas.size - h);
        for _ in 0..20 {
            if !taken.iter().any(|r| bx < r[2] && r[0] < bx + w && by < r[3] && r[1] < by + h) {
                break;
            }
            by += h + 2;
            if by + h > canvas.size {
                by = 0;
                bx = (bx + w + 4).min(canvas.size - w);
            }
        }
        taken.push([bx, by, bx + w, by + h]);
        // A dot on the part, a line to the box, then the box with the number.
        canvas.line(x, y, bx + w / 2, by + h / 2, INK);
        canvas.rect(x - 4, y - 4, 9, 9, INK);
        canvas.rect(x - 2, y - 2, 5, 5, FILL);
        canvas.rect(bx, by, w, h, INK);
        canvas.rect(bx + 2, by + 2, w - 4, h - 4, FILL);
        for (i, digit) in number.bytes().enumerate() {
            let glyph = DIGITS[(digit - b'0') as usize];
            let left = bx + PAD + i as i32 * (3 * SCALE + SCALE);
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..3 {
                    if bits & (0b100 >> col) != 0 {
                        canvas.rect(left + col * SCALE, by + PAD + row as i32 * SCALE, SCALE, SCALE, INK);
                    }
                }
            }
        }
        key.insert(number, Value::String(name.clone()));
    }
    json!({ "markers": key, "unmarked": unmarked })
}

struct Canvas<'a> {
    pixels: &'a mut [u8],
    size: i32,
}

impl Canvas<'_> {
    fn put(&mut self, x: i32, y: i32, rgb: [u8; 3]) {
        if (0..self.size).contains(&x) && (0..self.size).contains(&y) {
            let at = ((y * self.size + x) * 4) as usize;
            self.pixels[at..at + 3].copy_from_slice(&rgb);
            self.pixels[at + 3] = 255;
        }
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, rgb: [u8; 3]) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.put(xx, yy, rgb);
            }
        }
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, rgb: [u8; 3]) {
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for i in 0..=steps {
            let x = x0 + (x1 - x0) * i / steps;
            let y = y0 + (y1 - y0) * i / steps;
            self.rect(x, y, 2, 2, rgb);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_numbered_and_keep_clear_of_each_other() {
        let size = 200;
        let mut pixels = vec![30u8; (size * size * 4) as usize];
        let marks = vec![
            ("Base".to_owned(), Some([100.0, 100.0])),
            ("Behind".to_owned(), None),
            ("Pin".to_owned(), Some([101.0, 101.0])),
        ];
        let key = draw(&mut pixels, size, &marks);
        assert_eq!(key, json!({ "markers": { "1": "Base", "2": "Pin" }, "unmarked": ["Behind"] }));
        let at = |x: u32, y: u32| &pixels[((y * size + x) * 4) as usize..][..3];
        assert_eq!(at(100, 100), FILL, "a dot on the part");
        // Two boxes drawn, not on top of each other: count rows that have any fill colour.
        let filled_rows = (0..size).filter(|&y| (0..size).any(|x| at(x, y) == FILL)).count();
        assert!(filled_rows > 2 * (5 * SCALE + PAD * 2 - 4) as usize, "{filled_rows}");

        let many: Vec<(String, Option<[f32; 2]>)> = (0..70).map(|i| (format!("P{i}"), Some([10.0, 10.0]))).collect();
        let key = draw(&mut vec![0; (size * size * 4) as usize], size, &many);
        assert_eq!(key["markers"].as_object().unwrap().len(), MAX_MARKERS);
        assert_eq!(key["unmarked"].as_array().unwrap().len(), 10);
    }
}
