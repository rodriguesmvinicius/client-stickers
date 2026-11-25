//! The sticker maker's window: the sticker as it will be sent, and the tools
//! the phone's maker has around it.
//!
//! The canvas is the 512×512 sticker scaled to fit. Dragging moves the
//! picture, or the text or emoji under the pointer; the wheel resizes
//! whichever is held (Shift turns text and emoji); the pen draws freehand.
//! A still source becomes a still sticker; a GIF or video becomes an animated
//! one from the stretch chosen under the canvas. Either can carry a sound.
//!
//! Every change redraws the preview off the UI thread with the same code
//! that draws the file sent, so the canvas is the sticker, not an
//! approximation of it. The picture itself — with its cutout applied — is
//! prepared once and reused across those redraws.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    App, Bounds, Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement, Pixels, Point, Render, RenderImage,
    ScrollEvent, SharedString, StatefulInteractiveElement as _, Styled, Window, div, img,
    prelude::*,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::popover::Popover;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _};
use tiny_skia::Pixmap;

use super::ProductIcon;
use super::emoji_picker::{EmojiPicked, EmojiPicker};
use crate::session::SessionHandle;
use crate::sticker_maker::design::{At, Design, Item, Placement, SIDE_F, Stroke};
use crate::sticker_maker::{Clip, MAX_CLIP_MS, Made, Motion, Sound, Source};
use crate::theme::ActiveProductTheme as _;

/// What the maker tells the app.
pub(crate) enum StickerMakerEvent {
    Close,
    /// The finished sticker. Shared rather than moved: an event is handed
    /// out by reference.
    Send(Arc<Made>),
}

/// The pen's colours, and text's.
const PALETTE: [[u8; 4]; 8] = [
    [255, 255, 255, 255],
    [20, 20, 20, 255],
    [235, 64, 52, 255],
    [255, 149, 0, 255],
    [255, 214, 10, 255],
    [52, 199, 89, 255],
    [10, 132, 255, 255],
    [175, 82, 222, 255],
];
/// The pen's widths, in sticker pixels.
const PENS: [f32; 3] = [6.0, 12.0, 22.0];
/// How far a clip's start and length move per press.
const CLIP_STEP_MS: u32 = 500;
/// How many changes can be undone.
const HISTORY: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Move,
    Draw,
}

enum Drag {
    Picture {
        from: (f32, f32),
        placement: Placement,
    },
    Item {
        index: usize,
        from: (f32, f32),
        at: At,
    },
    Stroke(usize),
}

enum SoundChoice {
    None,
    /// The video's own sound, over the same stretch as the clip.
    Video,
    /// An audio file, from a chosen point.
    File {
        name: SharedString,
        sound: Sound,
    },
}

pub(crate) struct StickerMakerView {
    /// What the cutout is asked through. None only in tests.
    session: Option<SessionHandle>,
    source: Source,
    /// The frame the canvas shows: the photo, or the clip's first frame.
    frame: Arc<image::RgbaImage>,
    design: Design,
    history: Vec<Design>,
    tool: Tool,
    color: [u8; 4],
    pen: f32,
    selected: Option<usize>,
    drag: Option<Drag>,
    /// Whether the wheel has already saved an undo step for this gesture.
    wheeling: bool,
    text: Entity<InputState>,
    text_open: bool,
    emoji_picker: Entity<EmojiPicker>,
    emoji_open: bool,
    clip: Clip,
    sound: SoundChoice,
    sound_start_ms: u32,
    busy: Option<SharedString>,
    error: Option<SharedString>,
    /// The frame, cut out, ready to draw: rebuilt when either changes.
    subject: Option<Arc<Pixmap>>,
    preview: Option<Arc<RenderImage>>,
    rendering: bool,
    dirty: bool,
    canvas: Rc<Cell<Bounds<Pixels>>>,
    focus: FocusHandle,
}

impl EventEmitter<StickerMakerEvent> for StickerMakerView {}

