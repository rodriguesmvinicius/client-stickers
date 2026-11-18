//! Choosing where a message is forwarded: the chats this window knows,
//! found by name, a few ticked, sent with one button.
//!
//! The list is the window's own chat list as it was when the dialog opened.
//! A chat that arrives while it is up is not missed for long — the dialog is
//! a moment's work — and a list that reshuffled under the pointer would tick
//! the wrong row.

use gpui::{
    Entity, EventEmitter, FocusHandle, Focusable as _, SharedString, UniformListScrollHandle, div,
    prelude::*, uniform_list,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Selectable as _,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
};

use crate::components::Avatar;
use crate::theme::ActiveProductTheme as _;

/// WhatsApp's own limit on one forward, which keeps a forward a forward and
/// not a broadcast.
pub(crate) const MAX_TARGETS: usize = 5;
/// How many rows of the list are on screen.
const VISIBLE_ROWS: f32 = 6.5;

/// One chat the message can go to.
#[derive(Debug, Clone)]
pub(crate) struct ForwardTarget {
    pub jid: String,
    pub name: String,
    pub is_group: bool,
    pub avatar: Option<String>,
}

/// What the dialog decided.
pub(crate) enum ForwardEvent {
    Send(Vec<String>),
    Cancel,
}

pub(crate) struct ForwardPicker {
    search: Entity<InputState>,
    targets: Vec<ForwardTarget>,
    /// Indices into `targets`, in order, that answer the search.
    matches: Vec<usize>,
    /// The chosen chats, in the order they were ticked.
    chosen: Vec<String>,
    preview: SharedString,
    scroll: UniformListScrollHandle,
}

impl EventEmitter<ForwardEvent> for ForwardPicker {}

impl ForwardPicker {
    pub fn new(
        targets: Vec<ForwardTarget>,
        preview: String,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder(crate::tr!(SearchChatsToForward)));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.filter(cx),
            // Enter ticks the best match, so a forward to one person is
            // typing a few letters and Enter twice.
            InputEvent::PressEnter { .. } => {
                let first = this
                    .matches
                    .first()
                    .and_then(|ix| this.targets.get(*ix))
                    .map(|target| target.jid.clone());
                match first {
                    Some(jid) if !this.chosen.contains(&jid) => this.toggle(&jid, cx),
                    _ if !this.chosen.is_empty() => this.send(cx),
                    _ => {}
                }
            }
            _ => {}
        })
        .detach();
        let matches = (0..targets.len()).collect();
        Self {
            search,
            targets,
            matches,
            chosen: Vec::new(),
            preview: preview.into(),
            scroll: UniformListScrollHandle::new(),
        }
    }

    pub fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.search.read(cx).focus_handle(cx)
    }

    fn filter(&mut self, cx: &mut gpui::Context<Self>) {
        let query = self.search.read(cx).text().to_string();
        self.matches = matching(&self.targets, &query);
        cx.notify();
    }

    fn toggle(&mut self, jid: &str, cx: &mut gpui::Context<Self>) {
        if let Some(at) = self.chosen.iter().position(|chosen| chosen == jid) {
            self.chosen.remove(at);
        } else if self.chosen.len() < MAX_TARGETS {
            self.chosen.push(jid.to_string());
        }
        cx.notify();
    }

    fn send(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.chosen.is_empty() {
            cx.emit(ForwardEvent::Send(self.chosen.clone()));
        }
    }

    fn name_of<'a>(&'a self, jid: &'a str) -> &'a str {
        self.targets
            .iter()
            .find(|target| target.jid == jid)
            .map_or(jid, |target| target.name.as_str())
    }
}

/// The targets whose name holds every word of `query`, ignoring case and
/// accents, in their list order.
fn matching(targets: &[ForwardTarget], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(fold).collect();
    targets
        .iter()
        .enumerate()
        .filter(|(_, target)| {
            let name = fold(&target.name);
            words.iter().all(|word| name.contains(word.as_str()))
        })
        .map(|(ix, _)| ix)
        .collect()
}

/// Lower case with the common Latin accents taken off, so "joao" finds João.
fn fold(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            other => other,
        })
        .collect()
}

