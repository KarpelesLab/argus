//! The tree builder: token stream → [`argus_dom::Document`].
//!
//! This is a pragmatic subset of the WHATWG "tree construction" stage (§13.2.6).
//! It implements the document skeleton (implicit `html`/`head`/`body` via the
//! Initial→…→InBody insertion modes), void elements, the common implied-end-tag
//! behavior (`<p>`, `<li>`, headings), and scoped end-tag matching — enough to
//! build faithful trees for typical content. The harder machinery — the full
//! table modes, the adoption agency algorithm for misnested formatting elements,
//! foster parenting, and template contents — is deferred and tracked in
//! `docs/subsystems/dom.md`. Conformance against html5lib-tests comes with it.

use crate::tokenizer::{tokenize, Token};
use argus_dom::{Attribute, Document, NodeData, NodeId, QualName};

/// Parse `input` into a [`Document`].
pub fn parse(input: &str) -> Document {
    let mut b = TreeBuilder::new();
    for tok in tokenize(input) {
        b.process(tok);
    }
    b.doc
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    AfterBody,
}

/// Void elements: inserted but never pushed onto the open-elements stack.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Block-ish start tags that imply a `</p>` when a `<p>` is open in button scope.
const CLOSES_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "dd",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
    "table",
    "ul",
];

const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];

/// Elements popped by "generate implied end tags".
const IMPLIED_END: &[&str] = &["dd", "dt", "li", "optgroup", "option", "p", "rp", "rt"];

/// Scope boundaries for the "has element in scope" check.
const SCOPE_BOUNDARY: &[&str] = &[
    "applet", "caption", "html", "table", "td", "th", "marquee", "object", "template",
];

struct TreeBuilder {
    doc: Document,
    open: Vec<NodeId>,
    mode: Mode,
    head: Option<NodeId>,
}

impl TreeBuilder {
    fn new() -> TreeBuilder {
        TreeBuilder {
            doc: Document::new(),
            open: Vec::new(),
            mode: Mode::Initial,
            head: None,
        }
    }

    fn process(&mut self, tok: Token) {
        // A handler returns `true` to ask for the same token to be reprocessed in
        // the (now updated) mode.
        loop {
            let reprocess = match self.mode {
                Mode::Initial => self.m_initial(&tok),
                Mode::BeforeHtml => self.m_before_html(&tok),
                Mode::BeforeHead => self.m_before_head(&tok),
                Mode::InHead => self.m_in_head(&tok),
                Mode::AfterHead => self.m_after_head(&tok),
                Mode::InBody => self.m_in_body(&tok),
                Mode::AfterBody => self.m_after_body(&tok),
            };
            if !reprocess {
                break;
            }
        }
    }

    // --- node helpers -------------------------------------------------------

    fn current(&self) -> NodeId {
        *self.open.last().unwrap_or(&self.doc.root())
    }

    fn name_of(&self, id: NodeId) -> Option<&str> {
        self.doc.node(id).as_element().map(|e| &*e.name.local)
    }

    fn is_named(&self, id: NodeId, name: &str) -> bool {
        self.name_of(id) == Some(name)
    }

    fn make_element(&mut self, name: &str, attrs: &[(String, String)]) -> NodeId {
        let attrs = attrs
            .iter()
            .map(|(n, v)| Attribute::new(n.as_str(), v.as_str()))
            .collect();
        self.doc.create_element(QualName::html(name), attrs)
    }

    /// Insert an element under the current node; return it without pushing.
    fn insert_element(&mut self, name: &str, attrs: &[(String, String)]) -> NodeId {
        let id = self.make_element(name, attrs);
        let parent = self.current();
        self.doc.append(parent, id);
        id
    }

    /// Insert and push an element onto the open-elements stack.
    fn insert_and_push(&mut self, name: &str, attrs: &[(String, String)]) -> NodeId {
        let id = self.insert_element(name, attrs);
        self.open.push(id);
        id
    }

