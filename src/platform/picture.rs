//! A picture under the overlay, prepared: what every platform that shows one
//! shares — how large and how blurred each picture is prepared, and the cache
//! that prepares each once, in the background, as soon as it's chosen. How a
//! wallpaper is found, how a file is decoded, and where the picture is shown
//! are each platform's own ([`Source`]).
//!
//! **Every picture is prepared once, in the background, as soon as it's
//! chosen**, and the wallpaper is read once per launch. Summoning is the one
//! moment that has to be instant, and reading and blurring then was the lag
//! a stale picture is better than.

use std::cell::RefCell;
use std::sync::{Arc, OnceLock};

use super::BackdropPicture;

/// A picture at a blur — `Config::overlay_blur`, by its bits. Prepared at one
/// blur, it isn't the picture asked for at another.
pub(super) type Picture = (BackdropPicture, u32);

/// A picture as PNG, filled in by a background thread — `None` inside if it
/// couldn't be read.
pub(super) type Prepared = Arc<OnceLock<Option<Vec<u8>>>>;

/// The picture chosen, and every one prepared this launch.
type Pictures = (Option<Picture>, Vec<(Picture, Prepared)>);

thread_local! {
    static PICTURES: RefCell<Pictures> = const { RefCell::new((None, Vec::new())) };
}

/// How a platform finds and readies a picture.
pub(super) struct Source {
    /// The wallpaper's file, looked up on the main thread.
    pub wallpaper: fn() -> Option<String>,
    /// The screen's longest side in pixels, where the platform can say.
    pub screen: fn() -> Option<u32>,
    /// The picture at a path, scaled to fit a size and blurred by a sigma, as
    /// PNG; `None` if it can't be read. Run on a background thread.
    pub make: fn(&str, u32, f32) -> Option<Vec<u8>>,
}

/// Chooses `picture` at `blur`, and starts preparing it.
pub(super) fn choose(picture: BackdropPicture, blur: f32, source: &Source) {
    let picture = (picture, blur.to_bits());
    let _ = prepared(&picture, source);
    PICTURES.with(|pictures| pictures.borrow_mut().0 = Some(picture));
}

/// The picture chosen last, if any — for macOS's window, which shows it;
/// elsewhere the app draws what it asked for.
#[cfg(target_os = "macos")]
pub(super) fn chosen() -> Option<Picture> {
    PICTURES.with(|pictures| pictures.borrow().0.clone())
}

/// Hands `picture` at `blur` to `ready` once it's prepared, from a background
/// thread.
pub(super) fn preview(
    picture: BackdropPicture,
    blur: f32,
    source: &Source,
    ready: impl FnOnce(Option<Vec<u8>>) + Send + 'static,
) {
    let prepared = prepared(&(picture, blur.to_bits()), source);
    std::thread::spawn(move || ready(prepared.wait().clone()));
}

/// `picture`, prepared or being prepared — started now, in the background, if
/// this is the first time it's asked for. Main thread, where the wallpaper's
/// file and the screen's size are looked up.
pub(super) fn prepared(picture: &Picture, source: &Source) -> Prepared {
    PICTURES.with(|pictures| {
        let mut pictures = pictures.borrow_mut();
        if let Some((_, prepared)) = pictures.1.iter().find(|(p, _)| p == picture) {
            return prepared.clone();
        }
        let prepared = Prepared::default();
        pictures.1.push((picture.clone(), prepared.clone()));
        // The wallpaper heavily — it's there for the colour and the light, not
        // the picture — and a picture of your own lightly, so it's still the
        // picture you chose. Each as a share of the picture's longest side, so
        // it's the same blur at whatever size it's prepared: 12 px at 640 and
        // 2.5 px at 1280, before `overlay_blur` scales them.
        let (path, spread) = match &picture.0 {
            BackdropPicture::Wallpaper => ((source.wallpaper)(), 12.0 / 640.0),
            BackdropPicture::File(path) => (Some(path.to_string_lossy().into_owned()), 2.5 / 1280.0),
        };
        // A 4K display, should the screen not say.
        let screen = (source.screen)().unwrap_or(3840);
        let (size, sigma) = preparation(spread * f32::from_bits(picture.1), screen);
        let (filling, make) = (prepared.clone(), source.make);
        std::thread::spawn(move || {
            let _ = filling.set(path.as_deref().and_then(|path| make(path, size, sigma)));
        });
        prepared
    })
}

/// A blur, in pixels of the picture as prepared, past which scaling it up to
/// the screen doesn't show — what a picture of your own was always prepared
/// with, at 1280.
const HIDES_SCALING: f32 = 2.5;

/// The smallest a picture is prepared: below it a blur can't hide the
/// scaling, however wide.
const MIN_PICTURE: u32 = 640;

/// How large to prepare a picture blurred by `spread` of its longest side, and
/// the blur in pixels at that size. As small as still hides being scaled up to
/// the screen — the less blur, the larger — and at most `screen`, the screen's
/// own longest side, which is where a sharp one goes.
fn preparation(spread: f32, screen: u32) -> (u32, f32) {
    if spread <= 0.0 {
        return (screen, 0.0);
    }
    let size = ((HIDES_SCALING / spread).round() as u32).clamp(MIN_PICTURE.min(screen), screen);
    (size, spread * size as f32)
}

/// `image` blurred by `sigma`, as PNG — none at 0, which `blur` would take for
/// its own default.
pub(super) fn blurred_png(image: ::image::RgbaImage, sigma: f32) -> Option<Vec<u8>> {
    let image = if sigma > 0.0 { ::image::imageops::blur(&image, sigma) } else { image };
    let mut bytes = Vec::new();
    ::image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut bytes), ::image::ImageFormat::Png)
        .ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_blur_prepares_pictures_as_they_always_were() {
        let screen = 3024;
        assert_eq!(preparation(12.0 / 640.0, screen), (640, 12.0), "the wallpaper");
        assert_eq!(preparation(2.5 / 1280.0, screen), (1280, 2.5), "a picture of your own");
    }

    #[test]
    fn less_blur_is_prepared_larger_and_none_at_the_screen() {
        let screen = 3024;
        // Half the blur: twice the size, the same 2.5 px — half as wide a share.
        assert_eq!(preparation(1.25 / 1280.0, screen), (2560, 2.5));
        assert_eq!(preparation(0.0, screen), (screen, 0.0), "sharp is the screen, unblurred");
        // Never larger than the screen, however little the blur.
        assert_eq!(preparation(0.1 / 1280.0, screen).0, screen);
        // Twice the wallpaper's blur stays at the floor, twice as wide there.
        assert_eq!(preparation(24.0 / 640.0, screen), (640, 24.0));
    }
}
