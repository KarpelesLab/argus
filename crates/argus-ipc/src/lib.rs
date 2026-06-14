//! Inter-process transport for Argus (Layer 0).
//!
//! This crate is **pure transport** — it knows nothing about browser message
//! semantics (those live in `argus-protocol`). It provides three things the
//! multi-process model in `docs/PROCESS_MODEL.md` needs:
//!
//! * [`Channel`] — a length-prefixed message stream over a UNIX socket, able to
//!   carry file descriptors out-of-band (SCM_RIGHTS).
//! * [`SharedMemory`] — an anonymous POSIX shared-memory region whose fd can be
//!   sent over a [`Channel`] and mapped on the other side.
//! * the `sys` glue that keeps the raw `libc` calls in one audited place.

mod channel;
mod shm;
mod sys;

pub use channel::Channel;
pub use shm::SharedMemory;
