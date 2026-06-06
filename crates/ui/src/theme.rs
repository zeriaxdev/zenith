//! Zed/Linear-flavored minimal palette + layout helpers.
//! Near-neutral dark grays, low saturation, one restrained accent.

use gpui::{div, hsla, prelude::*, Div, Hsla};

pub fn bg() -> Hsla { hsla(225. / 360., 0.08, 0.10, 1.) }
pub fn panel() -> Hsla { hsla(225. / 360., 0.07, 0.12, 1.) }
pub fn card() -> Hsla { hsla(225. / 360., 0.06, 0.16, 1.) }
pub fn card_hi() -> Hsla { hsla(225. / 360., 0.06, 0.21, 1.) }
pub fn border() -> Hsla { hsla(225. / 360., 0.05, 0.23, 1.) }
pub fn text() -> Hsla { hsla(220. / 360., 0.09, 0.91, 1.) }
pub fn muted() -> Hsla { hsla(220. / 360., 0.05, 0.52, 1.) }
pub fn accent() -> Hsla { hsla(146. / 360., 0.38, 0.45, 1.) }
pub fn accent_hi() -> Hsla { hsla(146. / 360., 0.42, 0.51, 1.) }
pub fn danger() -> Hsla { hsla(2. / 360., 0.55, 0.50, 1.) }
pub fn danger_hi() -> Hsla { hsla(2. / 360., 0.60, 0.56, 1.) }
pub fn white() -> Hsla { hsla(0., 0., 1., 1.) }

// ---- layout helpers ------------------------------------------------------
pub fn row() -> Div {
    div().flex().flex_row().items_center()
}
pub fn col() -> Div {
    div().flex().flex_col()
}
