//! Favorite stickers: kept in step with the phone, and sendable from here.
//!
//! WhatsApp syncs favorites as `favoriteSticker` mutations in the
//! `regular_low` app-state collection, keyed by the sticker's file hash. The
//! store applies the ones the phone sends (see `oxidezap-chat-store`); this
//! is the other direction — a favorite made here reaching the phone — plus
//! the full resync that repairs a store that missed the phone's one-time
//! full list, and sending a favorite into a chat.
//!
//! The hashtags are local: WhatsApp has nowhere to keep them.

use oxidezap_core::{DownloadableMedia, FavoriteSticker};
use whatsapp_rust::wacore_binary::jid::Jid;
use whatsapp_rust::waproto::whatsapp as wa;

use super::WhatsAppClient;
use super::sends::SentReceipt;
use crate::exec::Task;

/// A favorite to make or undo, as a message carried the sticker.
#[derive(Debug, Clone)]
pub struct StickerFavorite {
    pub media: DownloadableMedia,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub is_animated: bool,
    pub favorite: bool,
}

impl WhatsAppClient {
    /// The favorites the store holds, newest first, with their tags.
    pub fn favorite_stickers(&self) -> Task<Result<Vec<FavoriteSticker>, String>> {
        let session = self.session.clone();
        self.exec.spawn(async move {
            let Some(live) = session.lock().await.clone() else {
                return Err("no session yet".to_string());
            };
            live.chat_store
                .favorite_stickers()
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// Replay the whole `regular_low` collection, so every favorite the
    /// phone still has is applied again, then drop what it no longer has.
    ///
    /// The phone sends the full list once, when the device links; after that
    /// only changes arrive. A store that was not listening then — this
    /// client, before it knew about stickers — never sees the list unless it
    /// asks. Pin, mute and archive live in the same collection and are
    /// replayed too; the store writes those only when they are news.
    pub fn resync_favorite_stickers(&self) -> Task<Result<Vec<FavoriteSticker>, String>> {
        let session = self.session.clone();
        self.exec.spawn(async move {
            let Some(live) = session.lock().await.clone() else {
                return Err("no session yet".to_string());
            };
            let started_at = wacore::time::now_utc().timestamp_millis();
            let report = live
                .client
                .resync_app_state_collection(whatsapp_rust::WAPatchName::RegularLow)
                .await
                .map_err(|e| format!("WhatsApp did not resend the favorites: {e}"))?;
            // The replay's events are queued on the store's writer; wait for
            // them to land before reading, or the prune below would remove
            // favorites whose confirmation is still in the queue.
            live.chat_store.flush().await.map_err(|e| e.to_string())?;
            if report.all_synced() {
                match live
                    .chat_store
                    .prune_favorite_stickers_unseen_since(started_at)
                    .await
                {
                    Ok(0) => {}
                    Ok(n) => log::info!("stickers: {n} favorite(s) no longer on the phone"),
                    Err(e) => log::warn!("stickers: pruning after resync failed: {e}"),
                }
            } else {
                // A partial replay is not evidence a favorite went away.
                log::warn!("stickers: resync incomplete; keeping every stored favorite");
            }
            live.chat_store
                .favorite_stickers()
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// Favorite or unfavorite a sticker on every linked device.
    pub fn set_sticker_favorite(&self, change: StickerFavorite) -> Task<Result<(), String>> {
        let session = self.session.clone();
        self.exec.spawn(async move {
            let Some(live) = session.lock().await.clone() else {
                return Err("no session yet".to_string());
            };
            apply_favorite_change(&live, &change).await
        })
    }

    /// Unfavorite a sticker the store already lists, by its hash.
    pub fn unfavorite_sticker(&self, filehash: String) -> Task<Result<(), String>> {
        let session = self.session.clone();
        self.exec.spawn(async move {
            let Some(live) = session.lock().await.clone() else {
                return Err("no session yet".to_string());
            };
            let Some(sticker) = live
                .chat_store
                .favorite_sticker(&filehash)
                .await
                .map_err(|e| e.to_string())?
            else {
                // Already gone: what was asked for is true.
                return Ok(());
            };
            apply_favorite_change(
                &live,
                &StickerFavorite {
                    media: sticker.media,
                    width: sticker.width,
                    height: sticker.height,
                    is_animated: sticker.is_animated,
                    favorite: false,
                },
            )
            .await
        })
    }

    /// Replace the hashtags filed against a sticker on this device.
    pub fn set_sticker_tags(
        &self,
        filehash: String,
        tags: Vec<String>,
    ) -> Task<Result<(), String>> {
        let session = self.session.clone();
        self.exec.spawn(async move {
            let Some(live) = session.lock().await.clone() else {
                return Err("no session yet".to_string());
            };
            live.chat_store
                .set_sticker_tags(&filehash, tags)
                .await
                .map_err(|e| e.to_string())
        })
    }

    /// Send a favorite into a chat.
    ///
    /// Fetched and uploaded again rather than forwarded by reference: a
    /// favorite can be months old, and the CDN path it was saved with may
    /// already be gone for the person receiving it. The bytes are the same,
    /// so the sticker arrives with the same hash and stays the same favorite
    /// on both sides.
    pub fn send_favorite_sticker(
        &self,
        to: String,
        filehash: String,
    ) -> Task<Result<SentReceipt, String>> {
        let session = self.session.clone();
        self.exec.spawn(async move {
            let chat: Jid = to.parse().map_err(|_| "not a chat address".to_string())?;
            let Some(live) = session.lock().await.clone() else {
                return Err("no session yet".to_string());
            };
            let sticker = live
                .chat_store
                .favorite_sticker(&filehash)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "that sticker is not a favorite any more".to_string())?;
            let data = live
                .client
                .download(&sticker.media, Default::default())
                .await
                .map_err(|e| format!("the sticker could not be fetched: {e}"))?;
            super::sends::send_sticker_bytes(
                &live,
                &chat,
                data,
                StickerShape {
                    width: sticker.width,
                    height: sticker.height,
                    is_animated: sticker.is_animated,
                },
            )
            .await
        })
    }
}

/// Send the change to WhatsApp, then record it here.
async fn apply_favorite_change(
    live: &super::Session,
    change: &StickerFavorite,
) -> Result<(), String> {
    let write = favorite_write(change)?;
    let value = sync_value(&write, change.favorite);
    live.client
        .send_app_state_action(
            &whatsapp_rust::schemas::FAVORITE_STICKER,
            &[write.filehash.as_str()],
            &value,
        )
        .await
        .map_err(|e| format!("WhatsApp did not take the change: {e}"))?;
    // The server does not echo a device's own mutation back, so the store
    // learns of it here rather than from the event stream.
    let stored = if change.favorite {
        live.chat_store.record_favorite_sticker(write).await
    } else {
        live.chat_store
            .remove_favorite_sticker(&write.filehash)
            .await
    };
    stored.map_err(|e| e.to_string())
}

/// What a sent sticker says about its own picture.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct StickerShape {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub is_animated: bool,
}

/// The row a favorite change describes, refusing media that names no file.
fn favorite_write(change: &StickerFavorite) -> Result<oxidezap_chat_store::FavoriteWrite, String> {
    let media = &change.media;
    if media.file_sha256.len() != 32 {
        return Err("this sticker carries no file hash to favorite it by".to_string());
    }
    if media.direct_path.is_empty()
        || media.media_key.len() != 32
        || media.file_enc_sha256.len() != 32
    {
        return Err("this sticker carries too little to be fetched again".to_string());
    }
    Ok(oxidezap_chat_store::FavoriteWrite {
        filehash: oxidezap_core::base64::encode(&media.file_sha256),
        direct_path: media.direct_path.clone(),
        media_key: media.media_key.clone(),
        file_enc_sha256: media.file_enc_sha256.clone(),
        mimetype: if media.mime_type.is_empty() {
            "image/webp".to_string()
        } else {
            media.mime_type.clone()
        },
        file_length: i64::try_from(media.file_length).unwrap_or(0),
        width: change.width.and_then(|n| i32::try_from(n).ok()),
        height: change.height.and_then(|n| i32::try_from(n).ok()),
        is_animated: change.is_animated,
        favorited_at_ms: wacore::time::now_utc().timestamp_millis(),
    })
}

/// The `stickerAction` WhatsApp Web writes for the same change: the media
/// fields a linked device needs to download the sticker, and which way it
/// goes. Unfavoriting is this same `Set` with `isFavorite = false`.
fn sync_value(write: &oxidezap_chat_store::FavoriteWrite, favorite: bool) -> wa::SyncActionValue {
    let mut action = wa::sync_action_value::StickerAction::default();
    action.direct_path = Some(write.direct_path.clone());
    action.media_key = Some(write.media_key.clone());
    action.file_enc_sha256 = Some(write.file_enc_sha256.clone());
    action.mimetype = Some(write.mimetype.clone());
    action.file_length = u64::try_from(write.file_length).ok().filter(|n| *n > 0);
    action.width = write.width.and_then(|n| u32::try_from(n).ok());
    action.height = write.height.and_then(|n| u32::try_from(n).ok());
    action.is_favorite = Some(favorite);
    let mut value = wa::SyncActionValue::default();
    value.sticker_action = Some(action).into();
    value.timestamp = Some(write.favorited_at_ms);
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media() -> DownloadableMedia {
        DownloadableMedia {
            direct_path: "/v/t62.15575-24/example".into(),
            media_key: vec![1; 32],
            file_enc_sha256: vec![2; 32],
            file_sha256: vec![3; 32],
            static_url: None,
            is_thumbnail: false,
            file_length: 4_096,
            mime_type: "image/webp".into(),
            duration_secs: None,
            download_type: wacore::download::MediaType::Sticker,
        }
    }

    fn change(favorite: bool) -> StickerFavorite {
        StickerFavorite {
            media: media(),
            width: Some(512),
            height: Some(512),
            is_animated: false,
            favorite,
        }
    }

    #[test]
    fn the_key_is_the_plaintext_hash_in_base64() {
        let write = favorite_write(&change(true)).unwrap();
        assert_eq!(write.filehash, oxidezap_core::base64::encode(&[3; 32]));
    }

    #[test]
    fn a_favorite_carries_what_a_linked_device_needs_to_fetch_it() {
        let write = favorite_write(&change(true)).unwrap();
        let value = sync_value(&write, true);
        let action = value.sticker_action.as_option().unwrap();
        assert_eq!(action.is_favorite, Some(true));
        assert_eq!(
            action.direct_path.as_deref(),
            Some("/v/t62.15575-24/example")
        );
        assert_eq!(action.media_key.as_deref(), Some(&[1u8; 32][..]));
        assert_eq!(action.file_length, Some(4_096));
        assert!(value.timestamp.is_some());
        let undo = sync_value(&write, false);
        assert_eq!(
            undo.sticker_action.as_option().unwrap().is_favorite,
            Some(false)
        );
    }

    #[test]
    fn media_without_a_hash_or_a_key_is_refused() {
        let mut no_hash = change(true);
        no_hash.media.file_sha256.clear();
        assert!(favorite_write(&no_hash).is_err());
        let mut no_key = change(true);
        no_key.media.media_key.clear();
        assert!(favorite_write(&no_key).is_err());
    }
}
