//! Drawing a sticker: the picture where it was placed, cut out and
//! outlined when it was, and everything drawn on top, in order.
//!
//! On the CPU, with tiny-skia, because the result is a file: the same code
//! draws the preview and every frame that is sent, so what the person saw is
//! what arrives.

use tiny_skia::{
    FillRule, FilterQuality, LineCap, LineJoin, Paint, PathBuilder, Pixmap, PixmapPaint,
    Stroke as Line, Transform,
};

use super::design::{Design, EMOJI_SIZE, Item, SIDE};

/// How far the white border reaches past a cut-out subject, in sticker
/// pixels.
const OUTLINE: f32 = 10.0;

/// A picture as tiny-skia draws it: premultiplied, and with the cutout's
/// mask, when there is one, already in its alpha.
///
/// Built once per picture rather than per draw: the preview redraws on
/// every drag, and the picture does not change while it is dragged.
pub fn subject(picture: &image::RgbaImage, mask: Option<&image::GrayImage>) -> Option<Pixmap> {
    let (width, height) = picture.dimensions();
    let mut pixmap = Pixmap::new(width, height)?;
    let mask = mask.map(|mask| {
        // Stretched to the picture, the same way the model was fed it.
        image::imageops::resize(mask, width, height, image::imageops::FilterType::Triangle)
    });
    for ((index, pixel), out) in picture
        .pixels()
        .enumerate()
        .zip(pixmap.pixels_mut().iter_mut())
    {
        let [r, g, b, a] = pixel.0;
        let a = match &mask {
            Some(mask) => {
                let kept = mask.as_raw()[index] as u32;
                (a as u32 * kept / 255) as u8
            }
            None => a,
        };
        let premultiply = |c: u8| (c as u32 * a as u32 / 255) as u8;
        *out = tiny_skia::PremultipliedColorU8::from_rgba(
            premultiply(r),
            premultiply(g),
            premultiply(b),
            a,
        )
        .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
    }
    Some(pixmap)
}

/// The sticker: `subject` placed as `design` says, with its items on top.
pub fn compose(design: &Design, subject: &Pixmap) -> Option<Pixmap> {
    let mut canvas = Pixmap::new(SIDE, SIDE)?;
    let placement = design.placement;
    let transform = Transform::from_translate(placement.center.0, placement.center.1)
        .pre_scale(placement.scale, placement.scale)
        .pre_translate(
            -(subject.width() as f32) / 2.0,
            -(subject.height() as f32) / 2.0,
        );
    let quality = PixmapPaint {
        quality: FilterQuality::Bicubic,
        ..PixmapPaint::default()
    };
    if design.cutout.is_some() && design.outline {
        // The subject alone first, so its silhouette can be traced.
        let mut alone = Pixmap::new(SIDE, SIDE)?;
        alone.draw_pixmap(0, 0, subject.as_ref(), &quality, transform, None);
        let border = outline(&alone, OUTLINE);
        canvas.draw_pixmap(
            0,
            0,
            border.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        canvas.draw_pixmap(
            0,
            0,
            alone.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    } else {
        canvas.draw_pixmap(0, 0, subject.as_ref(), &quality, transform, None);
    }
    for item in &design.items {
        draw_item(&mut canvas, item);
    }
    Some(canvas)
}

fn draw_item(canvas: &mut Pixmap, item: &Item) {
    match item {
        Item::Stroke(stroke) => {
            let Some(&first) = stroke.points.first() else {
                return;
            };
            let mut path = PathBuilder::new();
            path.move_to(first.0, first.1);
            if stroke.points.len() == 1 {
                // A click is a dot: a line too short to see, with round caps.
                path.line_to(first.0 + 0.01, first.1);
            }
            for &(x, y) in &stroke.points[1..] {
                path.line_to(x, y);
            }
            let Some(path) = path.finish() else {
                return;
            };
            let line = Line {
                width: stroke.width,
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Line::default()
            };
            canvas.stroke_path(
                &path,
                &paint(stroke.color),
                &line,
                Transform::identity(),
                None,
            );
        }
        Item::Text { text, color, at } => {
            let Some(path) = super::text::outline(text) else {
                return;
            };
            let transform = Transform::from_translate(at.center.0, at.center.1)
                .pre_rotate(at.rotation.to_degrees())
                .pre_scale(at.scale, at.scale);
            // A border in whichever of black and white the colour stands out
            // against, so the text reads over any picture.
            let [r, g, b, _] = *color;
            let light = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32 > 150.0;
            let border = if light {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            };
            let line = Line {
                width: super::design::TEXT_SIZE * 0.16,
                line_join: LineJoin::Round,
                ..Line::default()
            };
            canvas.stroke_path(&path, &paint(border), &line, transform, None);
            canvas.fill_path(&path, &paint(*color), FillRule::Winding, transform, None);
        }
        Item::Emoji { image, at } => {
            let Some(artwork) = subject(image, None) else {
                return;
            };
            let size = EMOJI_SIZE / artwork.width().max(1) as f32;
            let transform = Transform::from_translate(at.center.0, at.center.1)
                .pre_rotate(at.rotation.to_degrees())
                .pre_scale(at.scale * size, at.scale * size)
                .pre_translate(
                    -(artwork.width() as f32) / 2.0,
                    -(artwork.height() as f32) / 2.0,
                );
            let quality = PixmapPaint {
                quality: FilterQuality::Bicubic,
                ..PixmapPaint::default()
            };
            canvas.draw_pixmap(0, 0, artwork.as_ref(), &quality, transform, None);
        }
    }
}

fn paint(color: [u8; 4]) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    paint.anti_alias = true;
    paint
}

/// A white band `reach` pixels wide around whatever is opaque in `layer`.
///
/// A chamfer distance transform — two passes over the alpha — and a band
/// that fades over its last pixel, so the edge is smooth without a blur.
fn outline(layer: &Pixmap, reach: f32) -> Pixmap {
    let (width, height) = (layer.width() as usize, layer.height() as usize);
    const FAR: f32 = 1.0e6;
    let mut distance: Vec<f32> = layer
        .pixels()
        .iter()
        .map(|pixel| if pixel.alpha() >= 128 { 0.0 } else { FAR })
        .collect();
    let (straight, diagonal) = (1.0f32, std::f32::consts::SQRT_2);
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let mut best = distance[i];
            if x > 0 {
                best = best.min(distance[i - 1] + straight);
            }
            if y > 0 {
                best = best.min(distance[i - width] + straight);
                if x > 0 {
                    best = best.min(distance[i - width - 1] + diagonal);
                }
                if x + 1 < width {
                    best = best.min(distance[i - width + 1] + diagonal);
                }
            }
            distance[i] = best;
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let i = y * width + x;
            let mut best = distance[i];
            if x + 1 < width {
                best = best.min(distance[i + 1] + straight);
            }
            if y + 1 < height {
                best = best.min(distance[i + width] + straight);
                if x + 1 < width {
                    best = best.min(distance[i + width + 1] + diagonal);
                }
                if x > 0 {
                    best = best.min(distance[i + width - 1] + diagonal);
                }
            }
            distance[i] = best;
        }
    }
    let mut border = Pixmap::new(width as u32, height as u32).expect("same size as the layer");
    for (out, d) in border.pixels_mut().iter_mut().zip(distance) {
        let alpha = ((reach + 1.0 - d).clamp(0.0, 1.0) * 255.0) as u8;
        *out = tiny_skia::PremultipliedColorU8::from_rgba(alpha, alpha, alpha, alpha)
            .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
    }
    border
}