impl StickerMakerView {
    pub fn new(
        source: Source,
        session: Option<SessionHandle>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let text = cx
            .new(|cx| InputState::new(window, cx).placeholder(crate::tr!(StickerTextPlaceholder)));
        cx.subscribe_in(
            &text,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => this.text_changed(cx),
                InputEvent::PressEnter { .. } => this.commit_text(window, cx),
                _ => {}
            },
        )
        .detach();
        let emoji_picker = cx.new(|cx| EmojiPicker::new(window, cx));
        cx.subscribe_in(
            &emoji_picker,
            window,
            |this, _, event: &EmojiPicked, window, cx| {
                this.add_emoji(event.0, window, cx);
            },
        )
        .detach();
        let (width, height) = source.poster.dimensions();
        let mut design = Design::new(width, height);
        // An animation fills the square, as the phone's does; a photo starts
        // whole so nothing is cut before the person decides.
        if source.is_animated() {
            design.placement = Placement::cover(width, height);
        }
        let clip = Clip {
            start_ms: 0,
            length_ms: source.duration_ms.clamp(1, MAX_CLIP_MS),
        };
        let sound = if source.audio.is_some() {
            SoundChoice::Video
        } else {
            SoundChoice::None
        };
        let mut view = Self {
            session,
            frame: source.poster.clone(),
            source,
            design,
            history: Vec::new(),
            tool: Tool::Move,
            color: PALETTE[0],
            pen: PENS[1],
            selected: None,
            drag: None,
            wheeling: false,
            text,
            text_open: false,
            emoji_picker,
            emoji_open: false,
            clip,
            sound,
            sound_start_ms: 0,
            busy: None,
            error: None,
            subject: None,
            preview: None,
            rendering: false,
            dirty: false,
            canvas: Rc::new(Cell::new(Bounds::default())),
            focus: cx.focus_handle(),
        };
        view.rebuild_subject(cx);
        view
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Give back the preview's texture. Called as the maker closes.
    pub fn release(&mut self, cx: &mut Context<Self>) {
        if let Some(old) = self.preview.take() {
            cx.defer(move |cx| {
                cx.drop_image(old, None);
            });
        }
    }

    // ── Drawing ─────────────────────────────────────────────────────────

