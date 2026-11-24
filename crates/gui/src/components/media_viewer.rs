//! A picture at the size of the window.
//!
//! Covers the app rather than floating over it: looking at a photo is a mode,
//! not a panel, and a scrim that leaves the conversation half-readable
//! underneath invites clicking through to it by accident.

use crate::retained_image::RetainedImage;
use std::sync::Arc;

use gpui::StyledImage as _;
use gpui::{
    App, Entity, InteractiveElement, IntoElement, ParentElement, RenderImage, SharedString,
    StatefulInteractiveElement, Styled, div, img, prelude::FluentBuilder as _,
};
use gpui_component::ActiveTheme as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::spinner::Spinner;
use gpui_component::{Disableable as _, FocusTrapElement as _, Icon, IconName, Sizable as _};

use crate::app::{MediaViewer, VIEWER_CONTEXT, ViewerNext, ViewerPrev, WhatsAppApp};
use crate::components::ProductIcon;
use crate::components::parts;
use crate::theme::Metrics;
use crate::utils::format_list_time;
use crate::video::VideoPlayerState;

/// How much of a caption the viewer shows before it scrolls, in multiples of
/// its own type size.
///
/// A caption is the sender's text and has no length limit, so this is what
/// keeps the picture from being pushed off its own screen. Roughly six lines
/// at the usual line height, which is most captions whole.
pub(crate) const CAPTION_LINES: f32 = 9.0;
use oxidezap_core::{ChatMessage, MediaType};

/// What the viewer needs that only the app can resolve.
pub struct ViewerProps {
    pub message: ChatMessage,
    /// The decoded image, when the app's cache has one. Video shows its
    /// current frame instead.
    pub image: Option<RetainedImage>,
    pub frame: Option<Arc<RenderImage>>,
    /// Where the video on screen is, when it is one.
    pub video: Option<ViewerVideo>,
    pub is_loading: bool,
    pub author: SharedString,
}

/// The playback of a video in the viewer, as the app's player reports it.
#[derive(Debug, Clone, Copy)]
pub struct ViewerVideo {
    pub state: VideoPlayerState,
    pub elapsed_secs: u32,
    /// From the loaded clip, or the message's own claim before it is.
    pub length_secs: Option<u32>,
}

