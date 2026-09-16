//! The icons as pixels, for the places that take pixels instead of an icon
//! name: the tray icon for the tray's `IconPixmap`, and the app icon for the
//! window icon (X11's `_NET_WM_ICON`). Rendered from `assets/icons/tray.svg`
//! and `assets/icons/app.svg` by `tools/icons.sh`, because the patched Slint
//! can't decode images.

/// One size of the icon: ARGB32, bytes A, R, G, B per pixel, straight alpha,
/// rows top to bottom (the `StatusNotifierItem` `IconPixmap` layout).
pub struct Pixmap {
    pub size_px: u16,
    pub argb: &'static [u8],
}

/// The tray icon, smallest first.
pub const PIXMAPS: [Pixmap; 4] = [
    Pixmap { size_px: 16, argb: include_bytes!("../assets/icons/tray-16.argb") },
    Pixmap { size_px: 22, argb: include_bytes!("../assets/icons/tray-22.argb") },
    Pixmap { size_px: 32, argb: include_bytes!("../assets/icons/tray-32.argb") },
    Pixmap { size_px: 48, argb: include_bytes!("../assets/icons/tray-48.argb") },
];

/// The app icon, for the window icon.
pub const APP: Pixmap = Pixmap { size_px: 48, argb: include_bytes!("../assets/icons/app-48.argb") };

/// The app icon as a Slint image, for `Window.icon`.
pub fn window_icon() -> slint::Image {
    let side_px = u32::from(APP.size_px);
    assert_eq!(APP.argb.len(), 4 * usize::from(APP.size_px) * usize::from(APP.size_px));
    let mut buffer = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(side_px, side_px);
    assert_eq!(buffer.make_mut_slice().len(), APP.argb.len() / 4);
    for (pixel, argb) in buffer.make_mut_slice().iter_mut().zip(APP.argb.chunks_exact(4)) {
        let [a, r, g, b] = [argb[0], argb[1], argb[2], argb[3]];
        *pixel = slint::Rgba8Pixel { r, g, b, a };
    }
    slint::Image::from_rgba8(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argb_at(pixmap: &Pixmap, x: usize, y: usize) -> [u8; 4] {
        let at = 4 * (y * usize::from(pixmap.size_px) + x);
        [pixmap.argb[at], pixmap.argb[at + 1], pixmap.argb[at + 2], pixmap.argb[at + 3]]
    }

    #[test]
    fn every_pixmap_matches_its_size() {
        for pixmap in PIXMAPS.iter().chain([&APP]) {
            let side = usize::from(pixmap.size_px);
            assert_eq!(pixmap.argb.len(), 4 * side * side, "{side} px");
        }
        assert!(PIXMAPS.windows(2).all(|pair| pair[0].size_px < pair[1].size_px));
    }

    #[test]
    fn corners_are_clear_and_the_face_is_ginger() {
        for pixmap in &PIXMAPS {
            let side = usize::from(pixmap.size_px);
            assert_eq!(argb_at(pixmap, 0, side - 1)[0], 0, "bottom-left corner of {side} px");
            // Below the eyes, beside the nose: the ginger face (#e8914a).
            let [a, r, g, b] = argb_at(pixmap, side * 3 / 10, side * 7 / 10);
            assert_eq!(a, 255, "{side} px");
            assert!(r > g && g > b, "{side} px face is {r},{g},{b}");
        }
    }

    #[test]
    fn the_app_icon_is_a_shield_with_the_cat() {
        let side = usize::from(APP.size_px);
        for (x, y) in [(0, 0), (side - 1, 0), (0, side - 1), (side - 1, side - 1)] {
            assert_eq!(argb_at(&APP, x, y)[0], 0, "corner {x},{y}");
        }
        // Left of the head, between the web's lines: the blue shield.
        let [a, r, g, b] = argb_at(&APP, side * 11 / 48, side * 19 / 48);
        assert_eq!(a, 255);
        assert!(b > g && g > r, "the shield is {r},{g},{b}");
        // The forehead, under the stripes: ginger fur.
        let [a, r, g, b] = argb_at(&APP, side * 23 / 48, side * 25 / 48);
        assert_eq!(a, 255);
        assert!(r > g && g > b, "the fur is {r},{g},{b}");
    }

    #[test]
    fn window_icon_is_the_app_icon() {
        let size = window_icon().size();
        assert_eq!((size.width, size.height), (u32::from(APP.size_px), u32::from(APP.size_px)));
    }
}