    /// Prepare the frame with its cutout, then redraw.
    fn rebuild_subject(&mut self, cx: &mut Context<Self>) {
        let frame = self.frame.clone();
        let mask = self.design.cutout.clone();
        let task = cx.background_spawn(async move {
            crate::sticker_maker::render::subject(&frame, mask.as_deref())
        });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let subject = task.await;
            let _ = this.update(cx, |this, cx| {
                this.subject = subject.map(Arc::new);
                this.refresh(cx);
            });
        })
        .detach();
    }

    /// Redraw the preview, coalescing changes that arrive while one draws.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        cx.notify();
        if self.rendering {
            self.dirty = true;
            return;
        }
        let Some(subject) = self.subject.clone() else {
            return;
        };
        self.rendering = true;
        self.dirty = false;
        let design = self.design.clone();
        let task = cx.background_spawn(async move {
            let canvas = crate::sticker_maker::render::compose(&design, &subject)?;
            let mut bgra = crate::sticker_maker::render::rgba(&canvas);
            for pixel in bgra.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            let buffer = image::RgbaImage::from_raw(canvas.width(), canvas.height(), bgra)?;
            Some(Arc::new(RenderImage::new(smallvec::smallvec![
                image::Frame::new(buffer)
            ])))
        });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let image = task.await;
            let _ = this.update(cx, |this, cx| {
                this.rendering = false;
                if let Some(image) = image
                    && let Some(old) = this.preview.replace(image)
                {
                    cx.defer(move |cx| {
                        cx.drop_image(old, None);
                    });
                }
                if this.dirty {
                    this.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn snapshot(&mut self) {
        self.history.push(self.design.clone());
        if self.history.len() > HISTORY {
            self.history.remove(0);
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.history.pop() {
            let cutout_changed = !same_mask(&previous.cutout, &self.design.cutout);
            self.design = previous;
            self.selected = None;
            if cutout_changed {
                self.rebuild_subject(cx);
            } else {
                self.refresh(cx);
            }
        }
    }

    // ── The pointer ─────────────────────────────────────────────────────

    /// Where `position` is on the sticker, in sticker pixels.
    fn sticker_point(&self, position: Point<Pixels>) -> (f32, f32) {
        let bounds = self.canvas.get();
        let width = f32::from(bounds.size.width).max(1.0);
        let height = f32::from(bounds.size.height).max(1.0);
        (
            f32::from(position.x - bounds.origin.x) / width * SIDE_F,
            f32::from(position.y - bounds.origin.y) / height * SIDE_F,
        )
    }

    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let point = self.sticker_point(event.position);
        self.wheeling = false;
        self.snapshot();
        match self.tool {
            Tool::Draw => {
                self.design.items.push(Item::Stroke(Stroke {
                    points: vec![point],
                    color: self.color,
                    width: self.pen,
                }));
                self.drag = Some(Drag::Stroke(self.design.items.len() - 1));
                self.selected = None;
            }
            Tool::Move => match self.design.item_at(point) {
                Some(index) => {
                    let at = self.design.items[index].at().unwrap_or_else(At::middle);
                    self.drag = Some(Drag::Item {
                        index,
                        from: point,
                        at,
                    });
                    self.select(Some(index), window, cx);
                }
                None => {
                    self.drag = Some(Drag::Picture {
                        from: point,
                        placement: self.design.placement,
                    });
                    self.select(None, window, cx);
                }
            },
        }
        self.refresh(cx);
    }

    fn drag_to(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let point = self.sticker_point(event.position);
        match &self.drag {
            Some(Drag::Picture { from, placement }) => {
                self.design.placement.center = (
                    placement.center.0 + point.0 - from.0,
                    placement.center.1 + point.1 - from.1,
                );
            }
            Some(Drag::Item { index, from, at }) => {
                let (index, from, at) = (*index, *from, *at);
                if let Some(target) = self.design.items.get_mut(index).and_then(Item::at_mut) {
                    target.center = (
                        at.center.0 + point.0 - from.0,
                        at.center.1 + point.1 - from.1,
                    );
                }
            }
            Some(Drag::Stroke(index)) => {
                if let Some(Item::Stroke(stroke)) = self.design.items.get_mut(*index) {
                    stroke.points.push(point);
                }
            }
            None => return,
        }
        self.refresh(cx);
    }

    fn release_pointer(&mut self) {
        self.drag = None;
    }

    fn wheel(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let delta = f32::from(event.delta.pixel_delta(gpui::px(16.0)).y);
        if delta == 0.0 {
            return;
        }
        if !self.wheeling {
            self.snapshot();
            self.wheeling = true;
        }
        let factor = (1.0 + delta / 300.0).clamp(0.8, 1.25);
        match self
            .selected
            .and_then(|index| self.design.items.get_mut(index))
        {
            Some(item) => {
                if let Some(at) = item.at_mut() {
                    if event.modifiers.shift {
                        at.rotation += delta / 300.0;
                    } else {
                        at.scale = (at.scale * factor).clamp(0.1, 8.0);
                    }
                }
            }
            None => {
                let placement = &mut self.design.placement;
                placement.scale = (placement.scale * factor).clamp(0.02, 20.0);
            }
        }
        cx.stop_propagation();
        self.refresh(cx);
    }

    // ── Tools ───────────────────────────────────────────────────────────

    fn select(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index;
        // A selected text is edited in the field.
        if let Some(Item::Text { text, color, .. }) = index.and_then(|i| self.design.items.get(i)) {
            let (text, color) = (text.clone(), *color);
            self.color = color;
            self.text_open = true;
            self.text
                .update(cx, |input, cx| input.set_value(text, window, cx));
        }
    }

    fn set_tool(&mut self, tool: Tool, cx: &mut Context<Self>) {
        self.tool = if self.tool == tool { Tool::Move } else { tool };
        if self.tool == Tool::Draw {
            self.selected = None;
        }
        cx.notify();
    }

    fn set_color(&mut self, color: [u8; 4], cx: &mut Context<Self>) {
        self.color = color;
        if let Some(Item::Text { color: current, .. }) =
            self.selected.and_then(|i| self.design.items.get_mut(i))
        {
            *current = color;
            self.refresh(cx);
        }
        cx.notify();
    }

    fn text_changed(&mut self, cx: &mut Context<Self>) {
        let value = self.text.read(cx).value().to_string();
        if let Some(Item::Text { text, .. }) =
            self.selected.and_then(|i| self.design.items.get_mut(i))
            && !value.trim().is_empty()
        {
            *text = value;
            self.refresh(cx);
        }
    }

    /// Enter in the field: a new text, unless one is being edited.
    fn commit_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.text.read(cx).value().trim().to_string();
        let editing = matches!(
            self.selected.and_then(|i| self.design.items.get(i)),
            Some(Item::Text { .. })
        );
        if !value.is_empty() && !editing {
            self.snapshot();
            self.design.items.push(Item::Text {
                text: value,
                color: self.color,
                at: At::middle(),
            });
            self.selected = Some(self.design.items.len() - 1);
            self.tool = Tool::Move;
            self.refresh(cx);
        }
        // Unselected first, so clearing the field edits nothing.
        self.selected = None;
        self.text
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn add_emoji(&mut self, emoji: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji_open = false;
        let Some(image) = crate::emoji::artwork(emoji, window) else {
            cx.notify();
            return;
        };
        self.snapshot();
        self.design.items.push(Item::Emoji {
            image,
            at: At::middle(),
        });
        self.selected = Some(self.design.items.len() - 1);
        self.tool = Tool::Move;
        self.refresh(cx);
    }

    fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.selected.take()
            && index < self.design.items.len()
        {
            self.snapshot();
            self.design.items.remove(index);
            self.text
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.refresh(cx);
        }
    }

    fn place(&mut self, fill: bool, cx: &mut Context<Self>) {
        self.snapshot();
        let (width, height) = self.frame.dimensions();
        self.design.placement = if fill {
            Placement::cover(width, height)
        } else {
            Placement::contain(width, height)
        };
        self.refresh(cx);
    }

    /// Cut the subject out, or put the background back.
    fn toggle_cutout(&mut self, cx: &mut Context<Self>) {
        if self.design.cutout.is_some() {
            self.snapshot();
            self.design.cutout = None;
            self.rebuild_subject(cx);
            return;
        }
        // Small is enough: the model reads 320×320.
        let frame = self.frame.clone();
        let encode = cx.background_spawn(async move {
            let picture = image::DynamicImage::ImageRgba8((*frame).clone()).thumbnail(640, 640);
            let mut png = Vec::new();
            picture
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .map(|()| png)
                .map_err(|e| e.to_string())
        });
        self.busy = Some(crate::tr!(StickerCutting).into());
        self.error = None;
        cx.notify();
        let Some(session) = self.session.clone() else {
            self.busy = None;
            self.error = Some(crate::tr!(StickerCutoutFailed).into());
            return;
        };
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let outcome = match encode.await {
                Ok(png) => match session.cutout(png).await {
                    Ok(Ok(oxidezap_ipc::CutoutReply::Mask { png })) => {
                        image::load_from_memory(&png)
                            .map(|mask| mask.to_luma8())
                            .map_err(|e| e.to_string())
                    }
                    Ok(Err(detail)) => Err(detail),
                    Err(_) => Err("the background service went away".to_string()),
                },
                Err(detail) => Err(detail),
            };
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Ok(mask) => {
                        this.snapshot();
                        this.design.cutout = Some(Arc::new(mask));
                        this.design.outline = true;
                        this.rebuild_subject(cx);
                    }
                    Err(detail) => {
                        log::warn!("cutout failed: {detail}");
                        this.error =
                            Some(format!("{}: {detail}", crate::tr!(StickerCutoutFailed)).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn toggle_outline(&mut self, cx: &mut Context<Self>) {
        self.snapshot();
        self.design.outline = !self.design.outline;
        self.refresh(cx);
    }

    // ── The clip and its sound ──────────────────────────────────────────

    fn move_clip(&mut self, start_delta: i64, length_delta: i64, cx: &mut Context<Self>) {
        let duration = self.source.duration_ms;
        let length = (self.clip.length_ms as i64 + length_delta).clamp(
            CLIP_STEP_MS as i64,
            MAX_CLIP_MS.min(duration.max(CLIP_STEP_MS)) as i64,
        ) as u32;
        let start = (self.clip.start_ms as i64 + start_delta)
            .clamp(0, duration.saturating_sub(length) as i64) as u32;
        let moved = start != self.clip.start_ms;
        self.clip = Clip {
            start_ms: start,
            length_ms: length,
        };
        if moved {
            self.show_frame_at(start, cx);
        }
        cx.notify();
    }

    /// Put the frame `ms` into the source on the canvas.
    fn show_frame_at(&mut self, ms: u32, cx: &mut Context<Self>) {
        let motion = self.source.motion.clone();
        let duration = self.source.duration_ms;
        let task = cx.background_spawn(async move {
            match motion {
                Motion::Frames(frames) => Source::frame_at(&frames, duration, ms).cloned(),
                Motion::Video(bytes) => crate::sticker_maker::VideoReader::open(&bytes)
                    .ok()
                    .and_then(|mut reader| reader.frame(ms))
                    .map(Arc::new),
                Motion::Still => None,
            }
        });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            if let Some(frame) = task.await {
                let _ = this.update(cx, |this, cx| {
                    this.frame = frame;
                    this.rebuild_subject(cx);
                });
            }
        })
        .detach();
    }

    fn pick_sound(&mut self, cx: &mut Context<Self>) {
        let chosen = crate::platform::picker::choose_category(
            cx,
            crate::platform::picker::AttachmentCategory::Document,
        );
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let Ok(chosen) = chosen.await else {
                return;
            };
            let Some(file) = chosen.files.into_iter().next() else {
                return;
            };
            let name: SharedString = file.file_name.clone().into();
            let decode = cx
                .background_executor()
                .spawn(async move { crate::sticker_maker::decode_audio(&file.bytes) });
            let decoded = decode.await;
            let _ = this.update(cx, |this, cx| {
                match decoded {
                    Ok(sound) => {
                        this.sound = SoundChoice::File { name, sound };
                        this.sound_start_ms = 0;
                        this.error = None;
                    }
                    Err(detail) => {
                        this.error =
                            Some(format!("{}: {detail}", crate::tr!(StickerSoundFailed)).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn move_sound(&mut self, delta: i64, cx: &mut Context<Self>) {
        if let SoundChoice::File { sound, .. } = &self.sound {
            let longest = sound.duration_ms().saturating_sub(CLIP_STEP_MS);
            self.sound_start_ms =
                (self.sound_start_ms as i64 + delta).clamp(0, longest as i64) as u32;
            cx.notify();
        }
    }

    // ── Sending ─────────────────────────────────────────────────────────

    fn send(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(crate::tr!(StickerMaking).into());
        self.error = None;
        cx.notify();
        let design = self.design.clone();
        let source = self.source.clone();
        let clip = self.clip;
        let sound = match &self.sound {
            SoundChoice::None => None,
            SoundChoice::Video => self
                .source
                .audio
                .clone()
                .map(|sound| (sound, clip.start_ms)),
            SoundChoice::File { sound, .. } => Some((sound.clone(), self.sound_start_ms)),
        };
        let task = cx.background_spawn(async move {
            crate::sticker_maker::make(
                &design,
                &source,
                clip,
                sound.as_ref().map(|(sound, start)| (sound, *start)),
            )
        });
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx| {
            let made = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match made {
                    Ok(made) => cx.emit(StickerMakerEvent::Send(Arc::new(made))),
                    Err(detail) => {
                        log::warn!("sticker failed: {detail}");
                        this.error =
                            Some(format!("{}: {detail}", crate::tr!(StickerMakeFailed)).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn same_mask(a: &Option<Arc<image::GrayImage>>, b: &Option<Arc<image::GrayImage>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        _ => false,
    }
}

fn seconds(ms: u32) -> String {
    format!("{:.1}s", ms as f32 / 1000.0)
}

fn rgb(color: [u8; 4]) -> gpui::Hsla {
    gpui::Rgba {
        r: color[0] as f32 / 255.0,
        g: color[1] as f32 / 255.0,
        b: color[2] as f32 / 255.0,
        a: 1.0,
    }
    .into()
}

impl Render for StickerMakerView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = cx.product().metrics;
        let side = metrics.icon_button() * 12.0;
        let busy = self.busy.is_some();
        let animated = self.source.is_animated();
        let has_cutout = self.design.cutout.is_some();
        let selected = self.selected.is_some();
        let text_selected = matches!(
            self.selected.and_then(|i| self.design.items.get(i)),
            Some(Item::Text { .. })
        );
        let entity = cx.entity();
        let bounds_cell = self.canvas.clone();

        let canvas = div()
            .id("sticker-canvas")
            .relative()
            .size(side)
            .flex_shrink_0()
            .rounded(metrics.radius_md())
            .overflow_hidden()
            .bg(cx.theme().muted)
            .border_1()
            .border_color(cx.theme().border)
            .cursor(if self.tool == Tool::Draw {
                gpui::CursorStyle::Crosshair
            } else {
                gpui::CursorStyle::OpenHand
            })
            .child(
                gpui::canvas(move |bounds, _, _| bounds_cell.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .size_full(),
            )
            .children(
                self.preview
                    .clone()
                    .map(|preview| img(preview).absolute().size_full()),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.press(event, window, cx)
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| this.wheel(event, cx)));

        let tool_button = |id: &'static str, icon: Icon, label: SharedString, active: bool| {
            let button = Button::new(id).icon(icon).label(label).small();
            if active {
                button.primary()
            } else {
                button.ghost()
            }
        };

        let emoji_entity = entity.clone();
        let picker = self.emoji_picker.clone();
        let tools = div()
            .flex()
            .flex_col()
            .gap(metrics.space_xs())
            .min_w(metrics.icon_button() * 5.0)
            .child(
                tool_button(
                    "sticker-cutout",
                    Icon::new(ProductIcon::Scissors),
                    if has_cutout {
                        crate::tr!(StickerKeepBackground).into()
                    } else {
                        crate::tr!(StickerCutout).into()
                    },
                    has_cutout,
                )
                .disabled(busy || animated)
                .on_click(cx.listener(|this, _, _, cx| this.toggle_cutout(cx))),
            )
            .when(has_cutout, |tools| {
                tools.child(
                    tool_button(
                        "sticker-outline",
                        Icon::new(IconName::Square),
                        crate::tr!(StickerOutline).into(),
                        self.design.outline,
                    )
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_outline(cx))),
                )
            })
            .child(
                tool_button(
                    "sticker-text",
                    Icon::new(IconName::CaseSensitive),
                    crate::tr!(StickerAddText).into(),
                    self.text_open,
                )
                .disabled(busy)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.text_open = !this.text_open;
                    if this.text_open {
                        this.text.update(cx, |input, cx| input.focus(window, cx));
                    }
                    cx.notify();
                })),
            )
            .child(
                Popover::new("sticker-emoji")
                    .anchor(gpui::Anchor::TopRight)
                    .open(self.emoji_open)
                    .on_open_change(move |open, _, cx| {
                        emoji_entity.update(cx, |view, cx| {
                            view.emoji_open = *open;
                            cx.notify();
                        });
                    })
                    .track_focus(&self.emoji_picker.read(cx).focus_handle(cx))
                    .trigger(
                        tool_button(
                            "sticker-emoji-trigger",
                            Icon::new(ProductIcon::Smile),
                            crate::tr!(StickerEmoji).into(),
                            self.emoji_open,
                        )
                        .disabled(busy),
                    )
                    .content(move |_, _, _| picker.clone()),
            )
            .child(
                tool_button(
                    "sticker-draw",
                    Icon::new(ProductIcon::Pen),
                    crate::tr!(StickerDraw).into(),
                    self.tool == Tool::Draw,
                )
                .disabled(busy)
                .on_click(cx.listener(|this, _, _, cx| this.set_tool(Tool::Draw, cx))),
            )
            .child(
                div()
                    .flex()
                    .gap(metrics.space_xs())
                    .child(
                        Button::new("sticker-fit")
                            .icon(IconName::Minimize)
                            .tooltip(crate::tr!(StickerFit))
                            .small()
                            .ghost()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.place(false, cx))),
                    )
                    .child(
                        Button::new("sticker-fill")
                            .icon(IconName::Maximize)
                            .tooltip(crate::tr!(StickerFill))
                            .small()
                            .ghost()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.place(true, cx))),
                    )
                    .child(
                        Button::new("sticker-undo")
                            .icon(IconName::Undo2)
                            .tooltip(crate::tr!(StickerUndo))
                            .small()
                            .ghost()
                            .disabled(busy || self.history.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
                    )
                    .child(
                        Button::new("sticker-delete")
                            .icon(IconName::Delete)
                            .tooltip(crate::tr!(StickerDelete))
                            .small()
                            .ghost()
                            .disabled(busy || !selected)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.delete_selected(window, cx)),
                            ),
                    ),
            )
            .when(
                self.tool == Tool::Draw || text_selected || self.text_open,
                |tools| {
                    tools.child(div().flex().flex_wrap().gap(metrics.space_xs()).children(
                        PALETTE.iter().enumerate().map(|(ix, color)| {
                            let color = *color;
                            let chosen = color == self.color;
                            div()
                                .id(("sticker-color", ix))
                                .size(metrics.icon_small() * 1.4)
                                .rounded_full()
                                .bg(rgb(color))
                                .border_2()
                                .border_color(if chosen {
                                    cx.theme().primary
                                } else {
                                    cx.theme().border
                                })
                                .cursor_pointer()
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.set_color(color, cx)),
                                )
                        }),
                    ))
                },
            )
            .when(self.tool == Tool::Draw, |tools| {
                tools.child(div().flex().gap(metrics.space_xs()).children(
                    PENS.iter().enumerate().map(|(ix, width)| {
                        let width = *width;
                        Button::new(("sticker-pen", ix))
                            .child(
                                div()
                                    .size(metrics.icon_small() * (0.35 + ix as f32 * 0.3))
                                    .rounded_full()
                                    .bg(cx.theme().foreground),
                            )
                            .small()
                            .map(|b| {
                                if width == self.pen {
                                    b.primary()
                                } else {
                                    b.ghost()
                                }
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pen = width;
                                cx.notify();
                            }))
                    }),
                ))
            });

        let text_row = self.text_open.then(|| {
            div()
                .w(side)
                .child(Input::new(&self.text).w_full().disabled(busy))
        });

        let step = |id: &'static str, icon: IconName| {
            Button::new(id).icon(icon).xsmall().ghost().disabled(busy)
        };
        let clip_row = animated.then(|| {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(metrics.space_sm())
                .text_size(metrics.text_small())
                .child(crate::tr!(StickerClipStart))
                .child(step("clip-start-less", IconName::Minus).on_click(
                    cx.listener(|this, _, _, cx| this.move_clip(-(CLIP_STEP_MS as i64), 0, cx)),
                ))
                .child(seconds(self.clip.start_ms))
                .child(step("clip-start-more", IconName::Plus).on_click(
                    cx.listener(|this, _, _, cx| this.move_clip(CLIP_STEP_MS as i64, 0, cx)),
                ))
                .child(div().w(metrics.space_md()))
                .child(crate::tr!(StickerClipLength))
                .child(step("clip-length-less", IconName::Minus).on_click(
                    cx.listener(|this, _, _, cx| this.move_clip(0, -(CLIP_STEP_MS as i64), cx)),
                ))
                .child(seconds(self.clip.length_ms))
                .child(step("clip-length-more", IconName::Plus).on_click(
                    cx.listener(|this, _, _, cx| this.move_clip(0, CLIP_STEP_MS as i64, cx)),
                ))
        });

        let has_video_sound = self.source.audio.is_some();
        let sound_row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(metrics.space_sm())
            .text_size(metrics.text_small())
            .when(has_video_sound, |row| {
                row.child(
                    Button::new("sticker-video-sound")
                        .icon(Icon::new(ProductIcon::Sound))
                        .label(crate::tr!(StickerVideoSound))
                        .small()
                        .map(|b| {
                            if matches!(self.sound, SoundChoice::Video) {
                                b.primary()
                            } else {
                                b.ghost()
                            }
                        })
                        .disabled(busy)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.sound = if matches!(this.sound, SoundChoice::Video) {
                                SoundChoice::None
                            } else {
                                SoundChoice::Video
                            };
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("sticker-add-sound")
                    .icon(Icon::new(ProductIcon::Sound))
                    .label(crate::tr!(StickerAddSound))
                    .small()
                    .ghost()
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_sound(cx))),
            )
            .when_some(
                match &self.sound {
                    SoundChoice::File { name, .. } => Some(name.clone()),
                    _ => None,
                },
                |row, name| {
                    row.child(
                        div()
                            .max_w(metrics.icon_button() * 6.0)
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .child(name),
                    )
                    .child(crate::tr!(StickerSoundFrom))
                    .child(step("sound-less", IconName::Minus).on_click(
                        cx.listener(|this, _, _, cx| this.move_sound(-(CLIP_STEP_MS as i64), cx)),
                    ))
                    .child(seconds(self.sound_start_ms))
                    .child(step("sound-more", IconName::Plus).on_click(
                        cx.listener(|this, _, _, cx| this.move_sound(CLIP_STEP_MS as i64, cx)),
                    ))
                    .child(
                        Button::new("sticker-remove-sound")
                            .icon(IconName::Close)
                            .tooltip(crate::tr!(StickerRemoveSound))
                            .xsmall()
                            .ghost()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sound = SoundChoice::None;
                                cx.notify();
                            })),
                    )
                },
            );

        let status = self
            .busy
            .clone()
            .or_else(|| self.error.clone())
            .unwrap_or_else(|| crate::tr!(StickerHint).into());

        div()
            .id("sticker-maker")
            .track_focus(&self.focus)
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p(metrics.space_xl())
            .bg(crate::components::parts::scrim(cx).opacity(0.92))
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag_to(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.release_pointer()),
            )
            .child(
                div()
                    .id("sticker-maker-card")
                    .max_h_full()
                    .overflow_y_scroll()
                    .p(metrics.space_xl())
                    .rounded(metrics.radius_lg())
                    .bg(cx.theme().background)
                    .text_color(cx.theme().foreground)
                    .flex()
                    .flex_col()
                    .gap(metrics.space_md())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(metrics.text_title())
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(crate::tr!(CreateSticker)),
                            )
                            .child(
                                Button::new("sticker-maker-close")
                                    .icon(IconName::Close)
                                    .tooltip(crate::tr!(Close))
                                    .small()
                                    .ghost()
                                    .on_click(
                                        cx.listener(|_, _, _, cx| {
                                            cx.emit(StickerMakerEvent::Close)
                                        }),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap(metrics.space_lg())
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(metrics.space_sm())
                                    .child(canvas)
                                    .children(text_row)
                                    .children(clip_row),
                            )
                            .child(tools),
                    )
                    .child(sound_row)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(metrics.space_md())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(metrics.text_micro())
                                    .text_color(if self.error.is_some() && self.busy.is_none() {
                                        cx.theme().danger
                                    } else {
                                        cx.theme().muted_foreground
                                    })
                                    .child(status),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(metrics.space_sm())
                                    .child(
                                        Button::new("sticker-cancel")
                                            .label(crate::tr!(Cancel))
                                            .ghost()
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.emit(StickerMakerEvent::Close)
                                            })),
                                    )
                                    .child(
                                        Button::new("sticker-send")
                                            .label(crate::tr!(StickerSend))
                                            .primary()
                                            .loading(busy)
                                            .disabled(busy || self.subject.is_none())
                                            .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                                    ),
                            ),
                    ),
            )
    }
}

