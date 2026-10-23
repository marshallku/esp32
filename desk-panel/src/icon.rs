//! Key icons from `desk_core::icons`, tinted with the key's text colour.
//!
//! Coverage is blended against the key's own background, which is a solid
//! fill under the icon, so nothing is read back from the frame.

use desk_core::icons::{self, SIZE};
use embedded_graphics::{pixelcolor::Rgb565, prelude::*};

/// Draws `name` with its top-left corner at `origin`. Returns false, drawing
/// nothing, for a name this firmware does not have.
pub fn draw<D: DrawTarget<Color = Rgb565>>(target: &mut D, name: &str, origin: Point, fg: Rgb565, bg: Rgb565) -> bool {
    let Some(bitmap) = icons::get(name) else { return false };
    let palette: [Rgb565; 16] = core::array::from_fn(|a| blend(bg, fg, a as u8));
    let pixels = (0..SIZE).flat_map(|y| (0..SIZE).map(move |x| (x, y))).filter_map(|(x, y)| {
        let a = icons::alpha(bitmap, x, y);
        (a > 0).then(|| Pixel(origin + Point::new(x.into(), y.into()), palette[a as usize]))
    });
    let _ = target.draw_iter(pixels);
    true
}

/// `bg` → `fg` by `a`/15, per RGB565 channel.
fn blend(bg: Rgb565, fg: Rgb565, a: u8) -> Rgb565 {
    let mix = |b: u8, f: u8| ((u16::from(b) * u16::from(15 - a) + u16::from(f) * u16::from(a) + 7) / 15) as u8;
    Rgb565::new(mix(bg.r(), fg.r()), mix(bg.g(), fg.g()), mix(bg.b(), fg.b()))
}
