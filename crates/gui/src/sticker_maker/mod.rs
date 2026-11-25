//! The sticker maker: a picture, a GIF or a video made into a WhatsApp
//! sticker, the way the phone's sticker maker makes one.
//!
//! The source is placed on a 512×512 square, its subject can be cut out of
//! its background (by a model the daemon runs) and outlined, and text,
//! emoji and freehand lines go on top. A still source becomes a still
//! sticker; a GIF or a video becomes an animated one, from a stretch of it
//! the person chooses. Either can carry a sound — a video's own, or an
//! audio file — which is the new sound sticker.
//!
//! [`design`] is what the person decided, [`render`] draws it, and the
//! platform half (`export_native.rs`) reads sources and encodes the result;
//! a page has no sticker maker, and its half says so.

pub mod design;
pub mod render;
pub mod text;

#[cfg_attr(target_family = "wasm", path = "export_web.rs")]
#[cfg_attr(not(target_family = "wasm"), path = "export_native.rs")]
mod imp;

use std::sync::Arc;

pub use imp::{VideoReader, decode_audio, load};

use design::{Design, SIDE};

/// WhatsApp's limit on a still sticker.
const STILL_LIMIT: usize = 100 * 1024;
/// WhatsApp's limit on an animated sticker.
const ANIMATED_LIMIT: usize = 500 * 1024;
/// The longest animated sticker made here.
pub const MAX_CLIP_MS: u32 = 8_000;
/// The longest sound a sticker carries.
pub const MAX_SOUND_MS: u32 = 10_000;

/// A sound, as mono samples.
#[derive(Clone)]
pub struct Sound {
    pub samples: Arc<[f32]>,
    pub rate: u32,
}

impl Sound {
    /// How long it is.
    pub fn duration_ms(&self) -> u32 {
        (self.samples.len() as u64 * 1000 / self.rate.max(1) as u64) as u32
    }
}

/// How a source moves.
#[derive(Clone)]
pub enum Motion {
    /// A photo: one frame.
    Still,
    /// A GIF or an animated WebP: each frame and when it starts.
    Frames(Arc<Vec<(Arc<image::RgbaImage>, u32)>>),
    /// A video, decoded as it is read.
    Video(Arc<Vec<u8>>),
}

/// What a sticker is made from.
#[derive(Clone)]
pub struct Source {
    /// The first frame: what the editor shows, and the whole of a photo.
    pub poster: Arc<image::RgbaImage>,
    pub motion: Motion,
    /// How long it runs; zero for a photo.
    pub duration_ms: u32,
    /// A video's own sound.
    pub audio: Option<Sound>,
}

impl Source {
    pub fn is_animated(&self) -> bool {
        !matches!(self.motion, Motion::Still)
    }

    /// The frame showing `ms` into a GIF, looping.
    pub(crate) fn frame_at(
        frames: &[(Arc<image::RgbaImage>, u32)],
        duration: u32,
        ms: u32,
    ) -> Option<&Arc<image::RgbaImage>> {
        let ms = if duration > 0 { ms % duration } else { 0 };
        frames
            .iter()
            .rev()
            .find(|(_, start)| *start <= ms)
            .or(frames.first())
            .map(|(frame, _)| frame)
    }
}

/// The stretch of an animated source that becomes the sticker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clip {
    pub start_ms: u32,
    pub length_ms: u32,
}

/// A sticker ready to send.
pub struct Made {
    pub webp: Vec<u8>,
    pub is_animated: bool,
    /// An Opus clip in OGG, and its length in seconds.
    pub sound: Option<(Vec<u8>, u32)>,
}

/// Draw and encode the sticker. Slow — a video is decoded and every frame
/// drawn, perhaps more than once to fit the size limit — so this runs on the
/// background executor.
pub fn make(
    design: &Design,
    source: &Source,
    clip: Clip,
    sound: Option<(&Sound, u32)>,
) -> Result<Made, String> {
    let webp = if source.is_animated() {
        animate(design, source, clip)?
    } else {
        let subject = render::subject(&source.poster, design.cutout.as_deref())
            .ok_or("the picture is empty")?;
        let canvas = render::compose(design, &subject).ok_or("the sticker could not be drawn")?;
        still(&render::rgba(&canvas))?
    };
    let sound = match sound {
        Some((sound, start_ms)) => {
            let longest = if source.is_animated() {
                clip.length_ms
            } else {
                MAX_SOUND_MS
            };
            Some(opus_clip(sound, start_ms, longest.min(MAX_SOUND_MS))?)
        }
        None => None,
    };
    Ok(Made {
        webp,
        is_animated: source.is_animated(),
        sound,
    })
}

