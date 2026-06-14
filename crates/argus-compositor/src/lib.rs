//! Compositing (Phase 0 slice).
//!
//! The full compositor assembles layer bitmaps into a window surface (see
//! `docs/subsystems/rendering.md`). Phase 0 needs only the shared artifact that
//! crosses the process boundary: a [`Framebuffer`] — an RGBA8 pixel buffer backed
//! by `argus-ipc` shared memory, produced in a content process and mapped by the
//! browser process.

use argus_geometry::{Color, Size};
use argus_ipc::SharedMemory;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};

/// Bytes per pixel (RGBA8).
pub const BYTES_PER_PIXEL: usize = 4;

/// A shared-memory RGBA8 framebuffer of a fixed size.
pub struct Framebuffer {
    shm: SharedMemory,
    size: Size,
}

impl Framebuffer {
    /// Allocate a new framebuffer sized for `size` (must be non-empty).
    pub fn create(size: Size) -> io::Result<Framebuffer> {
        assert!(!size.is_empty(), "framebuffer size must be non-empty");
        let shm = SharedMemory::create(byte_len(size))?;
        Ok(Framebuffer { shm, size })
    }

    /// Map a framebuffer received as a shared-memory `fd` with known `size`.
    pub fn from_fd(fd: OwnedFd, size: Size) -> io::Result<Framebuffer> {
        assert!(!size.is_empty(), "framebuffer size must be non-empty");
        let shm = SharedMemory::from_fd(fd, byte_len(size))?;
        Ok(Framebuffer { shm, size })
    }

    /// The framebuffer dimensions.
    pub fn size(&self) -> Size {
        self.size
    }

    /// The backing shared-memory fd, for passing over an `argus-ipc` channel.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.shm.as_fd()
    }

    /// Raw RGBA8 bytes, row-major, top-to-bottom.
    pub fn pixels(&self) -> &[u8] {
        &self.shm.as_slice()[..byte_len(self.size)]
    }

    /// Mutable raw RGBA8 bytes, for compositing into the framebuffer.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        let n = byte_len(self.size);
        &mut self.shm.as_mut_slice()[..n]
    }

    /// Paint every pixel `color`.
    pub fn fill(&mut self, color: Color) {
        let rgba = color.to_rgba8();
        let bytes = &mut self.shm.as_mut_slice()[..byte_len(self.size)];
        for px in bytes.chunks_exact_mut(BYTES_PER_PIXEL) {
            px.copy_from_slice(&rgba);
        }
    }

    /// Read the pixel at `(x, y)`. Panics if out of bounds.
    pub fn pixel(&self, x: u32, y: u32) -> Color {
        assert!(
            x < self.size.width && y < self.size.height,
            "pixel out of bounds"
        );
        let i = (y as usize * self.size.width as usize + x as usize) * BYTES_PER_PIXEL;
        let p = &self.pixels()[i..i + BYTES_PER_PIXEL];
        Color::rgba(p[0], p[1], p[2], p[3])
    }
}

/// Number of bytes required to back a framebuffer of `size`.
pub fn byte_len(size: Size) -> usize {
    size.area() * BYTES_PER_PIXEL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_then_sample() {
        let mut fb = Framebuffer::create(Size::new(8, 4)).unwrap();
        let teal = Color::rgb(0, 128, 128);
        fb.fill(teal);
        assert_eq!(fb.pixel(0, 0), teal);
        assert_eq!(fb.pixel(7, 3), teal);
        assert_eq!(fb.pixels().len(), 8 * 4 * 4);
    }
}