pub fn render_media_viewer(
    viewer: &MediaViewer,
    props: ViewerProps,
    entity: Entity<WhatsAppApp>,
    focus_handle: &gpui::FocusHandle,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let scrim_entity = entity.clone();
    let key_prev_entity = entity.clone();
    let key_next_entity = entity.clone();
    let prev_entity = entity.clone();
    let next_entity = entity.clone();
    let close_entity = entity.clone();
    let video_entity = entity.clone();
    let controls_entity = entity.clone();
    let copy_entity = entity.clone();
    let save_entity = entity;
    let message_id = props.message.id.clone();
    let copy_id = message_id.clone();
    // A picture whose own bytes are here: a video, or one still waiting for
    // its file, has nothing to copy.
    let can_copy = props.video.is_none()
        && props.message.media.as_ref().is_some_and(|media| {
            media.media_type == MediaType::Image && !media.data.is_empty() && !media.data_is_preview
        });
    let video = props.video;
    let can_step = viewer.can_step();
    let position: Option<SharedString> = viewer.position().map(Into::into);
    let is_video = props
        .message
        .media
        .as_ref()
        .is_some_and(|media| media.media_type == MediaType::Video);
    let caption: Option<SharedString> = props
        .message
        .media
        .as_ref()
        .and_then(|media| media.caption.clone())
        .filter(|caption| !caption.is_empty())
        .or_else(|| Some(props.message.content.clone()).filter(|content| !content.is_empty()))
        .map(Into::into);
    let when: SharedString = format_list_time(&props.message.timestamp).into();

    div()
        .id("media-viewer")
        .key_context(VIEWER_CONTEXT)
        .track_focus(focus_handle)
        // The viewer is modal, so it has to actually *be* a lid. It covers
        // the window, but covering is only paint: a wheel event over the
        // scrim still reached the timeline underneath and scrolled the
        // conversation nobody could see. Swallowing it here is what makes the
        // picture the only thing on screen that responds.
        .on_scroll(|_, _window, cx| cx.stop_propagation())
        // Mouse-up is where GPUI synthesizes click handlers. Stop both halves
        // of the pointer sequence at the modal, after a child control has had
        // its own chance to act, and occlude overlapping hitboxes behind it.
        .on_mouse_down(gpui::MouseButton::Left, |_, _window, cx| {
            cx.stop_propagation();
        })
        .on_mouse_up(gpui::MouseButton::Left, |_, _window, cx| {
            cx.stop_propagation();
        })
        .on_click(|_, _window, cx| cx.stop_propagation())
        .occlude()
        .on_action(move |_: &ViewerPrev, _window, cx| {
            key_prev_entity.update(cx, |app, cx| app.step_media_viewer(false, cx));
        })
        .on_action(move |_: &ViewerNext, _window, cx| {
            key_next_entity.update(cx, |app, cx| app.step_media_viewer(true, cx));
        })
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        // Nearly opaque rather than a light wash: the point is that nothing
        // else is competing with the picture.
        .bg(parts::scrim(cx).opacity(0.92))
        .child(render_bar(
            props.author,
            when,
            position,
            // The bytes, not the picture. `save_media` writes `media.data`
            // and never asks the decoder anything, so gating on a decoded
            // image refused to save exactly the file worth saving elsewhere:
            // one this build cannot display but another program can.
            props
                .message
                .media
                .as_ref()
                .is_some_and(|media| !media.data.is_empty() && !media.data_is_preview),
            close_entity,
            save_entity,
            message_id,
            metrics,
            cx,
        ))
        .child(
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_between()
                .gap(metrics.space_lg())
                .px(metrics.space_xl())
                .child(step_button(
                    "viewer-prev",
                    IconName::ChevronLeft,
                    crate::tr!(Previous),
                    can_step,
                    move |cx| {
                        prev_entity.update(cx, |app, cx| app.step_media_viewer(false, cx));
                    },
                ))
                .child(
                    // Clicking the empty space around the picture closes the
                    // viewer, the way every picture viewer behaves; clicking
                    // the picture itself does not, so a mis-aimed drag on it
                    // is not a dismissal.
                    div()
                        .id("viewer-scrim")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_click(move |_, _window, cx| {
                            scrim_entity.update(cx, |app, cx| {
                                app.close_media_viewer(cx);
                            });
                        })
                        .child(
                            // The picture swallows its own clicks. Without
                            // this they bubble to the scrim and dismiss the
                            // viewer, so a mis-aimed drag on the photo closes
                            // the thing being looked at.
                            with_copy_menu(
                                div()
                                    .id("viewer-frame")
                                    .flex()
                                    .max_w_full()
                                    .max_h_full()
                                    .on_click(|_, _window, cx| cx.stop_propagation())
                                    .child(match video {
                                        Some(video) => render_video_stage(
                                            props.image,
                                            props.frame,
                                            video,
                                            video_entity,
                                            metrics,
                                            cx,
                                        )
                                        .into_any_element(),
                                        None => render_frame(
                                            props.image,
                                            props.frame,
                                            is_video,
                                            props.is_loading,
                                            metrics,
                                            cx,
                                        )
                                        .into_any_element(),
                                    }),
                                can_copy,
                                copy_entity,
                                copy_id,
                            ),
                        ),
                )
                .child(step_button(
                    "viewer-next",
                    IconName::ChevronRight,
                    crate::tr!(Next),
                    can_step,
                    move |cx| {
                        next_entity.update(cx, |app, cx| app.step_media_viewer(true, cx));
                    },
                )),
        )
        .children(video.map(|video| render_video_controls(video, controls_entity, metrics, cx)))
        .children(caption.map(|caption| {
            // Bounded, and scrolled past that. A caption is the sender's text
            // with no length limit, and a band that refuses to shrink pushes
            // the picture's own `flex_1 min_h_0` pane to zero: the photo
            // vanishes, the caption fills the screen, and what does not fit
            // is cut off the bottom with no way to reach it.
            div()
                .id("viewer-caption")
                .flex_shrink_0()
                .max_h(metrics.text_secondary() * CAPTION_LINES)
                .overflow_y_scroll()
                .px(metrics.space_xxxl())
                .py(metrics.space_xl())
                .flex()
                .justify_center()
                .child(
                    div()
                        .max_w(metrics.reading_width())
                        .text_size(metrics.text_secondary())
                        .text_color(parts::on_scrim(cx))
                        .child(caption),
                )
        }))
        // And the same for the keyboard: Tab inside a modal cycles its own
        // controls rather than walking into the chat list behind it.
        .focus_trap("media-viewer-trap", focus_handle)
}

