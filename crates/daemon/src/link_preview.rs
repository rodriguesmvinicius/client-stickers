//! The card a link gets while it is being typed: the page's title,
//! description and picture, read from the tags a page publishes for exactly
//! this (Open Graph, with Twitter's and plain HTML's as fallbacks).
//!
//! Fetched here because the daemon is what reaches the network — the window
//! never does — and only for a link the person typed into their own composer:
//! a link someone *sent* is drawn from the card in their message, and nothing
//! about it is fetched. The request names itself as WhatsApp, because that is
//! the client the card is for and the one sites answer with these tags; some
//! (X among them) serve a bare shell to anything they do not recognise.

use oxidezap_core::LinkPreview;

/// The first MiB of a page. The tags are in its head; a page that buries them
/// further is not read to the end for a card.
const PAGE_BYTES: u64 = 1 << 20;
/// A preview picture is a few hundred kilobytes; one past this is not worth
/// downloading to shrink to a thumbnail.
const IMAGE_BYTES: u64 = 8 << 20;
/// The longest side of the JPEG that goes out in the message. WhatsApp's own
/// cards are about this size, and it keeps the message small.
const THUMBNAIL_SIDE: u32 = 320;
/// A card's description is two lines of small print.
const DESCRIPTION_CHARS: usize = 300;
const USER_AGENT: &str = "WhatsApp/2.24.6.77 A";

/// The card for `url`, `None` when the page says nothing worth a card.
pub async fn fetch(url: String) -> Result<Option<LinkPreview>, String> {
    let url = url.trim().to_string();
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("only web links get a preview".to_string());
    }
    imp::fetch(url).await
}

/// What a page says about itself.
#[derive(Debug, Default, PartialEq, Eq)]
struct PageCard {
    title: String,
    description: String,
    /// As the page wrote it: possibly relative, resolved by the caller.
    image: Option<String>,
}

/// Read the card tags out of a page's HTML.
///
/// Not a parser: a scan for `<meta>` and `<title>`, which is what every
/// link-preview reader does, because the tags are flat and pages that get
/// them wrong get them wrong for everyone.
fn read_page(html: &str) -> PageCard {
    let lower = html.to_ascii_lowercase();
    let mut og = PageCard::default();
    let mut fallback = PageCard::default();
    let mut from = 0;
    while let Some(at) = lower[from..].find("<meta") {
        let start = from + at;
        let Some(end) = lower[start..].find('>').map(|e| start + e) else {
            break;
        };
        from = end + 1;
        let tag = &html[start + 5..end];
        let attributes = attributes(tag);
        let key = attributes
            .iter()
            .find(|(name, _)| name == "property" || name == "name" || name == "itemprop")
            .map(|(_, value)| value.to_ascii_lowercase());
        let Some(content) = attributes
            .iter()
            .find(|(name, _)| name == "content")
            .map(|(_, value)| decode_entities(value).trim().to_string())
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        let Some(key) = key else { continue };
        let set = |slot: &mut String| {
            if slot.is_empty() {
                *slot = content.clone();
            }
        };
        match key.as_str() {
            "og:title" => set(&mut og.title),
            "og:description" => set(&mut og.description),
            "og:image" | "og:image:url" | "og:image:secure_url" => {
                og.image.get_or_insert_with(|| content.clone());
            }
            "twitter:title" | "title" => set(&mut fallback.title),
            "twitter:description" | "description" => set(&mut fallback.description),
            "twitter:image" | "twitter:image:src" | "image" | "thumbnailurl" => {
                fallback.image.get_or_insert_with(|| content.clone());
            }
            _ => {}
        }
    }
    if fallback.title.is_empty()
        && let Some(open) = lower.find("<title")
        && let Some(gt) = lower[open..].find('>').map(|g| open + g + 1)
        && let Some(close) = lower[gt..].find("</title").map(|c| gt + c)
    {
        fallback.title = decode_entities(html[gt..close].trim());
    }
    let mut description = if og.description.is_empty() {
        fallback.description
    } else {
        og.description
    };
    if let Some((cut, _)) = description.char_indices().nth(DESCRIPTION_CHARS) {
        description.truncate(cut);
        description.push('…');
    }
    PageCard {
        title: if og.title.is_empty() {
            fallback.title
        } else {
            og.title
        },
        description,
        image: og.image.or(fallback.image),
    }
}

