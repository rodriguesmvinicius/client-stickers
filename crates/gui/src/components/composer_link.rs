//! The card for a link being typed: found in the text, fetched by the daemon
//! once typing pauses on it, shown above the field with a way to drop it, and
//! sent inside the message when it is still wanted.
//!
//! The composer owns the state because it owns the text; asking the daemon
//! is the app's, which holds the connection, so the composer says which link
//! it wants and is told what came back.

use gpui::StyledImage as _;
use gpui::{App, Entity, IntoElement, ParentElement, SharedString, Styled, Task, div, img};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::{ActiveTheme as _, IconName, Sizable as _};
use oxidezap_core::LinkPreview;

use super::InputAreaView;
use crate::components::parts;
use crate::retained_image::RetainedImage;
use crate::theme::Metrics;

/// How long typing has to rest on a link before it is fetched: long enough
/// that a link being typed out character by character is not fetched at
/// every one, short enough that a pasted link has its card at once.
pub(super) const SETTLE: std::time::Duration = std::time::Duration::from_millis(600);

/// Where the card for the link in the text stands.
#[derive(Default)]
pub(super) enum ComposerLink {
    /// No link in the text.
    #[default]
    None,
    /// A link, waiting for typing to settle on it.
    Settling { link: String, _timer: Task<()> },
    /// Asked for; the daemon is reading the page.
    Loading { link: String },
    Ready {
        preview: LinkPreview,
        image: Option<RetainedImage>,
    },
    /// Read, and the page had nothing worth a card — or the person closed
    /// it. Either way this link is not asked about again while it stays.
    Settled { link: String },
}

impl ComposerLink {
    /// The link this state is about, as it appears in the text.
    pub(super) fn link(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Settling { link, .. } | Self::Loading { link } | Self::Settled { link } => {
                Some(link)
            }
            Self::Ready { preview, .. } => Some(&preview.url),
        }
    }

    /// The card to send with `text`, if one is up and its link is still in
    /// what is being sent.
    pub(super) fn card_for(&self, text: &str) -> Option<LinkPreview> {
        match self {
            Self::Ready { preview, .. } if text.contains(preview.url.as_str()) => {
                Some(preview.clone())
            }
            _ => None,
        }
    }
}

/// The first link in `text`, as it is written there: what the card is
/// matched against when the message goes, so it is never rewritten.
pub(super) fn first_link(text: &str) -> Option<String> {
    text.split_whitespace().find_map(|word| {
        let word = word.trim_start_matches(['(', '<', '"', '\'', '«', '[']);
        let word = word.trim_end_matches([
            '.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '»',
        ]);
        let lower = word.to_ascii_lowercase();
        let has_host = |rest: &str| {
            rest.split(['/', '?', '#'])
                .next()
                .is_some_and(|host| host.contains('.'))
        };
        if let Some(rest) = lower
            .strip_prefix("https://")
            .or_else(|| lower.strip_prefix("http://"))
        {
            has_host(rest).then(|| word.to_string())
        } else if lower.starts_with("www.") && has_host(&lower) {
            Some(word.to_string())
        } else {
            None
        }
    })
}

/// What the daemon is asked to read for `link`: the link itself, or with a
/// scheme in front when it was typed without one.
pub(crate) fn fetchable(link: &str) -> String {
    let lower = link.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        link.to_string()
    } else {
        format!("https://{link}")
    }
}

/// The card above the field, or the line saying one is on its way.
pub(super) fn render(
    link: &ComposerLink,
    entity: Entity<InputAreaView>,
    metrics: Metrics,
    cx: &App,
) -> Option<gpui::AnyElement> {
    match link {
        ComposerLink::Loading { .. } => Some(
            div()
                .pb(metrics.space_sm())
                .text_size(metrics.text_micro())
                .text_color(cx.theme().muted_foreground)
                .child(crate::tr!(LinkPreviewLoading))
                .into_any_element(),
        ),
        ComposerLink::Ready { preview, image } => {
            let side = metrics.icon_button() * 2.5;
            let title: Option<SharedString> =
                (!preview.title.is_empty()).then(|| preview.title.clone().into());
            let description: Option<SharedString> =
                (!preview.description.is_empty()).then(|| preview.description.clone().into());
            let site: SharedString = preview.site().to_string().into();
            Some(
                div()
                    .mb(metrics.space_md())
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .rounded(metrics.radius_md())
                    .bg(cx.theme().muted)
                    .children(image.clone().map(|image| {
                        img(image.fitted(false))
                            .size(side)
                            .flex_shrink_0()
                            .object_fit(gpui::ObjectFit::Cover)
                    }))
                    .child(
                        parts::detail_stack()
                            .gap(metrics.space_xxs())
                            .px(metrics.space_lg())
                            .py(metrics.space_sm())
                            .children(title.map(|title| {
                                div()
                                    .text_size(metrics.text_small())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .line_clamp(1)
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
                    )
                    .child(
                        div().pr(metrics.space_sm()).child(
                            Button::new("drop-link-preview")
                                .icon(IconName::Close)
                                .ghost()
                                .xsmall()
                                .tooltip(crate::tr!(RemoveLinkPreview))
                                .cursor_pointer()
                                .on_click(move |_, _window, cx| {
                                    entity.update(cx, |view, cx| view.drop_link_preview(cx));
                                }),
                        ),
                    )
                    .into_any_element(),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_link_is_found_as_written() {
        assert_eq!(
            first_link("olha https://x.com/a/status/1?s=20 que coisa").as_deref(),
            Some("https://x.com/a/status/1?s=20")
        );
        assert_eq!(
            first_link("(veja: https://youtu.be/abc).").as_deref(),
            Some("https://youtu.be/abc")
        );
        assert_eq!(
            first_link("www.example.invalid/pagina!").as_deref(),
            Some("www.example.invalid/pagina")
        );
        assert_eq!(first_link("sem link aqui"), None);
        assert_eq!(first_link("https://localhost sem ponto"), None);
        assert_eq!(first_link("ftp://example.invalid"), None);
    }

    #[test]
    fn a_link_typed_without_a_scheme_is_fetched_over_https() {
        assert_eq!(
            fetchable("www.example.invalid"),
            "https://www.example.invalid"
        );
        assert_eq!(
            fetchable("http://example.invalid"),
            "http://example.invalid"
        );
    }

    #[test]
    fn the_card_goes_only_with_its_own_link() {
        let state = ComposerLink::Ready {
            preview: LinkPreview {
                url: "https://example.invalid/a".into(),
                title: "A".into(),
                description: String::new(),
                thumbnail: None,
            },
            image: None,
        };
        assert!(state.card_for("veja https://example.invalid/a").is_some());
        assert!(state.card_for("apagou o link").is_none());
        assert!(
            ComposerLink::None
                .card_for("https://example.invalid/a")
                .is_none()
        );
    }
}