/// Who sent it, when, and the two things to do with it.
#[allow(clippy::too_many_arguments)]
fn render_bar(
    author: SharedString,
    when: SharedString,
    position: Option<SharedString>,
    // Whether the viewer decoded anything, which is whether there is a file
    // to save.
    can_save: bool,
    close_entity: Entity<WhatsAppApp>,
    save_entity: Entity<WhatsAppApp>,
    message_id: String,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    // The theme's own inks are the wrong answer here: `background` is the
    // *deepest* surface in a dark preset, which is near-black text on a
    // near-black scrim, and in the light preset it is white on white. The
    // viewer's ground is its own pair of tokens for exactly that reason.
    let on_scrim = parts::on_scrim(cx);

    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .gap(metrics.space_lg())
        .px(metrics.space_xxl())
        .py(metrics.space_lg())
        .child(
            parts::detail_stack()
                .child(
                    parts::one_line()
                        .text_size(metrics.text_secondary())
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(on_scrim)
                        .child(author),
                )
                .child(
                    div()
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_size(metrics.text_meta())
                        .text_color(on_scrim.opacity(0.7))
                        .child(when),
                ),
        )
        .children(position.map(|position| {
            div()
                .flex_shrink_0()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(metrics.text_meta())
                .text_color(on_scrim.opacity(0.7))
                .child(position)
        }))
        .child(
            Button::new("viewer-save")
                .icon(Icon::new(IconName::ArrowDown))
                .ghost()
                .disabled(!can_save)
                .tooltip(if can_save {
                    crate::tr!(SaveToDownloads)
                } else {
                    crate::tr!(NothingToSaveThisFileCouldNotBeRead)
                })
                .when(can_save, |button| button.cursor_pointer())
                .on_click(move |_, _window, cx| {
                    save_entity.update(cx, |app, cx| app.save_media(&message_id, cx));
                }),
        )
        .child(
            Button::new("viewer-close")
                .icon(Icon::new(IconName::Close))
                .ghost()
                .tooltip(crate::tr!(Close))
                .cursor_pointer()
                .on_click(move |_, _window, cx| {
                    close_entity.update(cx, |app, cx| {
                        app.close_media_viewer(cx);
                    });
                }),
        )
}

/// The picture frame, with "Copy image" on right-click when there is a
/// picture file to copy.
fn with_copy_menu(
    frame: gpui::Stateful<gpui::Div>,
    can_copy: bool,
    entity: Entity<WhatsAppApp>,
    message_id: String,
) -> gpui::AnyElement {
    if !can_copy {
        return frame.into_any_element();
    }
    frame
        .context_menu(move |menu, _window, _cx| {
            let entity = entity.clone();
            let id = message_id.clone();
            menu.item(
                PopupMenuItem::new(crate::tr!(CopyImage)).on_click(move |_, _window, cx| {
                    entity.update(cx, |app, cx| app.copy_media_image(&id, cx));
                }),
            )
        })
        .into_any_element()
}

