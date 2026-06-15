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

/// A small decorative image embedded as a `data:` URL, for the sample page.
const SAMPLE_IMAGE: &str = include_str!("../sample_image.txt");

/// Back/forward navigation history: a stack of visited URLs plus the current
/// index. Navigating to a new URL truncates any forward entries (standard
/// browser semantics).
#[derive(Debug, Default)]
struct History {
    stack: Vec<String>,
    index: usize,
}

impl History {
    fn new(url: String) -> History {
        History {
            stack: vec![url],
            index: 0,
        }
    }

    /// Record a navigation to `url` (dropping any forward history).
    fn push(&mut self, url: String) {
        if self.stack.get(self.index) == Some(&url) {
            return; // no-op navigation to the same URL
        }
        self.stack.truncate(self.index + 1);
        self.stack.push(url);
        self.index = self.stack.len() - 1;
    }

    /// Move back one entry and return its URL, if possible.
    fn back(&mut self) -> Option<&str> {
        if self.index > 0 {
            self.index -= 1;
            Some(&self.stack[self.index])
        } else {
            None
        }
    }

    /// Move forward one entry and return its URL, if possible.
    fn forward(&mut self) -> Option<&str> {
        if self.index + 1 < self.stack.len() {
            self.index += 1;
            Some(&self.stack[self.index])
        } else {
            None
        }
    }
}

