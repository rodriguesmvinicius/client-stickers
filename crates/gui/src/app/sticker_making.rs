//! Opening the sticker maker and sending what it makes.
//!
//! The file is chosen with the attachment chooser, read off the UI thread,
//! and the maker opens over the conversation it was opened from; the
//! sticker it makes goes to that chat even if the person has looked
//! elsewhere meanwhile.

use std::sync::Arc;

use gpui::{AppContext as _, Context, Entity, Subscription, Window};

use super::WhatsAppApp;
use super::notices::Tone;
use crate::components::{StickerMakerEvent, StickerMakerView};

/// The maker while it is open, and the chat its sticker goes to.
pub(super) struct StickerMaking {
    pub jid: String,
    pub view: Entity<StickerMakerView>,
    _events: Subscription,
}

impl WhatsAppApp {
    /// Choose a picture, GIF or video, and open the maker on it.
    pub(super) fn open_sticker_maker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(_), Some(jid)) = (&self.client, self.selected_chat.clone()) else {
            return;
        };
        let chosen = crate::platform::picker::choose_category(
            cx,
            crate::platform::picker::AttachmentCategory::PhotosVideos,
        );
        let window_handle = window.window_handle();
        cx.spawn(async move |app: gpui::WeakEntity<Self>, cx| {
            let chosen = match chosen.await {
                Ok(chosen) => chosen,
                Err(detail) => {
                    log::warn!("sticker maker: the chooser failed: {detail}");
                    return;
                }
            };
            let Some(file) = chosen.files.into_iter().next() else {
                if let Some(refused) = chosen.refused.into_iter().next() {
                    let _ = app.update(cx, |app, cx| app.notify_user(refused, Tone::Problem, cx));
                }
                return;
            };
            let _ = app.update(cx, |app, cx| {
                app.notify_user(crate::tr!(StickerOpening), Tone::Info, cx);
            });
            let mime = file.mime_type.clone();
            let load = cx
                .background_executor()
                .spawn(async move { crate::sticker_maker::load(file.bytes, &mime) });
            let source = match load.await {
                Ok(source) => source,
                Err(detail) => {
                    log::warn!("sticker maker: {detail}");
                    let _ = app.update(cx, |app, cx| {
                        app.notify_user(
                            format!("{}: {detail}", crate::tr!(StickerOpenFailed)),
                            Tone::Problem,
                            cx,
                        );
                    });
                    return;
                }
            };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = app.update(cx, |app, cx| {
                    app.show_sticker_maker(jid, source, window, cx)
                });
            });
        })
        .detach();
    }

    fn show_sticker_maker(
        &mut self,
        jid: String,
        source: crate::sticker_maker::Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = &self.client else {
            return;
        };
        let session = (**client).clone();
        let view = cx.new(|cx| StickerMakerView::new(source, Some(session), window, cx));
        let events = cx.subscribe(&view, |app, _, event: &StickerMakerEvent, cx| match event {
            StickerMakerEvent::Close => {
                app.close_sticker_maker(cx);
            }
            StickerMakerEvent::Send(made) => app.send_made_sticker(made.clone(), cx),
        });
        window.focus(&view.read(cx).focus_handle(), cx);
        if let Some(old) = self.sticker_maker.replace(StickerMaking {
            jid,
            view,
            _events: events,
        }) {
            old.view.update(cx, |view, cx| view.release(cx));
        }
        cx.notify();
    }

    /// Close the maker. Whether there was one to close.
    pub(super) fn close_sticker_maker(&mut self, cx: &mut Context<Self>) -> bool {
        match self.sticker_maker.take() {
            Some(making) => {
                making.view.update(cx, |view, cx| view.release(cx));
                cx.notify();
                true
            }
            None => false,
        }
    }

    fn send_made_sticker(&mut self, made: Arc<crate::sticker_maker::Made>, cx: &mut Context<Self>) {
        let Some(jid) = self.sticker_maker.as_ref().map(|making| making.jid.clone()) else {
            return;
        };
        let Some(client) = &self.client else {
            return;
        };
        let answer = client.stickers(oxidezap_ipc::StickerRequest::Create {
            to: jid,
            webp: made.webp.clone(),
            is_animated: made.is_animated,
            sound: made
                .sound
                .as_ref()
                .map(|(ogg, seconds)| oxidezap_ipc::StickerSound {
                    ogg: ogg.clone(),
                    seconds: *seconds,
                }),
        });
        self.close_sticker_maker(cx);
        cx.spawn(async move |app: gpui::WeakEntity<Self>, cx| {
            let failure = match answer.await {
                Ok(Ok(_)) => return,
                Ok(Err(detail)) => detail,
                Err(_) => "the connection to the background service ended".to_string(),
            };
            let _ = app.update(cx, |app, cx| {
                app.notify_user(
                    format!("{}: {failure}", crate::tr!(StickerMakeFailed)),
                    Tone::Problem,
                    cx,
                );
            });
        })
        .detach();
    }
}
