//! Favorite stickers, written from WhatsApp's `favoriteSticker` app-state
//! action, plus the hashtags and hints filed against them on this device.
//!
//! What a favorite is and why its key is the file hash is said once, on
//! [`oxidezap_core::FavoriteSticker`].

use diesel::prelude::*;
use wacore::types::events::FavoriteStickerUpdate;
use waproto::whatsapp as wa;

use crate::schema;

/// One favorite as WhatsApp describes it, before it is a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FavoriteWrite {
    pub filehash: String,
    pub direct_path: String,
    pub media_key: Vec<u8>,
    pub file_enc_sha256: Vec<u8>,
    pub mimetype: String,
    pub file_length: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub is_animated: bool,
    pub favorited_at_ms: i64,
}

impl FavoriteWrite {
    /// The row a `stickerAction` describes, or `None` when it carries too
    /// little to ever fetch the file — a favorite nobody can draw is not one
    /// worth listing.
    #[must_use]
    pub fn from_action(
        filehash: &str,
        action: &wa::sync_action_value::StickerAction,
        favorited_at_ms: i64,
    ) -> Option<Self> {
        let direct_path = action.direct_path.clone().filter(|p| !p.is_empty())?;
        let media_key = action.media_key.clone().filter(|k| k.len() == 32)?;
        let file_enc_sha256 = action.file_enc_sha256.clone().filter(|h| h.len() == 32)?;
        Some(Self {
            filehash: filehash.to_string(),
            direct_path,
            media_key,
            file_enc_sha256,
            mimetype: action
                .mimetype
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| "image/webp".to_string()),
            file_length: action
                .file_length
                .and_then(|n| i64::try_from(n).ok())
                .unwrap_or(0),
            width: action.width.and_then(|n| i32::try_from(n).ok()),
            height: action.height.and_then(|n| i32::try_from(n).ok()),
            // WhatsApp's own flag is `isLottie`; an animated WebP is drawn
            // from its frames whatever this says, so it is only a hint.
            is_animated: action.is_lottie.unwrap_or(false),
            favorited_at_ms,
        })
    }
}

/// Apply one synced `favoriteSticker` mutation.
///
/// An unfavorite is a `Set` with `isFavorite = false`, never a syncd
/// `Remove`, so this is the only arm that ever deletes a favorite. The tags
/// are left alone either way.
pub(super) fn apply_favorite(
    conn: &mut SqliteConnection,
    device_id: i32,
    update: &FavoriteStickerUpdate,
) -> QueryResult<()> {
    let now = wacore::time::now_utc().timestamp_millis();
    if update.action.is_favorite != Some(true) {
        return delete_favorite(conn, device_id, &update.filehash);
    }
    let Some(write) = FavoriteWrite::from_action(
        &update.filehash,
        &update.action,
        update.timestamp.timestamp_millis(),
    ) else {
        log::warn!("chat-store: favorite sticker without media fields; not listed");
        return Ok(());
    };
    upsert_favorite(conn, device_id, &write, now)
}

pub(crate) fn upsert_favorite(
    conn: &mut SqliteConnection,
    device_id: i32,
    write: &FavoriteWrite,
    seen_at_ms: i64,
) -> QueryResult<()> {
    use schema::favorite_stickers::dsl;
    diesel::insert_into(dsl::favorite_stickers)
        .values((
            dsl::device_id.eq(device_id),
            dsl::filehash.eq(&write.filehash),
            dsl::direct_path.eq(&write.direct_path),
            dsl::media_key.eq(&write.media_key),
            dsl::file_enc_sha256.eq(&write.file_enc_sha256),
            dsl::mimetype.eq(&write.mimetype),
            dsl::file_length.eq(write.file_length),
            dsl::width.eq(write.width),
            dsl::height.eq(write.height),
            dsl::is_animated.eq(write.is_animated),
            dsl::favorited_at_ms.eq(write.favorited_at_ms),
            dsl::seen_at_ms.eq(seen_at_ms),
        ))
        .on_conflict((dsl::device_id, dsl::filehash))
        .do_update()
        .set((
            dsl::direct_path.eq(&write.direct_path),
            dsl::media_key.eq(&write.media_key),
            dsl::file_enc_sha256.eq(&write.file_enc_sha256),
            dsl::mimetype.eq(&write.mimetype),
            dsl::file_length.eq(write.file_length),
            dsl::width.eq(write.width),
            dsl::height.eq(write.height),
            dsl::is_animated.eq(write.is_animated),
            dsl::favorited_at_ms.eq(write.favorited_at_ms),
            dsl::seen_at_ms.eq(seen_at_ms),
        ))
        .execute(conn)
        .map(|_| ())
}

pub(crate) fn delete_favorite(
    conn: &mut SqliteConnection,
    device_id: i32,
    filehash: &str,
) -> QueryResult<()> {
    use schema::favorite_stickers::dsl;
    diesel::delete(
        dsl::favorite_stickers
            .filter(dsl::device_id.eq(device_id))
            .filter(dsl::filehash.eq(filehash)),
    )
    .execute(conn)
    .map(|_| ())
}

/// Which message a sticker was seen in, for asking the phone to re-upload it.
#[derive(Debug, Clone, Copy)]
pub(super) struct StickerOrigin<'a> {
    pub chat: &'a str,
    pub message_id: &'a str,
    pub sender: &'a str,
    pub from_me: bool,
    pub at_ms: i64,
}

