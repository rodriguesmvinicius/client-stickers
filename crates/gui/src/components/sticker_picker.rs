//! Favorite stickers, beside the emoji picker: synced with the phone, found
//! by hashtag, sent with one click.
//!
//! The list comes from the daemon when the popover opens, and the first open
//! in a run also asks WhatsApp for the whole favorites collection again —
//! the phone sends it in full only once, when a device links, so a store
//! that missed that moment never sees most of them otherwise. Pictures are
//! fetched only for the rows on screen, a few at a time, and the daemon's
//! media cache answers every one it has already seen without the network.
//!
//! Hashtags are this device's own (WhatsApp has nowhere to keep them).
//! Right-click a sticker to file them, or to remove it from the favorites.

use std::collections::{HashMap, HashSet, VecDeque};

use gpui::{
    Entity, EventEmitter, Focusable as _, MouseButton, SharedString, UniformListScrollHandle, div,
    img, prelude::*, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
};
use oxidezap_core::FavoriteSticker;
use oxidezap_ipc::{StickerReply, StickerRequest};

use crate::retained_image::RetainedImage;
use crate::session::SessionHandle;
use crate::theme::ActiveProductTheme as _;

const COLUMNS: usize = 4;
/// How many pictures are fetched at once. The daemon refuses past its own
/// permit budget, and a popover of thumbnails is not worth crowding out a
/// download somebody clicked.
const IN_FLIGHT: usize = 4;
/// How many rows of the grid are tall.
const VISIBLE_ROWS: f32 = 3.2;

/// A favorite was clicked: send it, by its hash.
pub(super) struct StickerPicked(pub String);

enum Thumb {
    Queued,
    Loading,
    Ready(RetainedImage),
    /// Why, shown on hover: a favorite whose file is gone from WhatsApp's
    /// servers and one the connection dropped are different problems.
    Failed(SharedString),
}

pub(super) struct StickerPicker {
    session: Option<SessionHandle>,
    search: Entity<InputState>,
    tags: Entity<InputState>,
    favorites: Vec<FavoriteSticker>,
    /// Indices into `favorites`, in order, that answer the search.
    matches: Vec<usize>,
    thumbs: HashMap<String, Thumb>,
    queue: VecDeque<String>,
    in_flight: usize,
    /// Favorites already sent for repair in this opening, so a file that
    /// cannot be found anywhere is asked about once rather than in a loop.
    repairing: HashSet<String>,
    /// The sticker whose tags are being edited, by hash.
    editing: Option<String>,
    status: Option<SharedString>,
    loaded: bool,
    syncing: bool,
    /// Whether this run has already asked WhatsApp for the full list.
    resynced: bool,
    scroll: UniformListScrollHandle,
    language: crate::i18n::Language,
}

impl EventEmitter<StickerPicked> for StickerPicker {}