/// The built-in sample document rendered by the windowed shell and page dumper.
pub fn sample_html() -> String {
    format!(
        "<!DOCTYPE html><html><head><title>Argus</title><style>\
:root {{ --accent: #2e86de; --ink: #1c2430 }}\
body {{ background-color: #f4f6fb; color: var(--ink) }}\
h1 {{ color: var(--accent); text-align: center }}\
h2 {{ color: #444 }}\
.card {{ background-color: #ffffff; border: 1px solid #d0d7e2; padding: 16px; margin: 12px 0; border-radius: 10px }}\
.pill {{ background-color: var(--accent); color: #ffffff; padding: 6px 14px; border-radius: 14px; width: 180px; text-align: center; margin: 8px 0 }}\
.note {{ background-color: #fff3cd; color: #5a4b00; border: 1px solid #f0d000; padding: 12px }}\
.brand {{ color: #c0392b }}\
.center {{ text-align: center }}\
.tbl td, .tbl th {{ border: 1px solid #ccd3df; background-color: #ffffff }}\
.tbl th {{ background-color: #eef1f7 }}\
.tbl tbody tr:nth-child(even) td {{ background-color: #eaf0fb }}\
.row {{ display: flex }}\
.col {{ background-color: #ffffff; border: 1px solid #d0d7e2; padding: 10px; margin: 4px }}\
</style></head><body>\
<h1>Argus</h1>\
<div class=\"pill\">rounded pill</div>\
<div class=\"pill\" style=\"opacity: 0.45\">half-opacity pill</div>\
<div class=\"card\">\
<p style=\"text-align: justify\">A web browser written in <strong class=\"brand\">pure Rust</strong>. This page was \
fetched over the network, parsed into a DOM, run through a real CSS cascade, laid out \
with the box model, and painted with shaped, anti-aliased glyphs and decoded images — \
all inside a sandboxed content process.</p>\
<img src=\"{SAMPLE_IMAGE}\" width=\"160\" height=\"90\">\
</div>\
<div class=\"card\">\
<h3>Scripting (Phase 2): live DOM bindings</h3>\
<p id=\"js-status\" style=\"color: #b00\">JavaScript has not run yet.</p>\
<ul id=\"js-list\"><li>placeholder (replaced by innerHTML)</li></ul>\
<div id=\"counter-btn\" class=\"pill\" style=\"width: 140px\">Click me</div>\
<p>clicked <strong id=\"counter\">0</strong> times (try it in the window).</p>\
<input id=\"field\" value=\"click me and type\">\
<input placeholder=\"...or a grey placeholder\">\
<select><option>First choice</option><option selected>Selected choice (rendered)</option></select>\
<input type=\"checkbox\" checked>\
<input type=\"checkbox\">\
<input type=\"radio\" checked>\
<button>A button</button>\
</div>\
<h2>Box model &amp; images</h2>\
<p class=\"note\">This box has a background, a border, and padding from a class \
selector; the gradient above is a PNG decoded by argus-image. The cascade, inline \
styles, the box model, and images all work.</p>\
<p class=\"center\" style=\"color: #2e7d32\">This line is centered and colored green by \
an inline style attribute.</p>\
<h3>What works</h3>\
<p>Inline styling now works: a <strong>bold strong</strong>, a \
<span style=\"color:#c0392b\">red span</span>, and a <a href=\"https://example.com\">\
blue link</a> all flow inside this paragraph with correct spacing.</p>\
<ul>\
<li>HTML parsing, the DOM, and a real CSS cascade</li>\
<li>The box model: margins, borders, padding, width</li>\
<li>Networking over rsurl, and decoded images</li>\
</ul>\
<hr>\
<ol>\
<li>JavaScript via kataan, with synchronous DOM bindings</li>\
<li>Navigation, tabs, and history</li>\
<li>More CSS: flexbox, grid, and the long tail</li>\
</ol>\
<ol style=\"list-style-type: lower-roman\">\
<li>Roman one</li><li>Roman two</li><li>Roman three</li>\
</ol>\
<ul style=\"list-style-type: square\">\
<li>Square bullet</li><li>Another square</li>\
</ul>\
<p style=\"text-transform: uppercase\">text-transform makes this uppercase.</p>\
<p>Inline features: H<sub>2</sub>O, E = mc<sup>2</sup>, <small>small print</small>, \
<mark>highlighted text</mark>, and inline <code>code()</code>.</p>\
<h3>A table</h3>\
<table class=\"tbl\"><thead><tr><th>Subsystem</th><th>Crate</th><th>Status</th></tr></thead>\
<tbody>\
<tr><td>HTML parser</td><td>argus-html</td><td>working</td></tr>\
<tr><td>CSS cascade</td><td>argus-css</td><td>working</td></tr>\
<tr><td>Layout</td><td>argus-layout</td><td>block + inline + tables</td></tr>\
</tbody></table>\
<h3>Flexbox</h3>\
<div class=\"row\">\
<div class=\"col\">First column in a flex row.</div>\
<div class=\"col\">Second column, sharing the width equally.</div>\
<div class=\"col\">Third column of the flex container.</div>\
</div>\
<h3>Grid</h3>\
<div style=\"display:grid; grid-template-columns: repeat(2, 1fr); gap: 12px\">\
<div class=\"col\">Grid cell one</div>\
<div class=\"col\">Grid cell two</div>\
<div class=\"col\">Grid cell three</div>\
<div class=\"col\">Grid cell four</div>\
</div>\
<p>Line breaks<br>split a paragraph,<br>and <s>struck-out</s> or <del>deleted</del> text renders with a line through it.</p>\
<h3>Preformatted</h3>\
<pre>  line one (two leading spaces)\n    line two (four)\ntab\tafter\nfn main() {{ println!(); }}</pre>\
<script>\
function fib(n){{ return n < 2 ? n : fib(n-1) + fib(n-2); }}\
var s = document.getElementById('js-status');\
s.textContent = 'JavaScript ran and edited the DOM via getElementById: fib(20) = ' + fib(20) + '.';\
s.style.color = '#2e7d32';\
document.getElementById('js-list').innerHTML = '<li>built by</li><li>document</li><li>.innerHTML</li>';\
var li = document.createElement('li');\
li.textContent = 'and one more via createElement + appendChild';\
document.querySelector('#js-list').appendChild(li);\
var clicks = 0;\
document.getElementById('counter-btn').addEventListener('click', function(e){{\
  clicks = clicks + 1;\
  document.getElementById('counter').textContent = '' + clicks;\
}});\
console.log('kataan ran: fib(20) = ' + fib(20));\
</script>\
</body></html>"
    )
}

