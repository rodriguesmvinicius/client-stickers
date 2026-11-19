//! GIFs from GIPHY, beside the stickers: trending when it opens, a search
//! field and a row of quick searches above a grid, and one click to send.
//!
//! The daemon does the asking — the window never reaches GIPHY itself — and
//! the API key it asks with is the person's own: GIPHY gives them for free,
//! and one baked into a build would be everyone's. Until there is one, the
//! picker asks for it in place of the grid.
//!
//! Previews are GIPHY's small animated WebPs, fetched only for the cells on
//! screen, a few at a time. The grid shows each one's first frame and plays
//! the one under the pointer, and cells scrolled well away give back their
//! decoded pixels, so a long scroll holds a screenful rather than all of it.

use std::collections::{HashMap, VecDeque};

use gpui::{Entity, EventEmitter, SharedString, UniformListScrollHandle, div, img, prelude::*};
use gpui::{Focusable as _, Task, uniform_list};
use gpui_component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
};
use oxidezap_ipc::{GifItem, GifReply, GifRequest};

use crate::retained_image::RetainedImage;
use crate::session::SessionHandle;
use crate::theme::ActiveProductTheme as _;

const COLUMNS: usize = 3;
/// How many previews are fetched at once.
const IN_FLIGHT: usize = 4;
/// How many rows of the grid are tall.
const VISIBLE_ROWS: f32 = 3.4;
/// How long typing has to rest before the search is asked: a word typed out
/// is one search, not one per letter.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(450);
/// How many cells keep their decoded pictures: the grid on screen and a
/// screenful either side of it.
const DRAWN_KEPT: usize = COLUMNS * 10;
/// Where a key is made.
const GIPHY_DEVELOPERS: &str = "https://developers.giphy.com/dashboard/";

/// A GIF was clicked: send it.
pub(super) struct GifPicked(pub GifItem);

enum Thumb {
    Queued,
    Loading,
    Ready(RetainedImage),
    Failed,
}

/// The quick searches under the field, as their labels; the label is what is
/// searched, so they follow the interface's language.
fn categories() -> [SharedString; 8] {
    [
        crate::tr!(GifTrending).into(),
        crate::tr!(GifCategoryHaha).into(),
        crate::tr!(GifCategoryLove).into(),
        crate::tr!(GifCategorySad).into(),
        crate::tr!(GifCategoryHello).into(),
        crate::tr!(GifCategoryCongrats).into(),
        crate::tr!(GifCategoryThanks).into(),
        crate::tr!(GifCategoryWow).into(),
    ]
}

pub(super) struct GifPicker {
    session: Option<SessionHandle>,
    search: Entity<InputState>,
    key: Entity<InputState>,
    /// What the grid shows the results of; empty is trending.
    query: String,
    gifs: Vec<GifItem>,
    /// The offset of the next page, while GIPHY has one.
    next: Option<u32>,
    /// Bumped by every new search, so a page that comes back for an old one
    /// is dropped instead of mixed in.
    generation: u64,
    loading: bool,
    loaded: bool,
    needs_key: bool,
    status: Option<SharedString>,
    thumbs: HashMap<String, Thumb>,
    queue: VecDeque<String>,
    in_flight: usize,
    hovered: Option<String>,
    drawn: VecDeque<String>,
    scroll: UniformListScrollHandle,
    language: crate::i18n::Language,
    _settle: Option<Task<()>>,
}

impl EventEmitter<GifPicked> for GifPicker {}

