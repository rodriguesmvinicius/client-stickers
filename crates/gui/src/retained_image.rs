//! Decoded assets live as long as the cache or a painted element needs them.
//!
//! GPUI's global asset cache retains completed decodes until explicitly removed.
//! Dropping the encoded image alone does not remove its decoded pixels. Each
//! image therefore owns its decode entities, and each painted element leases
//! the entity through element state. The last owner retires the asset and its
//! atlas tiles after window updates unwind. Pending GPUI loads are cancelled
//! by the same removal, so late work cannot repopulate an abandoned entry.

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use gpui::{
    App, AppContext as _, Asset, ElementId, Entity, Global, Image, ImageCacheError, ImageSource,
    RenderImage, WeakEntity,
};

#[derive(Clone)]
pub(crate) struct RetainedImage(Rc<Inner>);

/// How much of the picture a decode keeps.
///
/// A bubble draws a photo a few hundred pixels across, and a phone photo
/// decodes to tens of megabytes of RGBA at its own size — kept for as long
/// as the bubble is cached, and uploaded to the GPU atlas whole. So the
/// surfaces that draw small ask for a decode at the size they draw, and only
/// the viewer, which shows a picture at the size of the window, decodes it
/// all.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(crate) enum Fit {
    /// Every pixel and every frame.
    Full,
    /// Scaled down to [`PICTURE_SIDE`] or [`STICKER_SIDE`], every frame.
    Fitted,
    /// Scaled the same, first frame only: a grid of animated stickers is
    /// dozens of frames each, and a grid is browsed rather than watched.
    Still,
}

/// The longest side a picture in a bubble is decoded to. Twice the widest a
/// bubble draws one at, so a high-density screen still gets a pixel per
/// pixel.
pub(crate) const PICTURE_SIDE: u32 = 720;
/// The same for a sticker, which a bubble and the picker draw smaller.
pub(crate) const STICKER_SIDE: u32 = 384;

struct Inner {
    image: Arc<Image>,
    assets: RefCell<HashMap<(bool, Fit), Entity<Decoded>>>,
}

struct Decoded {
    source: Source,
    output: Option<Arc<RenderImage>>,
}

// GPUI Image IDs are content-derived. Independent messages with identical
// bytes must share retirement ownership as well as the global asset key.
#[derive(Default)]
struct Assets(HashMap<(u64, bool, Fit), WeakEntity<Decoded>>);

impl Global for Assets {}

#[derive(Clone, Hash)]
struct Source {
    image: Arc<Image>,
    sticker: bool,
    fit: Fit,
}

struct Decoder;

impl Asset for Decoder {
    type Source = Source;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(source: Source, cx: &mut App) -> impl Future<Output = Self::Output> + Send + 'static {
        let renderer = cx.svg_renderer();
        async move {
            match (source.sticker, source.fit) {
                (true, Fit::Full) => {
                    crate::components::message_bubble::decode_sticker(&source.image.bytes)
                        .map_err(Into::into)
                }
                (true, fit) => crate::components::message_bubble::decode_sticker_fitted(
                    &source.image.bytes,
                    STICKER_SIDE,
                    fit == Fit::Still,
                )
                .map_err(Into::into),
                (false, Fit::Full) => source.image.to_image_data(renderer).map_err(Into::into),
                (false, _) => match decode_fitted(&source.image, PICTURE_SIDE) {
                    Some(decoded) => decoded.map_err(Into::into),
                    // A format with frames or no pixels of its own (GIF, SVG)
                    // keeps GPUI's own decode: scaling would lose the one or
                    // has nothing to scale in the other.
                    None => source.image.to_image_data(renderer).map_err(Into::into),
                },
            }
        }
    }
}

impl std::ops::Deref for RetainedImage {
    type Target = Image;

    fn deref(&self) -> &Image {
        &self.0.image
    }
}

impl RetainedImage {
    /// The decode a surface asked for, whichever fit it asked at.
    #[cfg(test)]
    pub(crate) fn decoded(&self, sticker: bool, cx: &App) -> Option<Arc<RenderImage>> {
        let assets = self.0.assets.borrow();
        [Fit::Full, Fit::Fitted, Fit::Still]
            .into_iter()
            .find_map(|fit| assets.get(&(sticker, fit)))?
            .read(cx)
            .output
            .clone()
    }

