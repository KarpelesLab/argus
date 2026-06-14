//! Anonymous shared memory.

use crate::sys;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};

/// A mapped, anonymous POSIX shared-memory region.
///
/// Create one with [`SharedMemory::create`], send its fd ([`SharedMemory::as_fd`])
/// over a [`crate::Channel`], and reconstruct it on the other side with
/// [`SharedMemory::from_fd`]. Both mappings then refer to the same physical pages.
pub struct SharedMemory {
    fd: OwnedFd,
    ptr: *mut u8,
    len: usize,
}

// The mapping is owned exclusively by this handle; moving it between threads is
// sound. It is deliberately not `Sync` (no internal locking).
unsafe impl Send for SharedMemory {}

impl SharedMemory {
    /// Allocate and map a new region of `len` bytes (must be non-zero).
    pub fn create(len: usize) -> io::Result<SharedMemory> {
        assert!(len > 0, "shared memory length must be non-zero");
        let fd = sys::shm_create_fd(len)?;
        // SAFETY: fd is a fresh shm object sized to `len`.
        let ptr = unsafe { sys::mmap_shared(fd.as_raw_fd(), len)? };
        Ok(SharedMemory { fd, ptr, len })
    }

    /// Map an existing region received as `fd`, known to be `len` bytes.
    pub fn from_fd(fd: OwnedFd, len: usize) -> io::Result<SharedMemory> {
        assert!(len > 0, "shared memory length must be non-zero");
        // SAFETY: caller guarantees `fd` is a shm object of at least `len` bytes.
        let ptr = unsafe { sys::mmap_shared(fd.as_raw_fd(), len)? };
        Ok(SharedMemory { fd, ptr, len })
    }

    /// Length of the region in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Always false (a region is non-empty by construction); present for lint parity.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// The region's fd, for passing over a [`crate::Channel`].
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Read-only view of the mapped bytes.
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr/len come from a successful mmap and stay valid until Drop.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    /// Mutable view of the mapped bytes.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above; `&mut self` guarantees unique access on this side.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for SharedMemory {
    fn drop(&mut self) {
        // SAFETY: ptr/len came from mmap_shared and are unmapped exactly once.
        unsafe { sys::munmap(self.ptr, self.len) };
    }
}