impl gpui::Focusable for StickerMakerView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, VisualTestContext, point, px, size};

    use super::*;

    struct Harness {
        view: Entity<StickerMakerView>,
    }

    impl Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.view.clone())
        }
    }

    fn photo() -> Source {
        Source {
            poster: Arc::new(image::RgbaImage::from_pixel(
                200,
                100,
                image::Rgba([200, 40, 40, 255]),
            )),
            motion: Motion::Still,
            duration_ms: 0,
            audio: None,
        }
    }

    fn setup(test_cx: &mut gpui::TestAppContext) -> (VisualTestContext, Entity<StickerMakerView>) {
        test_cx.update(|cx| {
            crate::platform::fonts::fonts(cx);
            gpui_component::init(cx);
            crate::theme::init(cx);
        });
        let mut made = None;
        let window = test_cx.open_window(size(px(1000.), px(800.)), |window, cx| {
            let view = cx.new(|cx| StickerMakerView::new(photo(), None, window, cx));
            made = Some(view.clone());
            let harness = cx.new(|_| Harness { view });
            gpui_component::Root::new(harness, window, cx)
        });
        let view = made.expect("the maker opened");
        let mut cx = VisualTestContext::from_window(window.into(), test_cx);
        cx.run_until_parked();
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (cx, view)
    }

    /// The point `at` (in sticker pixels) on the canvas, in the window.
    fn on_canvas(
        cx: &mut VisualTestContext,
        view: &Entity<StickerMakerView>,
        at: (f32, f32),
    ) -> Point<Pixels> {
        cx.update(|_, cx| {
            let bounds = view.read(cx).canvas.get();
            let width = f32::from(bounds.size.width);
            point(
                bounds.origin.x + px(at.0 / SIDE_F * width),
                bounds.origin.y + px(at.1 / SIDE_F * width),
            )
        })
    }

    #[gpui::test]
    fn the_maker_previews_draws_moves_types_undoes_and_sends(test_cx: &mut gpui::TestAppContext) {
        let (mut cx, view) = setup(test_cx);
        cx.update(|_, cx| {
            let maker = view.read(cx);
            assert!(maker.preview.is_some(), "the sticker is previewed");
            assert!(
                f32::from(maker.canvas.get().size.width) > 100.0,
                "the canvas is laid out"
            );
        });

        // A line with the pen.
        cx.update(|_, cx| view.update(cx, |maker, cx| maker.set_tool(Tool::Draw, cx)));
        let from = on_canvas(&mut cx, &view, (100.0, 100.0));
        let to = on_canvas(&mut cx, &view, (300.0, 120.0));
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        cx.update(|_, cx| {
            let items = &view.read(cx).design.items;
            assert!(matches!(items.as_slice(), [Item::Stroke(stroke)] if stroke.points.len() >= 2));
        });

        // Dragging with the pen put away moves the picture.
        cx.update(|_, cx| view.update(cx, |maker, cx| maker.set_tool(Tool::Draw, cx)));
        let before = cx.update(|_, cx| view.read(cx).design.placement.center);
        let from = on_canvas(&mut cx, &view, (256.0, 400.0));
        let to = on_canvas(&mut cx, &view, (306.0, 420.0));
        cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let after = cx.update(|_, cx| view.read(cx).design.placement.center);
        assert!(
            (after.0 - before.0 - 50.0).abs() < 2.0,
            "{before:?} -> {after:?}"
        );
        assert!(
            (after.1 - before.1 - 20.0).abs() < 2.0,
            "{before:?} -> {after:?}"
        );

        // Text, typed and committed with Enter.
        cx.update(|window, cx| {
            view.update(cx, |maker, cx| {
                maker
                    .text
                    .update(cx, |input, cx| input.set_value("Oi!", window, cx));
                maker.commit_text(window, cx);
            })
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            let maker = view.read(cx);
            assert!(
                matches!(maker.design.items.last(), Some(Item::Text { text, .. }) if text == "Oi!")
            );
            assert_eq!(maker.text.read(cx).value().as_ref(), "");
        });

        // Undo takes the text back off.
        cx.update(|_, cx| view.update(cx, |maker, cx| maker.undo(cx)));
        cx.update(|_, cx| assert_eq!(view.read(cx).design.items.len(), 1));

        // Send: a still, lossy WebP within WhatsApp's limit.
        let sent: Rc<RefCell<Option<Arc<Made>>>> = Rc::default();
        let seen = sent.clone();
        let _events = cx.update(|_, cx| {
            cx.subscribe(&view, move |_, event: &StickerMakerEvent, _| {
                if let StickerMakerEvent::Send(made) = event {
                    *seen.borrow_mut() = Some(made.clone());
                }
            })
        });
        cx.update(|_, cx| view.update(cx, |maker, cx| maker.send(cx)));
        cx.run_until_parked();
        let made = sent.borrow().clone().expect("the sticker was made");
        assert!(!made.is_animated);
        assert!(made.sound.is_none());
        assert_eq!(&made.webp[..4], b"RIFF");
        assert!(made.webp.len() <= 100 * 1024);
    }
}
