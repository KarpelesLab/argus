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

mod tabbar;

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

    /// The URL of the current entry.
    fn current(&self) -> &str {
        &self.stack[self.index]
    }
}

/// One browser tab: its own navigation history and scroll offset. Tabs are
/// independent — back/forward and scrolling in one never affect another.
struct Tab {
    history: History,
    scroll_y: u32,
}

impl Tab {
    fn new(url: String) -> Tab {
        Tab {
            history: History::new(url),
            scroll_y: 0,
        }
    }

    /// The URL currently shown in this tab.
    fn url(&self) -> &str {
        self.history.current()
    }
}

/// The set of open tabs with exactly one active. Always non-empty: closing the
/// last tab is refused (the caller decides whether that means "quit").
struct Tabs {
    tabs: Vec<Tab>,
    active: usize,
}

impl Tabs {
    fn new(url: String) -> Tabs {
        Tabs {
            tabs: vec![Tab::new(url)],
            active: 0,
        }
    }

    fn len(&self) -> usize {
        self.tabs.len()
    }

    fn active_index(&self) -> usize {
        self.active
    }

    fn active(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn active_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    /// Open a new tab at `url` and make it active; returns its index.
    fn open(&mut self, url: String) -> usize {
        self.tabs.push(Tab::new(url));
        self.active = self.tabs.len() - 1;
        self.active
    }

    /// Close the tab at `i`, adjusting the active index so it still points at a
    /// valid tab. Returns `false` (and changes nothing) when only one tab is left.
    fn close(&mut self, i: usize) -> bool {
        if self.tabs.len() <= 1 || i >= self.tabs.len() {
            return false;
        }
        self.tabs.remove(i);
        if self.active > i || self.active >= self.tabs.len() {
            self.active -= 1;
        }
        true
    }

    /// Activate the tab at `i` (no-op if out of range).
    fn switch_to(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.active = i;
        }
    }

    /// Activate the next/previous tab, wrapping around.
    fn next(&mut self) {
        self.active = (self.active + 1) % self.tabs.len();
    }

    fn prev(&mut self) {
        self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
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

/// Fallback fonts (emoji, CJK, broad-coverage) for glyphs the primary lacks; sent
/// to the content process after the primary so its FaceChain can consult them.
fn fallback_font_bytes() -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for path in [
        "/System/Library/Fonts/Apple Color Emoji.ttc", // emoji
        "/System/Library/Fonts/Apple Symbols.ttf",     // symbols
        "/System/Library/Fonts/PingFang.ttc",          // CJK
        "/System/Library/Fonts/Hiragino Sans GB.ttc",  // CJK fallback
        "/System/Library/Fonts/SFArabic.ttf",          // Arabic
        "/System/Library/Fonts/GeezaPro.ttc",          // Arabic fallback
        "/System/Library/Fonts/SFHebrew.ttf",          // Hebrew
        "/System/Library/Fonts/Supplemental/Raanana.ttc", // Hebrew fallback
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            out.push(bytes);
        }
    }
    out
}

/// A fixed-width system font for `font-family: monospace` and `<code>`/`<pre>`.
fn monospace_font_bytes() -> Option<Vec<u8>> {
    for path in [
        "/System/Library/Fonts/Menlo.ttc",
        "/System/Library/Fonts/Monaco.ttf",
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Courier New.ttf",
        "/System/Library/Fonts/Supplemental/Courier New.ttf",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            return Some(bytes);
        }
    }
    None
}

/// Send the primary system font, glyph-fallback fonts, then a monospace face.
fn provide_fonts(content: &Child) -> io::Result<()> {
    if let Some(bytes) = system_font_bytes() {
        proto::send(content.channel(), Msg::ProvideFont { bytes }, &[])?;
        for bytes in fallback_font_bytes() {
            proto::send(content.channel(), Msg::ProvideFont { bytes }, &[])?;
        }
        if let Some(bytes) = monospace_font_bytes() {
            proto::send(content.channel(), Msg::ProvideMonoFont { bytes }, &[])?;
        }
    }
    Ok(())
}

/// Send the content process a font and a document to render.
fn provide_page(content: &Child, html: &str) -> io::Result<()> {
    provide_fonts(content)?;
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
        Ok(decode_html(&body))
    }
}

/// Decode HTML bytes to a string, honoring a declared charset. UTF-8 is used when
/// valid (or declared); a declared legacy charset (`windows-1252`/`iso-8859-1`/
/// `latin-1`) or invalid UTF-8 falls back to windows-1252.
fn decode_html(body: &[u8]) -> String {
    // Sniff a charset declaration from the head (`<meta charset>` or content-type).
    let head = String::from_utf8_lossy(&body[..body.len().min(2048)]).to_ascii_lowercase();
    let declared = head
        .split("charset")
        .nth(1)
        .map(|s| s.trim_start_matches([' ', '=', '"', '\'']))
        .map(|s| {
            s.split([' ', '"', '\'', '/', '>', ';'])
                .next()
                .unwrap_or("")
                .to_string()
        });
    let legacy = matches!(
        declared.as_deref(),
        Some("windows-1252" | "iso-8859-1" | "latin1" | "latin-1" | "cp1252")
    );
    if !legacy {
        if let Ok(s) = std::str::from_utf8(body) {
            return s.to_string();
        }
    }
    // windows-1252 (a superset of Latin-1): map each byte to its code point, with
    // the 0x80–0x9F C1 overrides.
    body.iter().map(|&b| win1252_char(b)).collect()
}

/// Map a windows-1252 byte to its Unicode character.
fn win1252_char(b: u8) -> char {
    const C1: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{8D}',
        '\u{017D}', '\u{8F}', '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}',
        '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}',
        '\u{9D}', '\u{017E}', '\u{0178}',
    ];
    match b {
        0x80..=0x9F => C1[(b - 0x80) as usize],
        _ => b as char,
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
    argus_domscript::apply_scripts_with_url(&mut doc, url);
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
    argus_domscript::apply_scripts_with_url(&mut doc, url);
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
        "aside" => "complementary",
        "section" => "region",
        "article" => "article",
        "figure" => "figure",
        "dialog" => "dialog",
        "select" => "listbox",
        "option" => "option",
        "progress" => "progressbar",
        "output" => "status",
        "fieldset" => "group",
        "details" => "group",
        "summary" => "button",
        "textarea" => "textbox",
        "p" => "paragraph",
        "table" => "table",
        "tr" => "row",
        "td" => "cell",
        "th" => "columnheader",
        "form" => "form",
        "hr" => "separator",
        "meter" => "meter",
        "menu" => "list",
        "datalist" => "listbox",
        _ => return None,
    })
}

/// The role for an `<input>`, refined by its `type` (a checkbox/radio/button/etc.
/// are distinct roles from a plain textbox). `hidden` inputs have no role.
fn input_role(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "checkbox" => "checkbox",
        "radio" => "radio",
        "button" | "submit" | "reset" | "image" => "button",
        "search" => "searchbox",
        "range" => "slider",
        "number" => "spinbutton",
        "email" | "tel" | "url" | "text" | "password" => "textbox",
        "hidden" => return None,
        _ => "textbox",
    })
}

/// ARIA/native state annotations appended after the role/name (e.g. `[disabled]`,
/// `[checked]`, `[expanded=true]`), for richer a11y snapshots.
fn aria_states(e: &argus_dom::ElementData) -> String {
    let mut s = String::new();
    let truthy = |v: Option<&str>| v == Some("true");
    if e.attr("disabled").is_some() || truthy(e.attr("aria-disabled")) {
        s.push_str(" [disabled]");
    }
    if e.attr("checked").is_some() || truthy(e.attr("aria-checked")) {
        s.push_str(" [checked]");
    }
    if e.attr("required").is_some() || truthy(e.attr("aria-required")) {
        s.push_str(" [required]");
    }
    if truthy(e.attr("aria-pressed")) {
        s.push_str(" [pressed]");
    }
    if let Some(v) = e.attr("aria-expanded").filter(|v| *v == "true" || *v == "false") {
        s.push_str(&format!(" [expanded={v}]"));
    }
    if let Some(v) = e.attr("aria-current").filter(|v| !v.is_empty()) {
        s.push_str(&format!(" [current={v}]"));
    }
    s
}