    pub(crate) fn new(image: Image) -> Self {
        Self(Rc::new(Inner {
            image: Arc::new(image),
            assets: RefCell::new(HashMap::new()),
        }))
    }

    /// Every pixel: for a surface that shows the picture at full size.
    pub(crate) fn source(&self, sticker: bool) -> ImageSource {
        self.decoded_as(sticker, Fit::Full)
    }

    /// Scaled to what a bubble or a grid can show. See [`Fit`].
    pub(crate) fn fitted(&self, sticker: bool) -> ImageSource {
        self.decoded_as(sticker, Fit::Fitted)
    }

    /// Scaled, and only the first frame. See [`Fit::Still`].
    pub(crate) fn still(&self, sticker: bool) -> ImageSource {
        self.decoded_as(sticker, Fit::Still)
    }

    /// Let go of the full-size decode, keeping the small ones.
    ///
    /// The viewer asks for one picture at a time at full size; this is what
    /// it calls on the one it has left, so walking an album does not leave
    /// every photo in it decoded whole for as long as the bubbles are cached.
    /// A surface still painting it keeps its own lease until it repaints.
    pub(crate) fn release_full(&self) {
        self.release(Fit::Full);
    }

    /// Let go of the decodes at one fit, keeping the others.
    pub(crate) fn release(&self, fit: Fit) {
        self.0
            .assets
            .borrow_mut()
            .retain(|(_, held), _| *held != fit);
    }

    /// Let go of every decode, keeping only the encoded bytes: for an owner
    /// that holds many images but draws a few at a time.
    pub(crate) fn release_decodes(&self) {
        self.0.assets.borrow_mut().clear();
    }

    fn decoded_as(&self, sticker: bool, fit: Fit) -> ImageSource {
        let owner = self.clone();
        ImageSource::from(move |window: &mut gpui::Window, cx: &mut App| {
            let asset = owner
                .0
                .assets
                .borrow_mut()
                .entry((sticker, fit))
                .or_insert_with(|| {
                    let key = (owner.id, sticker, fit);
                    if let Some(asset) = cx
                        .default_global::<Assets>()
                        .0
                        .get(&key)
                        .and_then(WeakEntity::upgrade)
                    {
                        return asset;
                    }
                    let asset = cx.new(|cx| {
                        cx.on_release(|asset: &mut Decoded, cx| {
                            let source = asset.source.clone();
                            let output = asset.output.clone();
                            cx.defer(move |cx| {
                                let key = (source.image.id, source.sticker, source.fit);
                                let assets = cx.default_global::<Assets>();
                                // A new owner may acquire this content between
                                // release and this deferred retirement.
                                if assets.0.get(&key).and_then(WeakEntity::upgrade).is_some() {
                                    return;
                                }
                                assets.0.remove(&key);
                                cx.remove_asset::<Decoder>(&source);
                                if let Some(output) = output {
                                    cx.drop_image(output, None);
                                }
                            });
                        })
                        .detach();
                        Decoded {
                            source: Source {
                                image: owner.0.image.clone(),
                                sticker,
                                fit,
                            },
                            output: None,
                        }
                    });
                    cx.default_global::<Assets>()
                        .0
                        .insert(key, asset.downgrade());
                    asset
                })
                .clone();

            // A cached paint can retain atlas coordinates without retaining its
            // ImageSource. Keep the asset with that element state as well.
            window.with_global_id(
                ElementId::NamedInteger(
                    match (sticker, fit) {
                        (true, Fit::Full) => "sticker-asset",
                        (true, Fit::Fitted) => "sticker-fitted-asset",
                        (true, Fit::Still) => "sticker-still-asset",
                        (false, Fit::Full) => "image-asset",
                        (false, Fit::Fitted) => "image-fitted-asset",
                        (false, Fit::Still) => "image-still-asset",
                    }
                    .into(),
                    owner.id,
                ),
                |id, window| {
                    window.with_element_state(id, |_: Option<Entity<Decoded>>, _| {
                        ((), asset.clone())
                    });
                },
            );
            asset.update(cx, |asset, cx| {
                let output = window.use_asset::<Decoder>(&asset.source, cx);
                if let Some(Ok(image)) = &output {
                    asset.output = Some(image.clone());
                }
                output
            })
        })
    }
}

