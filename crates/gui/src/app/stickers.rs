//! The app's half of favorite stickers: handing the picker a connection,
//! sending what it picks, and favoriting a sticker from a message.
//!
//! The picker itself is a composer popover (`components::sticker_picker`);
//! what it cannot do from there is know which chat is open or say anything
//! once it has closed, which is what this side is for.

use gpui::{Context, Entity, Window};
use oxidezap_ipc::StickerRequest;

use super::WhatsAppApp;
use super::notices::Tone;
use crate::components::InputAreaView;

impl WhatsAppApp {
    /// The sticker popover opened: let it list through this connection.
    pub(super) fn open_stickers(&mut self, input: &Entity<InputAreaView>, cx: &mut Context<Self>) {
        let Some(client) = &self.client else {
            return;
        };
        let handle = (**client).clone();
        input.update(cx, |view, cx| view.open_stickers(handle, cx));
    }

    /// Send a favorite into the open chat.
    pub(super) fn send_favorite_sticker(&mut self, filehash: String, cx: &mut Context<Self>) {
        let (Some(client), Some(chat)) = (&self.client, self.selected_chat.clone()) else {
            return;
        };
        let answer = client.stickers(StickerRequest::Send { to: chat, filehash });
        cx.spawn(async move |app: gpui::WeakEntity<Self>, cx| {
            let failure = match answer.await {
                Ok(Ok(_)) => return,
                Ok(Err(detail)) => detail,
                Err(_) => "the connection to the background service ended".to_string(),
            };
            let _ = app.update(cx, |app, cx| {
                app.notify_user(
                    format!("{}: {failure}", crate::tr!(StickerSendFailed)),
                    Tone::Problem,
                    cx,
                );
            });
        })
        .detach();
    }

    /// Add the sticker a message carries to the favorites, on the phone too.
    pub(super) fn favorite_sticker_message(
        &mut self,
        message_id: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(media) = self
            .selected_chat_data()
            .and_then(|chat| chat.messages.iter().find(|m| m.id == message_id))
            .and_then(|message| message.media.as_ref())
            .filter(|media| media.media_type == oxidezap_core::MediaType::Sticker)
            .cloned()
        else {
            return;
        };
        let Some(downloadable) = media.downloadable.clone() else {
            self.notify_user(
                format!(
                    "{}: {}",
                    crate::tr!(StickerFavoriteFailed),
                    "this sticker carries nothing to fetch it by"
                ),
                Tone::Problem,
                cx,
            );
            return;
        };
        let Some(client) = &self.client else {
            return;
        };
        let answer = client.stickers(StickerRequest::SetFavorite {
            media: Box::new(downloadable),
            width: media.width,
            height: media.height,
            is_animated: media.is_animated,
            favorite: true,
        });
        cx.spawn(async move |app: gpui::WeakEntity<Self>, cx| {
            let outcome = match answer.await {
                Ok(Ok(_)) => Ok(()),
                Ok(Err(detail)) => Err(detail),
                Err(_) => Err("the connection to the background service ended".to_string()),
            };
            let _ = app.update(cx, |app, cx| match outcome {
                Ok(()) => app.notify_user(crate::tr!(StickerFavorited), Tone::Info, cx),
                Err(detail) => app.notify_user(
                    format!("{}: {detail}", crate::tr!(StickerFavoriteFailed)),
                    Tone::Problem,
                    cx,
                ),
            });
        })
        .detach();
    }
}
