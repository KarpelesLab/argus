//! The browser↔child IPC message taxonomy and its wire codec.
//!
//! Phase 0 is intentionally tiny: enough to bring a child up, ask a content
//! process for one frame, and shut it down. The codec is hand-rolled (no serde)
//! to keep the dependency surface minimal, matching the wider stack's ethos; it
//! will grow into the full taxonomy sketched in `docs/PROCESS_MODEL.md`.
//!
//! Descriptors (e.g. a framebuffer's shared-memory handle) travel out-of-band as
//! `argus-ipc` ancillary data, not inside the encoded payload. Each message
//! declares how many fds it expects via [`Msg::expected_fds`].

use argus_geometry::Size;
use argus_ipc::Channel;
use std::io;
use std::os::fd::{BorrowedFd, OwnedFd};

/// Wire-format version. Bumped on any incompatible change; checked at handshake.
pub const PROTOCOL_VERSION: u16 = 1;

/// Send `msg` over `channel`, attaching `fds` (their count must match
/// [`Msg::expected_fds`]).
pub fn send(channel: &Channel, msg: Msg, fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    debug_assert_eq!(
        msg.expected_fds(),
        fds.len(),
        "fd count mismatch for {msg:?}"
    );
    channel.send(&msg.encode(), fds)
}

/// Receive the next message and any descriptors it carried.
pub fn recv(channel: &Channel) -> io::Result<(Msg, Vec<OwnedFd>)> {
    let (bytes, fds) = channel.recv()?;
    let msg = Msg::decode(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((msg, fds))
}

/// Parent side of the opening handshake: announce `viewport`, await `Ready`.
pub fn parent_handshake(channel: &Channel, viewport: Size) -> io::Result<()> {
    send(
        channel,
        Msg::Hello {
            version: PROTOCOL_VERSION,
            viewport,
        },
        &[],
    )?;
    match recv(channel)?.0 {
        Msg::Ready { version } if version == PROTOCOL_VERSION => Ok(()),
        Msg::Ready { version } => Err(version_mismatch(version)),
        other => Err(unexpected("Ready", other)),
    }
}

/// Child side of the opening handshake: await `Hello`, reply `Ready`, and return
/// the announced viewport.
pub fn child_handshake(channel: &Channel) -> io::Result<Size> {
    let viewport = match recv(channel)?.0 {
        Msg::Hello { version, viewport } if version == PROTOCOL_VERSION => viewport,
        Msg::Hello { version, .. } => return Err(version_mismatch(version)),
        other => return Err(unexpected("Hello", other)),
    };
    send(
        channel,
        Msg::Ready {
            version: PROTOCOL_VERSION,
        },
        &[],
    )?;
    Ok(viewport)
}

fn version_mismatch(got: u16) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("protocol version mismatch: peer {got} != ours {PROTOCOL_VERSION}"),
    )
}

fn unexpected(want: &str, got: Msg) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("expected {want}, got {got:?}"),
    )
}

/// A single Phase 0 message. Direction is by convention (see each variant).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Msg {
    /// browser → child: start of session, carrying the protocol version and the
    /// initial viewport.
    Hello { version: u16, viewport: Size },
    /// child → browser: handshake acknowledgement.
    Ready { version: u16 },
    /// browser → content: please paint a frame at the current viewport.
    RequestFrame,
    /// content → browser: a framebuffer of `size` is attached as one fd; its
    /// bytes are RGBA8, `size.area() * 4` long.
    FrameReady { size: Size },
    /// browser → content: a primary-button press at content pixel `(x, y)`.
    InputClick { x: u32, y: u32 },
    /// browser → content: font file bytes for text rendering (the sandboxed content
    /// process cannot read fonts from disk itself).
    ProvideFont { bytes: Vec<u8> },
    /// browser → content: the HTML document to render.
    LoadDocument { html: String },
    /// browser → net service: fetch this URL.
    LoadUrl { url: String },
    /// net service → browser: a fetched resource (`status == 0` means failure).
    ResourceLoaded { status: u16, body: Vec<u8> },
    /// browser → child: exit cleanly.
    Shutdown,
}

/// Error decoding a message off the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DecodeError {
    /// Buffer ended before a full message was read.
    Truncated,
    /// Unknown message tag.
    BadTag(u8),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("truncated message"),
            DecodeError::BadTag(t) => write!(f, "unknown message tag {t}"),
        }
    }
}

impl std::error::Error for DecodeError {}

// Tags. Stable on the wire.
const TAG_HELLO: u8 = 1;
const TAG_READY: u8 = 2;
const TAG_REQUEST_FRAME: u8 = 3;
const TAG_FRAME_READY: u8 = 4;
const TAG_INPUT_CLICK: u8 = 5;
const TAG_SHUTDOWN: u8 = 6;
const TAG_PROVIDE_FONT: u8 = 7;
const TAG_LOAD_DOCUMENT: u8 = 8;
const TAG_LOAD_URL: u8 = 9;
const TAG_RESOURCE_LOADED: u8 = 10;

impl Msg {
    /// Number of file descriptors that accompany this message out-of-band.
    pub fn expected_fds(&self) -> usize {
        match self {
            Msg::FrameReady { .. } => 1,
            _ => 0,
        }
    }

