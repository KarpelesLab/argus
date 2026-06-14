//! The browser process: the trusted entry process that spawns and coordinates
//! everything else.
//!
//! Phase 0 implements the skeleton end-to-end: spawn a content process and a net
//! service, handshake both, ask content for a frame and verify the pixels arrived
//! intact over shared memory, then deliberately kill the content process to prove
//! crash isolation before shutting the rest down cleanly. The on-screen window
//! and real tab/navigation logic build on this in later phases.

use argus_compositor::Framebuffer;
use argus_geometry::{Color, Size};
use argus_platform::{spawn_child, Child};
use argus_protocol::{self as proto, Msg};
use argus_util::{log, Role};
use std::io;

/// Run the Phase 0 browser-process skeleton.
pub fn run() -> io::Result<()> {
    log::set_role(Role::Browser);
    let viewport = Size::new(800, 600);
    log!("starting; viewport {}x{}", viewport.width, viewport.height);

    // Spawn the sandboxed content process and a trusted net service.
    let mut content = spawn_child(Role::Content)?;
    let mut net = spawn_child(Role::NetService)?;
    log!(
        "spawned content pid {} and net pid {}",
        content.pid(),
        net.pid()
    );

    proto::parent_handshake(content.channel(), viewport)?;
    proto::parent_handshake(net.channel(), viewport)?;
    log!(
        "both children handshook at protocol v{}",
        proto::PROTOCOL_VERSION
    );

    // Ask content to paint, then verify the framebuffer it shared back.
    let (size, color) = request_and_verify_frame(&content)?;
    log!(
        "verified {}x{} frame, uniform rgba({},{},{},{})",
        size.width,
        size.height,
        color.r,
        color.g,
        color.b,
        color.a
    );

    // Crash isolation: kill content and confirm the browser (and net) survive.
    log!("killing content to exercise crash isolation");
    content.kill()?;
    match proto::recv(content.channel()) {
        Err(_) => log!("content channel closed; browser process unaffected"),
        Ok((m, _)) => log!("unexpected message from a killed content process: {m:?}"),
    }
    let content_status = content.wait()?;
    log!("reaped content: {content_status}");

    // The net service is independent and still responsive: shut it down cleanly.
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    let net_status = net.wait()?;
    log!("reaped net: {net_status}");

    println!(
        "PHASE0 OK: {}x{} frame rgba({},{},{},{}) over shared memory; crash isolation verified",
        size.width, size.height, color.r, color.g, color.b, color.a
    );
    Ok(())
}

/// Ask `content` for a frame, map the shared framebuffer, and verify it is a
/// single opaque color. Returns the size and color observed.
fn request_and_verify_frame(content: &Child) -> io::Result<(Size, Color)> {
    proto::send(content.channel(), Msg::RequestFrame, &[])?;
    let (msg, mut fds) = proto::recv(content.channel())?;
    let size = match msg {
        Msg::FrameReady { size } => size,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected FrameReady, got {other:?}"),
            ))
        }
    };
    let fd = fds.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "FrameReady carried no framebuffer fd",
        )
    })?;
    let fb = Framebuffer::from_fd(fd, size)?;
    let color = verify_uniform(&fb)?;
    Ok((size, color))
}

/// Confirm every sampled pixel is identical and opaque, returning that color.
fn verify_uniform(fb: &Framebuffer) -> io::Result<Color> {
    let Size { width, height } = fb.size();
    let c0 = fb.pixel(0, 0);
    let samples = [
        (0, 0),
        (width - 1, 0),
        (0, height - 1),
        (width - 1, height - 1),
        (width / 2, height / 2),
    ];
    for (x, y) in samples {
        if fb.pixel(x, y) != c0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("framebuffer not uniform: pixel ({x},{y}) differs from (0,0)"),
            ));
        }
    }
    if c0.a != 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "framebuffer is not opaque",
        ));
    }
    Ok(c0)
}