/// Remember what a sticker message says about its sticker: the words it came
/// with (emojis, accessibility label) so a favorite can be found by them, and
/// — when the message is known — where its file is and which message it was,
/// so a favorite whose own CDN path has gone can be fetched through it.
/// Best effort: the message is what matters.
pub(super) fn note_sticker(
    conn: &mut SqliteConnection,
    device_id: i32,
    message: &wa::Message,
    origin: Option<StickerOrigin<'_>>,
) {
    let base = crate::materialize::normalized_message(message);
    let sticker = base.sticker_message.as_option().or_else(|| {
        base.lottie_sticker_message
            .as_option()
            .and_then(|w| w.message.as_option())
            .and_then(|m| m.sticker_message.as_option())
    });
    let Some(sticker) = sticker else {
        return;
    };
    let Some(hash) = sticker.file_sha256.as_deref().filter(|h| h.len() == 32) else {
        return;
    };
    let filehash = oxidezap_core::base64::encode(hash);
    if let Some(origin) = origin
        && let Err(error) = note_seen(conn, device_id, &filehash, sticker, origin)
    {
        log::warn!("chat-store: sticker source not recorded: {error}");
    }
    let emojis: Vec<String> = sticker
        .emojis
        .as_deref()
        .map(|e| e.split(',').map(str::to_owned).collect())
        .unwrap_or_default();
    let words = oxidezap_core::sticker_hints_from(&emojis, sticker.accessibility_label.as_deref());
    if words.is_empty() {
        return;
    }
    use schema::sticker_hints::dsl;
    let joined = words.join(" ");
    if let Err(error) = diesel::insert_into(dsl::sticker_hints)
        .values((
            dsl::device_id.eq(device_id),
            dsl::filehash.eq(&filehash),
            dsl::words.eq(&joined),
        ))
        .on_conflict((dsl::device_id, dsl::filehash))
        .do_update()
        .set(dsl::words.eq(&joined))
        .execute(conn)
    {
        log::warn!("chat-store: sticker hints not recorded: {error}");
    }
}

/// Keep the newest message carrying this sticker. A backfilled older message
/// never replaces a newer one: the newer upload is the likelier to be alive.
fn note_seen(
    conn: &mut SqliteConnection,
    device_id: i32,
    filehash: &str,
    sticker: &wa::message::StickerMessage,
    origin: StickerOrigin<'_>,
) -> QueryResult<()> {
    let (Some(direct_path), Some(media_key), Some(file_enc_sha256)) = (
        sticker.direct_path.as_deref().filter(|p| !p.is_empty()),
        sticker.media_key.as_deref().filter(|k| k.len() == 32),
        sticker.file_enc_sha256.as_deref().filter(|h| h.len() == 32),
    ) else {
        return Ok(());
    };
    if origin.chat.is_empty() || origin.message_id.is_empty() {
        return Ok(());
    }
    use schema::sticker_seen::dsl as s;
    let file_length = sticker
        .file_length
        .and_then(|n| i64::try_from(n).ok())
        .unwrap_or(0);
    let mimetype = sticker
        .mimetype
        .as_deref()
        .filter(|m| !m.is_empty())
        .unwrap_or("image/webp");
    let newer_known: Option<i64> = s::sticker_seen
        .filter(s::device_id.eq(device_id))
        .filter(s::filehash.eq(filehash))
        .select(s::seen_at_ms)
        .first(conn)
        .optional()?;
    if newer_known.is_some_and(|known| known > origin.at_ms) {
        return Ok(());
    }
    diesel::insert_into(s::sticker_seen)
        .values((
            s::device_id.eq(device_id),
            s::filehash.eq(filehash),
            s::chat_jid.eq(origin.chat),
            s::message_id.eq(origin.message_id),
            s::sender_jid.eq(origin.sender),
            s::from_me.eq(origin.from_me),
            s::direct_path.eq(direct_path),
            s::media_key.eq(media_key),
            s::file_enc_sha256.eq(file_enc_sha256),
            s::file_length.eq(file_length),
            s::mimetype.eq(mimetype),
            s::seen_at_ms.eq(origin.at_ms),
        ))
        .on_conflict((s::device_id, s::filehash))
        .do_update()
        .set((
            s::chat_jid.eq(origin.chat),
            s::message_id.eq(origin.message_id),
            s::sender_jid.eq(origin.sender),
            s::from_me.eq(origin.from_me),
            s::direct_path.eq(direct_path),
            s::media_key.eq(media_key),
            s::file_enc_sha256.eq(file_enc_sha256),
            s::file_length.eq(file_length),
            s::mimetype.eq(mimetype),
            s::seen_at_ms.eq(origin.at_ms),
        ))
        .execute(conn)
        .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(favorite: bool) -> wa::sync_action_value::StickerAction {
        let mut action = wa::sync_action_value::StickerAction::default();
        action.is_favorite = Some(favorite);
        action.direct_path = Some("/v/t62.15575-24/example".into());
        action.media_key = Some(vec![1; 32]);
        action.file_enc_sha256 = Some(vec![2; 32]);
        action.mimetype = Some("image/webp".into());
        action.file_length = Some(4_096);
        action.width = Some(512);
        action.height = Some(512);
        action
    }

    #[test]
    fn a_complete_action_becomes_a_row() {
        let write = FavoriteWrite::from_action("AwMD", &action(true), 7).unwrap();
        assert_eq!(write.filehash, "AwMD");
        assert_eq!(write.file_length, 4_096);
        assert_eq!(write.width, Some(512));
        assert_eq!(write.favorited_at_ms, 7);
    }

    #[test]
    fn an_action_that_cannot_be_fetched_is_not_listed() {
        let mut missing_key = action(true);
        missing_key.media_key = None;
        assert!(FavoriteWrite::from_action("AwMD", &missing_key, 7).is_none());
        let mut short_hash = action(true);
        short_hash.file_enc_sha256 = Some(vec![2; 4]);
        assert!(FavoriteWrite::from_action("AwMD", &short_hash, 7).is_none());
    }
}
