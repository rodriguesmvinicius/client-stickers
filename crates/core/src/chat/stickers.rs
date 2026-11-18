//! Favorite stickers and the device-local hashtags filed against them.
//!
//! A sticker's identity is its `filehash`: the base64 SHA-256 of the
//! decrypted file. WhatsApp keys its own favorites (`favoriteSticker` in the
//! `regular_low` app-state collection) by exactly that, and it is the same on
//! every device and for every person who sends the same sticker, so hashtags
//! filed against it follow the sticker rather than any one message carrying
//! it.
//!
//! The favorites themselves are synchronized with the phone. The hashtags are
//! not: WhatsApp has nowhere to put them, so they live in this account's store
//! and nowhere else.

use serde::{Deserialize, Serialize};

use super::DownloadableMedia;

/// One favorite sticker, with what it takes to draw and send it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteSticker {
    /// Base64 SHA-256 of the decrypted file — WhatsApp's own key for it.
    pub filehash: String,
    /// Where the file is on the CDN. A favorite synced from another device
    /// carries only this, not the bytes, so this is how it is fetched.
    pub media: DownloadableMedia,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_animated: bool,
    /// When it was favorited, in unix milliseconds. Newest first is the
    /// order the phone shows them in.
    pub favorited_at_ms: i64,
    /// Hashtags filed against it on this device, normalized (see
    /// [`normalize_tag`]) and sorted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Words the sticker came with — its emojis and accessibility label, as
    /// sent alongside it in a message this device received. Searched like
    /// tags, offered as suggestions, never written as tags on their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

/// Fold one hashtag to the form it is stored and compared in.
///
/// The leading `#`s go, case folds, and whatever is not a letter, a digit,
/// `_` or `-` is dropped — so `#Gato`, `gato` and `#gato!` are one tag.
/// Accented letters stay as they are: `#ação` and `#acao` are different
/// words to the person typing them. Emoji are kept, because a sticker's own
/// emoji are the most natural tag it has. `None` when nothing is left.
#[must_use]
pub fn normalize_tag(raw: &str) -> Option<String> {
    let tag: String = raw
        .trim()
        .trim_start_matches('#')
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || is_emoji_char(*c))
        .collect();
    (!tag.is_empty()).then_some(tag)
}

