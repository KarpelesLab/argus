//! Geometry and color primitives (Layer 0).
//!
//! Phase 0 needs only enough to describe a framebuffer: integer device-pixel
//! sizes/points/rects and an RGBA color. CSS units, transforms, and float
//! coordinate spaces arrive with layout in Phase 1.

mod color;
mod rect;

pub use color::Color;
pub use rect::{Point, Rect, Size};
