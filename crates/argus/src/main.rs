//! The Argus executable.
//!
//! A single binary that re-executes itself in different process [`Role`]s. With
//! no `--role` it is the trusted **browser** entry process; spawned children run
//! with `--role=content|net|storage` and inherit their IPC channel on
//! [`CHILD_CHANNEL_FD`]. See `docs/PROCESS_MODEL.md`.

use argus_ipc::Channel;
use argus_platform::CHILD_CHANNEL_FD;
use argus_util::Role;

fn main() {
    let role = parse_role();
    argus_util::log::set_role(role);

    let url = flag_value("--url=");

    // `--dump-page=PATH`: render the page (sample or `--url`) off-screen to a PNG.
    if role == Role::Browser {
        if let Some(path) = flag_value("--dump-page=") {
            if let Err(err) = dump_page(&path, url.as_deref()) {
                eprintln!("[browser] dump-page failed: {err}");
                std::process::exit(1);
            }
            return;
        }
        // `--dump-dom`: print the parsed DOM (headless automation) and exit.
        if has_flag("--dump-dom") {
            match argus_browser::dump_dom(url.as_deref()) {
                Ok(dom) => print!("{dom}"),
                Err(err) => {
                    eprintln!("[browser] dump-dom failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
    }

    let result = match role {
        // Default to an on-screen window; `--headless` runs the verifier and exits.
        Role::Browser if has_flag("--headless") => argus_browser::run(),
        Role::Browser => argus_browser::run_default(url),
        Role::Content => argus_content::run(child_channel()),
        Role::NetService | Role::StorageService => argus_services::run(role, child_channel()),
    };

    if let Err(err) = result {
        eprintln!("[{role}] fatal: {err}");
        std::process::exit(1);
    }
}

/// Render a page (the sample, or `url`) and write it to `path` as a PNG.
fn dump_page(path: &str, url: Option<&str>) -> std::io::Result<()> {
    use argus_geometry::Size;
    use oxideav_core::{PixelFormat, VideoFrame, VideoPlane};

    let viewport = Size::new(800, 1400);
    let (size, rgba) = argus_browser::render_once(url, viewport)?;

    let frame = VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: (size.width * 4) as usize,
            data: rgba,
        }],
    };
    let png = oxideav_png::encode_single(&frame, size.width, size.height, PixelFormat::Rgba, &[])
        .map_err(|e| std::io::Error::other(format!("png encode: {e:?}")))?;
    std::fs::write(path, png)?;
    eprintln!("[browser] wrote {path} ({}x{})", size.width, size.height);
    Ok(())
}

/// Determine this process's role from `--role=<name>` (default: browser).
fn parse_role() -> Role {
    for arg in std::env::args().skip(1) {
        if let Some(name) = arg.strip_prefix("--role=") {
            return name.parse().unwrap_or_else(|_| {
                eprintln!("argus: unknown --role={name}");
                std::process::exit(2);
            });
        }
    }
    Role::Browser
}

/// Whether `flag` appears in the process arguments.
fn has_flag(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

/// The value of a `prefix=value` argument, if present.
fn flag_value(prefix: &str) -> Option<String> {
    std::env::args().find_map(|a| a.strip_prefix(prefix).map(|v| v.to_string()))
}

/// Reconstruct the IPC channel a child inherited from its parent.
fn child_channel() -> Channel {
    // SAFETY: the parent placed our channel on CHILD_CHANNEL_FD before exec, and
    // nothing else in this process has touched that descriptor.
    unsafe { Channel::from_raw_fd(CHILD_CHANNEL_FD) }
}
