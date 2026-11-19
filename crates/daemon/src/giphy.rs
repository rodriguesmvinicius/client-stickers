//! GIPHY for the composer's GIF picker: trending and search, the previews
//! the grid draws, and the MP4 a send uploads.
//!
//! Done here because the daemon is what reaches the network. GIPHY gives each
//! app its own key and asks that it not be shipped in source, so the person
//! pastes theirs once (it is free at developers.giphy.com) and it is kept next
//! to the database, or read from `OXIDEZAP_GIPHY_KEY`. Only GIPHY's own hosts
//! are fetched from: a preview or video address that names anywhere else is
//! refused, so the picker cannot be made to fetch an arbitrary page.

use oxidezap_ipc::{GifItem, GifReply};
use serde::Deserialize;

/// Results per page: four rows of three, twice.
const PAGE: u32 = 24;
/// The largest MP4 sent as-is. GIPHY's originals of a long loop run to tens
/// of megabytes; past this the smaller fixed-width rendition goes instead.
const ORIGINAL_MP4_BYTES: u64 = 6 << 20;
/// What any one download may weigh.
const DOWNLOAD_BYTES: u64 = 16 << 20;
const KEY_FILE: &str = "giphy-api-key";
const KEY_ENV: &str = "OXIDEZAP_GIPHY_KEY";

/// A page of GIFs, trending when `query` is blank.
pub async fn search(query: String, offset: u32) -> Result<GifReply, String> {
    let Some(key) = key() else {
        return Ok(GifReply::NeedsKey);
    };
    let query = query.trim().to_string();
    let url = if query.is_empty() {
        format!(
            "https://api.giphy.com/v1/gifs/trending?api_key={}&limit={PAGE}&offset={offset}&rating=pg-13&bundle=messaging_non_clips",
            encode(&key)
        )
    } else {
        format!(
            "https://api.giphy.com/v1/gifs/search?api_key={}&q={}&limit={PAGE}&offset={offset}&rating=pg-13&lang=pt&bundle=messaging_non_clips",
            encode(&key),
            encode(&query)
        )
    };
    let (status, body) = imp::get(url, 2 << 20).await?;
    // GIPHY's word for a key it does not accept, revoked or mistyped: the
    // picker asks for one again rather than showing an error to retry.
    if matches!(status, 401 | 403) {
        return Ok(GifReply::NeedsKey);
    }
    if !(200..300).contains(&status) {
        return Err(format!("GIPHY answered {status}"));
    }
    let page: Page = serde_json::from_slice(&body)
        .map_err(|e| format!("GIPHY answered with something unexpected: {e}"))?;
    if page
        .meta
        .as_ref()
        .is_some_and(|meta| matches!(meta.status, Some(401 | 403)))
    {
        return Ok(GifReply::NeedsKey);
    }
    Ok(page.into_reply(offset))
}

/// The picture one grid cell draws.
pub async fn preview(url: String) -> Result<GifReply, String> {
    if !is_giphy(&url) {
        return Err("only GIPHY's own pictures are fetched".to_string());
    }
    let bytes = ok(imp::get(url, DOWNLOAD_BYTES).await?)?;
    Ok(GifReply::Preview { bytes })
}

/// Keep `key`, or forget the kept one when it is blank.
pub fn set_key(key: &str) -> Result<GifReply, String> {
    let key = key.trim();
    if key.len() > 128 || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("that does not look like a GIPHY API key".to_string());
    }
    let path = key_path().ok_or("there is nowhere to keep the key on this device")?;
    if key.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        std::fs::write(&path, key).map_err(|e| format!("the key could not be saved: {e}"))?;
    }
    Ok(GifReply::Done)
}

/// What a send uploads: the MP4, and a small JPEG of its first frame for the
/// bubble to show before it is played.
pub async fn fetch_for_send(gif: &GifItem) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
    if !is_giphy(&gif.mp4_url) {
        return Err("only GIPHY's own videos are sent".to_string());
    }
    let video = ok(imp::get(gif.mp4_url.clone(), DOWNLOAD_BYTES).await?)?;
    let thumbnail = if is_giphy(&gif.preview_url) {
        imp::get(gif.preview_url.clone(), DOWNLOAD_BYTES)
            .await
            .ok()
            .and_then(|answer| ok(answer).ok())
            .and_then(|bytes| poster(&bytes))
    } else {
        None
    };
    Ok((video, thumbnail))
}

/// The body of a successful answer.
fn ok((status, body): (u16, Vec<u8>)) -> Result<Vec<u8>, String> {
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(format!("GIPHY answered {status}"))
    }
}

