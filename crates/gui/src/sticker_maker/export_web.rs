//! The page's half of the sticker maker: there is none. A page has no WebP
//! encoder and no video decoder it can run synchronously, and the entry
//! point is not offered there ([`crate::platform::capabilities`]); these
//! answer for a call that slips past that.

use super::{Sound, Source};

const UNAVAILABLE: &str = "the sticker maker is not available in the web version";

pub fn load(_bytes: Vec<u8>, _mime: &str) -> Result<Source, String> {
    Err(UNAVAILABLE.to_string())
}

pub fn decode_audio(_bytes: &[u8]) -> Result<Sound, String> {
    Err(UNAVAILABLE.to_string())
}

pub struct VideoReader;

impl VideoReader {
    pub fn open(_bytes: &[u8]) -> Result<Self, String> {
        Err(UNAVAILABLE.to_string())
    }

    pub fn frame(&mut self, _ms: u32) -> Option<image::RgbaImage> {
        None
    }
}

pub(super) fn encode_still(_rgba: &[u8], _side: u32, _quality: f32) -> Result<Vec<u8>, String> {
    Err(UNAVAILABLE.to_string())
}

pub(super) fn encode_animation(
    _side: u32,
    _count: u32,
    _step_ms: u32,
    _quality: f32,
    _frame: &mut dyn FnMut(u32) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    Err(UNAVAILABLE.to_string())
}

pub(super) fn encode_opus(_samples: &[f32], _rate: u32, _seconds: u32) -> Result<Vec<u8>, String> {
    Err(UNAVAILABLE.to_string())
}