/// `name="value"` pairs of one tag, names lower-cased. Quoted with either
/// quote or not at all, as HTML allows.
fn attributes(tag: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let bytes = tag.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if i == name_start {
            i += 1;
            continue;
        }
        let name = tag[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            found.push((name, String::new()));
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = match bytes.get(i) {
            Some(&quote @ (b'"' | b'\'')) => {
                let start = i + 1;
                let end = tag[start..]
                    .find(quote as char)
                    .map_or(tag.len(), |e| start + e);
                i = end + 1;
                &tag[start..end]
            }
            Some(_) => {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                &tag[start..i]
            }
            None => "",
        };
        found.push((name, value.to_string()));
    }
    found
}

/// The character references a title or description is written with.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        // A reference is short; a `;` further on ends something else.
        let Some(semi) = rest
            .char_indices()
            .take(12)
            .find(|(_, c)| *c == ';')
            .map(|(at, _)| at)
        else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// `href` as an absolute URL, read against the page it was found on.
fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return Some(href.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(path) = href.strip_prefix("//") {
        return Some(format!("{scheme}://{path}"));
    }
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let origin = format!("{scheme}://{}", &rest[..host_end]);
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    if href.is_empty() || lower.starts_with("data:") || lower.contains(':') {
        return None;
    }
    let path = &rest[host_end..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let directory = path.rfind('/').map_or("/", |slash| &path[..=slash]);
    Some(format!("{origin}{directory}{href}"))
}

/// The preview picture as the small JPEG a message carries.
fn thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let format = oxidezap_session::media::allowed_image_format(bytes)?;
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    let picture = reader.decode().ok()?;
    let small = picture.thumbnail(THUMBNAIL_SIDE, THUMBNAIL_SIDE).to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 75)
        .encode_image(&small)
        .ok()?;
    Some(jpeg)
}

/// The card from what was fetched, when there is anything on it.
fn card(url: String, page: PageCard, thumbnail: Option<Vec<u8>>) -> Option<LinkPreview> {
    let preview = LinkPreview {
        url,
        title: page.title,
        description: page.description,
        thumbnail: thumbnail.map(std::sync::Arc::new),
    };
    preview.is_worth_showing().then_some(preview)
}

#[cfg(not(target_family = "wasm"))]
mod imp {
    use std::io::Read as _;
    use std::time::Duration;

    use oxidezap_core::LinkPreview;

    use super::{IMAGE_BYTES, PAGE_BYTES, PageCard, USER_AGENT};

    pub(super) async fn fetch(url: String) -> Result<Option<LinkPreview>, String> {
        oxidezap_session::unblock(move || blocking(url))
            .await
            .map_err(|e| e.to_string())?
    }

    fn blocking(url: String) -> Result<Option<LinkPreview>, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .max_redirects(6)
            .user_agent(USER_AGENT)
            .build()
            .into();
        let mut response = agent
            .get(&url)
            .header(
                "Accept",
                "text/html,application/xhtml+xml;q=0.9,image/*;q=0.8,*/*;q=0.5",
            )
            .header("Accept-Language", "pt-BR,pt;q=0.9,en;q=0.8")
            .call()
            .map_err(|e| format!("the page did not answer: {e}"))?;
        let landed = {
            use ureq::ResponseExt as _;
            response.get_uri().to_string()
        };
        let kind = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        // A link straight to a picture is its own card.
        if kind.starts_with("image/") {
            let bytes = response
                .body_mut()
                .with_config()
                .limit(IMAGE_BYTES)
                .read_to_vec()
                .map_err(|e| format!("the picture did not download: {e}"))?;
            return Ok(super::card(
                url,
                PageCard::default(),
                super::thumbnail(&bytes),
            ));
        }
        if !kind.is_empty() && !kind.contains("html") {
            return Ok(None);
        }
        let mut html = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(PAGE_BYTES)
            .read_to_end(&mut html)
            .map_err(|e| format!("the page did not download: {e}"))?;
        let page = super::read_page(&String::from_utf8_lossy(&html));
        let thumbnail = page
            .image
            .as_deref()
            .and_then(|href| super::resolve(&landed, href))
            .and_then(|image| {
                agent
                    .get(&image)
                    .header("Accept", "image/*")
                    .call()
                    .ok()?
                    .body_mut()
                    .with_config()
                    .limit(IMAGE_BYTES)
                    .read_to_vec()
                    .ok()
            })
            .and_then(|bytes| super::thumbnail(&bytes));
        Ok(super::card(url, page, thumbnail))
    }
}