impl Render for ForwardPicker {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let metrics = cx.product().metrics;
        let row_height = metrics.avatar_row() + metrics.space_md() * 2.;
        let full = self.chosen.len() >= MAX_TARGETS;
        let chosen_names: Vec<String> = self
            .chosen
            .iter()
            .map(|jid| self.name_of(jid).to_string())
            .collect();
        div()
            .id("forward-modal")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(metrics.space_xxl())
            .bg(crate::components::parts::scrim(cx).opacity(0.92))
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .w_full()
                    .max_w(metrics.bubble_max_width())
                    .p(metrics.space_xl())
                    .rounded(metrics.radius_lg())
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground)
                    .flex()
                    .flex_col()
                    .gap(metrics.space_md())
                    .child(
                        div()
                            .text_size(metrics.text_title())
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(crate::tr!(ForwardMessageTo)),
                    )
                    .child(
                        div()
                            .px(metrics.space_md())
                            .py(metrics.space_sm())
                            .rounded(metrics.radius_sm())
                            .bg(cx.theme().muted)
                            .text_size(metrics.text_small())
                            .text_color(cx.theme().muted_foreground)
                            .line_clamp(2)
                            .child(self.preview.clone()),
                    )
                    .child(Input::new(&self.search).w_full())
                    .child(
                        div()
                            .text_size(metrics.text_micro())
                            .text_color(cx.theme().muted_foreground)
                            .child(if full {
                                format!(
                                    "{}/{MAX_TARGETS} · {}",
                                    self.chosen.len(),
                                    crate::tr!(ForwardLimitReached)
                                )
                            } else {
                                format!("{}/{MAX_TARGETS}", self.chosen.len())
                            }),
                    )
                    .when(self.matches.is_empty(), |panel| {
                        panel.child(
                            div()
                                .text_size(metrics.text_small())
                                .text_color(cx.theme().muted_foreground)
                                .child(crate::tr!(NoChatsMatch)),
                        )
                    })
                    .child(
                        uniform_list(
                            "forward-targets",
                            self.matches.len(),
                            cx.processor(
                                move |this, range: std::ops::Range<usize>, _window, cx| {
                                    let metrics = cx.product().metrics;
                                    range
                                        .filter_map(|ix| {
                                            let target =
                                                this.targets.get(*this.matches.get(ix)?)?;
                                            let chosen = this.chosen.contains(&target.jid);
                                            let disabled =
                                                !chosen && this.chosen.len() >= MAX_TARGETS;
                                            let jid = target.jid.clone();
                                            Some(
                                                Button::new(SharedString::from(format!(
                                                    "forward-to-{}",
                                                    target.jid
                                                )))
                                                .ghost()
                                                .selected(chosen)
                                                .disabled(disabled)
                                                .w_full()
                                                .h(row_height)
                                                .flex()
                                                .items_center()
                                                .justify_start()
                                                .gap(metrics.space_md())
                                                .px(metrics.space_md())
                                                .child(
                                                    Avatar::new(
                                                        target.jid.clone(),
                                                        &target.name,
                                                        metrics.avatar_row(),
                                                    )
                                                    .group(target.is_group)
                                                    .picture(target.avatar.clone(), None)
                                                    .on(cx.theme().background),
                                                )
                                                .child(
                                                    crate::components::parts::one_line()
                                                        .flex_1()
                                                        .text_size(metrics.text_body())
                                                        .child(target.name.clone()),
                                                )
                                                .when(chosen, |row| {
                                                    row.child(
                                                        div()
                                                            .text_color(cx.theme().primary)
                                                            .font_weight(gpui::FontWeight::BOLD)
                                                            .child("✓"),
                                                    )
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.toggle(&jid, cx);
                                                })),
                                            )
                                        })
                                        .collect::<Vec<_>>()
                                },
                            ),
                        )
                        .track_scroll(&self.scroll)
                        .h(row_height * VISIBLE_ROWS),
                    )
                    .when(!chosen_names.is_empty(), |panel| {
                        panel.child(
                            div()
                                .text_size(metrics.text_small())
                                .text_color(cx.theme().muted_foreground)
                                .line_clamp(2)
                                .child(chosen_names.join(", ")),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(metrics.space_md())
                            .child(
                                Button::new("forward-cancel")
                                    .label(crate::tr!(Cancel))
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(ForwardEvent::Cancel);
                                    })),
                            )
                            .child(
                                Button::new("forward-send")
                                    .primary()
                                    .label(crate::tr!(Forward))
                                    .disabled(self.chosen.is_empty())
                                    .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(name: &str) -> ForwardTarget {
        ForwardTarget {
            jid: format!("{name}@example.invalid"),
            name: name.to_string(),
            is_group: false,
            avatar: None,
        }
    }

    #[test]
    fn every_word_has_to_be_in_the_name_whatever_the_accents() {
        let targets = [
            target("João Silva"),
            target("Maria"),
            target("Grupo da Família"),
        ];
        assert_eq!(matching(&targets, ""), vec![0, 1, 2]);
        assert_eq!(matching(&targets, "joao"), vec![0]);
        assert_eq!(matching(&targets, "FAMILIA grupo"), vec![2]);
        assert!(matching(&targets, "pedro").is_empty());
    }
}