/// Whether an element is hidden by the HTML `hidden` attribute or an inline
/// `display: none` / `visibility: hidden` (the common cases the pure-DOM
/// extractors can detect without full style resolution).
fn el_hidden(e: &argus_dom::ElementData) -> bool {
    e.attr("hidden").is_some()
        || e.attr("style").is_some_and(|s| {
            let s: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
            s.contains("display:none") || s.contains("visibility:hidden")
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

    /// The text of the first child element named `child_tag` (e.g. a `<figure>`'s
    /// `<figcaption>`, a `<fieldset>`'s `<legend>`, a `<table>`'s `<caption>`).
    fn child_name(doc: &Document, id: NodeId, child_tag: &str) -> Option<String> {
        for c in doc.children(id) {
            if matches!(&doc.node(c).data, NodeData::Element(e) if e.name.is_html(child_tag)) {
                let mut s = String::new();
                text_of(doc, c, &mut s);
                let s = clean(&s);
                return (!s.is_empty()).then_some(s);
            }
        }
        None
    }

    /// The text content of the element with `id` (for `aria-labelledby`).
    fn id_text(doc: &Document, node: NodeId, id: &str) -> Option<String> {
        if doc.node(node).as_element().and_then(|e| e.attr("id")) == Some(id) {
            let mut s = String::new();
            text_of(doc, node, &mut s);
            return Some(s);
        }
        doc.children(node).find_map(|c| id_text(doc, c, id))
    }

    /// The accessible name a `<label>` gives a form control: a `<label for=id>`
    /// elsewhere in the document, or an ancestor `<label>` wrapping the control.
    fn label_name(doc: &Document, control: NodeId) -> Option<String> {
        fn find_label_for(doc: &Document, id: NodeId, target: &str) -> Option<NodeId> {
            if let NodeData::Element(e) = &doc.node(id).data {
                if e.name.is_html("label") && e.attr("for") == Some(target) {
                    return Some(id);
                }
            }
            doc.children(id).find_map(|c| find_label_for(doc, c, target))
        }
        if let Some(cid) = doc.node(control).as_element().and_then(|e| e.attr("id")) {
            if let Some(lbl) = find_label_for(doc, doc.root(), cid) {
                let mut s = String::new();
                text_of(doc, lbl, &mut s);
                let s = clean(&s);
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
        // Wrapping <label> ancestor.
        let mut p = doc.node(control).parent();
        while let Some(pid) = p {
            if matches!(&doc.node(pid).data, NodeData::Element(e) if e.name.is_html("label")) {
                let mut s = String::new();
                text_of(doc, pid, &mut s);
                let s = clean(&s);
                return (!s.is_empty()).then_some(s);
            }
            p = doc.node(pid).parent();
        }
        None
    }

    fn walk(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
        if let NodeData::Element(e) = &doc.node(id).data {
            // `aria-hidden="true"`, the HTML `hidden` attribute, or inline
            // display:none/visibility:hidden removes the element and its subtree.
            if e.attr("aria-hidden") == Some("true") || el_hidden(e) {
                return;
            }
        }
        let mut next_depth = depth;
        if let NodeData::Element(e) = &doc.node(id).data {
            let tag = &*e.name.local;
            if !matches!(tag, "head" | "title" | "style" | "script" | "meta" | "link") {
                // `role="presentation"`/`"none"` strips the element's semantics: it
                // emits no line of its own, but its children are still walked.
                let presentational = e
                    .attr("role")
                    .is_some_and(|r| matches!(r.trim(), "presentation" | "none"));
                // An explicit `role` attribute overrides the tag's implicit role;
                // `<input>` refines its role by `type`.
                let role: Option<&str> = if presentational {
                    None
                } else {
                    e.attr("role").filter(|r| !r.is_empty()).or_else(|| {
                        if tag == "input" {
                            input_role(e.attr("type").unwrap_or("text"))
                        } else if tag == "th" {
                            // `<th scope=row>` is a row header, else a column header.
                            match e.attr("scope") {
                                Some(s) if s.eq_ignore_ascii_case("row") => Some("rowheader"),
                                _ => Some("columnheader"),
                            }
                        } else {
                            implicit_role(tag)
                        }
                    })
                };
                if let Some(role) = role {
                    // Accessible name: `aria-label`, else `alt` for images, else text.
                    let name = if let Some(ids) = e.attr("aria-labelledby") {
                        // `aria-labelledby` wins: join the referenced elements' text.
                        let mut parts = Vec::new();
                        for rid in ids.split_whitespace() {
                            if let Some(t) = id_text(doc, doc.root(), rid) {
                                parts.push(t);
                            }
                        }
                        clean(&parts.join(" "))
                    } else if let Some(label) = e.attr("aria-label") {
                        clean(label)
                    } else if tag == "img" {
                        clean(e.attr("alt").unwrap_or(""))
                    } else if matches!(tag, "input" | "select" | "textarea") {
                        // Form controls take their name from an associated <label>.
                        label_name(doc, id).unwrap_or_default()
                    } else if let Some(cap) = match tag {
                        // These containers are named by a specific caption child, not
                        // by all of their descendant text.
                        "fieldset" => Some("legend"),
                        "figure" => Some("figcaption"),
                        "table" => Some("caption"),
                        _ => None,
                    } {
                        child_name(doc, id, cap).unwrap_or_default()
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
                    // Headings annotate their level (h1–h6 or `aria-level`).
                    if role == "heading" {
                        let level = tag
                            .strip_prefix('h')
                            .and_then(|n| n.parse::<u32>().ok())
                            .filter(|n| (1..=6).contains(n))
                            .or_else(|| e.attr("aria-level").and_then(|l| l.trim().parse().ok()));
                        if let Some(level) = level {
                            out.push_str(&format!(" [level={level}]"));
                        }
                    }
                    out.push_str(&aria_states(e));
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
    argus_domscript::apply_scripts_with_url(&mut doc, url);
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
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    let base = effective_base(&doc, url);
    Ok(extract_links(&doc, base.as_deref()))
}

/// Headless automation: fetch a page and return its `<img>`s as
/// `resolved-src<TAB>alt<TAB>WxH` lines (W/H from the `width`/`height` attrs, `?`
/// if absent), in document order. Used by `--dump-images`.
pub fn dump_images(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    let base = effective_base(&doc, url);
    Ok(extract_images(&doc, base.as_deref()))
}

/// Collect `<img>` elements as `src<TAB>alt<TAB>WxH` lines (pure, unit-testable).
fn extract_images(doc: &argus_dom::Document, base: Option<&str>) -> String {
    use argus_dom::{Document, NodeData, NodeId};
    fn walk(doc: &Document, id: NodeId, base: Option<&str>, out: &mut String) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("img") {
                // Prefer the explicit `src`; fall back to the best `srcset`
                // candidate so srcset-only responsive images are still listed.
                let url = e
                    .attr("src")
                    .or_else(|| e.attr("srcset").and_then(srcset_best));
                if let Some(src) = url {
                    let alt = e.attr("alt").unwrap_or("");
                    let w = e.attr("width").unwrap_or("?");
                    let h = e.attr("height").unwrap_or("?");
                    out.push_str(&format!("{}\t{alt}\t{w}x{h}\n", resolve_url(base, src)));
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

/// Pick the highest-resolution candidate URL from an `srcset` attribute. Each
/// comma-separated candidate is `URL [descriptor]` where the descriptor is a `w`
/// (width) or `x` (density) value; the largest descriptor wins, falling back to
/// the last candidate when descriptors are absent or unparseable.
fn srcset_best(srcset: &str) -> Option<&str> {
    let mut best: Option<(&str, f32)> = None;
    for cand in srcset.split(',') {
        let cand = cand.trim();
        if cand.is_empty() {
            continue;
        }
        let mut parts = cand.split_whitespace();
        let Some(url) = parts.next() else { continue };
        // `w`/`x` descriptor → numeric weight; bare URL (single candidate) → 1x.
        let weight = parts
            .next()
            .and_then(|d| d.trim_end_matches(['w', 'x']).parse::<f32>().ok())
            .unwrap_or(1.0);
        if best.is_none_or(|(_, bw)| weight >= bw) {
            best = Some((url, weight));
        }
    }
    best.map(|(u, _)| u)
}

/// The base URL for resolving the document's relative links: the first
/// `<base href>` (resolved against the page URL), else the page URL itself.
fn effective_base(doc: &argus_dom::Document, url: Option<&str>) -> Option<String> {
    use argus_dom::{Document, NodeData, NodeId};
    fn find(doc: &Document, id: NodeId) -> Option<String> {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("base") {
                if let Some(h) = e.attr("href").filter(|h| !h.trim().is_empty()) {
                    return Some(h.to_string());
                }
            }
        }
        doc.children(id).find_map(|c| find(doc, c))
    }
    match find(doc, doc.root()) {
        Some(href) => Some(resolve_url(url, &href)),
        None => url.map(str::to_string),
    }
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
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    Ok(extract_headings(&doc))
}

/// Headless automation: fetch a page and return a structured JSON summary
/// (`{title, headings:[{level,text}], links:[{text,href}]}`) for machine-readable
/// pipelines. Used by `--dump-json`.
pub fn dump_json(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    let base = effective_base(&doc, url);
    Ok(extract_json(&doc, base.as_deref()))
}

/// Headless automation: fetch a page and return its DOM as a nested JSON tree
/// (`{tag, attrs, children}`, text nodes as `{text}`) — a CDP-style structured
/// snapshot. Used by `--dump-domtree`.
pub fn dump_domtree(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    let mut out = String::new();
    // Serialize the document's element children (skipping the synthetic root).
    let root = doc.root();
    out.push('[');
    let mut first = true;
    for c in doc.children(root) {
        if let Some(node) = dom_node_json(&doc, c) {
            if !first {
                out.push(',');
            }
            out.push_str(&node);
            first = false;
        }
    }
    out.push_str("]\n");
    Ok(out)
}

/// One DOM node as JSON: elements become `{"tag","attrs","children"}`; non-blank
/// text nodes become `{"text":"…"}`; whitespace-only text and comments are
/// dropped. Returns `None` for skipped nodes.
fn dom_node_json(doc: &argus_dom::Document, id: argus_dom::NodeId) -> Option<String> {
    use argus_dom::NodeData;
    match &doc.node(id).data {
        NodeData::Text(t) => {
            let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
            (!t.is_empty()).then(|| format!("{{\"text\":\"{}\"}}", json_escape(&t)))
        }
        NodeData::Element(e) => {
            let mut attrs: Vec<_> = e.attrs.iter().collect();
            attrs.sort_by(|a, b| a.name.cmp(&b.name));
            let attr_json: Vec<String> = attrs
                .iter()
                .map(|a| format!("\"{}\":\"{}\"", json_escape(&a.name), json_escape(&a.value)))
                .collect();
            let children: Vec<String> = doc
                .children(id)
                .filter_map(|c| dom_node_json(doc, c))
                .collect();
            Some(format!(
                "{{\"tag\":\"{}\",\"attrs\":{{{}}},\"children\":[{}]}}",
                json_escape(&e.name.local),
                attr_json.join(","),
                children.join(",")
            ))
        }
        _ => None,
    }
}

/// Escape a string for inclusion in a JSON double-quoted value.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Build a JSON object `{title, headings, links}` from the document. Pure (no I/O),
/// unit-testable; `base` resolves relative link hrefs.
fn extract_json(doc: &argus_dom::Document, base: Option<&str>) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    fn text_of(doc: &Document, id: NodeId) -> String {
        fn go(doc: &Document, id: NodeId, out: &mut String) {
            match &doc.node(id).data {
                NodeData::Text(t) => out.push_str(t),
                _ => {
                    for c in doc.children(id) {
                        go(doc, c, out);
                    }
                }
            }
        }
        let mut s = String::new();
        go(doc, id, &mut s);
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    let mut title = String::new();
    let mut headings: Vec<(u8, String)> = Vec::new();
    let mut links: Vec<(String, String)> = Vec::new();
    fn walk(
        doc: &Document,
        id: NodeId,
        base: Option<&str>,
        title: &mut String,
        headings: &mut Vec<(u8, String)>,
        links: &mut Vec<(String, String)>,
    ) {
        if let NodeData::Element(e) = &doc.node(id).data {
            let tag = &*e.name.local;
            match tag {
                "title" if title.is_empty() => *title = text_of(doc, id),
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    let level = tag.as_bytes()[1] - b'0';
                    headings.push((level, text_of(doc, id)));
                }
                "a" => {
                    if let Some(href) = e.attr("href") {
                        links.push((text_of(doc, id), resolve_url(base, href)));
                    }
                }
                _ => {}
            }
        }
        for c in doc.children(id) {
            walk(doc, c, base, title, headings, links);
        }
    }
    walk(doc, doc.root(), base, &mut title, &mut headings, &mut links);

    let hs: Vec<String> = headings
        .iter()
        .map(|(l, t)| format!("{{\"level\":{l},\"text\":\"{}\"}}", json_escape(t)))
        .collect();
    let ls: Vec<String> = links
        .iter()
        .map(|(t, h)| {
            format!(
                "{{\"text\":\"{}\",\"href\":\"{}\"}}",
                json_escape(t),
                json_escape(h)
            )
        })
        .collect();
    format!(
        "{{\"title\":\"{}\",\"headings\":[{}],\"links\":[{}]}}\n",
        json_escape(&title),
        hs.join(","),
        ls.join(",")
    )
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

/// Headless automation: fetch a page and return a summary of its **forms** and
/// their controls (name/type/value), for scripted form-filling and scraping.
/// Used by `--dump-forms`.
pub fn dump_forms(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    let base = effective_base(&doc, url);
    Ok(extract_forms(&doc, base.as_deref()))
}

/// Headless automation: fetch a page and return its **metadata** (title, lang,
/// charset, description/keywords/author/robots, canonical URL, and `og:`/`twitter:`
/// social tags) as `key: value` lines. Used by `--dump-meta` (SEO / scraping).
/// Headless automation: fetch a page and return its `<table>`s as TSV — each row
/// a line of tab-separated cell texts, tables separated by a blank line. Used by
/// `--dump-tables` (data scraping).
pub fn dump_tables(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    Ok(extract_tables(&doc))
}

/// Collect each `<table>` as TSV rows (cells tab-separated, whitespace-collapsed);
/// tables are separated by a blank line. Pure (no I/O), unit-testable.
fn extract_tables(doc: &argus_dom::Document) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    fn cell_text(doc: &Document, id: NodeId) -> String {
        fn go(doc: &Document, id: NodeId, out: &mut String) {
            match &doc.node(id).data {
                NodeData::Text(t) => out.push_str(t),
                _ => {
                    for c in doc.children(id) {
                        go(doc, c, out);
                    }
                }
            }
        }
        let mut s = String::new();
        go(doc, id, &mut s);
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }
    // Rows of a table: each `<tr>` (anywhere under the table) → its `<td>`/`<th>`.
    fn rows_of(doc: &Document, table: NodeId, out: &mut String) {
        fn walk_rows(doc: &Document, id: NodeId, out: &mut String) {
            if let NodeData::Element(e) = &doc.node(id).data {
                if e.name.is_html("tr") {
                    let cells: Vec<String> = doc
                        .children(id)
                        .filter(|&c| {
                            matches!(&doc.node(c).data, NodeData::Element(e)
                                if e.name.is_html("td") || e.name.is_html("th"))
                        })
                        .map(|c| cell_text(doc, c))
                        .collect();
                    out.push_str(&cells.join("\t"));
                    out.push('\n');
                    return; // don't descend into nested tables' rows here
                }
            }
            for c in doc.children(id) {
                walk_rows(doc, c, out);
            }
        }
        walk_rows(doc, table, out);
    }

    let mut tables: Vec<NodeId> = Vec::new();
    fn find_tables(doc: &Document, id: NodeId, out: &mut Vec<NodeId>) {
        if matches!(&doc.node(id).data, NodeData::Element(e) if e.name.is_html("table")) {
            out.push(id);
        }
        for c in doc.children(id) {
            find_tables(doc, c, out);
        }
    }
    find_tables(doc, doc.root(), &mut tables);

    let mut out = String::new();
    for (i, &t) in tables.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        rows_of(doc, t, &mut out);
    }
    out
}

pub fn dump_meta(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    let base = effective_base(&doc, url);
    Ok(extract_meta(&doc, base.as_deref()))
}

/// Headless automation: fetch a page and return its JSON-LD structured data —
/// the verbatim bodies of every `<script type="application/ld+json">` block,
/// wrapped into a single JSON array (`[block, block, …]`) for scraping/SEO
/// pipelines. Used by `--dump-jsonld`.
pub fn dump_jsonld(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    Ok(extract_jsonld(&doc))
}

/// Headless automation: fetch a page and return its HTML **microdata**
/// (`itemscope`/`itemtype`/`itemprop`) as a JSON array of `{type, props}` items,
/// for structured-data scraping. Used by `--dump-microdata`.
pub fn dump_microdata(url: Option<&str>) -> io::Result<String> {
    log::set_role(Role::Browser);
    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), Size::new(800, 600))?;
    let html = resolve_html(&net, url);
    proto::send(net.channel(), Msg::Shutdown, &[])?;
    net.wait()?;
    let mut doc = argus_html::parse(&html);
    argus_domscript::apply_scripts_with_url(&mut doc, url);
    let base = effective_base(&doc, url);
    Ok(extract_microdata(&doc, base.as_deref()))
}

/// Collect top-level microdata items (`itemscope` elements not nested inside
/// another item) as a JSON array of `{"type": …, "props": {name: value}}`. A
/// property's value is its `content`/`href`/`src`/`datetime` attribute where it
/// has one, else its text. Nested item values are the nested item's type URL.
/// Pure (no I/O), unit-testable.
fn extract_microdata(doc: &argus_dom::Document, base: Option<&str>) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    fn text_of(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
        }
    }
    // The value of an `itemprop` element.
    fn prop_value(doc: &Document, id: NodeId, base: Option<&str>) -> String {
        let Some(e) = doc.node(id).as_element() else {
            return String::new();
        };
        let tag = &*e.name.local;
        if let Some(t) = e.attr("itemtype") {
            // A nested item: use its type URL as the value.
            return t.trim().to_string();
        }
        // URL-valued elements resolve against the base; others use their value attr.
        match tag {
            "a" | "area" | "link" => return e.attr("href").map(|h| resolve_url(base, h)).unwrap_or_default(),
            "img" | "audio" | "video" | "source" | "iframe" | "embed" | "track" => {
                return e.attr("src").map(|h| resolve_url(base, h)).unwrap_or_default()
            }
            "object" => return e.attr("data").map(|h| resolve_url(base, h)).unwrap_or_default(),
            "meta" => return e.attr("content").unwrap_or("").to_string(),
            "time" => {
                if let Some(dt) = e.attr("datetime") {
                    return dt.to_string();
                }
            }
            _ => {}
        }
        let mut s = String::new();
        text_of(doc, id, &mut s);
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    // Collect each item's direct `itemprop` descendants (not crossing into a
    // nested `itemscope`).
    fn item_props(doc: &Document, item: NodeId, base: Option<&str>, out: &mut Vec<(String, String)>) {
        for c in doc.children(item) {
            let Some(e) = doc.node(c).as_element() else {
                continue;
            };
            if let Some(name) = e.attr("itemprop") {
                out.push((name.trim().to_string(), prop_value(doc, c, base)));
            }
            // Recurse unless this child starts a new (nested) item.
            if e.attr("itemscope").is_none() {
                item_props(doc, c, base, out);
            }
        }
    }

    // Find top-level itemscope elements (no itemscope ancestor).
    fn find_items(doc: &Document, id: NodeId, in_item: bool, out: &mut Vec<NodeId>) {
        let is_scope = doc
            .node(id)
            .as_element()
            .is_some_and(|e| e.attr("itemscope").is_some());
        if is_scope && !in_item {
            out.push(id);
        }
        for c in doc.children(id) {
            find_items(doc, c, in_item || is_scope, out);
        }
    }

    let mut items = Vec::new();
    find_items(doc, doc.root(), false, &mut items);
    let mut json = Vec::new();
    for item in items {
        let ty = doc
            .node(item)
            .as_element()
            .and_then(|e| e.attr("itemtype"))
            .unwrap_or("")
            .trim();
        let mut props = Vec::new();
        item_props(doc, item, base, &mut props);
        let props_json: Vec<String> = props
            .iter()
            .map(|(k, v)| format!("\"{}\":\"{}\"", json_escape(k), json_escape(v)))
            .collect();
        json.push(format!(
            "{{\"type\":\"{}\",\"props\":{{{}}}}}",
            json_escape(ty),
            props_json.join(",")
        ));
    }
    format!("[{}]\n", json.join(","))
}

/// Collect the bodies of every `<script type="application/ld+json">` element
/// (type matched case-insensitively, ignoring any `; charset=…` suffix) into a
/// JSON array of their verbatim, whitespace-trimmed contents. Blank blocks are
/// skipped; an empty result is `[]`. Pure (no I/O), unit-testable.
fn extract_jsonld(doc: &argus_dom::Document) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    fn text_of(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
        }
    }
    fn walk(doc: &Document, id: NodeId, out: &mut Vec<String>) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("script") {
                let ty = e.attr("type").unwrap_or("");
                let ty = ty.split(';').next().unwrap_or("").trim();
                if ty.eq_ignore_ascii_case("application/ld+json") {
                    let mut body = String::new();
                    text_of(doc, id, &mut body);
                    let body = body.trim();
                    if !body.is_empty() {
                        out.push(body.to_string());
                    }
                }
            }
        }
        for c in doc.children(id) {
            walk(doc, c, out);
        }
    }
    let mut blocks = Vec::new();
    walk(doc, doc.root(), &mut blocks);
    format!("[{}]\n", blocks.join(","))
}

/// Collect document metadata as `key: value` lines: `title`, `lang` (`<html lang>`),
/// `charset`, `<meta name=…>` values (description/keywords/author/robots/viewport/
/// theme-color), `<meta property=og:…>` / `name=twitter:…` social tags, and the
/// `<link>` URLs `canonical`, `icon` (favicon), `alternate:<lang>` hreflang
/// variants, and RSS/Atom `feed`s (all resolved against `base`). Pure (no I/O),
/// unit-testable.
fn extract_meta(doc: &argus_dom::Document, base: Option<&str>) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    fn text_of(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
        }
    }
    // Ordered (key, value) pairs; document order within each category.
    let mut out: Vec<(String, String)> = Vec::new();
    let collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");

    fn walk(
        doc: &Document,
        id: NodeId,
        base: Option<&str>,
        out: &mut Vec<(String, String)>,
        collapse: &dyn Fn(&str) -> String,
        text_of: &dyn Fn(&Document, NodeId, &mut String),
    ) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("title") {
                let mut t = String::new();
                text_of(doc, id, &mut t);
                out.push(("title".to_string(), collapse(&t)));
            } else if e.name.is_html("html") {
                if let Some(lang) = e.attr("lang") {
                    out.push(("lang".to_string(), lang.to_string()));
                }
            } else if e.name.is_html("meta") {
                if let Some(cs) = e.attr("charset") {
                    out.push(("charset".to_string(), cs.to_string()));
                }
                let content = e.attr("content").unwrap_or("");
                if let Some(name) = e.attr("name") {
                    if !content.is_empty() {
                        out.push((name.to_ascii_lowercase(), collapse(content)));
                    }
                } else if let Some(prop) = e.attr("property") {
                    if !content.is_empty() {
                        out.push((prop.to_ascii_lowercase(), collapse(content)));
                    }
                } else if let Some(equiv) = e.attr("http-equiv") {
                    // `http-equiv="refresh"` is a client-side redirect/refresh
                    // crawlers care about; surface its `content` (delay; url=…).
                    if !content.is_empty() {
                        out.push((equiv.to_ascii_lowercase(), collapse(content)));
                    }
                }
            } else if e.name.is_html("link") {
                let rel = e.attr("rel").unwrap_or("");
                let href = e.attr("href").filter(|h| !h.trim().is_empty());
                if let Some(href) = href {
                    if rel.eq_ignore_ascii_case("canonical") {
                        out.push(("canonical".to_string(), resolve_url(base, href)));
                    } else if rel.split_ascii_whitespace().any(|t| t.eq_ignore_ascii_case("icon")) {
                        // `icon` / `shortcut icon` — the favicon URL.
                        out.push(("icon".to_string(), resolve_url(base, href)));
                    } else if rel.eq_ignore_ascii_case("alternate") {
                        // hreflang alternates (key includes the language) or RSS/Atom feeds.
                        if let Some(lang) = e.attr("hreflang") {
                            out.push((format!("alternate:{}", lang.to_ascii_lowercase()), resolve_url(base, href)));
                        } else if e.attr("type").is_some_and(|t| t.contains("rss") || t.contains("atom")) {
                            out.push(("feed".to_string(), resolve_url(base, href)));
                        }
                    }
                }
            }
        }
        for c in doc.children(id) {
            walk(doc, c, base, out, collapse, text_of);
        }
    }
    walk(doc, doc.root(), base, &mut out, &collapse, &text_of);

    let mut s = String::new();
    for (k, v) in out {
        s.push_str(&format!("{k}: {v}\n"));
    }
    s
}