/// Decode a still picture no larger than `side` on its longest edge, in the
/// layout GPUI draws (BGRA), honouring the EXIF orientation as GPUI's own
/// decode does. `None` for a format this does not scale; the caller falls
/// back to GPUI's decode.
fn decode_fitted(image: &Image, side: u32) -> Option<anyhow::Result<Arc<RenderImage>>> {
    let format = match image.format {
        gpui::ImageFormat::Png => image::ImageFormat::Png,
        gpui::ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        gpui::ImageFormat::Webp => image::ImageFormat::WebP,
        gpui::ImageFormat::Bmp => image::ImageFormat::Bmp,
        gpui::ImageFormat::Tiff => image::ImageFormat::Tiff,
        _ => return None,
    };
    Some((|| {
        use image::ImageDecoder as _;
        let mut decoder =
            image::ImageReader::with_format(std::io::Cursor::new(&image.bytes), format)
                .into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut picture = image::DynamicImage::from_decoder(decoder)?;
        picture.apply_orientation(orientation);
        let (width, height) = (picture.width(), picture.height());
        if width.max(height) > side {
            let (w, h) = fitted_size(width, height, side);
            picture = picture.resize_exact(w, h, image::imageops::FilterType::Triangle);
        }
        let mut pixels = picture.into_rgba8();
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        Ok(Arc::new(RenderImage::new(smallvec::smallvec![
            image::Frame::new(pixels)
        ])))
    })())
}