impl StickerPicker {
    pub fn new(window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(crate::tr!(SearchStickersByTag)));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.filter(cx),
            InputEvent::PressEnter { .. } => {
                if let Some(first) = this.matches.first().and_then(|ix| this.favorites.get(*ix)) {
                    cx.emit(StickerPicked(first.filehash.clone()));
                }
            }
            _ => {}
        })
        .detach();
        let tags = cx
            .new(|cx| InputState::new(window, cx).placeholder(crate::tr!(StickerTagsPlaceholder)));
        cx.subscribe(&tags, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.save_tags(cx);
            }
        })
        .detach();
        Self {
            session: None,
            search,
            tags,
            favorites: Vec::new(),
            matches: Vec::new(),
            thumbs: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: 0,
            repairing: HashSet::new(),
            editing: None,
            status: None,
            loaded: false,
            syncing: false,
            resynced: false,
            scroll: UniformListScrollHandle::new(),
            language: crate::i18n::Language::current(),
        }
    }

    pub fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }

    /// The popover opened: list what the store has, and the first time in a
    /// run, ask the phone for the full set too.
    pub fn opened(&mut self, session: SessionHandle, cx: &mut gpui::Context<Self>) {
        let reconnected = self.session.is_some();
        self.session = Some(session);
        if reconnected {
            // A new connection serves a new media cache; pictures that failed
            // on the old one deserve another try.
            self.thumbs
                .retain(|_, thumb| matches!(thumb, Thumb::Ready(_)));
            self.repairing.clear();
        }
        self.request_list(StickerRequest::List, cx);
        if !self.resynced {
            self.resync(cx);
        }
    }

    fn resync(&mut self, cx: &mut gpui::Context<Self>) {
        if self.syncing {
            return;
        }
        self.resynced = true;
        self.syncing = true;
        self.status = Some(crate::tr!(SyncingStickers).into());
        cx.notify();
        self.request_list(StickerRequest::Resync, cx);
    }

    fn request_list(&mut self, request: StickerRequest, cx: &mut gpui::Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let resync = matches!(request, StickerRequest::Resync);
        let answer = session.stickers(request);
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                if resync {
                    this.syncing = false;
                }
                match answer {
                    Ok(Ok(StickerReply::Favorites { stickers })) => {
                        if resync {
                            this.status = None;
                        }
                        this.loaded = true;
                        this.favorites = stickers;
                        this.filter(cx);
                    }
                    Ok(Ok(StickerReply::Done | StickerReply::Repaired { .. })) => {}
                    Ok(Err(detail)) => {
                        log::warn!("stickers: {detail}");
                        if resync {
                            this.status =
                                Some(format!("{}: {detail}", crate::tr!(StickerSyncFailed)).into());
                        }
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A change the daemon only acknowledges; the list is read again after.
    fn change(&mut self, request: StickerRequest, cx: &mut gpui::Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let answer = session.stickers(request);
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(Err(detail)) = &answer {
                    this.status = Some(detail.clone().into());
                }
                this.request_list(StickerRequest::List, cx);
            });
        })
        .detach();
    }

    fn filter(&mut self, cx: &mut gpui::Context<Self>) {
        let query = self.search.read(cx).text().to_string();
        self.matches = self
            .favorites
            .iter()
            .enumerate()
            .filter(|(_, sticker)| sticker.matches(&query))
            .map(|(ix, _)| ix)
            .collect();
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        cx.notify();
    }

    /// Ask for a picture the grid is about to draw, unless it is already on
    /// its way or here.
    fn want(&mut self, filehash: &str) {
        if !self.thumbs.contains_key(filehash) {
            self.thumbs.insert(filehash.to_string(), Thumb::Queued);
            self.queue.push_back(filehash.to_string());
        }
    }

    fn pump(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        while self.in_flight < IN_FLIGHT
            && let Some(hash) = self.queue.pop_front()
        {
            let Some(sticker) = self.favorites.iter().find(|s| s.filehash == hash) else {
                self.thumbs.remove(&hash);
                continue;
            };
            self.thumbs.insert(hash.clone(), Thumb::Loading);
            self.in_flight += 1;
            let bytes = session.download_downloadable_media(sticker.media.clone());
            cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
                let bytes = bytes.await;
                let _ = this.update(cx, |this, cx| {
                    this.in_flight = this.in_flight.saturating_sub(1);
                    let thumb = match bytes {
                        Ok(Ok(bytes)) if !bytes.is_empty() => Thumb::Ready(RetainedImage::new(
                            gpui::Image::from_bytes(gpui::ImageFormat::Webp, bytes.to_vec()),
                        )),
                        Ok(Ok(_)) => Thumb::Failed("the file came back empty".into()),
                        Ok(Err(failure)) if this.repairing.insert(hash.clone()) => {
                            // Usually a CDN path that has gone (410): ask for
                            // one that answers, once.
                            log::info!(
                                "sticker picture unavailable, repairing: {}",
                                failure.detail
                            );
                            this.repair(hash.clone(), cx);
                            Thumb::Loading
                        }
                        Ok(Err(failure)) => {
                            log::warn!("sticker picture unavailable: {}", failure.detail);
                            Thumb::Failed(failure.detail.clone().into())
                        }
                        Err(_) => Thumb::Failed("the connection to the service ended".into()),
                    };
                    this.thumbs.insert(hash, thumb);
                    this.pump(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn repair(&mut self, filehash: String, cx: &mut gpui::Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let answer = session.stickers(StickerRequest::Repair {
            filehash: filehash.clone(),
        });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(Ok(StickerReply::Repaired { sticker })) => {
                        if let Some(slot) =
                            this.favorites.iter_mut().find(|s| s.filehash == filehash)
                        {
                            slot.media = sticker.media;
                        }
                        this.thumbs.remove(&filehash);
                        this.want(&filehash);
                        this.pump(cx);
                    }
                    Ok(Err(detail)) => {
                        this.thumbs.insert(filehash, Thumb::Failed(detail.into()));
                    }
                    _ => {
                        this.thumbs.insert(
                            filehash,
                            Thumb::Failed("the repair did not come back".into()),
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn begin_edit(
        &mut self,
        filehash: String,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let current = self
            .favorites
            .iter()
            .find(|s| s.filehash == filehash)
            .map(|s| oxidezap_core::format_sticker_tags(&s.tags))
            .unwrap_or_default();
        self.tags
            .update(cx, |input, cx| input.set_value(current, window, cx));
        self.editing = Some(filehash);
        self.status = None;
        cx.notify();
    }

    fn save_tags(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(filehash) = self.editing.take() else {
            return;
        };
        let tags = oxidezap_core::parse_sticker_tags(&self.tags.read(cx).text().to_string());
        // Shown at once; the daemon's answer replaces it with what it stored.
        if let Some(sticker) = self.favorites.iter_mut().find(|s| s.filehash == filehash) {
            sticker.tags = tags.clone();
        }
        self.filter(cx);
        self.change(StickerRequest::SetTags { filehash, tags }, cx);
    }

    fn add_suggestion(
        &mut self,
        tag: &str,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let mut text = self.tags.read(cx).text().to_string();
        if !text.trim().is_empty() {
            text.push(' ');
        }
        text.push('#');
        text.push_str(tag);
        self.tags
            .update(cx, |input, cx| input.set_value(text, window, cx));
    }

    fn unfavorite(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(filehash) = self.editing.take() else {
            return;
        };
        self.favorites.retain(|s| s.filehash != filehash);
        self.filter(cx);
        self.change(StickerRequest::Unfavorite { filehash }, cx);
    }

    fn render_editor(
        &self,
        filehash: &str,
        cell: gpui::Pixels,
        metrics: crate::theme::Metrics,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement + use<> {
        let sticker = self.favorites.iter().find(|s| s.filehash == filehash);
        let current = oxidezap_core::parse_sticker_tags(&self.tags.read(cx).text().to_string());
        let suggestions: Vec<String> = sticker
            .map(|s| {
                s.hints
                    .iter()
                    .filter(|hint| !current.contains(hint))
                    .take(6)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let preview = self.thumb_element(filehash, cell * 0.8);
        div()
            .flex()
            .flex_col()
            .gap(metrics.space_xs())
            .p(metrics.space_xs())
            .rounded(metrics.space_xs())
            .border_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.space_xs())
                    .child(preview)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.tags).w_full()),
                    ),
            )
            .when(!suggestions.is_empty(), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(metrics.space_xs())
                        .text_size(metrics.text_micro())
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::tr!(SuggestedTags))
                        .children(suggestions.into_iter().enumerate().map(|(ix, tag)| {
                            let label = format!("#{tag}");
                            Button::new(("sticker-suggestion", ix))
                                .ghost()
                                .xsmall()
                                .label(label)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.add_suggestion(&tag, window, cx);
                                    cx.notify();
                                }))
                        })),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(metrics.space_xs())
                    .child(
                        Button::new("sticker-save-tags")
                            .primary()
                            .xsmall()
                            .label(crate::tr!(SaveTags))
                            .on_click(cx.listener(|this, _, _, cx| this.save_tags(cx))),
                    )
                    .child(
                        Button::new("sticker-unfavorite")
                            .ghost()
                            .danger()
                            .xsmall()
                            .label(crate::tr!(RemoveFromFavorites))
                            .on_click(cx.listener(|this, _, _, cx| this.unfavorite(cx))),
                    )
                    .child(
                        Button::new("sticker-cancel-tags")
                            .ghost()
                            .xsmall()
                            .label(crate::tr!(Cancel))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.editing = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    fn thumb_element(&self, filehash: &str, size: gpui::Pixels) -> gpui::AnyElement {
        match self.thumbs.get(filehash) {
            Some(Thumb::Ready(image)) => img(image.source(true))
                .size(size)
                .object_fit(gpui::ObjectFit::Contain)
                .into_any_element(),
            Some(Thumb::Failed(why)) => {
                let why = why.clone();
                div()
                    .id(SharedString::from(format!("sticker-failed-{filehash}")))
                    .size(size)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child("⚠")
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(why.clone()).build(window, cx)
                    })
                    .into_any_element()
            }
            _ => div().size(size).into_any_element(),
        }
    }
}

impl Render for StickerPicker {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let language = crate::i18n::Language::current();
        if self.language != language {
            self.language = language;
            self.search.update(cx, |input, cx| {
                input.set_placeholder(crate::tr!(SearchStickersByTag), window, cx)
            });
            self.tags.update(cx, |input, cx| {
                input.set_placeholder(crate::tr!(StickerTagsPlaceholder), window, cx)
            });
        }
        let metrics = cx.product().metrics;
        // Four to a row at half again the old size, which with 3.2 rows on
        // screen makes the popover a fifth wider and taller than five at 2x.
        let cell = metrics.icon_button() * 3.;
        let editing = self.editing.clone();
        let count = self.favorites.len();
        let failed = self
            .thumbs
            .values()
            .filter(|thumb| matches!(thumb, Thumb::Failed(_)))
            .count();
        let searching = !self.search.read(cx).text().to_string().trim().is_empty();
        div()
            .flex()
            .flex_col()
            .gap(metrics.space_sm())
            .w(cell * COLUMNS as f32)
            .text_color(cx.theme().popover_foreground)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(metrics.space_xs())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(metrics.space_xs())
                            .child(crate::tr!(FavoriteStickers))
                            .when(count > 0, |el| {
                                el.child(
                                    div()
                                        .text_size(metrics.text_micro())
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{count} {}",
                                            crate::tr!(StickerCountLabel)
                                        )),
                                )
                            }),
                    )
                    .child(
                        Button::new("sticker-resync")
                            .ghost()
                            .xsmall()
                            .label(crate::tr!(SyncWithPhone))
                            .disabled(self.syncing)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.resynced = false;
                                this.resync(cx);
                            })),
                    ),
            )
            .child(Input::new(&self.search).w_full())
            .when(failed > 0, |panel| {
                panel.child(
                    div()
                        .text_size(metrics.text_micro())
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{failed} {}", crate::tr!(StickerPicturesFailed))),
                )
            })
            .children(self.status.clone().map(|status| {
                div()
                    .text_size(metrics.text_micro())
                    .text_color(cx.theme().muted_foreground)
                    .child(status)
            }))
            .children(
                editing
                    .as_deref()
                    .map(|hash| self.render_editor(hash, cell, metrics, cx)),
            )
            .when(self.loaded && self.matches.is_empty(), |panel| {
                panel.child(
                    div()
                        .text_size(metrics.text_micro())
                        .text_color(cx.theme().muted_foreground)
                        .child(if searching {
                            crate::tr!(NoStickerMatchesTag)
                        } else {
                            crate::tr!(NoFavoriteStickers)
                        }),
                )
            })
            .child(
                uniform_list(
                    "sticker-grid",
                    self.matches.len().div_ceil(COLUMNS),
                    cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                        let rows: Vec<Vec<String>> = range
                            .map(|row| {
                                this.matches
                                    .iter()
                                    .skip(row * COLUMNS)
                                    .take(COLUMNS)
                                    .filter_map(|ix| this.favorites.get(*ix))
                                    .map(|s| s.filehash.clone())
                                    .collect()
                            })
                            .collect();
                        for hash in rows.iter().flatten() {
                            this.want(hash);
                        }
                        this.pump(cx);
                        let tagged: HashSet<&str> = this
                            .favorites
                            .iter()
                            .filter(|s| !s.tags.is_empty())
                            .map(|s| s.filehash.as_str())
                            .collect();
                        rows.iter()
                            .map(|row| {
                                div().flex().h(cell).children(row.iter().map(|hash| {
                                    let send = hash.clone();
                                    let edit = hash.clone();
                                    let selected = this.editing.as_deref() == Some(hash.as_str());
                                    div()
                                        .id(SharedString::from(format!("sticker-{hash}")))
                                        .w(cell)
                                        .h(cell)
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(metrics.space_xs())
                                        .cursor_pointer()
                                        .hover(|s| s.bg(cx.theme().secondary))
                                        .when(selected, |el| {
                                            el.border_1().border_color(cx.theme().primary)
                                        })
                                        .when(tagged.contains(hash.as_str()), |el| {
                                            el.border_b_2().border_color(cx.theme().border)
                                        })
                                        .child(this.thumb_element(hash, cell * 0.85))
                                        .on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(StickerPicked(send.clone()));
                                        }))
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(move |this, _, window, cx| {
                                                this.begin_edit(edit.clone(), window, cx);
                                            }),
                                        )
                                }))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .h(cell * VISIBLE_ROWS)
                .track_scroll(&self.scroll),
            )
            .child(
                div()
                    .text_size(metrics.text_micro())
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::tr!(EditStickerTags)),
            )
    }
}

#[cfg(test)]
mod tests {
    use oxidezap_core::{FavoriteSticker, parse_sticker_tags};

    #[test]
    fn the_editor_round_trips_what_it_shows() {
        let tags = parse_sticker_tags("#gato #bravo");
        assert_eq!(
            parse_sticker_tags(&oxidezap_core::format_sticker_tags(&tags)),
            tags
        );
        let _ = std::mem::size_of::<FavoriteSticker>();
    }
}