/// Locate a usable system font on disk (the browser process is trusted and may
/// read the filesystem; content cannot).
fn system_font_bytes() -> Option<Vec<u8>> {
    for path in [
        "/System/Library/Fonts/Geneva.ttf",
        "/System/Library/Fonts/Monaco.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

/// Send the content process a font and a document to render.
fn provide_page(content: &Child, html: &str) -> io::Result<()> {
    if let Some(bytes) = system_font_bytes() {
        proto::send(content.channel(), Msg::ProvideFont { bytes }, &[])?;
    } else {
        log!("no system font found; content will render the fallback color");
    }
    proto::send(
        content.channel(),
        Msg::LoadDocument {
            html: html.to_string(),
        },
        &[],
    )
}

/// Ask the net service to fetch `url`, returning the raw body (empty on failure).
fn fetch_bytes(net: &Child, url: &str) -> io::Result<Vec<u8>> {
    proto::send(
        net.channel(),
        Msg::LoadUrl {
            url: url.to_string(),
        },
        &[],
    )?;
    match proto::recv(net.channel())?.0 {
        Msg::ResourceLoaded { status, body } => Ok(if status == 0 { Vec::new() } else { body }),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected ResourceLoaded, got {other:?}"),
        )),
    }
}

fn fetch_html(net: &Child, url: &str) -> io::Result<String> {
    let body = fetch_bytes(net, url)?;
    if body.is_empty() {
        Ok(error_page(
            url,
            "could not load (network error or empty response)",
        ))
    } else {
        Ok(String::from_utf8_lossy(&body).into_owned())
    }
}

/// Resolve `href` against the current page `base` (minimal: absolute, protocol-
/// relative, root-relative, and same-directory relative URLs).
fn resolve_url(base: Option<&str>, href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }
    let Some(base) = base else {
        return href.to_string();
    };
    if let Some(rest) = href.strip_prefix("//") {
        let scheme = base.split("://").next().unwrap_or("https");
        return format!("{scheme}://{rest}");
    }
    // Split base into scheme://authority and path.
    let (scheme_auth, path) = match base.find("://") {
        Some(i) => {
            let after = &base[i + 3..];
            match after.find('/') {
                Some(j) => (&base[..i + 3 + j], &after[j..]),
                None => (base, "/"),
            }
        }
        None => return href.to_string(),
    };
    if let Some(abs) = href.strip_prefix('/') {
        format!("{scheme_auth}/{abs}")
    } else {
        // Strip the last path segment (the "directory").
        let dir = &path[..path.rfind('/').map(|i| i + 1).unwrap_or(0)];
        format!("{scheme_auth}{dir}{href}")
    }
}

fn error_page(url: &str, message: &str) -> String {
    format!(
        "<!DOCTYPE html><html><head><title>Error</title>\
         <style>body{{color:#900}} p{{color:#333}}</style></head><body>\
         <h1>Could not load page</h1><p>{url}</p><p>{message}</p></body></html>"
    )
}

/// The page to show: a fetched URL or the built-in sample.
fn resolve_html(net: &Child, url: Option<&str>) -> String {
    match url {
        Some(u) => fetch_html(net, u).unwrap_or_else(|e| error_page(u, &e.to_string())),
        None => sample_html(),
    }
}

/// Headless automation: fetch a page (or the sample) and return its parsed DOM
/// serialized in the html5lib `#document` format. Used by the `--dump-dom` tool.
pub fn dump_dom(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    // Reflect synchronous DOM mutations from the page's scripts (Phase 2).
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts(&mut doc);
    Ok(doc.serialize())
}

/// Headless automation: fetch a page and return its **accessibility tree** — the
/// ARIA role and accessible name of each semantic element (a start on the a11y
/// tree from `docs/subsystems/embedding.md`). Used by `--dump-a11y`.
pub fn dump_a11y(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts(&mut doc);
    Ok(a11y_tree(&doc))
}

/// ARIA role implied by an HTML tag (None = generic/presentational).
fn implicit_role(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "a" => "link",
        "button" => "button",
        "img" => "img",
        "ul" | "ol" => "list",
        "li" => "listitem",
        "nav" => "navigation",
        "main" => "main",
        "header" => "banner",
        "footer" => "contentinfo",
        "input" | "textarea" => "textbox",
        "p" => "paragraph",
        "table" => "table",
        "tr" => "row",
        "td" => "cell",
        "th" => "columnheader",
        "form" => "form",
        _ => return None,
    })
}

