//! The sticker maker's cutout: find the subject of a picture so the
//! background around it can go.
//!
//! The model is `silueta`, a 43 MB U²-Net from the rembg project. It is not
//! shipped in the build: it is fetched from rembg's release the first time a
//! cutout is asked for, checked against its SHA-256, and kept next to the
//! store, so a person who never cuts anything out never downloads it. It runs
//! here, in the daemon, because this is the process that reaches the network
//! and the one place a 43 MB model is loaded once rather than per window.
//!
//! Inference is tract — plain Rust, so nothing extra to install or link —
//! and the loaded model is dropped a couple of minutes after its last use:
//! it costs a few hundred megabytes while held, and a sticker maker is
//! opened now and then, not kept open.

use oxidezap_ipc::CutoutReply;

/// Where the model is fetched from the first time.
const MODEL_URL: &str =
    "https://github.com/danielgatis/rembg/releases/download/v0.0.0/silueta.onnx";
/// The model's SHA-256, so a corrupted or substituted download is refused.
const MODEL_SHA256: &str = "75da6c8d2f8096ec743d071951be73b4a8bc7b3e51d9a6625d63644f90ffeedb";
/// Its size, to tell a finished download from a partial one without
/// hashing 43 MB at every cutout.
const MODEL_BYTES: u64 = 44_173_029;
/// The side the model reads and answers at.
const SIDE: u32 = 320;

/// Cut the subject out of `picture` (PNG or JPEG): the mask, as a greyscale
/// PNG at the model's size.
pub async fn cutout(picture: Vec<u8>) -> Result<CutoutReply, String> {
    let input = oxidezap_session::unblock(move || prepare(&picture))
        .await
        .map_err(|e| e.to_string())??;
    let mask = imp::infer(input).await?;
    Ok(CutoutReply::Mask { png: mask })
}

/// The picture as the model reads it: 320×320 RGB, normalised with the
/// ImageNet mean and deviation, channels first.
fn prepare(picture: &[u8]) -> Result<Vec<f32>, String> {
    let picture = image::load_from_memory(picture)
        .map_err(|e| format!("the picture could not be read: {e}"))?
        .resize_exact(SIDE, SIDE, image::imageops::FilterType::Triangle)
        .to_rgb8();
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const DEVIATION: [f32; 3] = [0.229, 0.224, 0.225];
    let plane = (SIDE * SIDE) as usize;
    let mut input = vec![0.0f32; plane * 3];
    for (index, pixel) in picture.pixels().enumerate() {
        for channel in 0..3 {
            input[channel * plane + index] =
                (pixel[channel] as f32 / 255.0 - MEAN[channel]) / DEVIATION[channel];
        }
    }
    Ok(input)
}