    fn insert_text(&mut self, s: &str) {
        let parent = self.current();
        if let Some(last) = self.doc.node(parent).last_child() {
            if let NodeData::Text(t) = self.doc.data_mut(last) {
                t.push_str(s);
                return;
            }
        }
        let t = self.doc.create_text(s);
        self.doc.append(parent, t);
    }

    fn insert_comment_in(&mut self, parent: NodeId, s: &str) {
        let c = self.doc.create_comment(s);
        self.doc.append(parent, c);
    }

    // --- stack helpers ------------------------------------------------------

    fn has_in_scope(&self, name: &str, button: bool) -> bool {
        for &id in self.open.iter().rev() {
            if self.is_named(id, name) {
                return true;
            }
            if let Some(n) = self.name_of(id) {
                if SCOPE_BOUNDARY.contains(&n) || (button && n == "button") {
                    return false;
                }
            }
        }
        false
    }

    fn generate_implied_end_tags(&mut self, except: Option<&str>) {
        while let Some(&top) = self.open.last() {
            match self.name_of(top) {
                Some(n) if IMPLIED_END.contains(&n) && Some(n) != except => {
                    self.open.pop();
                }
                _ => break,
            }
        }
    }

    /// Pop the stack until (and including) the first element named `name`.
    fn pop_until(&mut self, name: &str) {
        while let Some(id) = self.open.pop() {
            if self.is_named(id, name) {
                break;
            }
        }
    }

    fn close_p(&mut self) {
        if self.has_in_scope("p", true) {
            self.generate_implied_end_tags(Some("p"));
            self.pop_until("p");
        }
    }

    // --- insertion modes ----------------------------------------------------

    fn m_initial(&mut self, tok: &Token) -> bool {
        match tok {
            Token::Characters(s) if is_ws(s) => false,
            Token::Comment(s) => {
                let root = self.doc.root();
                self.insert_comment_in(root, s);
                false
            }
            Token::Doctype { name } => {
                let dt = self
                    .doc
                    .create_doctype(name.clone().unwrap_or_default(), "", "");
                let root = self.doc.root();
                self.doc.append(root, dt);
                self.mode = Mode::BeforeHtml;
                false
            }
            Token::Eof => false,
            _ => {
                self.mode = Mode::BeforeHtml;
                true
            }
        }
    }

    fn m_before_html(&mut self, tok: &Token) -> bool {
        match tok {
            Token::Doctype { .. } => false,
            Token::Comment(s) => {
                let root = self.doc.root();
                self.insert_comment_in(root, s);
                false
            }
            Token::Characters(s) if is_ws(s) => false,
            Token::StartTag { name, attrs, .. } if name == "html" => {
                self.insert_and_push("html", attrs);
                self.mode = Mode::BeforeHead;
                false
            }
            _ => {
                self.insert_and_push("html", &[]);
                self.mode = Mode::BeforeHead;
                true
            }
        }
    }

    fn m_before_head(&mut self, tok: &Token) -> bool {
        match tok {
            Token::Characters(s) if is_ws(s) => false,
            Token::Comment(s) => {
                let p = self.current();
                self.insert_comment_in(p, s);
                false
            }
            Token::Doctype { .. } => false,
            Token::StartTag { name, attrs, .. } if name == "head" => {
                let id = self.insert_and_push("head", attrs);
                self.head = Some(id);
                self.mode = Mode::InHead;
                false
            }
            _ => {
                let id = self.insert_and_push("head", &[]);
                self.head = Some(id);
                self.mode = Mode::InHead;
                true
            }
        }
    }