impl GifPicker {
    pub fn new(window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(crate::tr!(SearchGiphy)));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.typed(cx),
            InputEvent::PressEnter { .. } => {
                this._settle = None;
                this.search_now(cx);
            }
            _ => {}
        })
        .detach();
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(crate::tr!(GiphyKeyPlaceholder))
        });
        cx.subscribe(&key, |this, _, event: &InputEvent, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.save_key(cx);
            }
        })
        .detach();
        Self {
            session: None,
            search,
            key,
            query: String::new(),
            gifs: Vec::new(),
            next: None,
            generation: 0,
            loading: false,
            loaded: false,
            needs_key: false,
            status: None,
            thumbs: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: 0,
            hovered: None,
            drawn: VecDeque::new(),
            scroll: UniformListScrollHandle::new(),
            language: crate::i18n::Language::current(),
            _settle: None,
        }
    }

    pub fn focus_handle(&self, cx: &gpui::App) -> gpui::FocusHandle {
        if self.needs_key {
            self.key.read(cx).focus_handle(cx)
        } else {
            self.search.read(cx).focus_handle(cx)
        }
    }

    /// The popover opened: the first time, or after a failure, ask for the
    /// page the field describes.
    pub fn opened(&mut self, session: SessionHandle, cx: &mut gpui::Context<Self>) {
        let reconnected = self.session.is_some();
        self.session = Some(session);
        if reconnected {
            self.thumbs
                .retain(|_, thumb| matches!(thumb, Thumb::Ready(_)));
        }
        if !self.loaded || self.gifs.is_empty() {
            self.search_now(cx);
        }
    }

    fn typed(&mut self, cx: &mut gpui::Context<Self>) {
        let text = self.search.read(cx).text().to_string();
        if text.trim() == self.query {
            return;
        }
        self._settle = Some(cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            crate::platform::sleep(SETTLE).await;
            let _ = this.update(cx, |this, cx| this.search_now(cx));
        }));
    }

    /// Ask for the first page of what the field says, dropping what is shown.
    fn search_now(&mut self, cx: &mut gpui::Context<Self>) {
        self.query = self.search.read(cx).text().to_string().trim().to_string();
        self.generation += 1;
        self.gifs.clear();
        self.next = None;
        self.queue.clear();
        self.hovered = None;
        for thumb in self.thumbs.values() {
            if let Thumb::Ready(image) = thumb {
                image.release_decodes();
            }
        }
        // Previews on their way still land, under their own ids; the rest go.
        self.thumbs
            .retain(|_, thumb| matches!(thumb, Thumb::Loading));
        self.drawn.clear();
        self.loading = false;
        self.scroll.scroll_to_item(0, gpui::ScrollStrategy::Top);
        self.fetch_page(0, cx);
    }

    fn more(&mut self, cx: &mut gpui::Context<Self>) {
        if let Some(offset) = self.next
            && !self.loading
        {
            self.fetch_page(offset, cx);
        }
    }

    fn fetch_page(&mut self, offset: u32, cx: &mut gpui::Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        self.loading = true;
        self.status = None;
        let generation = self.generation;
        let answer = session.gifs(GifRequest::Search {
            query: self.query.clone(),
            offset,
        });
        cx.notify();
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match answer {
                    Ok(Ok(GifReply::Page { gifs, next })) => {
                        this.loaded = true;
                        this.needs_key = false;
                        this.next = next;
                        let known: std::collections::HashSet<String> =
                            this.gifs.iter().map(|gif| gif.id.clone()).collect();
                        this.gifs
                            .extend(gifs.into_iter().filter(|gif| !known.contains(&gif.id)));
                    }
                    Ok(Ok(GifReply::NeedsKey)) => {
                        this.needs_key = true;
                    }
                    Ok(Ok(_)) => {}
                    Ok(Err(detail)) => {
                        log::warn!("gifs: {detail}");
                        this.status = Some(format!("{}: {detail}", crate::tr!(GifsFailed)).into());
                    }
                    Err(_) => {
                        this.status = Some(crate::tr!(GifsFailed).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn save_key(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(session) = &self.session else {
            return;
        };
        let key = self.key.read(cx).text().to_string().trim().to_string();
        if key.is_empty() {
            return;
        }
        let answer = session.gifs(GifRequest::SetKey { key });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let answer = answer.await;
            let _ = this.update(cx, |this, cx| {
                match answer {
                    Ok(Ok(_)) => {
                        this.status = None;
                        this.search_now(cx);
                    }
                    Ok(Err(detail)) => this.status = Some(detail.into()),
                    Err(_) => this.status = Some(crate::tr!(GifsFailed).into()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_category(
        &mut self,
        label: SharedString,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        let text = if label.as_ref() == crate::tr!(GifTrending) {
            String::new()
        } else {
            label.to_string()
        };
        self.search
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self._settle = None;
        self.search_now(cx);
    }

    fn note_drawn(&mut self, id: &str) {
        if let Some(at) = self.drawn.iter().position(|seen| seen == id) {
            self.drawn.remove(at);
        }
        self.drawn.push_back(id.to_string());
        while self.drawn.len() > DRAWN_KEPT {
            if let Some(gone) = self.drawn.pop_front()
                && let Some(Thumb::Ready(image)) = self.thumbs.get(&gone)
            {
                image.release_decodes();
            }
        }
    }

    fn hover(&mut self, id: &str, hovered: bool, cx: &mut gpui::Context<Self>) {
        if hovered {
            self.hovered = Some(id.to_string());
        } else if self.hovered.as_deref() == Some(id) {
            self.hovered = None;
            if let Some(Thumb::Ready(image)) = self.thumbs.get(id) {
                image.release(crate::retained_image::Fit::Fitted);
            }
        } else {
            return;
        }
        cx.notify();
    }

    fn want(&mut self, id: &str) {
        if !self.thumbs.contains_key(id) {
            self.thumbs.insert(id.to_string(), Thumb::Queued);
            self.queue.push_back(id.to_string());
        }
    }

    fn pump(&mut self, cx: &mut gpui::Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        while self.in_flight < IN_FLIGHT
            && let Some(id) = self.queue.pop_front()
        {
            let Some(gif) = self.gifs.iter().find(|gif| gif.id == id) else {
                self.thumbs.remove(&id);
                continue;
            };
            self.thumbs.insert(id.clone(), Thumb::Loading);
            self.in_flight += 1;
            let answer = session.gifs(GifRequest::Preview {
                url: gif.preview_url.clone(),
            });
            cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
                let answer = answer.await;
                let _ = this.update(cx, |this, cx| {
                    this.in_flight = this.in_flight.saturating_sub(1);
                    let thumb = match answer {
                        Ok(Ok(GifReply::Preview { bytes })) if !bytes.is_empty() => {
                            Thumb::Ready(RetainedImage::new(gpui::Image::from_bytes(
                                gpui::ImageFormat::Webp,
                                bytes,
                            )))
                        }
                        Ok(Err(detail)) => {
                            log::info!("gif preview unavailable: {detail}");
                            Thumb::Failed
                        }
                        _ => Thumb::Failed,
                    };
                    this.thumbs.insert(id, thumb);
                    this.pump(cx);
                    cx.notify();
                });
            })
            .detach();
        }
    }

    fn thumb_element(
        &self,
        id: &str,
        width: gpui::Pixels,
        height: gpui::Pixels,
    ) -> gpui::AnyElement {
        match self.thumbs.get(id) {
            Some(Thumb::Ready(image)) => img(if self.hovered.as_deref() == Some(id) {
                image.fitted(true)
            } else {
                image.still(true)
            })
            .w(width)
            .h(height)
            .object_fit(gpui::ObjectFit::Cover)
            .into_any_element(),
            _ => div().w(width).h(height).into_any_element(),
        }
    }

    fn render_key_prompt(
        &self,
        metrics: crate::theme::Metrics,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement + use<> {
        div()
            .flex()
            .flex_col()
            .gap(metrics.space_sm())
            .py(metrics.space_md())
            .child(
                div()
                    .text_size(metrics.text_small())
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::tr!(GiphyKeyNeeded)),
            )
            .child(Input::new(&self.key).w_full().mask_toggle())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(metrics.space_xs())
                    .child(
                        Button::new("giphy-key-site")
                            .ghost()
                            .small()
                            .label(crate::tr!(GiphyKeyOpenSite))
                            .on_click(|_, _, cx| cx.open_url(GIPHY_DEVELOPERS)),
                    )
                    .child(
                        Button::new("giphy-key-save")
                            .primary()
                            .small()
                            .label(crate::tr!(GiphyKeySave))
                            .on_click(cx.listener(|this, _, _, cx| this.save_key(cx))),
                    ),
            )
    }
}

impl Render for GifPicker {
    fn render(
        &mut self,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let language = crate::i18n::Language::current();
        if self.language != language {
            self.language = language;
            self.search.update(cx, |input, cx| {
                input.set_placeholder(crate::tr!(SearchGiphy), window, cx)
            });
            self.key.update(cx, |input, cx| {
                input.set_placeholder(crate::tr!(GiphyKeyPlaceholder), window, cx)
            });
        }
        let metrics = cx.product().metrics;
        let cell_w = metrics.icon_button() * 4.;
        let cell_h = metrics.icon_button() * 3.;
        let rows = self.gifs.len().div_ceil(COLUMNS);
        let current = if self.query.is_empty() {
            SharedString::from(crate::tr!(GifTrending))
        } else {
            SharedString::from(self.query.clone())
        };

        let chips = div().flex().flex_wrap().gap(metrics.space_xxs()).children(
            categories().into_iter().enumerate().map(|(ix, label)| {
                let chosen = label == current;
                let pick = label.clone();
                let chip = Button::new(("gif-category", ix))
                    .xsmall()
                    .label(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick_category(pick.clone(), window, cx);
                    }));
                if chosen { chip.primary() } else { chip.ghost() }
            }),
        );

        div()
            .flex()
            .flex_col()
            .gap(metrics.space_sm())
            .w(cell_w * COLUMNS as f32)
            .text_color(cx.theme().popover_foreground)
            .child(crate::tr!(Gifs))
            .when(!self.needs_key, |panel| {
                panel.child(Input::new(&self.search).w_full()).child(chips)
            })
            .children(self.status.clone().map(|status| {
                div()
                    .text_size(metrics.text_micro())
                    .text_color(cx.theme().muted_foreground)
                    .child(status)
            }))
            .when(self.needs_key, |panel| {
                panel.child(self.render_key_prompt(metrics, cx))
            })
            .when(!self.needs_key, |panel| {
                panel
                    .when(self.gifs.is_empty(), |panel| {
                        panel.child(
                            div()
                                .h(cell_h * VISIBLE_ROWS)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(metrics.text_micro())
                                .text_color(cx.theme().muted_foreground)
                                .child(if self.loading || !self.loaded {
                                    crate::tr!(GifsLoading)
                                } else {
                                    crate::tr!(NoGifsFound)
                                }),
                        )
                    })
                    .when(!self.gifs.is_empty(), |panel| {
                        panel.child(
                            uniform_list(
                                "gif-grid",
                                rows,
                                cx.processor(
                                    move |this, range: std::ops::Range<usize>, _window, cx| {
                                        if range.end + 2 >= rows {
                                            this.more(cx);
                                        }
                                        let cells: Vec<Vec<GifItem>> = range
                                            .map(|row| {
                                                this.gifs
                                                    .iter()
                                                    .skip(row * COLUMNS)
                                                    .take(COLUMNS)
                                                    .cloned()
                                                    .collect()
                                            })
                                            .collect();
                                        for gif in cells.iter().flatten() {
                                            this.want(&gif.id);
                                            this.note_drawn(&gif.id);
                                        }
                                        this.pump(cx);
                                        cells
                                            .into_iter()
                                            .map(|row| {
                                                div().flex().h(cell_h).children(row.into_iter().map(
                                                    |gif| {
                                                        let pointed = gif.id.clone();
                                                        let title: SharedString =
                                                            gif.title.clone().into();
                                                        let thumb = this.thumb_element(
                                                            &gif.id,
                                                            cell_w - metrics.space_xs(),
                                                            cell_h - metrics.space_xs(),
                                                        );
                                                        div()
                                                            .id(SharedString::from(format!(
                                                                "gif-{}",
                                                                gif.id
                                                            )))
                                                            .w(cell_w)
                                                            .h(cell_h)
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .cursor_pointer()
                                                            .child(
                                                                div()
                                                                    .overflow_hidden()
                                                                    .rounded(metrics.radius_sm())
                                                                    .bg(cx.theme().muted)
                                                                    .child(thumb),
                                                            )
                                                            .when(!title.is_empty(), |cell| {
                                                                cell.tooltip(move |window, cx| {
                                                                    gpui_component::tooltip::Tooltip::new(
                                                                        title.clone(),
                                                                    )
                                                                    .build(window, cx)
                                                                })
                                                            })
                                                            .on_hover(cx.listener(
                                                                move |this, hovered: &bool, _, cx| {
                                                                    this.hover(&pointed, *hovered, cx);
                                                                },
                                                            ))
                                                            .on_click(cx.listener(
                                                                move |_, _, _, cx| {
                                                                    cx.emit(GifPicked(gif.clone()));
                                                                },
                                                            ))
                                                    },
                                                ))
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .h(cell_h * VISIBLE_ROWS)
                            .track_scroll(&self.scroll),
                        )
                    })
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .text_size(metrics.text_micro())
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::tr!(PoweredByGiphy)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::categories;

    #[test]
    fn trending_is_the_first_quick_search() {
        let all = categories();
        assert_eq!(
            all.first().map(|s| s.to_string()),
            Some(crate::tr!(GifTrending).to_string())
        );
    }
}
