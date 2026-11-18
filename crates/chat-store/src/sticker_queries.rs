//! Reads and direct writes for favorite stickers and their hashtags.
//!
//! The synced side — a favorite arriving from the phone — is applied by the
//! writer like every other event (see `store::stickers`). What is here is
//! what the session does on this device's own behalf: listing, recording a
//! favorite it just sent to WhatsApp, filing tags, and pruning after a resync.

use std::collections::HashMap;

use diesel::prelude::*;
use oxidezap_core::{DownloadableMedia, FavoriteSticker};

use wacore::store::error::StoreError;

use crate::error::{Result, db_err};
use crate::schema;
use crate::store::ChatStore;
pub use crate::store::stickers::FavoriteWrite;

type FavoriteRow = (
    String,
    String,
    Vec<u8>,
    Vec<u8>,
    String,
    i64,
    Option<i32>,
    Option<i32>,
    bool,
    i64,
);

fn favorite_of(row: FavoriteRow) -> Option<FavoriteSticker> {
    let (
        filehash,
        direct_path,
        media_key,
        file_enc_sha256,
        mimetype,
        file_length,
        width,
        height,
        is_animated,
        favorited_at_ms,
    ) = row;
    // A key that is not a 32-byte digest names no sticker WhatsApp would.
    oxidezap_core::base64::decode(&filehash).filter(|h| h.len() == 32)?;
    Some(FavoriteSticker {
        media: DownloadableMedia {
            direct_path,
            media_key,
            file_enc_sha256,
            // Left out on purpose. Nothing guarantees the favorite's key is
            // the digest of the exact bytes at the recorded path, and a
            // mismatch would make the downloader refuse a good file. The
            // encrypted digest and the media key's HMAC still prove the bytes
            // are the ones the phone recorded; the key is the identity, not
            // the proof.
            file_sha256: Vec::new(),
            static_url: None,
            is_thumbnail: false,
            file_length: u64::try_from(file_length).unwrap_or(0),
            mime_type: mimetype,
            duration_secs: None,
            download_type: wacore::download::MediaType::Sticker,
        },
        filehash,
        width: width.and_then(|n| u32::try_from(n).ok()),
        height: height.and_then(|n| u32::try_from(n).ok()),
        is_animated,
        favorited_at_ms,
        tags: Vec::new(),
        hints: Vec::new(),
    })
}

impl ChatStore {
    /// Every favorite, newest first, with its tags and hints.
    pub async fn favorite_stickers(&self) -> Result<Vec<FavoriteSticker>> {
        let device_id = self.device_id();
        Ok(self
            .db()
            .run(move |conn| {
                use schema::favorite_stickers::dsl as f;
                let rows: Vec<FavoriteRow> = f::favorite_stickers
                    .filter(f::device_id.eq(device_id))
                    .select((
                        f::filehash,
                        f::direct_path,
                        f::media_key,
                        f::file_enc_sha256,
                        f::mimetype,
                        f::file_length,
                        f::width,
                        f::height,
                        f::is_animated,
                        f::favorited_at_ms,
                    ))
                    .load(conn)
                    .map_err(db_err)?;
                let mut tags = tags_by_hash(conn, device_id)?;
                let mut hints = hints_by_hash(conn, device_id)?;
                let mut stickers: Vec<FavoriteSticker> = rows
                    .into_iter()
                    .filter_map(favorite_of)
                    .map(|mut sticker| {
                        sticker.tags = tags.remove(&sticker.filehash).unwrap_or_default();
                        sticker.hints = hints.remove(&sticker.filehash).unwrap_or_default();
                        sticker
                    })
                    .collect();
                oxidezap_core::sort_favorite_stickers(&mut stickers);
                Ok(stickers)
            })
            .await?)
    }

    /// One favorite by hash, without its tags.
    pub async fn favorite_sticker(&self, filehash: &str) -> Result<Option<FavoriteSticker>> {
        let device_id = self.device_id();
        let filehash = filehash.to_string();
        Ok(self
            .db()
            .run(move |conn| {
                use schema::favorite_stickers::dsl as f;
                let row: Option<FavoriteRow> = f::favorite_stickers
                    .filter(f::device_id.eq(device_id))
                    .filter(f::filehash.eq(&filehash))
                    .select((
                        f::filehash,
                        f::direct_path,
                        f::media_key,
                        f::file_enc_sha256,
                        f::mimetype,
                        f::file_length,
                        f::width,
                        f::height,
                        f::is_animated,
                        f::favorited_at_ms,
                    ))
                    .first(conn)
                    .optional()
                    .map_err(db_err)?;
                Ok(row.and_then(favorite_of))
            })
            .await?)
    }

