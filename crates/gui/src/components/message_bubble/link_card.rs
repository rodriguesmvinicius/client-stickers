//! The card a link's preview draws above the text that carries it.
//!
//! Everything on it came in the message: the sending phone fetched the page
//! and put its title, description and a small picture into the text message,
//! so drawing it asks nothing of the linked site. Pressing it opens the link,
//! as pressing the link itself does.

use gpui::{App, IntoElement, ParentElement, SharedString, Styled, StyledImage as _, div, img};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use oxidezap_core::LinkPreview;

use crate::components::parts;
use crate::retained_image::RetainedImage;
use crate::theme::Metrics;

pub fn render_link_card(
    preview: &LinkPreview,
    message_id: &str,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let url = preview.url.clone();
    let site: SharedString = preview.site().to_string().into();
    let title: Option<SharedString> =
        (!preview.title.is_empty()).then(|| preview.title.clone().into());
    let description: Option<SharedString> =
        (!preview.description.is_empty()).then(|| preview.description.clone().into());
    let side = metrics.icon_button() * 2.25;
    // A few kilobytes of JPEG, decoded at the size it is drawn. Built per
    // paint: GPUI names an image by its content, so the decode is found again
    // rather than redone.
    let thumbnail = preview.thumbnail.as_ref().map(|bytes| {
        RetainedImage::new(gpui::Image::from_bytes(
            gpui::ImageFormat::Jpeg,
            bytes.as_ref().clone(),
        ))
    });

    Button::new(SharedString::from(format!("link-card-{message_id}")))
        .ghost()
        .w_full()
        .h_auto()
        .p_0()
        .mb(metrics.space_sm())
        .rounded(metrics.radius_sm())
        .tooltip(SharedString::from(preview.url.clone()))
        .cursor_pointer()
        .child(
            div()
                .w_full()
                .flex()
                .overflow_hidden()
                .rounded(metrics.radius_sm())
                .bg(cx.theme().muted.opacity(0.7))
                .children(thumbnail.map(|image| {
                    img(image.fitted(false))
                        .size(side)
                        .flex_shrink_0()
                        .object_fit(gpui::ObjectFit::Cover)
                }))
                .child(
                    parts::detail_stack()
                        .gap(metrics.space_xxs())
                        .px(metrics.space_md())
                        .py(metrics.space_sm())
                        .children(title.map(|title| {
                            div()
                                .text_size(metrics.text_small())
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .line_clamp(2)
                                .child(title)
                        }))
                        .children(description.map(|description| {
                            div()
                                .text_size(metrics.text_micro())
                                .text_color(cx.theme().muted_foreground)
                                .line_clamp(2)
                                .child(description)
                        }))
                        .child(
                            parts::one_line()
                                .text_size(metrics.text_micro())
                                .text_color(cx.theme().muted_foreground.opacity(0.8))
                                .child(site),
                        ),
                ),
        )
        .on_click(move |_, _window, cx| cx.open_url(&url))
}
