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

    // `--eval=<js>`: run JavaScript through kataan and print console output + result
    // (a Runtime.evaluate-style headless surface). No DOM bindings yet.
    if let Some(src) = flag_value("--eval=") {
        match argus_script::run_script(&src) {
            Ok(r) => {
                print!("{}", r.console);
                println!("=> {}", r.value);
            }
            Err(e) => {
                eprintln!("script error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

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
        // `--dump-text`: print the rendered (innerText-style) text and exit.
        if has_flag("--dump-text") {
            match argus_browser::dump_text(url.as_deref()) {
                Ok(text) => print!("{text}"),
                Err(err) => {
                    eprintln!("[browser] dump-text failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-a11y`: print the accessibility tree and exit.
        if has_flag("--dump-a11y") {
            match argus_browser::dump_a11y(url.as_deref()) {
                Ok(tree) => print!("{tree}"),
                Err(err) => {
                    eprintln!("[browser] dump-a11y failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-links`: print the page's hyperlinks (text + resolved href) and exit.
        if has_flag("--dump-links") {
            match argus_browser::dump_links(url.as_deref()) {
                Ok(links) => print!("{links}"),
                Err(err) => {
                    eprintln!("[browser] dump-links failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-headings`: print the page's heading outline and exit.
        if has_flag("--dump-headings") {
            match argus_browser::dump_headings(url.as_deref()) {
                Ok(h) => print!("{h}"),
                Err(err) => {
                    eprintln!("[browser] dump-headings failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-forms`: print the page's forms and their controls and exit.
        if has_flag("--dump-forms") {
            match argus_browser::dump_forms(url.as_deref()) {
                Ok(f) => print!("{f}"),
                Err(err) => {
                    eprintln!("[browser] dump-forms failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-meta`: print the page's metadata (title/lang/og/etc.) and exit.
        if has_flag("--dump-meta") {
            match argus_browser::dump_meta(url.as_deref()) {
                Ok(m) => print!("{m}"),
                Err(err) => {
                    eprintln!("[browser] dump-meta failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-json`: print a structured JSON summary (title/headings/links).
        if has_flag("--dump-json") {
            match argus_browser::dump_json(url.as_deref()) {
                Ok(j) => print!("{j}"),
                Err(err) => {
                    eprintln!("[browser] dump-json failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-images`: print the page's images (src/alt/dimensions) and exit.
        if has_flag("--dump-images") {
            match argus_browser::dump_images(url.as_deref()) {
                Ok(t) => print!("{t}"),
                Err(err) => {
                    eprintln!("[browser] dump-images failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-tables`: print the page's tables as TSV and exit.
        if has_flag("--dump-tables") {
            match argus_browser::dump_tables(url.as_deref()) {
                Ok(t) => print!("{t}"),
                Err(err) => {
                    eprintln!("[browser] dump-tables failed: {err}");
                    std::process::exit(1);
                }
            }
            return;
        }
        // `--dump-domtree`: print the DOM as a nested JSON tree (CDP-style).
        if has_flag("--dump-domtree") {
            match argus_browser::dump_domtree(url.as_deref()) {
                Ok(t) => print!("{t}"),
                Err(err) => {
                    eprintln!("[browser] dump-domtree failed: {err}");
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

    let viewport = Size::new(800, 1600);
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