fn key() -> Option<String> {
    std::env::var(KEY_ENV)
        .ok()
        .filter(|key| !key.trim().is_empty())
        .or_else(|| std::fs::read_to_string(key_path()?).ok())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

fn key_path() -> Option<std::path::PathBuf> {
    let database = std::path::PathBuf::from(oxidezap_session::database_path());
    Some(database.parent()?.join(KEY_FILE))
}

/// Whether `url` is on one of GIPHY's hosts, over HTTPS.
fn is_giphy(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    host.bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'.')
        && (host == "giphy.com" || host.ends_with(".giphy.com"))
}

/// Percent-encode a query value.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// The first frame of a preview as a small JPEG, when it is a format this
/// build decodes (GIPHY's WebP renditions are).
fn poster(bytes: &[u8]) -> Option<Vec<u8>> {
    let picture = image::load_from_memory(bytes).ok()?;
    let small = picture.thumbnail(160, 160).to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 70)
        .encode_image(&small)
        .ok()?;
    Some(jpeg)
}

#[derive(Deserialize)]
struct Page {
    #[serde(default)]
    data: Vec<Gif>,
    #[serde(default)]
    pagination: Option<Pagination>,
    #[serde(default)]
    meta: Option<Meta>,
}

#[derive(Deserialize)]
struct Meta {
    #[serde(default)]
    status: Option<u16>,
}

#[derive(Deserialize)]
struct Pagination {
    #[serde(default)]
    total_count: Option<u64>,
    #[serde(default)]
    count: Option<u64>,
}

#[derive(Deserialize)]
struct Gif {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    images: Renditions,
}

#[derive(Deserialize, Default)]
struct Renditions {
    #[serde(default)]
    fixed_width_downsampled: Option<Rendition>,
    #[serde(default)]
    fixed_width: Option<Rendition>,
    #[serde(default)]
    fixed_width_still: Option<Rendition>,
    #[serde(default)]
    original: Option<Rendition>,
    #[serde(default)]
    downsized_small: Option<Rendition>,
}

/// One of GIPHY's renditions. Its numbers come as strings.
#[derive(Deserialize, Default)]
struct Rendition {
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    webp: Option<String>,
    #[serde(default)]
    mp4: Option<String>,
    #[serde(default)]
    mp4_size: Option<String>,
    #[serde(default)]
    width: Option<String>,
    #[serde(default)]
    height: Option<String>,
}

impl Rendition {
    fn number(value: Option<&String>) -> Option<u64> {
        value?.parse().ok()
    }
}

fn present(value: Option<&String>) -> Option<String> {
    value.filter(|value| !value.is_empty()).cloned()
}

impl Gif {
    fn into_item(self) -> Option<GifItem> {
        let images = self.images;
        let preview_url = images
            .fixed_width_downsampled
            .as_ref()
            .and_then(|r| present(r.webp.as_ref()))
            .or_else(|| {
                images
                    .fixed_width
                    .as_ref()
                    .and_then(|r| present(r.webp.as_ref()))
            })
            .or_else(|| {
                images
                    .fixed_width_still
                    .as_ref()
                    .and_then(|r| present(r.url.as_ref()))
            })?;
        let original = images.original.as_ref();
        let original_mp4 = original
            .filter(|r| {
                Rendition::number(r.mp4_size.as_ref()).is_some_and(|n| n <= ORIGINAL_MP4_BYTES)
            })
            .and_then(|r| present(r.mp4.as_ref()));
        let (mp4_url, size) = match original_mp4 {
            Some(mp4) => (mp4, original),
            None => {
                let smaller = images
                    .fixed_width
                    .as_ref()
                    .filter(|r| r.mp4.as_ref().is_some_and(|m| !m.is_empty()))
                    .or(images.downsized_small.as_ref())?;
                (present(smaller.mp4.as_ref())?, Some(smaller))
            }
        };
        let dimension = |pick: fn(&Rendition) -> Option<&String>| {
            size.and_then(|r| Rendition::number(pick(r)))
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0)
        };
        Some(GifItem {
            id: self.id,
            title: self.title,
            preview_url,
            mp4_url,
            width: dimension(|r| r.width.as_ref()),
            height: dimension(|r| r.height.as_ref()),
        })
    }
}