    /// Encode to bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16);
        match self {
            Msg::Hello { version, viewport } => {
                buf.push(TAG_HELLO);
                buf.extend_from_slice(&version.to_le_bytes());
                put_size(&mut buf, *viewport);
            }
            Msg::Ready { version } => {
                buf.push(TAG_READY);
                buf.extend_from_slice(&version.to_le_bytes());
            }
            Msg::RequestFrame => buf.push(TAG_REQUEST_FRAME),
            Msg::FrameReady { size } => {
                buf.push(TAG_FRAME_READY);
                put_size(&mut buf, *size);
            }
            Msg::InputClick { x, y } => {
                buf.push(TAG_INPUT_CLICK);
                buf.extend_from_slice(&x.to_le_bytes());
                buf.extend_from_slice(&y.to_le_bytes());
            }
            Msg::ProvideFont { bytes } => {
                buf.push(TAG_PROVIDE_FONT);
                put_bytes(&mut buf, bytes);
            }
            Msg::LoadDocument { html } => {
                buf.push(TAG_LOAD_DOCUMENT);
                put_bytes(&mut buf, html.as_bytes());
            }
            Msg::LoadUrl { url } => {
                buf.push(TAG_LOAD_URL);
                put_bytes(&mut buf, url.as_bytes());
            }
            Msg::ResourceLoaded { status, body } => {
                buf.push(TAG_RESOURCE_LOADED);
                buf.extend_from_slice(&status.to_le_bytes());
                put_bytes(&mut buf, body);
            }
            Msg::Shutdown => buf.push(TAG_SHUTDOWN),
        }
        buf
    }

    /// Decode from bytes produced by [`Msg::encode`].
    pub fn decode(bytes: &[u8]) -> Result<Msg, DecodeError> {
        let mut c = Cursor::new(bytes);
        let tag = c.u8()?;
        let msg = match tag {
            TAG_HELLO => Msg::Hello {
                version: c.u16()?,
                viewport: c.size()?,
            },
            TAG_READY => Msg::Ready { version: c.u16()? },
            TAG_REQUEST_FRAME => Msg::RequestFrame,
            TAG_FRAME_READY => Msg::FrameReady { size: c.size()? },
            TAG_INPUT_CLICK => Msg::InputClick {
                x: c.u32()?,
                y: c.u32()?,
            },
            TAG_PROVIDE_FONT => Msg::ProvideFont {
                bytes: c.bytes()?.to_vec(),
            },
            TAG_LOAD_DOCUMENT => Msg::LoadDocument {
                html: String::from_utf8_lossy(c.bytes()?).into_owned(),
            },
            TAG_LOAD_URL => Msg::LoadUrl {
                url: String::from_utf8_lossy(c.bytes()?).into_owned(),
            },
            TAG_RESOURCE_LOADED => Msg::ResourceLoaded {
                status: c.u16()?,
                body: c.bytes()?.to_vec(),
            },
            TAG_SHUTDOWN => Msg::Shutdown,
            other => return Err(DecodeError::BadTag(other)),
        };
        Ok(msg)
    }
}

fn put_size(buf: &mut Vec<u8>, s: Size) {
    buf.extend_from_slice(&s.width.to_le_bytes());
    buf.extend_from_slice(&s.height.to_le_bytes());
}

fn put_bytes(buf: &mut Vec<u8>, bytes: &[u8]) {
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn size(&mut self) -> Result<Size, DecodeError> {
        Ok(Size::new(self.u32()?, self.u32()?))
    }

    fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(m: Msg) {
        assert_eq!(Msg::decode(&m.encode()), Ok(m));
    }

    #[test]
    fn all_messages_round_trip() {
        round_trip(Msg::Hello {
            version: PROTOCOL_VERSION,
            viewport: Size::new(1280, 720),
        });
        round_trip(Msg::Ready {
            version: PROTOCOL_VERSION,
        });
        round_trip(Msg::RequestFrame);
        round_trip(Msg::FrameReady {
            size: Size::new(800, 600),
        });
        round_trip(Msg::InputClick { x: 12, y: 345 });
        round_trip(Msg::ProvideFont {
            bytes: vec![0, 1, 2, 250, 255],
        });
        round_trip(Msg::LoadDocument {
            html: "<p>hi & bye</p>".to_string(),
        });
        round_trip(Msg::LoadUrl {
            url: "https://example.com/x".to_string(),
        });
        round_trip(Msg::ResourceLoaded {
            status: 200,
            body: vec![60, 104, 49, 62],
        });
        round_trip(Msg::Shutdown);
    }

    #[test]
    fn frame_ready_expects_one_fd() {
        assert_eq!(
            Msg::FrameReady {
                size: Size::new(1, 1)
            }
            .expected_fds(),
            1
        );
        assert_eq!(Msg::RequestFrame.expected_fds(), 0);
    }

    #[test]
    fn rejects_bad_tag_and_truncation() {
        assert_eq!(Msg::decode(&[99]), Err(DecodeError::BadTag(99)));
        assert_eq!(Msg::decode(&[]), Err(DecodeError::Truncated));
        assert_eq!(Msg::decode(&[TAG_HELLO, 0x01]), Err(DecodeError::Truncated));
    }
}