/// The picture itself, contained so nothing is cropped.
fn render_frame(
    image: Option<RetainedImage>,
    frame: Option<Arc<RenderImage>>,
    is_video: bool,
    is_loading: bool,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    // The decoded frame first. A video's own bytes decode to its poster
    // image, so asking the still first would draw a frozen picture over a
    // player that is running — the same ordering mistake the status reader
    // made. The viewer only opens pictures today, which is what keeps this
    // from being visible rather than what makes it right.
    if let Some(frame) = frame {
        return img(crate::video::video_image(frame))
            .max_w_full()
            .max_h_full()
            .object_fit(gpui::ObjectFit::Contain)
            .rounded(metrics.radius_md())
            .into_any_element();
    }
    if let Some(image) = image {
        return img(image.source(false))
            .max_w_full()
            .max_h_full()
            .object_fit(gpui::ObjectFit::Contain)
            .rounded(metrics.radius_md())
            .into_any_element();
    }

    if is_loading {
        return div()
            .id("viewer-loading")
            .debug_selector(|| "viewer-loading".into())
            .flex()
            .flex_col()
            .items_center()
            .gap(metrics.space_lg())
            .child(
                Spinner::new()
                    .small()
                    .color(parts::on_scrim(cx).opacity(0.6)),
            )
            .child(
                div()
                    .text_size(metrics.text_secondary())
                    .text_color(parts::on_scrim(cx).opacity(0.6))
                    .child(crate::tr!(LoadingImage)),
            )
            .into_any_element();
    }

    // Bytes the viewer cannot decode. Saying so beats an empty black screen
    // that reads as a hung window.
    div()
        .id("viewer-unavailable")
        .debug_selector(|| "viewer-unavailable".into())
        .flex()
        .flex_col()
        .items_center()
        .gap(metrics.space_lg())
        .child(
            Icon::new(if is_video {
                ProductIcon::Film
            } else {
                ProductIcon::Image
            })
            .size(metrics.icon())
            .text_color(parts::on_scrim(cx).opacity(0.6)),
        )
        .child(
            div()
                .text_size(metrics.text_secondary())
                .text_color(parts::on_scrim(cx).opacity(0.6))
                .child(crate::tr!(ThisFileCannotBeShownHere)),
        )
        .into_any_element()
}

/// A video in the viewer: its current frame, or its poster before there is
/// one, with the state it is in drawn over it. Pressing it plays or pauses,
/// which is what pressing a playing video does everywhere else.
fn render_video_stage(
    poster: Option<RetainedImage>,
    frame: Option<Arc<RenderImage>>,
    video: ViewerVideo,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let on_scrim = parts::on_scrim(cx);
    let scrim = parts::scrim(cx);
    let picture = if let Some(frame) = frame {
        img(crate::video::video_image(frame))
            .max_w_full()
            .max_h_full()
            .object_fit(gpui::ObjectFit::Contain)
            .rounded(metrics.radius_md())
            .into_any_element()
    } else if let Some(poster) = poster {
        // The poster is a thumbnail, so it is shown at the size the
        // player's frames will take rather than its own few pixels.
        img(poster.source(false))
            .w(metrics.bubble_max_width())
            .max_w_full()
            .max_h_full()
            .object_fit(gpui::ObjectFit::Contain)
            .rounded(metrics.radius_md())
            .into_any_element()
    } else {
        div()
            .w(metrics.bubble_max_width())
            .h(metrics.bubble_max_width() * 0.75)
            .rounded(metrics.radius_md())
            .bg(scrim)
            .into_any_element()
    };
    let badge = if video.state.is_loading() {
        Some(
            Spinner::new()
                .large()
                .color(on_scrim.opacity(0.8))
                .into_any_element(),
        )
    } else if video.state.is_error() {
        Some(
            Icon::new(IconName::Redo)
                .size(metrics.icon_media_large())
                .text_color(on_scrim)
                .into_any_element(),
        )
    } else if !video.state.is_playing() {
        Some(
            Icon::default()
                .path("icons/play.svg")
                .size(metrics.icon_media_large())
                .text_color(on_scrim)
                .into_any_element(),
        )
    } else {
        None
    };
    div()
        .id("viewer-video")
        .relative()
        .flex()
        .max_w_full()
        .max_h_full()
        .cursor_pointer()
        .on_click(move |_, _window, cx| {
            cx.stop_propagation();
            entity.update(cx, |app, cx| app.toggle_viewer_video(cx));
        })
        .child(picture)
        .children(badge.map(|badge| {
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(metrics.media_control() * 1.5)
                        .rounded_full()
                        .bg(scrim.opacity(0.55))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(badge),
                )
        }))
}