/// The drawn sticker as straight-alpha RGBA, which is what an encoder takes.
pub fn rgba(pixmap: &Pixmap) -> Vec<u8> {
    pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::sticker_maker::design::{At, Placement, Stroke};

    fn red(width: u32, height: u32) -> image::RgbaImage {
        image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]))
    }

    fn at(canvas: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let pixel = canvas.pixel(x, y).unwrap().demultiply();
        [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]
    }

    #[test]
    fn a_picture_fills_where_it_is_placed_and_nothing_else() {
        let picture = red(100, 50);
        let design = Design::new(100, 50);
        let canvas = compose(&design, &subject(&picture, None).unwrap()).unwrap();
        // Contained: 512 wide, 256 tall, centred.
        assert_eq!(at(&canvas, 256, 256), [255, 0, 0, 255]);
        assert_eq!(at(&canvas, 256, 64)[3], 0);
    }

    #[test]
    fn a_cutout_keeps_the_subject_and_rings_it_in_white() {
        let picture = red(64, 64);
        // Keep the middle half.
        let mask = image::GrayImage::from_fn(64, 64, |x, y| {
            image::Luma([if (16..48).contains(&x) && (16..48).contains(&y) {
                255
            } else {
                0
            }])
        });
        let design = Design {
            placement: Placement::contain(64, 64),
            cutout: Some(Arc::new(mask.clone())),
            outline: true,
            items: Vec::new(),
        };
        let canvas = compose(&design, &subject(&picture, Some(&mask)).unwrap()).unwrap();
        assert_eq!(at(&canvas, 256, 256), [255, 0, 0, 255], "the subject");
        // The kept square spans 128..384; just outside it is the border.
        assert_eq!(at(&canvas, 256, 122), [255, 255, 255, 255], "the outline");
        assert_eq!(at(&canvas, 256, 40)[3], 0, "the background is gone");
    }

    #[test]
    fn strokes_text_and_emoji_are_drawn_over_the_picture() {
        let picture = red(10, 10);
        let mut design = Design::new(10, 10);
        design.items.push(Item::Stroke(Stroke {
            points: vec![(10.0, 10.0), (100.0, 10.0)],
            color: [0, 0, 255, 255],
            width: 8.0,
        }));
        design.items.push(Item::Emoji {
            image: Arc::new(image::RgbaImage::from_pixel(
                8,
                8,
                image::Rgba([0, 255, 0, 255]),
            )),
            at: At::middle(),
        });
        let canvas = compose(&design, &subject(&picture, None).unwrap()).unwrap();
        assert_eq!(at(&canvas, 50, 10), [0, 0, 255, 255]);
        assert_eq!(at(&canvas, 256, 256), [0, 255, 0, 255]);
        assert_eq!(rgba(&canvas).len(), (SIDE * SIDE * 4) as usize);
    }

    #[cfg(not(target_family = "wasm"))]
    #[test]
    fn text_is_filled_with_its_colour() {
        let picture = image::RgbaImage::from_pixel(10, 10, image::Rgba([0, 0, 0, 0]));
        let mut design = Design::new(10, 10);
        design.items.push(Item::Text {
            text: "I".into(),
            color: [255, 255, 0, 255],
            at: At {
                scale: 3.0,
                ..At::middle()
            },
        });
        let canvas = compose(&design, &subject(&picture, None).unwrap()).unwrap();
        assert_eq!(at(&canvas, 256, 256), [255, 255, 0, 255]);
    }
}
