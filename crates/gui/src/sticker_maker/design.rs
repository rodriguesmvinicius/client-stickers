//! What a sticker being made is, apart from its picture: where the picture
//! sits, what has been cut out of it, and everything drawn on top.
//!
//! All of it is in sticker pixels — a 512×512 square, WhatsApp's sticker
//! size — so the same design renders the preview and the file sent, and a
//! window resized in between changes nothing.

use std::sync::Arc;

/// A sticker's side, in pixels.
pub const SIDE: u32 = 512;
/// [`SIDE`] as the coordinates use it.
pub const SIDE_F: f32 = SIDE as f32;
/// How tall text is drawn at scale 1.
pub const TEXT_SIZE: f32 = 64.0;
/// How wide an emoji is drawn at scale 1.
pub const EMOJI_SIZE: f32 = 160.0;

/// Where the picture sits: its centre on the sticker, and how many sticker
/// pixels one picture pixel covers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub center: (f32, f32),
    pub scale: f32,
}

impl Placement {
    /// The whole picture on the sticker, as large as it fits: what a photo
    /// starts as, so nothing in it is lost before the person decides.
    pub fn contain(width: u32, height: u32) -> Self {
        Self {
            center: (SIDE_F / 2.0, SIDE_F / 2.0),
            scale: SIDE_F / width.max(height).max(1) as f32,
        }
    }

    /// The sticker filled, cropping whichever side is longer.
    pub fn cover(width: u32, height: u32) -> Self {
        Self {
            center: (SIDE_F / 2.0, SIDE_F / 2.0),
            scale: SIDE_F / width.min(height).max(1) as f32,
        }
    }
}

/// Where a piece of text or an emoji sits: centre, size and turn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct At {
    pub center: (f32, f32),
    pub scale: f32,
    /// Radians, clockwise.
    pub rotation: f32,
}

impl At {
    /// The middle of the sticker, at its natural size.
    pub fn middle() -> Self {
        Self {
            center: (SIDE_F / 2.0, SIDE_F / 2.0),
            scale: 1.0,
            rotation: 0.0,
        }
    }

    /// `point` in this item's own frame: unrotated, unscaled, centred.
    pub fn local(&self, point: (f32, f32)) -> (f32, f32) {
        let (dx, dy) = (point.0 - self.center.0, point.1 - self.center.1);
        let (sin, cos) = (-self.rotation).sin_cos();
        let scale = self.scale.max(0.01);
        ((dx * cos - dy * sin) / scale, (dx * sin + dy * cos) / scale)
    }
}

/// A freehand line.
#[derive(Clone, Debug, PartialEq)]
pub struct Stroke {
    pub points: Vec<(f32, f32)>,
    pub color: [u8; 4],
    pub width: f32,
}

/// Something drawn over the picture, in the order it was added.
#[derive(Clone, Debug)]
pub enum Item {
    Stroke(Stroke),
    Text {
        text: String,
        color: [u8; 4],
        at: At,
    },
    Emoji {
        /// The emoji's artwork, as the bundled font draws it.
        image: Arc<image::RgbaImage>,
        at: At,
    },
}

impl Item {
    /// Where it sits, for the items that can be picked up and moved.
    pub fn at(&self) -> Option<At> {
        match self {
            Self::Stroke(_) => None,
            Self::Text { at, .. } | Self::Emoji { at, .. } => Some(*at),
        }
    }

    pub fn at_mut(&mut self) -> Option<&mut At> {
        match self {
            Self::Stroke(_) => None,
            Self::Text { at, .. } | Self::Emoji { at, .. } => Some(at),
        }
    }

    /// Half its width and height in its own frame, for picking it up.
    pub fn half_extent(&self) -> Option<(f32, f32)> {
        match self {
            Self::Stroke(_) => None,
            Self::Text { text, .. } => super::text::measure(text)
                .map(|(width, height)| (width / 2.0 + 8.0, height / 2.0 + 8.0)),
            Self::Emoji { image, .. } => {
                let height = EMOJI_SIZE * image.height() as f32 / image.width().max(1) as f32;
                Some((EMOJI_SIZE / 2.0, height / 2.0))
            }
        }
    }

    /// Whether `point`, in sticker pixels, lands on this item.
    pub fn contains(&self, point: (f32, f32)) -> bool {
        let (Some(at), Some((half_width, half_height))) = (self.at(), self.half_extent()) else {
            return false;
        };
        let (x, y) = at.local(point);
        x.abs() <= half_width && y.abs() <= half_height
    }
}

/// Everything about the sticker except its picture.
#[derive(Clone, Debug)]
pub struct Design {
    pub placement: Placement,
    pub items: Vec<Item>,
    /// The subject's mask, white kept, stretched over the whole picture.
    pub cutout: Option<Arc<image::GrayImage>>,
    /// A white border around the cut-out subject, as WhatsApp draws one.
    pub outline: bool,
}

impl Design {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            placement: Placement::contain(width, height),
            items: Vec::new(),
            cutout: None,
            outline: true,
        }
    }

    /// The topmost item that can be picked up at `point`.
    pub fn item_at(&self, point: (f32, f32)) -> Option<usize> {
        self.items.iter().rposition(|item| item.contains(point))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_starts_whole_and_can_fill() {
        let contain = Placement::contain(1024, 512);
        assert_eq!(contain.scale, 0.5);
        let cover = Placement::cover(1024, 512);
        assert_eq!(cover.scale, 1.0);
    }

    #[test]
    fn an_item_is_picked_up_where_it_is_drawn() {
        let image = Arc::new(image::RgbaImage::new(10, 10));
        let mut at = At::middle();
        at.center = (100.0, 100.0);
        at.scale = 0.5;
        let emoji = Item::Emoji { image, at };
        // 160 wide at scale 0.5 is 80: 40 either side of the centre.
        assert!(emoji.contains((135.0, 100.0)));
        assert!(!emoji.contains((145.0, 100.0)));
        let design = Design {
            items: vec![emoji],
            ..Design::new(10, 10)
        };
        assert_eq!(design.item_at((100.0, 100.0)), Some(0));
        assert_eq!(design.item_at((300.0, 300.0)), None);
    }

    #[test]
    fn a_turned_item_is_picked_up_along_its_turn() {
        let mut at = At::middle();
        at.rotation = std::f32::consts::FRAC_PI_2;
        // A quarter turn clockwise puts what was to the right below.
        let (x, y) = at.local((256.0, 300.0));
        assert!((x - 44.0).abs() < 1e-3 && y.abs() < 1e-3);
    }
}