/// A still sticker, as good as fits in WhatsApp's limit.
fn still(rgba: &[u8]) -> Result<Vec<u8>, String> {
    for quality in [90.0, 80.0, 70.0, 55.0, 40.0, 25.0] {
        let webp = imp::encode_still(rgba, SIDE, quality)?;
        if webp.len() <= STILL_LIMIT {
            return Ok(webp);
        }
    }
    Err("the sticker is too detailed to fit in 100 KB".to_string())
}

/// An animated sticker: tried at a high frame rate and quality first, then
/// lower until it fits WhatsApp's limit.
fn animate(design: &Design, source: &Source, clip: Clip) -> Result<Vec<u8>, String> {
    let length = clip.length_ms.clamp(100, MAX_CLIP_MS);
    for (fps, quality) in [(15, 65.0), (12, 50.0), (10, 40.0), (8, 30.0), (6, 22.0)] {
        let step = 1000 / fps;
        let count = (length / step).max(1);
        let mut video = match &source.motion {
            Motion::Video(bytes) => Some(VideoReader::open(bytes)?),
            _ => None,
        };
        let mut frame = |index: u32| -> Result<Vec<u8>, String> {
            let ms = clip.start_ms + index * step;
            let picture = match (&source.motion, video.as_mut()) {
                (Motion::Frames(frames), _) => {
                    Source::frame_at(frames, source.duration_ms, ms).cloned()
                }
                (Motion::Video(_), Some(reader)) => reader.frame(ms).map(Arc::new),
                _ => None,
            }
            .unwrap_or_else(|| source.poster.clone());
            let subject =
                render::subject(&picture, design.cutout.as_deref()).ok_or("a frame is empty")?;
            let canvas = render::compose(design, &subject).ok_or("a frame could not be drawn")?;
            Ok(render::rgba(&canvas))
        };
        let webp = imp::encode_animation(SIDE, count, step, quality, &mut frame)?;
        if webp.len() <= ANIMATED_LIMIT {
            return Ok(webp);
        }
        log::info!(
            "animated sticker at {fps} fps, quality {quality} is {} KB; trying smaller",
            webp.len() / 1024
        );
    }
    Err("the animation is too long or detailed to fit in 500 KB; try a shorter stretch".to_string())
}

/// `sound` from `start_ms`, at most `longest_ms` of it, as Opus in OGG.
fn opus_clip(sound: &Sound, start_ms: u32, longest_ms: u32) -> Result<(Vec<u8>, u32), String> {
    let rate = sound.rate.max(1) as u64;
    let from = (start_ms as u64 * rate / 1000) as usize;
    let to = from + (longest_ms as u64 * rate / 1000) as usize;
    let samples = sound
        .samples
        .get(from.min(sound.samples.len())..to.min(sound.samples.len()))
        .unwrap_or_default();
    if samples.is_empty() {
        return Err("the sound is empty there".to_string());
    }
    let seconds = (samples.len() as u64).div_ceil(rate).max(1) as u32;
    let ogg = imp::encode_opus(samples, sound.rate, seconds)?;
    Ok((ogg, seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames() -> Vec<(Arc<image::RgbaImage>, u32)> {
        (0..3)
            .map(|i| (Arc::new(image::RgbaImage::new(1, 1 + i)), i * 100))
            .collect()
    }

    #[test]
    fn a_gif_frame_is_the_one_showing_then_and_it_loops() {
        let frames = frames();
        assert_eq!(Source::frame_at(&frames, 300, 150).unwrap().height(), 2);
        assert_eq!(Source::frame_at(&frames, 300, 250).unwrap().height(), 3);
        assert_eq!(Source::frame_at(&frames, 300, 310).unwrap().height(), 1);
    }

    #[test]
    fn a_sounds_length_comes_from_its_samples() {
        let sound = Sound {
            samples: vec![0.0; 24_000].into(),
            rate: 16_000,
        };
        assert_eq!(sound.duration_ms(), 1_500);
    }
}