/// Play/pause, how far along, and how long: under the picture, where the
/// phone puts them.
fn render_video_controls(
    video: ViewerVideo,
    entity: Entity<WhatsAppApp>,
    metrics: Metrics,
    cx: &App,
) -> impl IntoElement + use<> {
    let on_scrim = parts::on_scrim(cx);
    let fraction = video
        .length_secs
        .filter(|length| *length > 0)
        .map_or(0., |length| {
            (video.elapsed_secs as f32 / length as f32).clamp(0., 1.)
        });
    let clock = format!(
        "{} / {}",
        oxidezap_core::format_duration(video.elapsed_secs),
        video
            .length_secs
            .map_or_else(|| "--:--".to_string(), oxidezap_core::format_duration)
    );
    let track = metrics.bubble_max_width();
    div()
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .gap(metrics.space_lg())
        .px(metrics.space_xl())
        .pt(metrics.space_md())
        .child(
            Button::new("viewer-play")
                .icon(
                    Icon::default()
                        .path(if video.state.is_playing() {
                            "icons/pause.svg"
                        } else {
                            "icons/play.svg"
                        })
                        .text_color(on_scrim),
                )
                .ghost()
                .disabled(video.state.is_loading())
                .tooltip(if video.state.is_playing() {
                    crate::tr!(Pause)
                } else {
                    crate::tr!(Play)
                })
                .cursor_pointer()
                .on_click(move |_, _window, cx| {
                    entity.update(cx, |app, cx| app.toggle_viewer_video(cx));
                }),
        )
        .child(
            div()
                .w(track)
                .h(metrics.space_xs())
                .rounded_full()
                .bg(on_scrim.opacity(0.25))
                .child(
                    div()
                        .h_full()
                        .w(track * fraction)
                        .rounded_full()
                        .bg(on_scrim),
                ),
        )
        .child(
            div()
                .flex_shrink_0()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(metrics.text_meta())
                .text_color(on_scrim.opacity(0.8))
                .child(clock),
        )
}

fn step_button<F: Fn(&mut App) + 'static>(
    id: &'static str,
    icon: IconName,
    tooltip: &'static str,
    enabled: bool,
    on_click: F,
) -> impl IntoElement + use<F> {
    Button::new(id)
        .icon(Icon::new(icon))
        .ghost()
        .large()
        .tooltip(tooltip)
        .disabled(!enabled)
        .when(enabled, |button| button.cursor_pointer())
        .on_click(move |_, _window, cx| on_click(cx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ActiveProductTheme as _;
    use gpui::{Context, Render, Window};

    struct Fixture {
        loading: bool,
    }
    impl Render for Fixture {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            render_frame(None, None, false, self.loading, cx.product().metrics, cx)
        }
    }

    #[gpui::test]
    fn loading_is_replaced_by_a_terminal_error_when_the_read_fails(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let (fixture, cx) = cx.add_window_view(|_, _| Fixture { loading: true });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("viewer-loading").is_some());
        assert!(cx.debug_bounds("viewer-unavailable").is_none());
        fixture.update(cx, |fixture, cx| {
            fixture.loading = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        assert!(cx.debug_bounds("viewer-loading").is_none());
        assert!(cx.debug_bounds("viewer-unavailable").is_some());
    }
}