/// The model's answer as a greyscale PNG, stretched to use the whole range
/// the way rembg does: its raw output is a probability that rarely reaches
/// either end.
fn mask_png(raw: &[f32]) -> Result<Vec<u8>, String> {
    let (low, high) = raw.iter().fold((f32::MAX, f32::MIN), |(low, high), &v| {
        (low.min(v), high.max(v))
    });
    let span = (high - low).max(1e-6);
    let pixels: Vec<u8> = raw
        .iter()
        .map(|&v| (((v - low) / span) * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect();
    let image = image::GrayImage::from_raw(SIDE, SIDE, pixels)
        .ok_or_else(|| "the model answered at an unexpected size".to_string())?;
    let mut png = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png)
}

/// Whether `bytes` are the model, by their hash.
#[cfg(not(target_family = "wasm"))]
fn is_the_model(bytes: &[u8]) -> bool {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex == MODEL_SHA256
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use wacore::time::Instant;

    use tract_onnx::prelude::*;

    use super::{MODEL_BYTES, MODEL_URL, SIDE};

    type Model = Arc<TypedRunnableModel>;

    /// The loaded model and when it was last used, while it is held.
    static LOADED: Mutex<Option<(Model, Instant)>> = Mutex::new(None);
    /// How long the model stays loaded after its last cutout.
    const IDLE: Duration = Duration::from_secs(120);

    pub(super) async fn infer(input: Vec<f32>) -> Result<Vec<u8>, String> {
        let path = model_path()?;
        if !matches!(std::fs::metadata(&path), Ok(meta) if meta.len() == MODEL_BYTES) {
            download(path.clone()).await?;
        }
        let mask = oxidezap_session::unblock(move || -> Result<Vec<u8>, String> {
            let model = model(&path)?;
            let input: Tensor =
                tract_ndarray::Array4::from_shape_vec((1, 3, SIDE as usize, SIDE as usize), input)
                    .map_err(|e| e.to_string())?
                    .into();
            let output = model
                .run(tvec!(input.into()))
                .map_err(|e| format!("the cutout failed: {e}"))?;
            let raw = output
                .first()
                .ok_or_else(|| "the model gave no answer".to_string())?
                .to_plain_array_view::<f32>()
                .map_err(|e| e.to_string())?
                .iter()
                .copied()
                .collect::<Vec<f32>>();
            if raw.len() != (SIDE * SIDE) as usize {
                return Err("the model answered at an unexpected size".to_string());
            }
            super::mask_png(&raw)
        })
        .await
        .map_err(|e| e.to_string())??;
        forget_when_idle();
        Ok(mask)
    }

    /// The model, loading it if it is not held.
    fn model(path: &std::path::Path) -> Result<Model, String> {
        let mut loaded = LOADED
            .lock()
            .map_err(|_| "the model lock broke".to_string())?;
        if let Some((model, used)) = loaded.as_mut() {
            *used = Instant::now();
            return Ok(model.clone());
        }
        let onnx = tract_onnx::onnx();
        let mut proto = onnx
            .proto_model_for_path(path)
            .map_err(|e| format!("the model could not be read: {e}"))?;
        // U²-Net's upsampling is exported with PyTorch's half-pixel rule,
        // which tract does not name. It differs from plain half-pixel only
        // when an output side is 1, which no layer here ever produces.
        if let Some(graph) = proto.graph.as_mut() {
            for node in graph
                .node
                .iter_mut()
                .filter(|node| node.op_type == "Resize")
            {
                for attribute in node.attribute.iter_mut() {
                    if attribute.name == "coordinate_transformation_mode"
                        && attribute.s == b"pytorch_half_pixel"
                    {
                        attribute.s = b"half_pixel".to_vec();
                    }
                }
            }
        }
        let model = onnx
            .model_for_proto_model(&proto)
            .and_then(|model| {
                model.with_input_fact(0, f32::fact([1, 3, SIDE as usize, SIDE as usize]).into())
            })
            .and_then(|model| model.into_optimized())
            .and_then(|model| model.into_runnable())
            .map_err(|e| format!("the model could not be prepared: {e}"))?;
        *loaded = Some((model.clone(), Instant::now()));
        Ok(model)
    }

    /// Drop the model once nothing has used it for [`IDLE`].
    fn forget_when_idle() {
        oxidezap_session::spawn(async move {
            oxidezap_platform::sleep(IDLE + Duration::from_secs(1)).await;
            if let Ok(mut loaded) = LOADED.lock()
                && loaded
                    .as_ref()
                    .is_some_and(|(_, used)| used.elapsed() >= IDLE)
            {
                *loaded = None;
                log::info!("cutout model released after {}s idle", IDLE.as_secs());
            }
        });
    }

    /// Where the model is kept: beside the store, so it goes where the
    /// person's data goes and nowhere a system cleaner empties.
    fn model_path() -> Result<std::path::PathBuf, String> {
        let database = std::path::PathBuf::from(oxidezap_session::database_path());
        let folder = database
            .parent()
            .ok_or_else(|| "the store has no folder".to_string())?
            .join("models");
        Ok(folder.join("silueta.onnx"))
    }

    /// Fetch the model, check it, and put it in place.
    async fn download(path: std::path::PathBuf) -> Result<(), String> {
        log::info!("fetching the cutout model ({} MB)", MODEL_BYTES / 1_000_000);
        oxidezap_session::unblock(move || -> Result<(), String> {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(600)))
                .max_redirects(6)
                .build()
                .into();
            let bytes = agent
                .get(MODEL_URL)
                .call()
                .map_err(|e| format!("the cutout model could not be downloaded: {e}"))?
                .body_mut()
                .with_config()
                .limit(MODEL_BYTES + 1024)
                .read_to_vec()
                .map_err(|e| format!("the cutout model download broke off: {e}"))?;
            if bytes.len() as u64 != MODEL_BYTES || !super::is_the_model(&bytes) {
                return Err("the downloaded cutout model is not the expected file".to_string());
            }
            let folder = path
                .parent()
                .ok_or_else(|| "the model has no folder".to_string())?;
            std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
            let partial = path.with_extension("onnx.part");
            std::fs::write(&partial, &bytes).map_err(|e| e.to_string())?;
            std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

#[cfg(target_family = "wasm")]
mod imp {
    pub(super) async fn infer(_input: Vec<f32>) -> Result<Vec<u8>, String> {
        Err("cutting out the background is not available in the web version".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_becomes_the_models_input() {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            40,
            20,
            image::Rgb([255, 0, 0]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
        let input = prepare(&png).unwrap();
        assert_eq!(input.len(), (SIDE * SIDE * 3) as usize);
        // Red, normalised: high in the first plane, low in the other two.
        assert!(input[0] > 2.0);
        assert!(input[(SIDE * SIDE) as usize] < -1.5);
    }

    #[test]
    fn the_mask_uses_the_whole_range() {
        let mut raw = vec![0.2f32; (SIDE * SIDE) as usize];
        raw[0] = 0.9;
        let png = mask_png(&raw).unwrap();
        let mask = image::load_from_memory(&png).unwrap().to_luma8();
        assert_eq!(mask.dimensions(), (SIDE, SIDE));
        assert_eq!(mask.get_pixel(0, 0)[0], 255);
        assert_eq!(mask.get_pixel(1, 0)[0], 0);
    }

    #[test]
    #[cfg(not(target_family = "wasm"))]
    fn only_the_expected_model_is_accepted() {
        assert!(!is_the_model(b"not the model"));
    }

    #[test]
    fn bytes_that_are_not_a_picture_are_refused() {
        assert!(prepare(b"not a picture").is_err());
    }

    /// The real model, end to end: fetched, checked, run on a red disc over
    /// a striped background. Needs the network, so it is not run by default.
    #[test]
    #[ignore = "downloads the 43 MB model"]
    #[cfg(not(target_family = "wasm"))]
    fn the_model_finds_the_subject() {
        let mut picture = image::RgbImage::new(400, 400);
        for (x, y, pixel) in picture.enumerate_pixels_mut() {
            let (dx, dy) = (x as f32 - 200.0, y as f32 - 210.0);
            *pixel = if dx * dx + dy * dy < 110.0 * 110.0 {
                image::Rgb([220, 60, 40])
            } else {
                image::Rgb([
                    ((x * 7 + y * 3) % 60 + 100) as u8,
                    ((x / 8) % 2 * 80 + 90) as u8,
                    120,
                ])
            };
        }
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(picture)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let CutoutReply::Mask { png } = runtime.block_on(cutout(png)).expect("cutout runs");
        let mask = image::load_from_memory(&png).unwrap().to_luma8();
        assert!(mask.get_pixel(160, 168)[0] > 200, "the disc is kept");
        assert!(mask.get_pixel(5, 5)[0] < 50, "the background goes");
    }
}
