//! Putting a picture on the system clipboard, for "Copy image".
//!
//! The picture is turned into a PNG once, off the UI thread, whatever the
//! file was — a JPEG photo, a WebP — because PNG is what every program that
//! pastes images reads. Windows gets a second copy as a device-independent
//! bitmap (`CF_DIB`): GPUI's own writer offers only the registered "PNG"
//! format there, which browsers and chat apps paste but Paint and much of
//! the older desktop do not. Everywhere else GPUI's writer is the answer.

/// A picture ready for the clipboard: its PNG, and its pixels for a format
/// that wants them raw.
pub struct ClipboardPicture {
    png: Vec<u8>,
    /// Read only where a bitmap goes on the clipboard too.
    #[cfg_attr(not(windows), allow(dead_code))]
    rgba: image::RgbaImage,
}

/// Decode `bytes` and prepare them for the clipboard. Slow for a large
/// photo, so run it on the background executor.
pub fn prepare(bytes: &[u8]) -> Result<ClipboardPicture, String> {
    let decoded = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    let rgba = decoded.to_rgba8();
    // A PNG is already the answer; anything else is encoded once.
    let png = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        bytes.to_vec()
    } else {
        let mut png = Vec::new();
        image::DynamicImage::ImageRgba8(rgba.clone())
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        png
    };
    Ok(ClipboardPicture { png, rgba })
}

/// Replace the clipboard's contents with `picture`.
pub fn write(picture: ClipboardPicture, cx: &mut gpui::App) -> Result<(), String> {
    imp::write(picture, cx)
}

/// The picture as a bottom-up 32-bit `BITMAPINFOHEADER` DIB, the layout
/// `CF_DIB` means. Transparency is laid over white: most programs that read
/// a DIB ignore its alpha, and a transparent corner would paste black.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
fn dib(rgba: &image::RgbaImage) -> Vec<u8> {
    let (width, height) = rgba.dimensions();
    let pixels = width as usize * height as usize * 4;
    let mut out = Vec::with_capacity(40 + pixels);
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight: bottom-up
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression: BI_RGB
    out.extend_from_slice(&(pixels as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&[0; 16]); // resolution and palette: unused
    for row in (0..height).rev() {
        for x in 0..width {
            let [r, g, b, a] = rgba.get_pixel(x, row).0;
            let over_white = |c: u8| ((c as u32 * a as u32 + 255 * (255 - a as u32)) / 255) as u8;
            out.extend_from_slice(&[over_white(b), over_white(g), over_white(r), 255]);
        }
    }
    out
}

#[cfg(windows)]
mod imp {
    use super::ClipboardPicture;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };

    /// `CF_DIB`, from `winuser.h`.
    const CF_DIB: u32 = 8;

    pub(super) fn write(picture: ClipboardPicture, _cx: &mut gpui::App) -> Result<(), String> {
        let dib = super::dib(&picture.rgba);
        let png_name: Vec<u16> = "PNG\0".encode_utf16().collect();
        // SAFETY: plain Win32 clipboard calls. The clipboard is opened once
        // and closed on every path out, and each handle given to
        // `SetClipboardData` belongs to the system once it succeeds.
        unsafe {
            let png_format = RegisterClipboardFormatW(png_name.as_ptr());
            // Another program may hold the clipboard for a moment.
            let mut opened = false;
            for _ in 0..5 {
                if OpenClipboard(std::ptr::null_mut()) != 0 {
                    opened = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            if !opened {
                return Err("the clipboard is busy".to_string());
            }
            let result = (|| {
                if EmptyClipboard() == 0 {
                    return Err("the clipboard could not be cleared".to_string());
                }
                let mut wrote = false;
                if png_format != 0 {
                    wrote |= set(png_format, &picture.png);
                }
                wrote |= set(CF_DIB, &dib);
                if wrote {
                    Ok(())
                } else {
                    Err("the clipboard refused the picture".to_string())
                }
            })();
            CloseClipboard();
            result
        }
    }

    /// Hand `bytes` to the open clipboard as `format`.
    unsafe fn set(format: u32, bytes: &[u8]) -> bool {
        // SAFETY: the caller holds the clipboard open; the block is sized to
        // `bytes` and only written while locked.
        unsafe {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
            if handle.is_null() {
                return false;
            }
            let target = GlobalLock(handle);
            if target.is_null() {
                GlobalFree(handle);
                return false;
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
            GlobalUnlock(handle);
            if SetClipboardData(format, handle).is_null() {
                GlobalFree(handle);
                return false;
            }
            true
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::ClipboardPicture;

    pub(super) fn write(picture: ClipboardPicture, cx: &mut gpui::App) -> Result<(), String> {
        cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image::from_bytes(
            gpui::ImageFormat::Png,
            picture.png,
        )));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([200, 10, 30]),
        ))
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
        bytes
    }

    #[test]
    fn a_photo_becomes_a_png_of_the_same_size() {
        let picture = prepare(&jpeg(5, 3)).expect("decodes");
        assert!(picture.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let back = image::load_from_memory(&picture.png).unwrap();
        assert_eq!((back.width(), back.height()), (5, 3));
    }

    #[test]
    fn bytes_that_are_not_a_picture_are_refused() {
        assert!(prepare(b"not an image").is_err());
    }

    #[test]
    fn the_bitmap_is_bottom_up_bgr_over_white() {
        let mut rgba = image::RgbaImage::new(2, 2);
        rgba.put_pixel(0, 0, image::Rgba([255, 0, 0, 255])); // top-left red
        rgba.put_pixel(1, 1, image::Rgba([0, 0, 0, 0])); // bottom-right clear
        let dib = dib(&rgba);
        assert_eq!(dib.len(), 40 + 2 * 2 * 4);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        // The first row stored is the bottom one: its second pixel was clear,
        // so it is white.
        assert_eq!(&dib[44..48], &[255, 255, 255, 255]);
        // The top-left pixel is the first of the last row, as blue-green-red.
        assert_eq!(&dib[48..52], &[0, 0, 255, 255]);
    }
}
