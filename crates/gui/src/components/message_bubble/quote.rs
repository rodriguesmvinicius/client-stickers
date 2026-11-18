//! The quote bar inside a reply.

use gpui::{App, Entity, IntoElement, ParentElement, SharedString, Styled, div};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use oxidezap_core::{QuotedMessage, plain_message_text};

use crate::app::WhatsAppApp;
use crate::components::parts;
use crate::theme::{ActiveProductTheme as _, Metrics};

/// The message being replied to, drawn inside the replying bubble.
///
/// The bar takes the quoted author's colour — the same hue they carry in the
/// member list and their own bubbles — so who is being answered is legible
/// before the name is read.
pub fn render_quote(
    quoted: &QuotedMessage,
    sender_is_self: bool,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let target = quoted.message_id.clone();
    let hue = cx.product().speaker(&quoted.sender);
    let name: SharedString = crate::i18n::Language::current()
        .quote_author(quoted, sender_is_self)
        .to_owned()
        .into();
    // One line with nowhere to put emphasis: the markers come out, the same
    // way they do in a chat row's preview.
    let summary: SharedString =
        plain_message_text(crate::i18n::Language::current().quoted_summary(quoted))
            .into_owned()
            .into();

    // Jumping to the original is a command, so it is a `Button` — that is
    // what carries focus and keyboard activation. Styled flat and full width,
    // because inside a bubble it has to read as the quote it is, not as a
    // control sitting on top of one.
    //
    // The panel is what separates the quote from the reply under it: a faint
    // wash of the author's hue behind it, the bar at its edge, and a gap
    // below. Without them the quoted text and the reply were one grey block.
    Button::new(SharedString::from(format!("quote-{target}")))
        .ghost()
        .w_full()
        .h_auto()
        .p_0()
        .mb(metrics.space_sm())
        .rounded(metrics.radius_sm())
        .child(
            div()
                .w_full()
                .flex()
                .overflow_hidden()
                .rounded(metrics.radius_sm())
                .bg(hue.opacity(0.12))
                .child(
                    div()
                        .w(metrics.selection_bar_width() * 1.5)
                        .flex_shrink_0()
                        .bg(hue),
                )
                .child(
                    parts::detail_stack()
                        .gap(metrics.space_xxs())
                        .px(metrics.space_md())
                        .py(metrics.space_xs())
                        .child(
                            div()
                                .text_size(metrics.text_small())
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(hue)
                                .child(name),
                        )
                        .child(
                            parts::one_line()
                                .text_size(metrics.text_small())
                                .italic()
                                .text_color(cx.theme().muted_foreground.opacity(0.85))
                                .child(summary),
                        ),
                ),
        )
        .cursor_pointer()
        .on_click(move |_, _window, cx| {
            entity.update(cx, |app, cx| app.jump_to_message(&target, cx));
        })
}