/// Render each `<form>` as a header line (`form[i] action=… method=…`) followed
/// by indented control lines (`input name=… type=… value=…`, plus `checked` /
/// `selected`). Controls outside any form are grouped under `form[-]`. Pure (no
/// I/O) so it's unit-testable; `base` resolves the relative `action`.
fn extract_forms(doc: &argus_dom::Document, base: Option<&str>) -> String {
    use argus_dom::{Document, NodeData, NodeId};

    fn text_of(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    text_of(doc, c, out);
                }
            }
        }
    }
    // Describe one control element; returns None for non-control elements.
    fn control_line(doc: &Document, id: NodeId) -> Option<String> {
        let NodeData::Element(e) = &doc.node(id).data else {
            return None;
        };
        let name = e.attr("name").unwrap_or("");
        if e.name.is_html("input") {
            let ty = e.attr("type").unwrap_or("text");
            let val = e.attr("value").unwrap_or("");
            let mut s = format!("input name={name} type={ty} value={val}");
            if matches!(ty, "checkbox" | "radio") && e.attr("checked").is_some() {
                s.push_str(" checked");
            }
            Some(s)
        } else if e.name.is_html("textarea") {
            let mut val = String::new();
            text_of(doc, id, &mut val);
            let val = val.trim();
            Some(format!("textarea name={name} value={val}"))
        } else if e.name.is_html("button") {
            let ty = e.attr("type").unwrap_or("submit");
            Some(format!("button name={name} type={ty}"))
        } else if e.name.is_html("select") {
            // The selected option's value (the one marked `selected`, else first).
            let mut chosen: Option<String> = None;
            let mut first: Option<String> = None;
            fn opts(doc: &Document, id: NodeId, first: &mut Option<String>, chosen: &mut Option<String>) {
                for c in doc.children(id) {
                    if let NodeData::Element(e) = &doc.node(c).data {
                        if e.name.is_html("option") {
                            let mut label = String::new();
                            text_of(doc, c, &mut label);
                            let v = e.attr("value").map(str::to_string).unwrap_or_else(|| label.trim().to_string());
                            if first.is_none() {
                                *first = Some(v.clone());
                            }
                            if e.attr("selected").is_some() {
                                *chosen = Some(v);
                            }
                        }
                    }
                    opts(doc, c, first, chosen);
                }
            }
            opts(doc, id, &mut first, &mut chosen);
            let val = chosen.or(first).unwrap_or_default();
            Some(format!("select name={name} value={val}"))
        } else {
            None
        }
    }
    fn collect_controls(doc: &Document, id: NodeId, out: &mut String) {
        for c in doc.children(id) {
            if let NodeData::Element(e) = &doc.node(c).data {
                // Don't descend into nested forms; they get their own header.
                if e.name.is_html("form") {
                    continue;
                }
                if let Some(line) = control_line(doc, c) {
                    out.push_str("  ");
                    out.push_str(&line);
                    out.push('\n');
                }
            }
            collect_controls(doc, c, out);
        }
    }

    let mut out = String::new();
    let mut idx = 0usize;
    fn walk(doc: &Document, id: NodeId, base: Option<&str>, idx: &mut usize, out: &mut String) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("form") {
                let action = e.attr("action").map(|a| resolve_url(base, a)).unwrap_or_default();
                let method = e.attr("method").unwrap_or("get").to_lowercase();
                out.push_str(&format!("form[{}] action={action} method={method}\n", *idx));
                *idx += 1;
                collect_controls(doc, id, out);
                return; // controls already gathered; don't double-walk into them
            }
        }
        for c in doc.children(id) {
            walk(doc, c, base, idx, out);
        }
    }
    walk(doc, doc.root(), base, &mut idx, &mut out);
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
            } else if e.name.is_html("area") {
                // Image-map links carry their label in `alt` (no text content).
                if let Some(href) = e.attr("href") {
                    let text = e.attr("alt").unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" ");
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
                if is_hidden(tag) || el_hidden(e) {
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
    provide_fonts(&content)?;
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
/// scroll, and render a frame. Returns the rendered frame and its content height
/// (the caller composites the tab bar and presents).
#[cfg(target_os = "macos")]
fn load_page(
    content: &Child,
    net: &Child,
    window: &argus_platform::window::Window,
    target: Option<&str>,
) -> io::Result<(Framebuffer, u32)> {
    let page = match target {
        Some(u) => fetch_html(net, u).unwrap_or_else(|e| error_page(u, &e.to_string())),
        None => sample_html(),
    };
    let title = page_title(&page);
    window.set_title(if title.is_empty() { "Argus" } else { &title });
    provide_page(content, &page)?;
    proto::send(content.channel(), Msg::SetScroll { y: 0 }, &[])?;
    request_frame(content, net, target)
}

/// Composite the page `content` frame beneath the tab strip and present the whole
/// window. The window is `full`-sized; the content was rendered `TAB_BAR_H` px
/// shorter, so it sits below the bar.
#[cfg(target_os = "macos")]
fn present_framed(
    window: &argus_platform::window::Window,
    content: &Framebuffer,
    tabs: &Tabs,
    full: Size,
) -> io::Result<()> {
    let (w, h) = (full.width, full.height);
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
    tabbar::draw(&mut buf, w, tabs.len(), tabs.active_index());
    // Copy the content frame into the rows below the tab strip.
    let csize = content.size();
    let src = content.pixels();
    let row_bytes = (csize.width.min(w) as usize) * 4;
    for row in 0..csize.height {
        let dst_y = tabbar::TAB_BAR_H + row;
        if dst_y >= h {
            break;
        }
        let s = (row as usize) * (csize.width as usize) * 4;
        let d = (dst_y as usize) * (w as usize) * 4;
        if s + row_bytes <= src.len() && d + row_bytes <= buf.len() {
            buf[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
        }
    }
    window.present(&buf, full);
    Ok(())
}

/// Spawn a fresh content process for a new tab and handshake it at `vp`. Each tab
/// owns an isolated, sandboxed content process so its DOM/JS/scroll state is kept
/// when other tabs are active.
#[cfg(target_os = "macos")]
fn spawn_content_tab(vp: Size) -> io::Result<Child> {
    let child = spawn_child(Role::Content)?;
    proto::parent_handshake(child.channel(), vp)?;
    Ok(child)
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
    // The window is 800×600; the page renders into the area below the tab strip.
    let window_size = Size::new(800, 600);
    let content_vp = Size::new(window_size.width, window_size.height - tabbar::TAB_BAR_H);
    log!(
        "starting (windowed); window {}x{}, content {}x{}",
        window_size.width,
        window_size.height,
        content_vp.width,
        content_vp.height
    );

    let mut net = spawn_child(Role::NetService)?;
    proto::parent_handshake(net.channel(), content_vp)?;
    // Each tab is an isolated content process; `procs[i]` renders `tabs.tabs[i]`.
    let mut tabs = Tabs::new(url.clone().unwrap_or_default());
    let mut procs: Vec<Child> = vec![spawn_content_tab(content_vp)?];
    // A URL Option for the active tab (empty string = the built-in home page).
    let active_target = |tabs: &Tabs| -> Option<String> {
        let u = tabs.active().url();
        (!u.is_empty()).then(|| u.to_string())
    };

    let html = resolve_html(&net, active_target(&tabs).as_deref());
    provide_page(&procs[0], &html)?;
    log!("children handshook; page sent; opening window");

    // Present the first frame.
    let (frame, mut content_height) = request_frame(&procs[0], &net, active_target(&tabs).as_deref())?;
    let title = page_title(&html);
    let window = Window::open(if title.is_empty() { "Argus" } else { &title }, window_size);
    present_framed(&window, &frame, &tabs, window_size)?;
    log!("window open — per-tab processes; click a tab to switch (state kept), its right edge to close, + to open; Cmd+T/W/Shift+[ ]/1-9 too");

    // Load the active tab's page into its process and present it.
    macro_rules! reload_active {
        () => {{
            let proc = &procs[tabs.active_index()];
            let (frame, h) = load_page(proc, &net, &window, active_target(&tabs).as_deref())?;
            content_height = h;
            present_framed(&window, &frame, &tabs, window_size)?;
        }};
    }
    // Re-render the active tab's process *without* reloading — it kept its DOM,
    // JS, and scroll state, so switching back is instant and stateful.
    macro_rules! show_active {
        () => {{
            let proc = &procs[tabs.active_index()];
            let (frame, h) = request_frame(proc, &net, active_target(&tabs).as_deref())?;
            content_height = h;
            present_framed(&window, &frame, &tabs, window_size)?;
        }};
    }
    // Open a new tab: spawn its own content process, then load the home page.
    macro_rules! open_tab {
        () => {{
            tabs.open(String::new());
            procs.push(spawn_content_tab(content_vp)?);
            log!("opened tab {} of {} (own process)", tabs.active_index() + 1, tabs.len());
            reload_active!();
        }};
    }
    // Close tab `i`: refuse if it is the last; else shut down + reap its process.
    macro_rules! close_tab {
        ($i:expr) => {{
            let i = $i;
            if tabs.close(i) {
                let _ = proto::send(procs[i].channel(), Msg::Shutdown, &[]);
                let _ = procs.remove(i).wait();
                show_active!();
                true
            } else {
                false
            }
        }};
    }
    loop {
        match window.next_event() {
            Event::MouseDown { x, y } if y < tabbar::TAB_BAR_H => {
                // A click in the tab strip switches / closes / opens a tab.
                match tabbar::hit_test(x, y, tabs.len(), window_size.width) {
                    Some(tabbar::TabHit::New) => open_tab!(),
                    Some(tabbar::TabHit::Switch(i)) if i != tabs.active_index() => {
                        tabs.switch_to(i);
                        show_active!();
                    }
                    Some(tabbar::TabHit::Close(i)) => {
                        let closed = close_tab!(i);
                        if !closed {
                            break; // refused only for the last tab
                        }
                    }
                    _ => {}
                }
            }
            Event::MouseDown { x, y } => {
                // A click in the page area (offset past the tab strip).
                let y = y - tabbar::TAB_BAR_H;
                let ch = procs[tabs.active_index()].channel();
                proto::send(ch, Msg::InputClick { x, y }, &[])?;
                if let Msg::ClickResult { url } = proto::recv(ch)?.0 {
                    if !url.is_empty() {
                        let target = resolve_url(active_target(&tabs).as_deref(), &url);
                        log!("navigating to {target}");
                        tabs.active_mut().history.push(target);
                        tabs.active_mut().scroll_y = 0;
                        reload_active!();
                    } else {
                        // Non-navigation click: a DOM handler may have changed the
                        // page — re-render.
                        show_active!();
                    }
                }
            }
            Event::Scroll { dy } => {
                let max_scroll = content_height.saturating_sub(content_vp.height);
                let cur = tabs.active().scroll_y;
                let next = (cur as i64 - dy as i64).clamp(0, max_scroll as i64) as u32;
                if next != cur {
                    tabs.active_mut().scroll_y = next;
                    proto::send(procs[tabs.active_index()].channel(), Msg::SetScroll { y: next }, &[])?;
                    show_active!();
                }
            }
            Event::KeyChar { ch } => {
                proto::send(procs[tabs.active_index()].channel(), Msg::InputKey { ch }, &[])?;
                show_active!();
            }
            Event::Back => {
                if tabs.active_mut().history.back().is_some() {
                    tabs.active_mut().scroll_y = 0;
                    reload_active!();
                }
            }
            Event::Forward => {
                if tabs.active_mut().history.forward().is_some() {
                    tabs.active_mut().scroll_y = 0;
                    reload_active!();
                }
            }
            Event::NewTab => open_tab!(),
            Event::CloseTab => {
                if !close_tab!(tabs.active_index()) {
                    log!("closing last tab; shutting down");
                    break;
                }
            }
            Event::NextTab => {
                tabs.next();
                show_active!();
            }
            Event::PrevTab => {
                tabs.prev();
                show_active!();
            }
            Event::SwitchTab { index } => {
                let target = if index == 8 { tabs.len() - 1 } else { index };
                if target != tabs.active_index() && target < tabs.len() {
                    tabs.switch_to(target);
                    show_active!();
                }
            }
            Event::CloseRequested => {
                log!("window closed; shutting down");
                break;
            }
        }
    }

    // Shut down every tab's content process and the net service.
    for mut p in procs {
        let _ = proto::send(p.channel(), Msg::Shutdown, &[]);
        let _ = p.wait();
    }
    proto::send(net.channel(), Msg::Shutdown, &[])?;
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
    use super::{
        decode_html, dom_node_json, effective_base, extract_forms, extract_images, extract_json,
        extract_jsonld, extract_links, extract_meta, extract_microdata, extract_tables,
        page_title, render_text, resolve_url, srcset_best, History, Tabs,
    };

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
        assert!(tree.contains("heading \"Title\" [level=1]"), "heading w/ level:\n{tree}");
        // aria-label overrides the text content as the accessible name.
        assert!(tree.contains("button \"Close dialog\""), "aria-label:\n{tree}");
        // An explicit role attribute is honored.
        assert!(tree.contains("alert \"Heads up\""), "role attr:\n{tree}");
        // aria-hidden prunes the element.
        assert!(!tree.contains("secret"), "aria-hidden should prune:\n{tree}");
    }

    #[test]
    fn a11y_tree_input_roles_and_states() {
        let doc = argus_html::parse(
            "<input type=\"checkbox\" checked aria-label=\"Agree\">\
             <input type=\"text\" disabled aria-label=\"Name\">\
             <input type=\"hidden\" name=\"tok\">\
             <button aria-expanded=\"true\" aria-label=\"Menu\">M</button>\
             <select aria-label=\"Country\"><option>US</option></select>",
        );
        let tree = super::a11y_tree(&doc);
        // input type refines the role; native/ARIA states are annotated.
        assert!(tree.contains("checkbox \"Agree\" [checked]"), "checkbox state:\n{tree}");
        assert!(tree.contains("textbox \"Name\" [disabled]"), "disabled textbox:\n{tree}");
        // hidden inputs have no role (not surfaced).
        assert!(!tree.contains("\"tok\""), "hidden input pruned:\n{tree}");
        assert!(tree.contains("button \"Menu\" [expanded=true]"), "expanded button:\n{tree}");
        assert!(tree.contains("listbox \"Country\""), "select→listbox:\n{tree}");
        assert!(tree.contains("option"), "option role:\n{tree}");
    }

    #[test]
    fn a11y_tree_separator_meter_and_spinbutton_roles() {
        let doc = argus_html::parse(
            "<hr>\
             <meter value=\"0.6\" aria-label=\"Disk\"></meter>\
             <input type=\"number\" aria-label=\"Qty\">\
             <input type=\"email\" aria-label=\"Email\">",
        );
        let tree = super::a11y_tree(&doc);
        assert!(tree.contains("separator"), "hr→separator:\n{tree}");
        assert!(tree.contains("meter \"Disk\""), "meter role:\n{tree}");
        // <th scope=row> is a rowheader; a plain <th> is a columnheader.
        let t2 = super::a11y_tree(&argus_html::parse(
            "<table><tr><th scope=\"row\">R</th><th>C</th></tr></table>",
        ));
        assert!(t2.contains("rowheader"), "th scope=row → rowheader:\n{t2}");
        assert!(t2.contains("columnheader"), "plain th → columnheader:\n{t2}");

        // A control's <label> (for= or wrapping) gives its accessible name.
        let t3 = super::a11y_tree(&argus_html::parse(
            "<label for=\"e\">Email</label><input id=\"e\" type=\"text\">\
             <label>Phone <input type=\"tel\"></label>",
        ));
        assert!(t3.contains("textbox \"Email\""), "label[for] names the input:\n{t3}");
        assert!(t3.contains("textbox \"Phone\""), "wrapping label names the input:\n{t3}");

        // aria-labelledby wins and joins referenced elements' text.
        let t4 = super::a11y_tree(&argus_html::parse(
            "<span id=\"a\">Save</span><span id=\"b\">changes</span>\
             <button aria-labelledby=\"a b\">x</button>",
        ));
        assert!(t4.contains("button \"Save changes\""), "labelledby joins refs:\n{t4}");

        // Containers are named by their caption child, not all descendant text.
        let t5 = super::a11y_tree(&argus_html::parse(
            "<figure><img src=\"x\" alt=\"\"><figcaption>A cat</figcaption></figure>\
             <fieldset><legend>Address</legend><input></fieldset>",
        ));
        assert!(t5.contains("figure \"A cat\""), "figure named by figcaption:\n{t5}");
        assert!(t5.contains("group \"Address\""), "fieldset named by legend:\n{t5}");

        // role=presentation strips the element's own line but keeps its children.
        let t6 = super::a11y_tree(&argus_html::parse(
            "<ul role=\"presentation\"><li>Item</li></ul>",
        ));
        assert!(!t6.lines().any(|l| l.trim() == "list"), "presentation removes the list role:\n{t6}");
        assert!(t6.lines().any(|l| l.trim().starts_with("listitem")), "children still appear:\n{t6}");
        assert!(tree.contains("spinbutton \"Qty\""), "number→spinbutton:\n{tree}");
        assert!(tree.contains("textbox \"Email\""), "email→textbox:\n{tree}");
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
             <a href=\"//cdn.example/lib.js\">proto-relative</a>\
             <map><area href=\"/region\" alt=\"Map region\"></map>",
        );
        let out = extract_links(&doc, Some("https://site.example/dir/page.html"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "About\thttps://site.example/about");
        assert_eq!(lines[1], "Contact us\thttps://site.example/dir/contact.html");
        assert_eq!(lines[2], "External\thttps://other.example/x");
        assert_eq!(lines[3], "proto-relative\thttps://cdn.example/lib.js");
        // <area href> (image-map link) is listed with its alt as the text.
        assert_eq!(lines[4], "Map region\thttps://site.example/region");
        assert_eq!(lines.len(), 5, "<a href> and <area href> are listed");
    }

    #[test]
    fn dom_node_json_serializes_tree() {
        let doc = argus_html::parse("<div id=\"a\" class=\"c\">hi <b>x</b></div>");
        // Find the <div> (under html>body).
        fn find(doc: &argus_dom::Document, id: argus_dom::NodeId, tag: &str) -> Option<argus_dom::NodeId> {
            if matches!(&doc.node(id).data, argus_dom::NodeData::Element(e) if e.name.is_html(tag)) {
                return Some(id);
            }
            doc.children(id).find_map(|c| find(doc, c, tag))
        }
        let div = find(&doc, doc.root(), "div").unwrap();
        let json = dom_node_json(&doc, div).unwrap();
        // Attrs are sorted; the text and nested <b> appear as children.
        assert!(json.starts_with("{\"tag\":\"div\",\"attrs\":{\"class\":\"c\",\"id\":\"a\"}"), "{json}");
        assert!(json.contains("{\"text\":\"hi\"}"), "{json}");
        assert!(json.contains("{\"tag\":\"b\",\"attrs\":{},\"children\":[{\"text\":\"x\"}]}"), "{json}");
    }

    #[test]
    fn extract_json_emits_structured_summary() {
        let doc = argus_html::parse(
            "<title>My \"Page\"</title>\
             <h1>Top</h1><h2>Sub</h2>\
             <a href=\"/x\">Link</a>",
        );
        let out = extract_json(&doc, Some("https://site.example/"));
        // Title is JSON-escaped; headings carry their level; the href is resolved.
        assert!(out.contains("\"title\":\"My \\\"Page\\\"\""), "{out}");
        assert!(out.contains("{\"level\":1,\"text\":\"Top\"}"), "{out}");
        assert!(out.contains("{\"level\":2,\"text\":\"Sub\"}"), "{out}");
        assert!(
            out.contains("{\"text\":\"Link\",\"href\":\"https://site.example/x\"}"),
            "{out}"
        );
    }

    #[test]
    fn extract_images_lists_src_alt_dims() {
        let doc = argus_html::parse(
            "<img src=\"/a.png\" alt=\"Logo\" width=\"64\" height=\"32\">\
             <img src=\"b.jpg\">\
             <span>not an image</span>",
        );
        let out = extract_images(&doc, Some("https://site.example/dir/"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "https://site.example/a.png\tLogo\t64x32");
        assert_eq!(lines[1], "https://site.example/dir/b.jpg\t\t?x?");
        assert_eq!(lines.len(), 2, "only <img> elements: {out:?}");
    }

    #[test]
    fn srcset_best_picks_highest_descriptor() {
        // Width descriptors: the largest `w` wins.
        assert_eq!(
            srcset_best("small.jpg 480w, big.jpg 1024w, mid.jpg 768w"),
            Some("big.jpg")
        );
        // Density descriptors: the largest `x` wins.
        assert_eq!(srcset_best("a.png 1x, b.png 2x, c.png 3x"), Some("c.png"));
        // No descriptors: the (last) candidate is used.
        assert_eq!(srcset_best("only.jpg"), Some("only.jpg"));
        assert_eq!(srcset_best("  "), None);
    }

    #[test]
    fn extract_images_falls_back_to_srcset() {
        let doc = argus_html::parse(
            "<img srcset=\"/s-480.jpg 480w, /s-1024.jpg 1024w\" alt=\"Hero\">",
        );
        let out = extract_images(&doc, Some("https://site.example/"));
        assert_eq!(out.lines().next().unwrap(), "https://site.example/s-1024.jpg\tHero\t?x?");
    }

    #[test]
    fn decode_html_handles_utf8_and_windows1252() {
        // Valid UTF-8 passes through unchanged.
        assert_eq!(decode_html("héllo €".as_bytes()), "héllo €");
        // A windows-1252 page (declared) decodes the high bytes: 0xE9 = é, 0x80 = €.
        let mut bytes = b"<meta charset=windows-1252><p>caf".to_vec();
        bytes.push(0xE9); // é
        bytes.extend_from_slice(b" ");
        bytes.push(0x80); // €
        bytes.extend_from_slice(b"</p>");
        let s = decode_html(&bytes);
        assert!(s.contains("café"), "0xE9 -> é: {s:?}");
        assert!(s.contains('€'), "0x80 -> euro: {s:?}");
        // Invalid UTF-8 with no charset falls back to windows-1252 (no replacement char).
        let s2 = decode_html(&[b'x', 0xE9, b'y']);
        assert_eq!(s2, "xéy");
        assert!(!s2.contains('\u{FFFD}'), "no replacement char");
    }

    #[test]
    fn base_href_changes_relative_resolution() {
        // A <base href> overrides the page URL for resolving relative links.
        let doc = argus_html::parse(
            "<head><base href=\"https://cdn.example/assets/\"></head>\
             <body><a href=\"x.png\">x</a></body>",
        );
        let base = effective_base(&doc, Some("https://site.example/page.html"));
        assert_eq!(base.as_deref(), Some("https://cdn.example/assets/"));
        let links = extract_links(&doc, base.as_deref());
        assert!(links.contains("https://cdn.example/assets/x.png"), "{links}");

        // No <base> → relative links resolve against the page URL.
        let doc2 = argus_html::parse("<a href=\"y.png\">y</a>");
        let base2 = effective_base(&doc2, Some("https://site.example/dir/page.html"));
        assert_eq!(base2.as_deref(), Some("https://site.example/dir/page.html"));
    }

    #[test]
    fn extract_tables_emits_tsv() {
        let doc = argus_html::parse(
            "<table><tr><th>Name</th><th>Age</th></tr>\
               <tr><td>Ann</td><td>30</td></tr></table>\
             <table><tr><td>x</td></tr></table>",
        );
        let out = extract_tables(&doc);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "Name\tAge");
        assert_eq!(lines[1], "Ann\t30");
        // A blank line separates the two tables.
        assert!(lines.contains(&""), "tables separated by blank line: {out:?}");
        assert!(lines.contains(&"x"), "second table present: {out:?}");
    }

    #[test]
    fn extract_jsonld_collects_ld_json_blocks() {
        let doc = argus_html::parse(
            "<html><head>\
               <script type=\"application/ld+json\">  {\"@type\":\"Article\",\"name\":\"A\"}  </script>\
               <script type=\"text/javascript\">var x = 1;</script>\
               <script type=\"application/LD+JSON; charset=utf-8\">{\"@type\":\"Person\"}</script>\
             </head><body>x</body></html>",
        );
        let out = extract_jsonld(&doc);
        // Only the two ld+json blocks, trimmed, in document order; JS is skipped.
        assert_eq!(
            out,
            "[{\"@type\":\"Article\",\"name\":\"A\"},{\"@type\":\"Person\"}]\n",
            "{out}"
        );
    }

    #[test]
    fn extract_microdata_collects_items() {
        let doc = argus_html::parse(
            "<div itemscope itemtype=\"https://schema.org/Person\">\
               <span itemprop=\"name\">Ada Lovelace</span>\
               <a itemprop=\"url\" href=\"/ada\">link</a>\
               <meta itemprop=\"birthYear\" content=\"1815\">\
             </div>\
             <p>not an item</p>",
        );
        let out = extract_microdata(&doc, Some("https://site.example/"));
        assert!(out.contains("\"type\":\"https://schema.org/Person\""), "{out}");
        assert!(out.contains("\"name\":\"Ada Lovelace\""), "{out}");
        assert!(out.contains("\"url\":\"https://site.example/ada\""), "resolved href: {out}");
        assert!(out.contains("\"birthYear\":\"1815\""), "meta content: {out}");
    }

    #[test]
    fn extract_microdata_empty_when_none() {
        let doc = argus_html::parse("<p>plain page</p>");
        assert_eq!(extract_microdata(&doc, None), "[]\n");
    }

    #[test]
    fn extract_jsonld_empty_when_none() {
        let doc = argus_html::parse("<html><body><p>no structured data</p></body></html>");
        assert_eq!(extract_jsonld(&doc), "[]\n");
    }

    #[test]
    fn extract_meta_collects_page_metadata() {
        let doc = argus_html::parse(
            "<html lang=\"en\"><head>\
               <title>Hello   World</title>\
               <meta charset=\"utf-8\">\
               <meta name=\"description\" content=\"A   test   page\">\
               <meta property=\"og:title\" content=\"OG Title\">\
               <link rel=\"canonical\" href=\"/page\">\
               <link rel=\"shortcut icon\" href=\"/favicon.ico\">\
               <link rel=\"alternate\" hreflang=\"fr\" href=\"/fr/page\">\
               <meta http-equiv=\"refresh\" content=\"5; url=/next\">\
             </head><body>x</body></html>",
        );
        let out = extract_meta(&doc, Some("https://site.example/dir/"));
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.contains(&"title: Hello World"), "{out}");
        assert!(lines.contains(&"lang: en"), "{out}");
        assert!(lines.contains(&"charset: utf-8"), "{out}");
        assert!(lines.contains(&"description: A test page"), "{out}");
        assert!(lines.contains(&"og:title: OG Title"), "{out}");
        assert!(lines.contains(&"canonical: https://site.example/page"), "{out}");
        assert!(lines.contains(&"icon: https://site.example/favicon.ico"), "{out}");
        assert!(lines.contains(&"alternate:fr: https://site.example/fr/page"), "{out}");
        assert!(lines.contains(&"refresh: 5; url=/next"), "{out}");
    }

    #[test]
    fn extract_forms_lists_controls() {
        let doc = argus_html::parse(
            "<form action=\"/login\" method=\"post\">\
               <input name=\"email\" type=\"email\" value=\"a@b.c\">\
               <input name=\"remember\" type=\"checkbox\" checked>\
               <select name=\"country\">\
                 <option value=\"US\">United States</option>\
                 <option value=\"JP\" selected>Japan</option>\
               </select>\
               <textarea name=\"comment\">hi there</textarea>\
               <button name=\"go\" type=\"submit\">Go</button>\
             </form>",
        );
        let out = extract_forms(&doc, Some("https://site.example/page"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "form[0] action=https://site.example/login method=post");
        assert!(lines.contains(&"  input name=email type=email value=a@b.c"), "{out}");
        assert!(lines.contains(&"  input name=remember type=checkbox value= checked"), "{out}");
        // The selected option's value is reported, not the first.
        assert!(lines.contains(&"  select name=country value=JP"), "{out}");
        assert!(lines.contains(&"  textarea name=comment value=hi there"), "{out}");
        assert!(lines.contains(&"  button name=go type=submit"), "{out}");
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
    fn tabs_open_switch_and_independent_history() {
        let mut tabs = Tabs::new("a".into());
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs.active_index(), 0);
        // Opening a tab makes it active.
        assert_eq!(tabs.open("b".into()), 1);
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active().url(), "b");
        // Each tab has its own history.
        tabs.active_mut().history.push("b2".into());
        assert_eq!(tabs.active().url(), "b2");
        tabs.switch_to(0);
        assert_eq!(tabs.active().url(), "a", "tab 0 history untouched");
        // next/prev wrap around.
        tabs.next();
        assert_eq!(tabs.active_index(), 1);
        tabs.next();
        assert_eq!(tabs.active_index(), 0, "wraps to first");
        tabs.prev();
        assert_eq!(tabs.active_index(), 1, "wraps to last");
        // Per-tab scroll is independent.
        tabs.active_mut().scroll_y = 50;
        tabs.switch_to(0);
        assert_eq!(tabs.active().scroll_y, 0);
    }

    #[test]
    fn tabs_close_adjusts_active_and_refuses_last() {
        let mut tabs = Tabs::new("a".into());
        tabs.open("b".into());
        tabs.open("c".into()); // [a, b, c], active = 2
        // Closing a tab before the active one shifts active down.
        assert!(tabs.close(0)); // [b, c], active was 2 -> 1
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs.active().url(), "c");
        // Closing the active (last-index) tab moves active to the new last.
        assert_eq!(tabs.active_index(), 1);
        assert!(tabs.close(1)); // [b], active -> 0
        assert_eq!(tabs.active().url(), "b");
        // The final tab can't be closed.
        assert!(!tabs.close(0));
        assert_eq!(tabs.len(), 1);
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
    fn render_text_skips_hidden_elements() {
        let doc = argus_html::parse(
            "<p>shown</p>\
             <p hidden>hiddenattr</p>\
             <p style=\"display:none\">displaynone</p>\
             <p style=\"visibility: hidden\">invisible</p>",
        );
        let text = render_text(&doc);
        assert!(text.contains("shown"));
        assert!(!text.contains("hiddenattr"), "hidden attr dropped:\n{text}");
        assert!(!text.contains("displaynone"), "display:none dropped:\n{text}");
        assert!(!text.contains("invisible"), "visibility:hidden dropped:\n{text}");
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