/// Build the accessibility tree for a parsed document: each semantic element as an
/// indented `role "name"` line. Honors an explicit `role` attribute, `aria-label`
/// (accessible-name override), and `aria-hidden="true"` (prunes the subtree). Pure
/// (no I/O) so it's unit-testable.
fn a11y_tree(doc: &argus_dom::Document) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    fn text_of(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            NodeData::Element(_) => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
            _ => {}
        }
    }

    /// Collapse whitespace and truncate to 60 chars on a char boundary.
    fn clean(s: &str) -> String {
        let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() > 60 {
            let t: String = collapsed.chars().take(60).collect();
            format!("{t}…")
        } else {
            collapsed
        }
    }

    fn walk(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
        if let NodeData::Element(e) = &doc.node(id).data {
            // `aria-hidden="true"` removes the element and its subtree from the tree.
            if e.attr("aria-hidden") == Some("true") {
                return;
            }
        }
        let mut next_depth = depth;
        if let NodeData::Element(e) = &doc.node(id).data {
            let tag = &*e.name.local;
            if !matches!(tag, "head" | "title" | "style" | "script" | "meta" | "link") {
                // An explicit `role` attribute overrides the tag's implicit role.
                let role: Option<&str> = e
                    .attr("role")
                    .filter(|r| !r.is_empty())
                    .or_else(|| implicit_role(tag));
                if let Some(role) = role {
                    // Accessible name: `aria-label`, else `alt` for images, else text.
                    let name = if let Some(label) = e.attr("aria-label") {
                        clean(label)
                    } else if tag == "img" {
                        clean(e.attr("alt").unwrap_or(""))
                    } else {
                        let mut s = String::new();
                        text_of(doc, id, &mut s);
                        clean(&s)
                    };
                    for _ in 0..depth {
                        out.push_str("  ");
                    }
                    out.push_str(role);
                    if !name.is_empty() {
                        out.push_str(&format!(" \"{name}\""));
                    }
                    out.push('\n');
                    next_depth = depth + 1;
                }
            }
        }
        for c in doc.children(id) {
            walk(doc, c, next_depth, out);
        }
    }

    let mut out = String::from("document\n");
    walk(doc, doc.root(), 1, &mut out);
    out
}

/// Headless automation: fetch a page and return its **rendered text** — an
/// `innerText`-style projection that drops non-rendered elements, collapses
/// inline whitespace, and breaks lines at block boundaries and `<br>`. Used by
/// `--dump-text` (useful for scraping and snapshot tests).
pub fn dump_text(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts(&mut doc);
    Ok(render_text(&doc))
}

/// Headless automation: fetch a page and return its **hyperlinks**, one per line as
/// `link-text<TAB>resolved-href` in document order. Relative hrefs are resolved
/// against the page URL. Used by `--dump-links` (link extraction / crawling).
pub fn dump_links(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts(&mut doc);
    Ok(extract_links(&doc, url))
}

/// Headless automation: fetch a page and return its **heading outline** — each
/// `<h1>`–`<h6>` as a level-indented line. Used by `--dump-headings` (document
/// structure / accessibility analysis).
pub fn dump_headings(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts(&mut doc);
    Ok(extract_headings(&doc))
}

/// Collect `<h1>`–`<h6>` as `Hn: text` lines, indented two spaces per level below
/// the first heading's level. Pure (no I/O) so it's unit-testable.
fn extract_headings(doc: &argus_dom::Document) -> String {
    use argus_dom::{NodeData, NodeId};

    fn text_of(doc: &argus_dom::Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
        }
    }
    fn level(tag: &str) -> Option<u8> {
        match tag {
            "h1" => Some(1),
            "h2" => Some(2),
            "h3" => Some(3),
            "h4" => Some(4),
            "h5" => Some(5),
            "h6" => Some(6),
            _ => None,
        }
    }
    fn walk(doc: &argus_dom::Document, id: NodeId, top: &mut Option<u8>, out: &mut String) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if let Some(lvl) = level(&e.name.local) {
                let base = *top.get_or_insert(lvl);
                let indent = lvl.saturating_sub(base) as usize;
                let mut t = String::new();
                text_of(doc, id, &mut t);
                let text = t.split_whitespace().collect::<Vec<_>>().join(" ");
                out.push_str(&"  ".repeat(indent));
                out.push_str(&format!("H{lvl}: {text}\n"));
            }
        }
        for c in doc.children(id) {
            walk(doc, c, top, out);
        }
    }
    let mut out = String::new();
    let mut top = None;
    walk(doc, doc.root(), &mut top, &mut out);
    out
}

