//! A length-prefixed message channel over a UNIX socket, with fd passing.

use crate::sys;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Wire framing: a 4-byte little-endian payload length, then the payload. Any
/// file descriptors are attached (via SCM_RIGHTS) to the length header, so they
/// are delivered together with the start of each message regardless of how the
/// kernel chunks the stream.
pub struct Channel {
    stream: UnixStream,
}

impl Channel {
    /// Create a connected pair of channels (one per process).
    pub fn pair() -> io::Result<(Channel, Channel)> {
        let (a, b) = UnixStream::pair()?;
        Ok((Channel { stream: a }, Channel { stream: b }))
    }

    /// Adopt an already-connected socket fd (e.g. one inherited across `exec`).
    ///
    /// # Safety
    /// `fd` must be a valid, owned, connected stream-socket fd.
    pub unsafe fn from_raw_fd(fd: RawFd) -> Channel {
        Channel {
            stream: unsafe { UnixStream::from_raw_fd(fd) },
        }
    }

    /// Consume the channel, yielding the underlying socket fd (e.g. to hand to a
    /// child across `exec`). The fd is no longer closed by this channel.
    pub fn into_raw_fd(self) -> RawFd {
        self.stream.into_raw_fd()
    }

    fn raw(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Send `payload` with optional file descriptors attached.
    pub fn send(&self, payload: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
        let len = u32::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "message too large"))?;
        let header = len.to_le_bytes();
        let raw_fds: Vec<RawFd> = fds.iter().map(|f| f.as_raw_fd()).collect();

        // Write the 4-byte header, attaching the fds to the first chunk.
        let mut sent = 0;
        while sent < header.len() {
            let attach: &[RawFd] = if sent == 0 { &raw_fds } else { &[] };
            // SAFETY: raw() is a valid open socket; fds are borrowed and open.
            let n = unsafe { sys::sendmsg_fds(self.raw(), &header[sent..], attach)? };
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            sent += n;
        }

        // Write the payload (no fds). Empty payloads are legal (header only).
        let mut off = 0;
        while off < payload.len() {
            // SAFETY: raw() is a valid open socket.
            let n = unsafe { sys::sendmsg_fds(self.raw(), &payload[off..], &[])? };
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::WriteZero));
            }
            off += n;
        }
        Ok(())
    }

    /// Receive the next message: its payload bytes and any attached descriptors.
    ///
    /// Returns [`io::ErrorKind::UnexpectedEof`] when the peer has closed the
    /// channel cleanly between messages — the caller treats that as "peer gone".
    pub fn recv(&self) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
        let mut fds = Vec::new();

        let mut header = [0u8; 4];
        self.read_exact_collecting(&mut header, &mut fds)?;
        let len = u32::from_le_bytes(header) as usize;

        let mut payload = vec![0u8; len];
        if len > 0 {
            self.read_exact_collecting(&mut payload, &mut fds)?;
        }
        Ok((payload, fds))
    }

    /// Fill `buf` completely, accumulating any fds delivered along the way.
    fn read_exact_collecting(&self, buf: &mut [u8], fds: &mut Vec<OwnedFd>) -> io::Result<()> {
        let mut got = 0;
        while got < buf.len() {
            // SAFETY: raw() is a valid open socket.
            let (n, mut more) = unsafe { sys::recvmsg_fds(self.raw(), &mut buf[got..])? };
            fds.append(&mut more);
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            got += n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SharedMemory;

    #[test]
    fn round_trips_a_plain_message() {
        let (a, b) = Channel::pair().unwrap();
        a.send(b"hello argus", &[]).unwrap();
        let (msg, fds) = b.recv().unwrap();
        assert_eq!(msg, b"hello argus");
        assert!(fds.is_empty());
    }

    #[test]
    fn passes_shared_memory_across_the_channel() {
        let (a, b) = Channel::pair().unwrap();

        // Producer side: write a pattern into shared memory and send its fd.
        let mut shm = SharedMemory::create(4096).unwrap();
        shm.as_mut_slice()[..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let len = shm.len() as u32;
        a.send(&len.to_le_bytes(), &[shm.as_fd()]).unwrap();

        // Consumer side: receive the fd, map it, observe the same bytes.
        let (msg, mut fds) = b.recv().unwrap();
        assert_eq!(u32::from_le_bytes(msg.try_into().unwrap()), 4096);
        assert_eq!(fds.len(), 1);
        let mapped = SharedMemory::from_fd(fds.pop().unwrap(), 4096).unwrap();
        assert_eq!(&mapped.as_slice()[..4], &[0xDE, 0xAD, 0xBE, 0xEF]);

        // Shared pages: a write on one side is visible on the other.
        shm.as_mut_slice()[0] = 0x11;
        assert_eq!(mapped.as_slice()[0], 0x11);
    }

    #[test]
    fn eof_when_peer_drops() {
        let (a, b) = Channel::pair().unwrap();
        drop(a);
        let err = b.recv().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
