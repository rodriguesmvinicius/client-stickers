//! The tray dot the icon-less platforms draw.
//!
//! StatusNotifierItem names icons out of the user's theme, but a notification
//! area and a menu bar carry their own pixels — so Windows and macOS paint
//! the same 32×32 dot instead of naming one: grey while disconnected, green
//! while connected, amber while something waits to be read. Drawn here,
//! once, rather than once per tray, so no platform can disagree with another
//! about which state gets which colour. Dependency-free on purpose: this
//! module compiles everywhere, and its tests run on every platform.

use crate::state::TrayState;

/// The icon's size. Small on purpose: a menu bar renders it at eighteen
/// points, and a dot needs no more than this to stay round there.
pub const SIDE: u32 = 32;

/// Grey while the connection is down: what was last heard is then a number
/// nothing is refreshing, and an icon asking to be looked at over a stale
/// count is worse than one saying the connection is what is wrong.
#[cfg(any(test, target_os = "macos"))]
pub const DISCONNECTED: [u8; 4] = [0x80, 0x80, 0x80, 0xFF];
/// Green while connected with nothing waiting.
#[cfg(any(test, target_os = "macos"))]
pub const CONNECTED: [u8; 4] = [0x2E, 0xA0, 0x43, 0xFF];
/// Amber while something waits to be read.
pub const UNREAD: [u8; 4] = [0xD6, 0x45, 0x41, 0xFF];

/// Which dot a state gets. One function rather than one per tray, so the
/// icon and anything reasoning about it cannot disagree.
#[cfg(any(test, target_os = "macos"))]
#[must_use]
pub fn colour_for(state: &TrayState) -> [u8; 4] {
    match (state.connected, state.shown_unread()) {
        (false, _) => DISCONNECTED,
        (true, 0) => CONNECTED,
        (true, _) => UNREAD,
    }
}

/// A filled circle on transparency: the hosts draw it small, and a square of
/// colour would read as a chip off a theme.
#[cfg(any(test, target_os = "macos"))]
#[must_use]
pub fn dot(colour: [u8; 4]) -> Vec<u8> {
    let side = SIDE as f32;
    let (centre, radius) = ((side - 1.0) / 2.0, (side - 2.0) / 2.0);
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for y in 0..SIDE {
        for x in 0..SIDE {
            let distance = ((x as f32 - centre).powi(2) + (y as f32 - centre).powi(2)).sqrt();
            if distance <= radius {
                rgba.extend_from_slice(&colour);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    rgba
}

/// The application's own icon carrying the same three states as the dot, so
/// the tray shows which program it is and not only how it is doing: plain
/// while connected with nothing waiting, a red badge in the corner while
/// something waits, and faded to grey while the connection is down.
#[cfg(any(test, target_os = "windows"))]
#[must_use]
pub fn badged_app_icon(state: &TrayState) -> Vec<u8> {
    let mut rgba = super::app_icon::APP_ICON.to_vec();
    if !state.connected {
        for pixel in rgba.as_chunks_mut::<4>().0 {
            let [r, g, b, a] = [pixel[0], pixel[1], pixel[2], pixel[3]];
            let luma = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000;
            let luma = u8::try_from(luma).unwrap_or(u8::MAX);
            pixel.copy_from_slice(&[luma, luma, luma, a / 4 * 3]);
        }
        return rgba;
    }
    if state.shown_unread() > 0 {
        // Top right, over the balloon's edge, ringed in white so it reads
        // on a dark taskbar and a light one alike.
        let (cx, cy, radius, rim) = (24.5_f32, 7.5_f32, 7.0_f32, 1.5_f32);
        for y in 0..SIDE {
            for x in 0..SIDE {
                let distance = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
                let colour = if distance <= radius - rim {
                    UNREAD
                } else if distance <= radius {
                    [0xFF, 0xFF, 0xFF, 0xFF]
                } else {
                    continue;
                };
                let at = ((y * SIDE + x) * 4) as usize;
                rgba[at..at + 4].copy_from_slice(&colour);
            }
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(connected: bool, unread: u32) -> TrayState {
        TrayState { connected, unread }
    }

    /// The report Linux got first: messages waiting behind an icon that
    /// looked exactly like an idle one. Three colours, and the waiting one
    /// is not the idle one.
    #[test]
    fn unread_reaches_the_icon_itself() {
        assert_ne!(
            dot(colour_for(&state(true, 3))),
            dot(colour_for(&state(true, 0))),
            "an icon with something to read must not look like one without"
        );
        assert_ne!(
            dot(colour_for(&state(false, 3))),
            dot(colour_for(&state(true, 0))),
            "a disconnected icon must not look connected"
        );
    }

    /// A count nothing is refreshing is not news: the icon goes grey rather
    /// than holding a stale colour.
    #[test]
    fn a_disconnected_icon_is_grey_whatever_it_last_heard() {
        assert_eq!(
            dot(colour_for(&state(false, 3))),
            dot(colour_for(&state(false, 0)))
        );
        assert_eq!(colour_for(&state(false, 3)), DISCONNECTED);
    }

    /// The tray's app icon tells the same three states apart as the dot.
    #[test]
    fn the_app_icon_carries_the_state_too() {
        let idle = badged_app_icon(&state(true, 0));
        let waiting = badged_app_icon(&state(true, 3));
        let offline = badged_app_icon(&state(false, 3));
        assert_eq!(idle.len(), (SIDE * SIDE * 4) as usize);
        assert_eq!(idle, super::super::app_icon::APP_ICON.to_vec());
        assert_ne!(idle, waiting, "unread has to show");
        assert_ne!(idle, offline, "offline has to show");
        assert_eq!(offline, badged_app_icon(&state(false, 0)));
        let badge = ((7 * SIDE + 24) * 4) as usize;
        assert_eq!(&waiting[badge..badge + 4], &UNREAD);
    }

    /// Thirty-two by thirty-two of RGBA, with something drawn and something
    /// transparent: a square of colour would read as a chip off a theme.
    #[test]
    fn the_dot_is_round() {
        let pixels = dot(CONNECTED);
        assert_eq!(pixels.len(), 32 * 32 * 4);
        // Corners fall outside the circle.
        assert_eq!(&pixels[0..4], &[0, 0, 0, 0]);
        // The centre is the colour.
        let centre = (16 * 32 + 16) * 4;
        assert_eq!(&pixels[centre..centre + 4], &CONNECTED);
    }
}
