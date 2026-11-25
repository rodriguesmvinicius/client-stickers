//! The desktop half of the sticker maker: reading a picture, a GIF or a
//! video into a [`Source`], and encoding the result with libwebp and libopus.

use std::sync::Arc;
use std::time::Duration;

use super::{Motion, Sound, Source};

/// The longest side a photo is kept at: enough to zoom into, without holding
/// a 48-megapixel phone photo's pixels for a 512-pixel sticker.
const PHOTO_SIDE: u32 = 1600;
/// The longest side a GIF's frames are kept at.
const FRAME_SIDE: u32 = 640;
/// The most GIF frames kept.
const MAX_FRAMES: usize = 400;

/// Read what was picked into something a sticker can be made from.
pub fn load(bytes: Vec<u8>, mime: &str) -> Result<Source, String> {
    if mime.starts_with("video/") || bytes.get(4..8) == Some(b"ftyp") {
        return load_video(bytes);
    }
    match image::guess_format(&bytes) {
        Ok(image::ImageFormat::Gif) => {
            let decoder = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(&bytes))
                .map_err(|e| e.to_string())?;
            frames(image::AnimationDecoder::into_frames(decoder))
        }
        Ok(image::ImageFormat::WebP) => {
            let decoder = image::codecs::webp::WebPDecoder::new(std::io::Cursor::new(&bytes))
                .map_err(|e| e.to_string())?;
            if decoder.has_animation() {
                frames(image::AnimationDecoder::into_frames(decoder))
            } else {
                still(&bytes)
            }
        }
        _ => still(&bytes),
    }
}

fn still(bytes: &[u8]) -> Result<Source, String> {
    let picture = image::load_from_memory(bytes)
        .map_err(|e| format!("the picture could not be read: {e}"))?;
    let picture = if picture.width().max(picture.height()) > PHOTO_SIDE {
        picture.resize(
            PHOTO_SIDE,
            PHOTO_SIDE,
            image::imageops::FilterType::Triangle,
        )
    } else {
        picture
    };
    Ok(Source {
        poster: Arc::new(picture.to_rgba8()),
        motion: Motion::Still,
        duration_ms: 0,
        audio: None,
    })
}

fn frames(frames: image::Frames<'_>) -> Result<Source, String> {
    let mut kept = Vec::new();
    let mut at = 0u32;
    for frame in frames.take(MAX_FRAMES) {
        let frame = frame.map_err(|e| format!("the animation could not be read: {e}"))?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        // Browsers draw a zero delay as 100 ms; so does this.
        let delay = (numerator / denominator.max(1)).max(20);
        let delay = if numerator == 0 { 100 } else { delay };
        let mut picture = image::DynamicImage::ImageRgba8(frame.into_buffer());
        if picture.width().max(picture.height()) > FRAME_SIDE {
            picture = picture.resize(
                FRAME_SIDE,
                FRAME_SIDE,
                image::imageops::FilterType::Triangle,
            );
        }
        kept.push((Arc::new(picture.to_rgba8()), at));
        at += delay;
    }
    let poster = kept
        .first()
        .map(|(frame, _)| frame.clone())
        .ok_or("the animation has no frames")?;
    if kept.len() == 1 {
        return Ok(Source {
            poster,
            motion: Motion::Still,
            duration_ms: 0,
            audio: None,
        });
    }
    Ok(Source {
        poster,
        motion: Motion::Frames(Arc::new(kept)),
        duration_ms: at,
        audio: None,
    })
}

fn load_video(bytes: Vec<u8>) -> Result<Source, String> {
    let bytes = Arc::new(bytes);
    let mut reader = VideoReader::open(&bytes)?;
    let duration_ms = reader.decoder.duration().as_millis().min(u32::MAX as u128) as u32;
    let poster = reader
        .frame(0)
        .ok_or("the video's first frame could not be decoded")?;
    let audio = reader.decoder.take_audio().map(|audio| Sound {
        samples: audio.samples,
        rate: audio.sample_rate,
    });
    Ok(Source {
        poster: Arc::new(poster),
        motion: Motion::Video(bytes),
        duration_ms,
        audio,
    })
}

/// A video read frame by frame, forwards.
pub struct VideoReader {
    decoder: crate::video::StreamingVideoDecoder,
}

impl VideoReader {
    pub fn open(bytes: &[u8]) -> Result<Self, String> {
        let decoder = crate::video::StreamingVideoDecoder::new(bytes)
            .map_err(|e| format!("the video could not be read: {e}"))?;
        Ok(Self { decoder })
    }

    /// The picture showing `ms` into the video, as RGBA.
    pub fn frame(&mut self, ms: u32) -> Option<image::RgbaImage> {
        self.decoder.seek(Duration::from_millis(ms as u64));
        let frame = self.decoder.current_frame()?;
        let size = frame.image.size(0);
        let bgra = frame.image.as_bytes(0)?;
        let mut rgba = bgra.to_vec();
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        image::RgbaImage::from_raw(size.width.0 as u32, size.height.0 as u32, rgba)
    }
}