impl Page {
    fn into_reply(self, offset: u32) -> GifReply {
        let returned = self.data.len();
        let gifs: Vec<GifItem> = self.data.into_iter().filter_map(Gif::into_item).collect();
        let reached = u64::from(offset)
            + self
                .pagination
                .as_ref()
                .and_then(|p| p.count)
                .unwrap_or(returned as u64);
        let total = self.pagination.as_ref().and_then(|p| p.total_count);
        let more = returned > 0 && total.is_none_or(|total| reached < total);
        GifReply::Page {
            gifs,
            next: more.then(|| u32::try_from(reached).unwrap_or(u32::MAX)),
        }
    }
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::time::Duration;

    /// The status and the body, whatever the status: a refused key is an
    /// answer the caller reads, not a failure to reach GIPHY.
    pub(super) async fn get(url: String, limit: u64) -> Result<(u16, Vec<u8>), String> {
        oxidezap_session::unblock(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(20)))
                .max_redirects(4)
                .http_status_as_error(false)
                .build()
                .into();
            let mut response = agent
                .get(&url)
                .call()
                .map_err(|e| format!("GIPHY did not answer: {e}"))?;
            let status = response.status().as_u16();
            let body = response
                .body_mut()
                .with_config()
                .limit(limit)
                .read_to_vec()
                .map_err(|e| format!("GIPHY's answer did not arrive: {e}"))?;
            Ok((status, body))
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// A page cannot read GIPHY's API from its own origin without GIPHY's
/// consent per site, so the web build has no GIF picker to feed.
#[cfg(target_family = "wasm")]
mod imp {
    pub(super) async fn get(_url: String, _limit: u64) -> Result<(u16, Vec<u8>), String> {
        Err("GIFs are not available in the web version".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE_JSON: &str = r#"{
        "data": [
            {"id": "a", "title": "Oh happy day GIF", "images": {
                "fixed_width_downsampled": {"url": "https://media1.giphy.com/a/200w_d.gif", "webp": "https://media1.giphy.com/a/200w_d.webp", "width": "200", "height": "113"},
                "original": {"mp4": "https://media1.giphy.com/a/giphy.mp4", "mp4_size": "1048576", "width": "480", "height": "270"}
            }},
            {"id": "b", "title": "", "images": {
                "fixed_width": {"mp4": "https://media2.giphy.com/b/200w.mp4", "webp": "https://media2.giphy.com/b/200w.webp", "width": "200", "height": "200"},
                "original": {"mp4": "https://media2.giphy.com/b/giphy.mp4", "mp4_size": "40000000", "width": "1080", "height": "1080"}
            }},
            {"id": "c", "images": {}}
        ],
        "pagination": {"total_count": 100, "count": 3, "offset": 0},
        "meta": {"status": 200}
    }"#;

    #[test]
    fn a_page_becomes_items_with_the_right_renditions() {
        let page: Page = serde_json::from_str(PAGE_JSON).unwrap();
        let GifReply::Page { gifs, next } = page.into_reply(0) else {
            panic!("a page");
        };
        assert_eq!(next, Some(3));
        assert_eq!(gifs.len(), 2, "one with nothing to draw or send is dropped");
        assert_eq!(
            gifs[0].preview_url,
            "https://media1.giphy.com/a/200w_d.webp"
        );
        assert_eq!(gifs[0].mp4_url, "https://media1.giphy.com/a/giphy.mp4");
        assert_eq!((gifs[0].width, gifs[0].height), (480, 270));
        // A forty-megabyte original sends the fixed-width rendition instead.
        assert_eq!(gifs[1].mp4_url, "https://media2.giphy.com/b/200w.mp4");
        assert_eq!((gifs[1].width, gifs[1].height), (200, 200));
    }

    #[test]
    fn the_last_page_has_no_next() {
        let page: Page =
            serde_json::from_str(r#"{"data": [], "pagination": {"total_count": 0, "count": 0}}"#)
                .unwrap();
        assert_eq!(
            page.into_reply(0),
            GifReply::Page {
                gifs: Vec::new(),
                next: None
            }
        );
    }

    #[test]
    fn only_giphy_hosts_are_fetched() {
        assert!(is_giphy("https://media3.giphy.com/media/a/giphy.mp4"));
        assert!(is_giphy("https://i.giphy.com/a.webp"));
        assert!(!is_giphy("http://media.giphy.com/a.webp"));
        assert!(!is_giphy("https://giphy.com.example.invalid/a"));
        assert!(!is_giphy("https://evil.invalid/?giphy.com"));
    }

    #[test]
    fn queries_are_encoded() {
        assert_eq!(encode("bom dia ☀"), "bom+dia+%E2%98%80");
        assert_eq!(encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn a_key_must_look_like_one() {
        assert!(set_key("not a key!").is_err());
    }
}
