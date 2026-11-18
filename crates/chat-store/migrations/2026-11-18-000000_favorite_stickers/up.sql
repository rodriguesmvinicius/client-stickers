-- Favorite stickers, synced with WhatsApp's `favoriteSticker` app-state
-- action and keyed like it: by the base64 SHA-256 of the decrypted file.
-- An unfavorite deletes the row; `seen_at_ms` is when the row was last
-- confirmed by the server, so a full resync can drop rows it no longer has.
CREATE TABLE favorite_stickers (
 device_id INTEGER NOT NULL REFERENCES device(id) ON DELETE CASCADE,
 filehash TEXT NOT NULL,
 direct_path TEXT NOT NULL,
 media_key BLOB NOT NULL,
 file_enc_sha256 BLOB NOT NULL,
 mimetype TEXT NOT NULL,
 file_length BIGINT NOT NULL,
 width INTEGER,
 height INTEGER,
 is_animated BOOLEAN NOT NULL DEFAULT 0,
 favorited_at_ms BIGINT NOT NULL,
 seen_at_ms BIGINT NOT NULL,
 PRIMARY KEY (device_id, filehash)
);

-- Device-local hashtags. Kept apart from the favorites on purpose: WhatsApp
-- has nowhere to store them, and a sticker unfavorited and favorited again
-- should come back with the tags it had.
CREATE TABLE sticker_tags (
 device_id INTEGER NOT NULL REFERENCES device(id) ON DELETE CASCADE,
 filehash TEXT NOT NULL,
 tag TEXT NOT NULL,
 PRIMARY KEY (device_id, filehash, tag)
);

-- What a sticker came with when a message carried it: its emojis and its
-- accessibility label, as space-separated normalized words.
CREATE TABLE sticker_hints (
 device_id INTEGER NOT NULL REFERENCES device(id) ON DELETE CASCADE,
 filehash TEXT NOT NULL,
 words TEXT NOT NULL,
 PRIMARY KEY (device_id, filehash)
);