    /// Record a favorite this device just sent to WhatsApp. The server does
    /// not echo a device's own mutation back to it, so without this the
    /// picker would not show what was just favorited until the next resync.
    pub async fn record_favorite_sticker(&self, write: FavoriteWrite) -> Result<()> {
        let device_id = self.device_id();
        let now = wacore::time::now_utc().timestamp_millis();
        Ok(self
            .db()
            .run(move |conn| {
                crate::store::stickers::upsert_favorite(conn, device_id, &write, now)
                    .map_err(db_err)
            })
            .await?)
    }

    /// Drop a favorite this device just unfavorited. Its tags stay.
    pub async fn remove_favorite_sticker(&self, filehash: &str) -> Result<()> {
        let device_id = self.device_id();
        let filehash = filehash.to_string();
        Ok(self
            .db()
            .run(move |conn| {
                crate::store::stickers::delete_favorite(conn, device_id, &filehash).map_err(db_err)
            })
            .await?)
    }

    /// Replace the hashtags filed against a sticker. Normalized here, so
    /// whatever a front end sends is stored in the one spelling search uses.
    pub async fn set_sticker_tags(&self, filehash: &str, tags: Vec<String>) -> Result<()> {
        let device_id = self.device_id();
        let filehash = filehash.to_string();
        let mut tags: Vec<String> = tags
            .iter()
            .filter_map(|tag| oxidezap_core::normalize_sticker_tag(tag))
            .collect();
        tags.sort();
        tags.dedup();
        Ok(self
            .db()
            .run(move |conn| {
                use schema::sticker_tags::dsl as t;
                conn.transaction(|conn| {
                    diesel::delete(
                        t::sticker_tags
                            .filter(t::device_id.eq(device_id))
                            .filter(t::filehash.eq(&filehash)),
                    )
                    .execute(conn)?;
                    for tag in &tags {
                        diesel::insert_into(t::sticker_tags)
                            .values((
                                t::device_id.eq(device_id),
                                t::filehash.eq(&filehash),
                                t::tag.eq(tag),
                            ))
                            .execute(conn)?;
                    }
                    Ok(())
                })
                .map_err(db_err)
            })
            .await?)
    }

    /// After a full resync, drop the favorites the server did not confirm.
    ///
    /// The replay re-applies every favorite it still has, which refreshes
    /// `seen_at_ms`; a row older than the start of the replay is one the
    /// phone no longer lists. Returns how many went.
    pub async fn prune_favorite_stickers_unseen_since(&self, since_ms: i64) -> Result<usize> {
        let device_id = self.device_id();
        Ok(self
            .db()
            .run(move |conn| {
                use schema::favorite_stickers::dsl as f;
                diesel::delete(
                    f::favorite_stickers
                        .filter(f::device_id.eq(device_id))
                        .filter(f::seen_at_ms.lt(since_ms)),
                )
                .execute(conn)
                .map_err(db_err)
            })
            .await?)
    }

    /// How many favorites the store holds. Cheap; used to decide whether a
    /// first resync is worth asking for.
    pub async fn favorite_sticker_count(&self) -> Result<i64> {
        let device_id = self.device_id();
        Ok(self
            .db()
            .run(move |conn| {
                use schema::favorite_stickers::dsl as f;
                f::favorite_stickers
                    .filter(f::device_id.eq(device_id))
                    .count()
                    .get_result(conn)
                    .map_err(db_err)
            })
            .await?)
    }
}

fn tags_by_hash(
    conn: &mut SqliteConnection,
    device_id: i32,
) -> std::result::Result<HashMap<String, Vec<String>>, StoreError> {
    use schema::sticker_tags::dsl as t;
    let rows: Vec<(String, String)> = t::sticker_tags
        .filter(t::device_id.eq(device_id))
        .order((t::filehash, t::tag))
        .select((t::filehash, t::tag))
        .load(conn)
        .map_err(db_err)?;
    let mut by_hash: HashMap<String, Vec<String>> = HashMap::new();
    for (hash, tag) in rows {
        by_hash.entry(hash).or_default().push(tag);
    }
    Ok(by_hash)
}