/// A page cannot read another site's HTML — the browser refuses a
/// cross-origin read without that site's consent — so the web build composes
/// without cards rather than through a proxy that would see every link.
#[cfg(target_family = "wasm")]
mod imp {
    use oxidezap_core::LinkPreview;

    pub(super) async fn fetch(_url: String) -> Result<Option<LinkPreview>, String> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_graph_wins_and_entities_are_decoded() {
        let html = r#"<html><head>
            <title>Fallback &amp; title</title>
            <meta name="description" content="plain description">
            <meta property="og:title" content="Revista &quot;Oeste&quot; no X" />
            <meta content='Fachin vai enviar &#224; PGR' property='og:description'>
            <meta property="og:image" content="/media/card.jpg?x=1">
            <meta name="twitter:image" content="https://cdn.example.invalid/t.jpg">
            </head><body></body></html>"#;
        let page = read_page(html);
        assert_eq!(page.title, "Revista \"Oeste\" no X");
        assert_eq!(page.description, "Fachin vai enviar à PGR");
        assert_eq!(page.image.as_deref(), Some("/media/card.jpg?x=1"));
    }

    #[test]
    fn a_page_without_open_graph_falls_back_to_its_title_and_description() {
        let html = "<HEAD><TITLE> Só o título </TITLE>\
                    <META NAME=description CONTENT=curta></HEAD>";
        let page = read_page(html);
        assert_eq!(page.title, "Só o título");
        assert_eq!(page.description, "curta");
        assert_eq!(page.image, None);
    }

    #[test]
    fn a_long_description_is_cut_to_two_lines_worth() {
        let long = "palavra ".repeat(100);
        let html = format!(r#"<meta property="og:description" content="{long}">"#);
        let page = read_page(&html);
        assert_eq!(page.description.chars().count(), DESCRIPTION_CHARS + 1);
        assert!(page.description.ends_with('…'));
    }

    #[test]
    fn image_addresses_are_resolved_against_the_page() {
        let base = "https://www.example.invalid/a/b/page.html?q=1";
        assert_eq!(
            resolve(base, "https://cdn.example.invalid/x.jpg").as_deref(),
            Some("https://cdn.example.invalid/x.jpg")
        );
        assert_eq!(
            resolve(base, "//cdn.example.invalid/x.jpg").as_deref(),
            Some("https://cdn.example.invalid/x.jpg")
        );
        assert_eq!(
            resolve(base, "/x.jpg").as_deref(),
            Some("https://www.example.invalid/x.jpg")
        );
        assert_eq!(
            resolve(base, "x.jpg").as_deref(),
            Some("https://www.example.invalid/a/b/x.jpg")
        );
        assert_eq!(resolve(base, "data:image/png;base64,AAAA"), None);
    }

    #[test]
    fn a_picture_becomes_a_small_jpeg() {
        let mut png = Vec::new();
        image::RgbImage::new(1200, 600)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let jpeg = thumbnail(&png).expect("thumbnail");
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (THUMBNAIL_SIDE, 160));
    }

    #[test]
    fn a_stray_ampersand_stays_and_a_multibyte_neighbour_is_safe() {
        assert_eq!(decode_entities("A & B"), "A & B");
        assert_eq!(decode_entities("&ação sem fim"), "&ação sem fim");
        assert_eq!(decode_entities("&#x1F600;"), "😀");
    }

    #[test]
    fn a_page_with_nothing_on_it_is_no_card() {
        assert!(card("https://example.invalid".into(), PageCard::default(), None).is_none());
    }

    #[test]
    fn only_web_links_are_fetched() {
        let refused = futures_lite_block_on(fetch("file:///etc/passwd".into()));
        assert!(refused.is_err());
    }

    /// The refusal happens before any await, so polling once settles it.
    fn futures_lite_block_on<F: std::future::Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(output) => output,
            std::task::Poll::Pending => panic!("the refusal awaited something"),
        }
    }
}