    fn m_in_head(&mut self, tok: &Token) -> bool {
        match tok {
            // Whitespace, and the text content of an open `title`/`style`/`script`,
            // is inserted; other text directly in `head` ends the head.
            Token::Characters(s) => {
                let in_text_el = self
                    .name_of(self.current())
                    .is_some_and(|n| matches!(n, "title" | "style" | "script" | "noscript"));
                if is_ws(s) || in_text_el {
                    self.insert_text(s);
                    false
                } else {
                    self.open.pop(); // pop head
                    self.mode = Mode::AfterHead;
                    true
                }
            }
            Token::Comment(s) => {
                let p = self.current();
                self.insert_comment_in(p, s);
                false
            }
            Token::Doctype { .. } => false,
            Token::StartTag { name, attrs, .. }
                if matches!(
                    name.as_str(),
                    "base" | "basefont" | "bgsound" | "link" | "meta"
                ) =>
            {
                self.insert_element(name, attrs); // void in head
                false
            }
            Token::StartTag { name, attrs, .. }
                if matches!(name.as_str(), "title" | "style" | "script" | "noscript") =>
            {
                self.insert_and_push(name, attrs);
                false
            }
            Token::EndTag { name } if name == "head" => {
                self.open.pop();
                self.mode = Mode::AfterHead;
                false
            }
            Token::EndTag { name }
                if matches!(name.as_str(), "title" | "style" | "script" | "noscript") =>
            {
                if self.is_named(self.current(), name) {
                    self.open.pop();
                }
                false
            }
            _ => {
                // Anything else implies the end of the head.
                self.open.pop(); // pop head
                self.mode = Mode::AfterHead;
                true
            }
        }
    }

    fn m_after_head(&mut self, tok: &Token) -> bool {
        match tok {
            Token::Characters(s) if is_ws(s) => false,
            Token::Comment(s) => {
                let p = self.current();
                self.insert_comment_in(p, s);
                false
            }
            Token::Doctype { .. } => false,
            Token::StartTag { name, attrs, .. } if name == "body" => {
                self.insert_and_push("body", attrs);
                self.mode = Mode::InBody;
                false
            }
            _ => {
                self.insert_and_push("body", &[]);
                self.mode = Mode::InBody;
                true
            }
        }
    }

    fn m_in_body(&mut self, tok: &Token) -> bool {
        match tok {
            Token::Characters(s) => {
                self.insert_text(s);
                false
            }
            Token::Comment(s) => {
                let p = self.current();
                self.insert_comment_in(p, s);
                false
            }
            Token::Doctype { .. } => false,
            Token::Eof => false,
            Token::StartTag {
                name,
                attrs,
                self_closing,
            } => {
                self.start_in_body(name, attrs, *self_closing);
                false
            }
            Token::EndTag { name } => {
                self.end_in_body(name);
                false
            }
        }
    }

    fn start_in_body(&mut self, name: &str, attrs: &[(String, String)], _self_closing: bool) {
        // Ignore re-openings of the structural elements.
        if matches!(name, "html" | "head" | "body") {
            return;
        }
        if CLOSES_P.contains(&name) {
            self.close_p();
        }
        if name == "li" && self.has_in_scope("li", false) {
            self.generate_implied_end_tags(Some("li"));
            self.pop_until("li");
        }
        // A new <dd>/<dt> closes an open one (definition-list items are siblings).
        // `except` must name the item we pop_until, so implied-end-tag generation
        // doesn't pop it out from under us first.
        if matches!(name, "dd" | "dt") {
            let item = if self.has_in_scope("dd", false) {
                Some("dd")
            } else if self.has_in_scope("dt", false) {
                Some("dt")
            } else {
                None
            };
            if let Some(item) = item {
                self.generate_implied_end_tags(Some(item));
                self.pop_until(item);
            }
        }
        // Simplified table fixups: a <tr> directly in a <table> gets an implicit
        // <tbody>; a <td>/<th> not already in a row gets an implicit <tr> (inserting
        // a <tbody> first if it sits straight inside the <table>).
        if name == "tr" && self.is_named(self.current(), "table") {
            self.insert_and_push("tbody", &[]);
        }
        if matches!(name, "td" | "th") {
            if self.is_named(self.current(), "table") {
                self.insert_and_push("tbody", &[]);
            }
            if matches!(self.name_of(self.current()), Some("tbody" | "thead" | "tfoot")) {
                self.insert_and_push("tr", &[]);
            }
        }
        if HEADINGS.contains(&name) {
            if let Some(n) = self.name_of(self.current()) {
                if HEADINGS.contains(&n) {
                    self.open.pop();
                }
            }
        }
        if VOID.contains(&name) {
            self.insert_element(name, attrs);
        } else {
            self.insert_and_push(name, attrs);
        }
    }