/// `width`×`height` scaled to fit `side` on its longest edge, never below a
/// pixel.
pub(crate) fn fitted_size(width: u32, height: u32, side: u32) -> (u32, u32) {
    let longest = width.max(height).max(1);
    if longest <= side {
        return (width.max(1), height.max(1));
    }
    let scale = f64::from(side) / f64::from(longest);
    let scaled = |n: u32| ((f64::from(n) * scale).round() as u32).max(1);
    (scaled(width), scaled(height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        IntoElement, ParentElement as _, Render, Styled as _, TestAppContext, Window, div, img,
    };

    struct View(Option<ImageSource>);

    impl Render for View {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .children(self.0.clone().map(|source| img(source).size_full()))
        }
    }

    fn image(index: u8) -> RetainedImage {
        image_with_format(index, false)
    }

    fn image_with_format(index: u8, sticker: bool) -> RetainedImage {
        let pixels = image::RgbaImage::from_pixel(1024, 1024, image::Rgba([index, 7, 11, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(pixels)
            .write_to(
                &mut bytes,
                if sticker {
                    image::ImageFormat::WebP
                } else {
                    image::ImageFormat::Png
                },
            )
            .unwrap();
        RetainedImage::new(Image::from_bytes(
            if sticker {
                gpui::ImageFormat::Webp
            } else {
                gpui::ImageFormat::Png
            },
            bytes.into_inner(),
        ))
    }

    #[gpui::test]
    fn eviction_waits_for_visible_elements_and_reopening_uses_the_same_decode(
        cx: &mut TestAppContext,
    ) {
        let owner = image(1);
        let source = owner.source(false);
        let handle = cx.open_window(gpui::size(gpui::px(40.), gpui::px(40.)), |_, _| {
            View(Some(source))
        });
        let mut window = gpui::VisualTestContext::from_window(handle.into(), cx);
        window.run_until_parked();
        let decoded = window.read(|cx| {
            owner.0.assets.borrow()[&(false, Fit::Full)]
                .read(cx)
                .output
                .clone()
                .unwrap()
        });
        let weak = Arc::downgrade(&decoded);
        let mut probe = RenderImage::new(smallvec::smallvec![image::Frame::new(
            image::RgbaImage::new(1, 1)
        )]);
        probe.id = decoded.id;
        let source = Source {
            image: owner.0.image.clone(),
            sticker: false,
            fit: Fit::Full,
        };
        drop(decoded);

        // Closing while still cached must neither remove nor decode it again.
        handle
            .update(&mut window, |view, _, cx| {
                view.0 = None;
                cx.notify();
            })
            .unwrap();
        window.run_until_parked();
        handle
            .update(&mut window, |view, _, cx| {
                view.0 = Some(owner.source(false));
                cx.notify();
            })
            .unwrap();
        window.run_until_parked();
        assert!(window.read(|cx| {
            Arc::ptr_eq(
                &weak.upgrade().unwrap(),
                owner.0.assets.borrow()[&(false, Fit::Full)]
                    .read(cx)
                    .output
                    .as_ref()
                    .unwrap(),
            )
        }));

        // The cache evicts it while a surface still paints it.
        drop(owner);
        window.run_until_parked();
        assert!(weak.upgrade().is_some());
        assert!(window.read(|cx| cx.has_asset::<Decoder>(&source)));
        assert!(window.update(|window, _| window.has_image_atlas_entry(&probe)));

        handle
            .update(&mut window, |view, _, cx| {
                view.0 = None;
                cx.notify();
            })
            .unwrap();
        window.run_until_parked();
        window.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        window.run_until_parked();
        assert!(!window.read(|cx| cx.has_asset::<Decoder>(&source)));
        assert!(
            weak.upgrade().is_none(),
            "the decoded pixels have no remaining owner"
        );
        assert!(!window.update(|window, _| window.has_image_atlas_entry(&probe)));
    }

    #[gpui::test]
    fn closing_before_decode_finishes_does_not_repopulate_the_asset_cache(cx: &mut TestAppContext) {
        let owner = image(2);
        let source = Source {
            image: owner.0.image.clone(),
            sticker: false,
            fit: Fit::Full,
        };
        let handle = cx.open_window(gpui::size(gpui::px(40.), gpui::px(40.)), |_, _| {
            View(Some(owner.source(false)))
        });
        let mut window = gpui::VisualTestContext::from_window(handle.into(), cx);
        // Start drawing without pumping the background executor to completion.
        window.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        assert!(window.read(|cx| cx.has_asset::<Decoder>(&source)));
        assert!(window.read(|cx| owner.decoded(false, cx).is_none()));
        drop(owner);
        handle
            .update(&mut window, |view, _, cx| {
                view.0 = None;
                cx.notify();
            })
            .unwrap();
        window.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
        window.run_until_parked();
        assert!(!window.read(|cx| cx.has_asset::<Decoder>(&source)));
    }

    #[gpui::test]
    fn cached_paint_keeps_pixels_and_atlas_until_the_surface_repaints(cx: &mut TestAppContext) {
        struct Pane {
            source: Option<ImageSource>,
            renders: usize,
        }
        impl Render for Pane {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                self.renders += 1;
                div()
                    .size_full()
                    .children(self.source.clone().map(|source| img(source).size_full()))
            }
        }
        struct Root(Entity<Pane>);
        impl Render for Root {
            fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
                self.0
                    .clone()
                    .cached(gpui::StyleRefinement::default().size_full())
            }
        }
        let owner = image(3);
        let handle = cx.open_window(gpui::size(gpui::px(40.), gpui::px(40.)), |_, cx| {
            Root(cx.new(|_| Pane {
                source: Some(owner.source(false)),
                renders: 0,
            }))
        });
        cx.run_until_parked();
        let decoded = cx.read(|cx| owner.decoded(false, cx).unwrap());
        let weak = Arc::downgrade(&decoded);
        let mut probe = RenderImage::new(smallvec::smallvec![image::Frame::new(
            image::RgbaImage::new(1, 1)
        )]);
        probe.id = decoded.id;
        drop(decoded);
        drop(owner);
        let renders = handle
            .update(cx, |root, _, cx| {
                let renders = root.0.read(cx).renders;
                root.0.update(cx, |pane, _| pane.source = None);
                cx.notify();
                renders
            })
            .unwrap();
        cx.run_until_parked();
        handle
            .update(cx, |root, window, cx| {
                assert_eq!(
                    root.0.read(cx).renders,
                    renders,
                    "cached paint must be reused"
                );
                assert!(weak.upgrade().is_some());
                assert!(window.has_image_atlas_entry(&probe));
                root.0.update(cx, |_, cx| cx.notify());
            })
            .unwrap();
        cx.run_until_parked();
        handle.update(cx, |_, _, _| {}).unwrap();
        handle
            .update(cx, |_, window, _| {
                assert!(weak.upgrade().is_none());
                assert!(!window.has_image_atlas_entry(&probe));
            })
            .unwrap();
    }

    #[gpui::test]
    fn a_shared_image_survives_another_window_closing(cx: &mut TestAppContext) {
        let owner = image(4);
        let windows: Vec<_> = (0..2)
            .map(|_| {
                cx.open_window(gpui::size(gpui::px(40.), gpui::px(40.)), |_, _| {
                    View(Some(owner.source(false)))
                })
            })
            .collect();
        cx.run_until_parked();
        let decoded = cx.read(|cx| owner.decoded(false, cx).unwrap());
        let weak = Arc::downgrade(&decoded);
        let mut probe = RenderImage::new(smallvec::smallvec![image::Frame::new(
            image::RgbaImage::new(1, 1)
        )]);
        probe.id = decoded.id;
        drop(decoded);
        drop(owner);
        windows[0]
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        windows[1]
            .update(cx, |view, window, cx| {
                assert!(weak.upgrade().is_some());
                assert!(window.has_image_atlas_entry(&probe));
                view.0 = None;
                cx.notify();
            })
            .unwrap();
        cx.run_until_parked();
        windows[1].update(cx, |_, _, _| {}).unwrap();
        windows[1]
            .update(cx, |_, window, _| {
                assert!(weak.upgrade().is_none());
                assert!(!window.has_image_atlas_entry(&probe));
            })
            .unwrap();
    }

    #[gpui::test]
    fn independently_constructed_identical_images_share_retirement(cx: &mut TestAppContext) {
        for sticker in [false, true] {
            let first = image_with_format(5, sticker);
            let second = RetainedImage::new(Image::from_bytes(first.format, first.bytes.clone()));
            assert!(!Rc::ptr_eq(&first.0, &second.0));
            assert_eq!(first.id, second.id);
            let windows: Vec<_> = [&first, &second]
                .into_iter()
                .map(|owner| {
                    cx.open_window(gpui::size(gpui::px(40.), gpui::px(40.)), |_, _| {
                        View(Some(owner.source(sticker)))
                    })
                })
                .collect();
            cx.run_until_parked();
            let decoded = cx.read(|cx| first.decoded(sticker, cx).unwrap());
            assert!(cx.read(|cx| Arc::ptr_eq(&decoded, &second.decoded(sticker, cx).unwrap())));
            let weak = Arc::downgrade(&decoded);
            let source = Source {
                image: first.0.image.clone(),
                sticker,
                fit: Fit::Full,
            };
            let mut probe = RenderImage::new(smallvec::smallvec![image::Frame::new(
                image::RgbaImage::new(1, 1)
            )]);
            probe.id = decoded.id;
            drop(decoded);
            drop(first);
            windows[0]
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
            cx.run_until_parked();
            windows[1]
                .update(cx, |_, window, cx| {
                    assert!(
                        cx.has_asset::<Decoder>(&source),
                        "another message still owns this decode"
                    );
                    assert!(window.has_image_atlas_entry(&probe));
                })
                .unwrap();
            drop(second);
            windows[1]
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
            cx.run_until_parked();
            cx.update(|cx| {
                assert!(!cx.has_asset::<Decoder>(&source));
                assert!(weak.upgrade().is_none());
                assert!(cx.default_global::<Assets>().0.is_empty());
            });
        }
    }

    /// Reproducible native diagnostic, kept out of the ordinary test run.
    /// The legacy arm uses the same decoder without an owner, matching GPUI's
    /// global asset retention before this change. RSS/PSS are observations;
    /// live pixel bytes are counted through weak references to actual outputs.
    #[cfg(target_os = "linux")]
    #[gpui::test]
    #[ignore]
    fn synthetic_image_lifecycle_report(cx: &mut TestAppContext) {
        fn snapshot() -> String {
            let memory = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap();
            let memory = memory
                .lines()
                .filter(|line| line.starts_with("Rss:") || line.starts_with("Pss:"))
                .collect::<Vec<_>>()
                .join(" ");
            let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
            let fields: Vec<_> = stat
                .rsplit_once(')')
                .unwrap()
                .1
                .split_whitespace()
                .collect();
            format!(
                "{memory} cpu_ticks={}",
                fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap()
            )
        }
        let handle = cx.open_window(gpui::size(gpui::px(40.), gpui::px(40.)), |_, _| View(None));
        let mut window = gpui::VisualTestContext::from_window(handle.into(), cx);
        let arms = match std::env::var("OXIDEZAP_IMAGE_PROFILE_MODE").as_deref() {
            Ok("legacy") => vec![true],
            Ok("owned") => vec![false],
            _ => vec![true, false],
        };
        for legacy in arms {
            let mut cache = std::collections::VecDeque::new();
            let mut sources = Vec::new();
            let mut outputs = Vec::new();
            println!("legacy={legacy} begin {}", snapshot());
            for index in 0..80 {
                let owner = image(index);
                let source = Source {
                    image: owner.0.image.clone(),
                    sticker: false,
                };
                let drawn = if legacy {
                    let source = source.clone();
                    ImageSource::from(move |window: &mut Window, cx: &mut App| {
                        window.use_asset::<Decoder>(&source, cx)
                    })
                } else {
                    owner.source(false)
                };
                handle
                    .update(&mut window, |view, _, cx| {
                        view.0 = Some(drawn);
                        cx.notify();
                    })
                    .unwrap();
                window.run_until_parked();
                let output =
                    window.update(|_, cx| cx.fetch_asset::<Decoder>(&source).unwrap().unwrap());
                outputs.push(Arc::downgrade(&output));
                sources.push(source);
                cache.push_back(owner);
                if cache.len() > 50 {
                    cache.pop_front();
                }
            }
            let live = || {
                outputs
                    .iter()
                    .filter_map(std::sync::Weak::upgrade)
                    .map(|output| output.as_bytes(0).unwrap().len())
                    .sum::<usize>()
            };
            window.run_until_parked();
            window.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            window.run_until_parked();
            println!(
                "legacy={legacy} after_80 live_pixels={} {}",
                live(),
                snapshot()
            );
            assert_eq!(live(), if legacy { 80 } else { 50 } * 1024 * 1024 * 4);
            println!("legacy={legacy} before_redraw {}", snapshot());
            for _ in 0..10_000 {
                window.update(|window, cx| {
                    window.refresh();
                    window.draw(cx).clear(cx);
                });
            }
            println!("legacy={legacy} after_redraw {}", snapshot());
            cache.clear();
            handle
                .update(&mut window, |view, _, cx| {
                    view.0 = None;
                    cx.notify();
                })
                .unwrap();
            window.run_until_parked();
            window.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            });
            window.run_until_parked();
            println!(
                "legacy={legacy} after_close live_pixels={} {}",
                live(),
                snapshot()
            );
            assert_eq!(live(), if legacy { 80 } else { 0 } * 1024 * 1024 * 4);
            // Release the intentionally retained baseline before the next arm.
            window.update(|_, cx| {
                for source in sources {
                    cx.remove_asset::<Decoder>(&source);
                }
            });
            window.run_until_parked();
        }
    }

    #[test]
    fn a_large_picture_is_fitted_to_its_longest_side_and_a_small_one_kept() {
        assert_eq!(fitted_size(4000, 3000, 720), (720, 540));
        assert_eq!(fitted_size(1080, 1920, 720), (405, 720));
        assert_eq!(fitted_size(300, 200, 720), (300, 200));
        assert_eq!(fitted_size(10_000, 1, 720), (720, 1));
    }

    #[test]
    fn a_fitted_decode_is_no_larger_than_its_side() {
        let decoded = decode_fitted(&image(4), PICTURE_SIDE)
            .expect("png scales")
            .expect("decodes");
        let size = decoded.size(0);
        assert_eq!((size.width.0, size.height.0), (720, 720));
    }
}