/// Collect `<a href>` links as `text<TAB>resolved-href` lines, in document order.
/// Pure (no I/O) so it's unit-testable; `base` resolves relative hrefs.
fn extract_links(doc: &argus_dom::Document, base: Option<&str>) -> String {
    use argus_dom::{NodeData, NodeId};

    fn text_of(doc: &argus_dom::Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
        }
    }
    fn walk(doc: &argus_dom::Document, id: NodeId, base: Option<&str>, out: &mut String) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("a") {
                if let Some(href) = e.attr("href") {
                    let mut t = String::new();
                    text_of(doc, id, &mut t);
                    let text = t.split_whitespace().collect::<Vec<_>>().join(" ");
                    out.push_str(&format!("{text}\t{}\n", resolve_url(base, href)));
                }
            }
        }
        for c in doc.children(id) {
            walk(doc, c, base, out);
        }
    }
    let mut out = String::new();
    walk(doc, doc.root(), base, &mut out);
    out
}

/// Project a parsed document to `innerText`-style rendered text: drop
/// non-rendered elements, collapse inline whitespace, and break lines at block
/// boundaries and `<br>`. Table cells are tab-separated. Lines are trimmed.
fn render_text(doc: &argus_dom::Document) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    /// Tags that are never rendered as text.
    fn is_hidden(tag: &str) -> bool {
        matches!(
            tag,
            "head" | "title" | "style" | "script" | "meta" | "link" | "base" | "noscript"
        )
    }
    /// Tags that introduce a line break before and after their content.
    fn is_block(tag: &str) -> bool {
        matches!(
            tag,
            "html"
                | "body"
                | "p"
                | "div"
                | "section"
                | "article"
                | "header"
                | "footer"
                | "nav"
                | "main"
                | "aside"
                | "figure"
                | "blockquote"
                | "pre"
                | "hr"
                | "address"
                | "form"
                | "ul"
                | "ol"
                | "li"
                | "dl"
                | "dt"
                | "dd"
                | "table"
                | "tr"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
        )
    }

    // Build the text with explicit newline tokens, then normalize runs of blank
    // lines and trailing/leading whitespace at the end.
    fn walk(doc: &Document, id: NodeId, pre: bool, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => {
                if pre {
                    out.push_str(t);
                } else if t.chars().any(|c| !c.is_whitespace()) {
                    // Collapse internal whitespace but keep a leading/trailing space
                    // so adjacent inline text doesn't run together.
                    if t.starts_with(char::is_whitespace) {
                        out.push(' ');
                    }
                    let mut first = true;
                    for word in t.split_whitespace() {
                        if !first {
                            out.push(' ');
                        }
                        out.push_str(word);
                        first = false;
                    }
                    if t.ends_with(char::is_whitespace) {
                        out.push(' ');
                    }
                } else if !out.ends_with(char::is_whitespace) {
                    out.push(' ');
                }
            }
            NodeData::Element(e) => {
                let tag = &*e.name.local;
                if is_hidden(tag) {
                    return;
                }
                if tag == "br" {
                    out.push('\n');
                    return;
                }
                let block = is_block(tag);
                let cell = matches!(tag, "td" | "th");
                let child_pre = pre || tag == "pre";
                if block && !out.ends_with('\n') {
                    out.push('\n');
                }
                for c in doc.children(id) {
                    walk(doc, c, child_pre, out);
                }
                if cell && !out.ends_with('\t') {
                    out.push('\t'); // separate table cells, like innerText
                }
                if block && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            // Document / Doctype / Comment: descend into children if any.
            _ => {
                for c in doc.children(id) {
                    walk(doc, c, pre, out);
                }
            }
        }
    }

    let mut raw = String::new();
    walk(doc, doc.root(), false, &mut raw);

    // Normalize: trim each line, collapse consecutive blank lines, trim ends.
    let mut text = String::new();
    let mut blanks = 0;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        text.push_str(line);
        text.push('\n');
    }
    text.trim_start_matches('\n').to_string()
}