/// Split what was typed into a tag field — `#gato #bravo, raiva` — into
/// normalized, de-duplicated, sorted tags.
#[must_use]
pub fn parse_tags(input: &str) -> Vec<String> {
    let mut tags: Vec<String> = input
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .flat_map(|word| word.split('#'))
        .filter_map(normalize_tag)
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

/// The tags as someone would type them back: `#bravo #gato`.
#[must_use]
pub fn format_tags(tags: &[String]) -> String {
    tags.iter()
        .map(|tag| format!("#{tag}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Words worth suggesting as tags, from the emojis and accessibility label a
/// sticker message carried.
#[must_use]
pub fn hints_from(emojis: &[String], accessibility_label: Option<&str>) -> Vec<String> {
    let mut hints: Vec<String> = emojis
        .iter()
        .filter_map(|emoji| normalize_tag(emoji))
        .chain(
            accessibility_label
                .unwrap_or_default()
                .split(|c: char| !c.is_alphanumeric())
                // Two letters says nothing ("a", "de", "of").
                .filter(|word| word.chars().count() > 2)
                .filter_map(normalize_tag),
        )
        .collect();
    hints.sort();
    hints.dedup();
    hints
}

impl FavoriteSticker {
    /// Whether this sticker answers a search.
    ///
    /// Every word typed has to match, and a word matches when a tag or a hint
    /// starts with it — so `#ga` already finds `#gato`, and `#gato #bravo`
    /// narrows to stickers carrying both. The `#` is optional. An empty
    /// search matches everything.
    #[must_use]
    pub fn matches(&self, query: &str) -> bool {
        let words = parse_tags(query);
        words.iter().all(|word| {
            self.tags
                .iter()
                .chain(self.hints.iter())
                .any(|tag| tag.starts_with(word.as_str()))
        })
    }
}

/// Order favorites the way the phone does: newest first, and by hash after
/// that so the order is stable between two loads of the same set.
pub fn sort_favorites(stickers: &mut [FavoriteSticker]) {
    stickers.sort_by(|a, b| {
        b.favorited_at_ms
            .cmp(&a.favorited_at_ms)
            .then_with(|| a.filehash.cmp(&b.filehash))
    });
}

/// The pictographic ranges, plus the joiner and the variation selector that
/// glue multi-codepoint emoji together. Approximate on purpose: what this
/// decides is whether a character may stay in a tag, and the cost of a stray
/// symbol surviving is nil.
fn is_emoji_char(c: char) -> bool {
    matches!(c as u32,
        0x200D | 0xFE0F | 0x20E3
        | 0x2190..=0x21FF
        | 0x2300..=0x23FF
        | 0x2460..=0x27BF
        | 0x2900..=0x297F
        | 0x2B00..=0x2BFF
        | 0x1F000..=0x1FAFF)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wacore::download::MediaType as DownloadMediaType;

    fn sticker(hash: &str, at: i64, tags: &[&str], hints: &[&str]) -> FavoriteSticker {
        FavoriteSticker {
            filehash: hash.into(),
            media: DownloadableMedia {
                direct_path: "/v/t62/example".into(),
                media_key: vec![1; 32],
                file_enc_sha256: vec![2; 32],
                file_sha256: vec![3; 32],
                static_url: None,
                is_thumbnail: false,
                file_length: 100,
                mime_type: "image/webp".into(),
                duration_secs: None,
                download_type: DownloadMediaType::Sticker,
            },
            width: Some(512),
            height: Some(512),
            is_animated: false,
            favorited_at_ms: at,
            tags: tags.iter().map(|t| (*t).to_string()).collect(),
            hints: hints.iter().map(|t| (*t).to_string()).collect(),
        }
    }

    #[test]
    fn tags_fold_to_one_spelling() {
        assert_eq!(normalize_tag("#Gato"), Some("gato".into()));
        assert_eq!(normalize_tag("##gato!"), Some("gato".into()));
        assert_eq!(normalize_tag("  ação "), Some("ação".into()));
        assert_eq!(normalize_tag("#"), None);
        assert_eq!(normalize_tag("😂"), Some("😂".into()));
    }

    #[test]
    fn a_tag_field_parses_into_sorted_unique_tags() {
        assert_eq!(
            parse_tags("#gato #Bravo, gato;raiva#bom-dia"),
            vec!["bom-dia", "bravo", "gato", "raiva"]
        );
        assert!(parse_tags("  # , ").is_empty());
        assert_eq!(format_tags(&parse_tags("b a")), "#a #b");
    }

    #[test]
    fn hints_come_from_emojis_and_meaningful_label_words() {
        assert_eq!(
            hints_from(&["😂".into()], Some("A cat laughing at you")),
            vec!["cat", "laughing", "you", "😂"]
        );
    }

    #[test]
    fn search_needs_every_word_and_matches_by_prefix() {
        let s = sticker("h", 1, &["gato", "bravo"], &["😾"]);
        assert!(s.matches(""));
        assert!(s.matches("#ga"));
        assert!(s.matches("gato #bravo"));
        assert!(s.matches("😾"));
        assert!(!s.matches("#gato #feliz"));
        assert!(!s.matches("ato"));
    }

    #[test]
    fn favorites_sort_newest_first_then_by_hash() {
        let mut list = vec![
            sticker("b", 1, &[], &[]),
            sticker("a", 1, &[], &[]),
            sticker("c", 2, &[], &[]),
        ];
        sort_favorites(&mut list);
        let order: Vec<_> = list.iter().map(|s| s.filehash.as_str()).collect();
        assert_eq!(order, ["c", "a", "b"]);
    }
}