fn hints_by_hash(
    conn: &mut SqliteConnection,
    device_id: i32,
) -> std::result::Result<HashMap<String, Vec<String>>, StoreError> {
    use schema::sticker_hints::dsl as h;
    let rows: Vec<(String, String)> = h::sticker_hints
        .filter(h::device_id.eq(device_id))
        .select((h::filehash, h::words))
        .load(conn)
        .map_err(db_err)?;
    Ok(rows
        .into_iter()
        .map(|(hash, words)| (hash, words.split(' ').map(str::to_owned).collect()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust_sqlite_storage::SqliteStore;

    async fn store(name: &str) -> std::sync::Arc<ChatStore> {
        let store = SqliteStore::open(&format!(
            "file:memdb_stickers_{name}_{}?mode=memory&cache=shared",
            std::process::id()
        ))
        .await
        .expect("create store");
        // Production creates the device row; the favorites' foreign key needs it.
        store
            .database()
            .provision_device(store.device_id())
            .await
            .expect("seed device parent");
        ChatStore::new(&store).await.expect("run migrations")
    }

    fn write(hash_byte: u8, at: i64) -> FavoriteWrite {
        FavoriteWrite {
            filehash: oxidezap_core::base64::encode(&[hash_byte; 32]),
            direct_path: "/v/t62.15575-24/example".into(),
            media_key: vec![1; 32],
            file_enc_sha256: vec![2; 32],
            mimetype: "image/webp".into(),
            file_length: 4_096,
            width: Some(512),
            height: Some(512),
            is_animated: false,
            favorited_at_ms: at,
        }
    }

    #[tokio::test]
    async fn favorites_list_newest_first_with_their_tags() {
        let chats = store("list").await;
        let older = write(3, 10);
        let newer = write(4, 20);
        chats.record_favorite_sticker(older.clone()).await.unwrap();
        chats.record_favorite_sticker(newer.clone()).await.unwrap();
        chats
            .set_sticker_tags(
                &older.filehash,
                vec!["#Gato".into(), "bravo".into(), "gato".into()],
            )
            .await
            .unwrap();

        let listed = chats.favorite_stickers().await.unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].filehash, newer.filehash);
        assert_eq!(listed[1].tags, vec!["bravo", "gato"]);
        // Verified by the encrypted digest and HMAC, not by the key.
        assert!(listed[1].media.file_sha256.is_empty());
        assert_eq!(listed[1].media.file_enc_sha256, vec![2; 32]);
        assert!(listed[1].matches("#ga #bra"));
        assert_eq!(chats.favorite_sticker_count().await.unwrap(), 2);
    }

    #[tokio::test]
    async fn unfavoriting_keeps_the_tags_for_next_time() {
        let chats = store("unfavorite").await;
        let sticker = write(5, 10);
        chats
            .record_favorite_sticker(sticker.clone())
            .await
            .unwrap();
        chats
            .set_sticker_tags(&sticker.filehash, vec!["gato".into()])
            .await
            .unwrap();
        chats
            .remove_favorite_sticker(&sticker.filehash)
            .await
            .unwrap();
        assert!(chats.favorite_stickers().await.unwrap().is_empty());

        chats
            .record_favorite_sticker(sticker.clone())
            .await
            .unwrap();
        assert_eq!(
            chats.favorite_stickers().await.unwrap()[0].tags,
            vec!["gato"]
        );
    }

    #[tokio::test]
    async fn pruning_drops_only_what_a_resync_did_not_confirm() {
        let chats = store("prune").await;
        chats.record_favorite_sticker(write(6, 10)).await.unwrap();
        let cutoff = wacore::time::now_utc().timestamp_millis() + 1;
        // Rows recorded at or after the cutoff are the ones the replay saw.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        chats.record_favorite_sticker(write(7, 20)).await.unwrap();
        assert_eq!(
            chats
                .prune_favorite_stickers_unseen_since(cutoff)
                .await
                .unwrap(),
            1
        );
        let left = chats.favorite_stickers().await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].favorited_at_ms, 20);
    }
}
