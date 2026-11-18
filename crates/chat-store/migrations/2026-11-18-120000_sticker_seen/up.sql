-- The newest message seen carrying each sticker, by the sticker's file hash.
-- A favorite stores the CDN path the phone recorded when it was favorited,
-- which can be months old and gone (410). A message carrying the same
-- sticker usually has a newer upload, and if that is gone too, the message
-- is what the phone can be asked to re-upload. `seen_at_ms` is the message's
-- own time, so a backfilled old message never replaces a newer one.
CREATE TABLE sticker_seen (
 device_id INTEGER NOT NULL REFERENCES device(id) ON DELETE CASCADE,
 filehash TEXT NOT NULL,
 chat_jid TEXT NOT NULL,
 message_id TEXT NOT NULL,
 sender_jid TEXT NOT NULL,
 from_me BOOLEAN NOT NULL,
 direct_path TEXT NOT NULL,
 media_key BLOB NOT NULL,
 file_enc_sha256 BLOB NOT NULL,
 file_length BIGINT NOT NULL,
 mimetype TEXT NOT NULL,
 seen_at_ms BIGINT NOT NULL,
 PRIMARY KEY (device_id, filehash)
);