/// A sound file's samples.
pub fn decode_audio(bytes: &[u8]) -> Result<Sound, String> {
    let (samples, rate) = oxidezap_audio::decode_audio_file(bytes).map_err(|e| e.to_string())?;
    Ok(Sound {
        samples: samples.into(),
        rate,
    })
}

/// A lossy still WebP.
pub(super) fn encode_still(rgba: &[u8], side: u32, quality: f32) -> Result<Vec<u8>, String> {
    if rgba.len() != (side * side * 4) as usize {
        return Err("the sticker is the wrong size".to_string());
    }
    let mut output: *mut u8 = std::ptr::null_mut();
    // SAFETY: `rgba` holds `side` rows of `side * 4` bytes, checked above;
    // libwebp allocates `output` and it is freed with `WebPFree` below, after
    // being copied out.
    let bytes = unsafe {
        let length = libwebp_sys::WebPEncodeRGBA(
            rgba.as_ptr(),
            side as i32,
            side as i32,
            (side * 4) as i32,
            quality,
            &mut output,
        );
        if length == 0 || output.is_null() {
            return Err("the sticker could not be encoded".to_string());
        }
        let bytes = std::slice::from_raw_parts(output, length).to_vec();
        libwebp_sys::WebPFree(output.cast());
        bytes
    };
    Ok(bytes)
}

/// A lossy animated WebP of `count` frames `step_ms` apart, each asked for
/// as it is encoded so no more than one is held.
pub(super) fn encode_animation(
    side: u32,
    count: u32,
    step_ms: u32,
    quality: f32,
    frame: &mut dyn FnMut(u32) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    use webp_animation::{
        Encoder, EncoderOptions, EncodingConfig, EncodingType, LossyEncodingConfig,
    };
    let options = EncoderOptions {
        minimize_size: true,
        allow_mixed: true,
        encoding_config: Some(EncodingConfig {
            encoding_type: EncodingType::Lossy(LossyEncodingConfig::default()),
            quality,
            method: 4,
        }),
        ..EncoderOptions::default()
    };
    let mut encoder =
        Encoder::new_with_options((side, side), options).map_err(|e| format!("{e:?}"))?;
    for index in 0..count {
        let rgba = frame(index)?;
        encoder
            .add_frame(&rgba, (index * step_ms) as i32)
            .map_err(|e| format!("a frame could not be encoded: {e:?}"))?;
    }
    let webp = encoder
        .finalize((count * step_ms) as i32)
        .map_err(|e| format!("the animation could not be finished: {e:?}"))?;
    Ok(webp.to_vec())
}

/// Opus in OGG, as a voice note is encoded.
pub(super) fn encode_opus(samples: &[f32], rate: u32, seconds: u32) -> Result<Vec<u8>, String> {
    let note = oxidezap_audio::RecordedAudio {
        samples: samples.to_vec(),
        sample_rate: rate,
        duration_secs: seconds,
    }
    .prepare();
    oxidezap_audio::encode_to_opus_ogg(&note).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba([10, 200, 30, 255]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
        bytes
    }

    #[test]
    fn a_photo_is_a_still_source_kept_at_a_sensible_size() {
        let source = load(png(3200, 1600), "image/png").unwrap();
        assert!(!source.is_animated());
        assert_eq!(source.poster.dimensions(), (1600, 800));
    }

    #[test]
    fn a_gif_is_an_animated_source() {
        let mut bytes = Vec::new();
        {
            let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
            for shade in [0u8, 128, 255] {
                let frame = image::Frame::from_parts(
                    image::RgbaImage::from_pixel(8, 8, image::Rgba([shade, 0, 0, 255])),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(200, 1),
                );
                encoder.encode_frame(frame).unwrap();
            }
        }
        let source = load(bytes, "image/gif").unwrap();
        assert!(source.is_animated());
        assert_eq!(source.duration_ms, 600);
    }

    #[test]
    fn stills_and_animations_encode_as_lossy_webp() {
        let rgba = vec![128u8; 512 * 512 * 4];
        let still = encode_still(&rgba, 512, 80.0).unwrap();
        assert_eq!(&still[..4], b"RIFF");
        // Lossy (a `VP8 ` chunk, never `VP8L`), with its transparency
        // beside it in the extended container.
        assert!(still.windows(4).any(|w| w == b"VP8 "));
        assert!(!still.windows(4).any(|w| w == b"VP8L"));
        // Frames that differ: identical ones are merged into a still.
        let mut frame = |index: u32| Ok(vec![(index * 90) as u8; 512 * 512 * 4]);
        let animated = encode_animation(512, 3, 100, 50.0, &mut frame).unwrap();
        assert_eq!(&animated[12..16], b"VP8X");
        assert!(animated.windows(4).any(|w| w == b"ANIM"));
    }

    #[test]
    fn a_sound_becomes_opus_in_ogg() {
        let samples: Vec<f32> = (0..16_000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
        let ogg = encode_opus(&samples, 16_000, 1).unwrap();
        assert_eq!(&ogg[..4], b"OggS");
    }
}
