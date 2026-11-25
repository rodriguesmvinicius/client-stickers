//! Text on a sticker, as outlines: laid out from the font's own metrics and
//! turned into a path, so it can be filled and given a contrasting border
//! the way WhatsApp's sticker text has one.
//!
//! No shaping engine: a sticker's caption is a word or a line, and the
//! face's advances set Latin script — accents included — correctly. A glyph
//! the face does not have is left out rather than drawn as a box; emoji go on
//! a sticker through the emoji tool, as pictures.

use tiny_skia::{Path, PathBuilder};

use super::design::TEXT_SIZE;

/// The face sticker text is set in.
fn face() -> Option<ttf_parser::Face<'static>> {
    let bytes = imp::font()?;
    ttf_parser::Face::parse(bytes, 0).ok()
}

/// The outline a glyph is traced into, already scaled and placed.
struct Tracer {
    path: PathBuilder,
    scale: f32,
    x: f32,
    y: f32,
}

impl ttf_parser::OutlineBuilder for Tracer {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path
            .move_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path
            .line_to(self.x + x * self.scale, self.y - y * self.scale);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.path.quad_to(
            self.x + x1 * self.scale,
            self.y - y1 * self.scale,
            self.x + x * self.scale,
            self.y - y * self.scale,
        );
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.path.cubic_to(
            self.x + x1 * self.scale,
            self.y - y1 * self.scale,
            self.x + x2 * self.scale,
            self.y - y2 * self.scale,
            self.x + x * self.scale,
            self.y - y * self.scale,
        );
    }
    fn close(&mut self) {
        self.path.close();
    }
}

/// One line: each glyph and where it starts, and the line's width.
type Line = (Vec<(ttf_parser::GlyphId, f32)>, f32);

/// Each line, the height of one, and the ascent, at [`TEXT_SIZE`].
fn lines(face: &ttf_parser::Face<'_>, text: &str) -> (Vec<Line>, f32, f32) {
    let scale = TEXT_SIZE / face.units_per_em().max(1) as f32;
    let line_height =
        (face.ascender() as f32 - face.descender() as f32 + face.line_gap() as f32) * scale;
    let ascent = face.ascender() as f32 * scale;
    let lines = text
        .lines()
        .map(|line| {
            let mut x = 0.0;
            let glyphs = line
                .chars()
                .filter_map(|c| {
                    let glyph = face.glyph_index(c)?;
                    let at = x;
                    x += face.glyph_hor_advance(glyph).unwrap_or(0) as f32 * scale;
                    Some((glyph, at))
                })
                .collect::<Vec<_>>();
            (glyphs, x)
        })
        .collect();
    (lines, line_height, ascent)
}

/// How wide and tall `text` is at [`TEXT_SIZE`].
pub fn measure(text: &str) -> Option<(f32, f32)> {
    let face = face()?;
    let (lines, line_height, _) = lines(&face, text);
    let width = lines.iter().map(|(_, width)| *width).fold(0.0, f32::max);
    Some((width, line_height * lines.len().max(1) as f32))
}

/// `text` as one path, centred on the origin, each line centred on the one
/// above it.
pub fn outline(text: &str) -> Option<Path> {
    let face = face()?;
    let scale = TEXT_SIZE / face.units_per_em().max(1) as f32;
    let (lines, line_height, ascent) = lines(&face, text);
    let total = line_height * lines.len().max(1) as f32;
    let mut tracer = Tracer {
        path: PathBuilder::new(),
        scale,
        x: 0.0,
        y: 0.0,
    };
    for (row, (glyphs, width)) in lines.iter().enumerate() {
        let baseline = -total / 2.0 + ascent + row as f32 * line_height;
        for (glyph, x) in glyphs {
            tracer.x = x - width / 2.0;
            tracer.y = baseline;
            face.outline_glyph(*glyph, &mut tracer);
        }
    }
    tracer.path.finish()
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    /// The UI's semibold face. Embedded here because the desktop's UI fonts
    /// reach the text system another way, and the sticker maker draws its
    /// own pixels.
    pub(super) fn font() -> Option<&'static [u8]> {
        Some(include_bytes!(
            "../../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf"
        ))
    }
}

#[cfg(target_family = "wasm")]
mod imp {
    /// None: the page has no sticker maker, and no font bytes are embedded in
    /// the production module.
    pub(super) fn font() -> Option<&'static [u8]> {
        None
    }
}

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn text_has_a_size_and_a_shape() {
        let (width, height) = measure("Olá").expect("the face is there");
        assert!(width > 60.0 && width < 200.0, "{width}");
        assert!(height > 50.0, "{height}");
        let (_, two) = measure("Olá\nmundo").unwrap();
        assert!((two - 2.0 * height).abs() < 0.5);
        let path = outline("Olá").expect("traced");
        let bounds = path.bounds();
        // Centred on the origin.
        assert!((bounds.left() + bounds.right()).abs() < 4.0);
    }

    #[test]
    fn nothing_to_draw_is_no_path() {
        assert!(outline("").is_none());
    }
}