/// Render a page (a fetched `url`, or the sample) to pixels once, off-screen.
/// Returns the framebuffer size and RGBA bytes. Used by the `--dump-page` tool.
pub fn render_once(url: Option<&str>, viewport: Size) -> io::Result<(Size, Vec<u8>)> {
    log::set_role(Role::Browser);
    let mut content = spawn_child(Role::Content)?;
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(content.channel(), viewport)?;
    proto::parent_handshake(net.channel(), viewport)?;

    let html = resolve_html(&net, url);
    if let Some(bytes) = system_font_bytes() {
        proto::send(content.channel(), Msg::ProvideFont { bytes }, &[])?;
    }
    proto::send(content.channel(), Msg::LoadDocument { html }, &[])?;

    let (frame, _) = request_frame(&content, &net, url)?;
    let pixels = frame.pixels().to_vec();
    let size = frame.size();

    proto::send(content.channel(), Msg::Shutdown, &[])?;
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    content.wait()?;
    net.wait()?;
    Ok((size, pixels))
}

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
    let (frame, _) = request_frame(&content, &net, None)?;
    let color = verify_uniform(&frame)?;
    let size = frame.size();
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

/// Ask `content` to paint, serving any subresource fetches it makes through the
/// `net` service while it renders, and map the shared framebuffer it hands back.
fn request_frame(
    content: &Child,
    net: &Child,
    base: Option<&str>,
) -> io::Result<(Framebuffer, u32)> {
    proto::send(content.channel(), Msg::RequestFrame, &[])?;
    let (msg, mut fds) = loop {
        let (msg, fds) = proto::recv(content.channel())?;
        match msg {
            // Content needs a subresource: resolve it against the page URL, fetch,
            // and reply, then keep waiting for the frame.
            Msg::FetchResource { url } => {
                let target = resolve_url(base, &url);
                let body = fetch_bytes(net, &target).unwrap_or_default();
                proto::send(content.channel(), Msg::ResourceData { body }, &[])?;
            }
            other => break (other, fds),
        }
    };
    let (size, content_height) = match msg {
        Msg::FrameReady {
            size,
            content_height,
        } => (size, content_height),
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
    Ok((Framebuffer::from_fd(fd, size)?, content_height))
}

/// The text of a document's `<title>` element, trimmed and whitespace-collapsed
/// (empty if absent). A lightweight scan — no full parse needed.
fn page_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(open) = lower.find("<title") else {
        return String::new();
    };
    let Some(gt) = lower[open..].find('>').map(|i| open + i + 1) else {
        return String::new();
    };
    let end = lower[gt..]
        .find("</title>")
        .map(|i| gt + i)
        .unwrap_or(html.len());
    html[gt..end]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Load `target` (the sample when `None`/empty), provide it to content, reset its
/// scroll, render a frame, and present it. Returns the page's content height.
#[cfg(target_os = "macos")]
fn load_and_present(
    content: &Child,
    net: &Child,
    window: &argus_platform::window::Window,
    target: Option<&str>,
) -> io::Result<u32> {
    let page = match target {
        Some(u) => fetch_html(net, u).unwrap_or_else(|e| error_page(u, &e.to_string())),
        None => sample_html(),
    };
    let title = page_title(&page);
    window.set_title(if title.is_empty() { "Argus" } else { &title });
    provide_page(content, &page)?;
    proto::send(content.channel(), Msg::SetScroll { y: 0 }, &[])?;
    let (frame, h) = request_frame(content, net, target)?;
    window.present(frame.pixels(), frame.size());
    Ok(h)
}

/// Run the browser in its default mode for this platform: a real window where one
/// is available, the headless verifier otherwise. `url` selects the page (the
/// built-in sample when `None`).
#[cfg(target_os = "macos")]
pub fn run_default(url: Option<String>) -> io::Result<()> {
    run_windowed(url)
}

/// See [`run_default`].
#[cfg(not(target_os = "macos"))]
pub fn run_default(_url: Option<String>) -> io::Result<()> {
    run()
}

/// Run the browser with an on-screen window (macOS). Spawns content + net, fetches
/// the page (a URL or the sample), opens a window, presents content's framebuffer,
/// forwards clicks into the sandboxed content process, and repaints — until the
/// window is closed.
#[cfg(target_os = "macos")]
pub fn run_windowed(url: Option<String>) -> io::Result<()> {
    use argus_platform::window::{Event, Window};

    log::set_role(Role::Browser);
    let viewport = Size::new(800, 600);
    log!(
        "starting (windowed); viewport {}x{}",
        viewport.width,
        viewport.height
    );

    let mut content = spawn_child(Role::Content)?;
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(content.channel(), viewport)?;
    proto::parent_handshake(net.channel(), viewport)?;
    let mut current_url = url.clone();
    let mut history = History::new(current_url.clone().unwrap_or_default());
    let html = resolve_html(&net, current_url.as_deref());
    provide_page(&content, &html)?;
    log!("children handshook; page sent; opening window");

    // Present the first frame.
    let (frame, mut content_height) = request_frame(&content, &net, current_url.as_deref())?;
    let title = page_title(&html);
    let window = Window::open(if title.is_empty() { "Argus" } else { &title }, viewport);
    window.present(frame.pixels(), frame.size());
    log!("window open — click links to navigate, scroll the wheel, close to quit");

    let mut scroll_y: u32 = 0;
    loop {
        match window.next_event() {
            Event::MouseDown { x, y } => {
                proto::send(content.channel(), Msg::InputClick { x, y }, &[])?;
                // Content replies with the click result; navigate if a link was hit.
                if let Msg::ClickResult { url } = proto::recv(content.channel())?.0 {
                    if !url.is_empty() {
                        let target = resolve_url(current_url.as_deref(), &url);
                        log!("navigating to {target}");
                        history.push(target.clone());
                        current_url = Some(target);
                        scroll_y = 0;
                        content_height =
                            load_and_present(&content, &net, &window, current_url.as_deref())?;
                    } else {
                        // Non-navigation click: the content process may have dispatched
                        // a DOM event handler that changed the page — re-render.
                        let (frame, h) = request_frame(&content, &net, current_url.as_deref())?;
                        content_height = h;
                        window.present(frame.pixels(), frame.size());
                    }
                }
            }
            Event::Scroll { dy } => {
                let max_scroll = content_height.saturating_sub(viewport.height);
                let next = (scroll_y as i64 - dy as i64).clamp(0, max_scroll as i64) as u32;
                if next != scroll_y {
                    scroll_y = next;
                    proto::send(content.channel(), Msg::SetScroll { y: scroll_y }, &[])?;
                    let (frame, h) = request_frame(&content, &net, current_url.as_deref())?;
                    content_height = h;
                    window.present(frame.pixels(), frame.size());
                }
            }
            Event::KeyChar { ch } => {
                // Forward the keystroke to the focused field, then re-render.
                proto::send(content.channel(), Msg::InputKey { ch }, &[])?;
                let (frame, h) = request_frame(&content, &net, current_url.as_deref())?;
                content_height = h;
                window.present(frame.pixels(), frame.size());
            }
            Event::Back => {
                if let Some(prev) = history.back().map(str::to_string) {
                    log!("history back -> {prev:?}");
                    current_url = (!prev.is_empty()).then_some(prev);
                    scroll_y = 0;
                    content_height =
                        load_and_present(&content, &net, &window, current_url.as_deref())?;
                }
            }
            Event::Forward => {
                if let Some(next) = history.forward().map(str::to_string) {
                    log!("history forward -> {next:?}");
                    current_url = (!next.is_empty()).then_some(next);
                    scroll_y = 0;
                    content_height =
                        load_and_present(&content, &net, &window, current_url.as_deref())?;
                }
            }
            Event::CloseRequested => {
                log!("window closed; shutting down");
                break;
            }
        }
    }

    proto::send(content.channel(), Msg::Shutdown, &[])?;
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    content.wait()?;
    net.wait()?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{extract_links, page_title, render_text, resolve_url, History};

    #[test]
    fn a11y_tree_roles_names_and_aria() {
        let doc = argus_html::parse(
            "<nav><a href=\"/\">Home</a></nav>\
             <h1>Title</h1>\
             <button aria-label=\"Close dialog\">×</button>\
             <div role=\"alert\">Heads up</div>\
             <p aria-hidden=\"true\">secret</p>",
        );
        let tree = super::a11y_tree(&doc);
        assert!(tree.contains("navigation"), "nav role:\n{tree}");
        assert!(tree.contains("link \"Home\""), "link name:\n{tree}");
        assert!(tree.contains("heading \"Title\""), "heading:\n{tree}");
        // aria-label overrides the text content as the accessible name.
        assert!(tree.contains("button \"Close dialog\""), "aria-label:\n{tree}");
        // An explicit role attribute is honored.
        assert!(tree.contains("alert \"Heads up\""), "role attr:\n{tree}");
        // aria-hidden prunes the element.
        assert!(!tree.contains("secret"), "aria-hidden should prune:\n{tree}");
    }

    #[test]
    fn extract_headings_outline() {
        let doc = argus_html::parse(
            "<h1>Title</h1><h2>Section A</h2><h3>Sub</h3><h2>Section B</h2>",
        );
        let out = super::extract_headings(&doc);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "H1: Title");
        assert_eq!(lines[1], "  H2: Section A");
        assert_eq!(lines[2], "    H3: Sub");
        assert_eq!(lines[3], "  H2: Section B");
    }

    #[test]
    fn extract_links_resolves_relative_hrefs() {
        let doc = argus_html::parse(
            "<a href=\"/about\">About</a>\
             <a href=\"contact.html\">Contact us</a>\
             <a href=\"https://other.example/x\">External</a>\
             <span>not a link</span>\
             <a href=\"//cdn.example/lib.js\">proto-relative</a>",
        );
        let out = extract_links(&doc, Some("https://site.example/dir/page.html"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "About\thttps://site.example/about");
        assert_eq!(lines[1], "Contact us\thttps://site.example/dir/contact.html");
        assert_eq!(lines[2], "External\thttps://other.example/x");
        assert_eq!(lines[3], "proto-relative\thttps://cdn.example/lib.js");
        assert_eq!(lines.len(), 4, "only <a href> elements are listed");
    }

    #[test]
    fn extracts_page_title() {
        assert_eq!(
            page_title("<html><head><title>Hello  World</title></head></html>"),
            "Hello World"
        );
        // Attributes on <title> and mixed case are handled; missing title → empty.
        assert_eq!(page_title("<TITLE lang=en>Mixed</TITLE>"), "Mixed");
        assert_eq!(page_title("<html><body>no title</body></html>"), "");
    }

    #[test]
    fn history_back_forward_and_truncation() {
        let mut h = History::new("a".into());
        assert_eq!(h.back(), None); // nothing before the first page
        h.push("b".into());
        h.push("c".into()); // a, b, [c]
        assert_eq!(h.back(), Some("b")); // a, [b], c
        assert_eq!(h.back(), Some("a")); // [a], b, c
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some("b"));
        // Navigating from the middle drops the forward entries (b -> d).
        h.push("d".into()); // a, b, [d]
        assert_eq!(h.forward(), None);
        assert_eq!(h.back(), Some("b"));
        // Re-navigating to the current URL is a no-op.
        h.push("b".into());
        assert_eq!(h.forward(), Some("d"));
    }

    #[test]
    fn rendered_text_structure() {
        let doc = argus_html::parse(
            "<html><head><title>T</title><style>p{color:red}</style></head>\
             <body><h1>Title</h1><p>Hello <b>bold</b> world</p>\
             <ul><li>one</li><li>two</li></ul>\
             <p>a<br>b</p>\
             <table><tr><td>x</td><td>y</td></tr></table>\
             <script>ignore()</script></body></html>",
        );
        let text = render_text(&doc);
        // Non-rendered content is dropped.
        assert!(!text.contains("color:red"));
        assert!(!text.contains("ignore"));
        // Inline text collapses onto one line; block elements break lines.
        assert!(text.contains("Hello bold world"), "got:\n{text}");
        assert!(text.contains("Title"));
        // <br> breaks a line within a paragraph.
        assert!(text.contains("a\nb"), "br should break: {text:?}");
        // Table cells are tab-separated.
        assert!(text.contains("x\ty"), "cells tab-separated: {text:?}");
        // List items are on their own lines.
        assert!(text.contains("one\ntwo"), "list items: {text:?}");
    }

    #[test]
    fn url_resolution() {
        let base = Some("https://ex.com/a/b/page.html");
        assert_eq!(
            resolve_url(base, "https://other.com/x"),
            "https://other.com/x"
        );
        assert_eq!(resolve_url(base, "/top"), "https://ex.com/top");
        assert_eq!(
            resolve_url(base, "sibling.html"),
            "https://ex.com/a/b/sibling.html"
        );
        assert_eq!(resolve_url(base, "//cdn.com/x"), "https://cdn.com/x");
        assert_eq!(
            resolve_url(Some("https://ex.com"), "/p"),
            "https://ex.com/p"
        );
    }
}
