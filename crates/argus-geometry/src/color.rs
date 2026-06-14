//! Color.

/// An 8-bit-per-channel color. Channels are stored straight (non-premultiplied).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color::rgba(0, 0, 0, 0);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const WHITE: Color = Color::rgb(255, 255, 255);

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Color {
        Color { r, g, b, a }
    }

    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::rgba(r, g, b, 255)
    }

    /// Pack into `0xRRGGBBAA` byte order (R in the most-significant byte).
    pub const fn to_rgba8(self) -> [u8; 4] {
        [self.r, self.g, self.b, self.a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_in_rgba_order() {
        assert_eq!(Color::rgb(1, 2, 3).to_rgba8(), [1, 2, 3, 255]);
        assert_eq!(Color::TRANSPARENT.to_rgba8(), [0, 0, 0, 0]);
    }
}