    fn end_in_body(&mut self, name: &str) {
        match name {
            "body" | "html" => {
                self.mode = Mode::AfterBody;
            }
            "p" => {
                self.close_p();
            }
            n if HEADINGS.contains(&n) => {
                // Close the nearest open heading of any level.
                if HEADINGS.iter().any(|h| self.has_in_scope(h, false)) {
                    self.generate_implied_end_tags(None);
                    while let Some(id) = self.open.pop() {
                        if self.name_of(id).is_some_and(|n| HEADINGS.contains(&n)) {
                            break;
                        }
                    }
                }
            }
            n => {
                if self.has_in_scope(n, false) {
                    self.generate_implied_end_tags(Some(n));
                    self.pop_until(n);
                }
            }
        }
    }

    fn m_after_body(&mut self, tok: &Token) -> bool {
        match tok {
            Token::Comment(s) => {
                let root = self.doc.root();
                self.insert_comment_in(root, s);
                false
            }
            Token::Eof => false,
            Token::EndTag { name } if name == "html" => false,
            _ => {
                self.mode = Mode::InBody;
                true
            }
        }
    }
}

fn is_ws(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| matches!(c, ' ' | '\t' | '\n' | '\u{000C}' | '\r'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(input: &str) -> String {
        parse(input).serialize()
    }

    #[test]
    fn full_document() {
        let got =
            tree("<!DOCTYPE html><html><head><title>T</title></head><body><p>Hi</p></body></html>");
        let want = "\
| <!DOCTYPE html>
| <html>
|   <head>
|     <title>
|       \"T\"
|   <body>
|     <p>
|       \"Hi\"
";
        assert_eq!(got, want);
    }

    #[test]
    fn implicit_html_head_body() {
        let got = tree("Hello");
        let want = "\
| <html>
|   <head>
|   <body>
|     \"Hello\"
";
        assert_eq!(got, want);
    }

    #[test]
    fn implied_paragraph_close() {
        let got = tree("<p>one<p>two");
        let want = "\
| <html>
|   <head>
|   <body>
|     <p>
|       \"one\"
|     <p>
|       \"two\"
";
        assert_eq!(got, want);
    }

    #[test]
    fn nested_blocks_and_void() {
        let got = tree(r#"<div><img src=x><br>txt</div>"#);
        let want = "\
| <html>
|   <head>
|   <body>
|     <div>
|       <img>
|         src=\"x\"
|       <br>
|       \"txt\"
";
        assert_eq!(got, want);
    }

    #[test]
    fn list_items_imply_close() {
        let got = tree("<ul><li>a<li>b</ul>");
        let want = "\
| <html>
|   <head>
|   <body>
|     <ul>
|       <li>
|         \"a\"
|       <li>
|         \"b\"
";
        assert_eq!(got, want);
    }

    /// Robustness (a lightweight fuzz): the parser must never panic on arbitrary
    /// byte input. Drives thousands of structured-random documents through the
    /// full tokenizer + tree builder. Coverage-guided fuzzing lives in `fuzz/`.
    #[test]
    fn parser_survives_arbitrary_input() {
        let mut seed = 0x9E3779B97F4A7C15u64;
        let mut byte = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xff) as u8
        };
        // A bias table toward HTML-structural bytes makes inputs reach deeper
        // states than pure noise would.
        const BIAS: &[u8] = b"<>/=\"'&; \n\tabcdivp013!-[]CDATAscriptstyletabletrtd";
        for _ in 0..4000 {
            let len = (byte() as usize) * 3;
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    if byte() < 140 {
                        BIAS[byte() as usize % BIAS.len()]
                    } else {
                        byte()
                    }
                })
                .collect();
            let s = String::from_utf8_lossy(&bytes);
            let doc = parse(&s);
            // The output is always a well-formed arena (serialization can't panic).
            let _ = doc.serialize();
        }
    }
}
