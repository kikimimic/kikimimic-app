//! The palette, named. Values are the ones the window has always used
//! (GitHub Primer greys and blues), so the v4 layout keeps today's look.

use gpui::{Rgba, rgb};

pub fn fg() -> Rgba {
    rgb(0x1f2328)
}
pub fn muted() -> Rgba {
    rgb(0x8c8c8c)
}
pub fn border() -> Rgba {
    rgb(0xd0d7de)
}
pub fn subtle() -> Rgba {
    rgb(0xf6f8fa)
}
pub fn danger() -> Rgba {
    rgb(0xcf222e)
}
pub fn warn() -> Rgba {
    rgb(0x9a6700)
}
pub fn ok() -> Rgba {
    rgb(0x1a7f37)
}
