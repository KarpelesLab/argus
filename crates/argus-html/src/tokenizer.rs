//! The HTML tokenizer.
//!
//! A subset of the WHATWG tokenizer (§13.2.5) sufficient for typical static HTML:
//! tags with attributes (quoted/unquoted/empty), self-closing and void tags,
//! comments, doctypes, character references, and RAWTEXT/RCDATA elements
//! (`script`/`style`/`textarea`/`title`/…). Consecutive character data is
//! coalesced into one [`Token::Characters`] rather than emitted per code point.
//! The remaining states (full CDATA, scripting subtleties, the long tail of error
//! recovery) are deferred and tracked in `docs/subsystems/dom.md`.

use crate::entities::consume_char_ref;

/// A token produced by [`tokenize`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Token {
    /// `<!DOCTYPE …>` — only the name is retained in Phase 1.
    Doctype { name: Option<String> },
    /// A start tag, with lowercased name and `(name, value)` attributes.
    StartTag {
        name: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    /// An end tag.
    EndTag { name: String },
    /// A `<!-- … -->` (or bogus) comment's contents.
    Comment(String),
    /// A run of character data (already entity-decoded).
    Characters(String),
    /// End of input.
    Eof,
}

/// Tokenize `input` into a flat token stream ending in [`Token::Eof`].
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut t = Tokenizer::new(input);
    t.run();
    t.tokens
}

fn is_html_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{000C}')
}

/// Elements whose content is tokenized as raw text. The bool is whether character
/// references are processed (RCDATA) or not (RAWTEXT).
fn rawtext_kind(name: &str) -> Option<bool> {
    match name {
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(false),
        "textarea" | "title" => Some(true),
        _ => None,
    }
}

struct Tokenizer {
    input: Vec<char>,
    pos: usize,
    tokens: Vec<Token>,
}

impl Tokenizer {
    fn new(input: &str) -> Tokenizer {
        // Preprocess newlines per the spec: CRLF and CR become LF.
        let mut chars = Vec::with_capacity(input.len());
        let mut prev_cr = false;
        for c in input.chars() {
            match c {
                '\r' => {
                    chars.push('\n');
                    prev_cr = true;
                }
                '\n' if prev_cr => {
                    prev_cr = false; // collapse CRLF
                }
                _ => {
                    chars.push(c);
                    prev_cr = false;
                }
            }
        }
        Tokenizer {
            input: chars,
            pos: 0,
            tokens: Vec::new(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.input.get(self.pos).copied()
    }
    fn peek_at(&self, n: usize) -> Option<char> {
        self.input.get(self.pos + n).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if is_html_ws(c)) {
            self.pos += 1;
        }
    }
    fn emit(&mut self, tok: Token) {
        self.tokens.push(tok);
    }

    /// Does the input at `pos + offset` spell out end tag `name` (case-insensitive)
    /// followed by a tag-terminating character?
    fn at_end_tag(&self, name: &str, offset: usize) -> bool {
        let mut i = self.pos + offset;
        for nc in name.chars() {
            match self.input.get(i) {
                Some(c) if c.eq_ignore_ascii_case(&nc) => i += 1,
                _ => return false,
            }
        }
        match self.input.get(i) {
            None => true,
            Some(&c) => is_html_ws(c) || c == '/' || c == '>',
        }
    }

    fn matches(&self, s: &str, ci: bool) -> bool {
        for (i, sc) in s.chars().enumerate() {
            match self.input.get(self.pos + i) {
                Some(&c) if (ci && c.eq_ignore_ascii_case(&sc)) || (!ci && c == sc) => {}
                _ => return false,
            }
        }
        true
    }

    fn run(&mut self) {
        let mut text = String::new();
        loop {
            match self.peek() {
                None => {
                    self.flush(&mut text);
                    self.emit(Token::Eof);
                    break;
                }
                Some('<') => match self.peek_at(1) {
                    Some(c) if c.is_ascii_alphabetic() => {
                        self.flush(&mut text);
                        self.pos += 1;
                        self.read_tag(false);
                    }
                    Some('/') => {
                        self.flush(&mut text);
                        self.pos += 2;
                        self.end_tag_open();
                    }
                    Some('!') => {
                        self.flush(&mut text);
                        self.pos += 2;
                        self.markup_declaration();
                    }
                    Some('?') => {
                        self.flush(&mut text);
                        self.pos += 2;
                        self.bogus_comment();
                    }
                    _ => {
                        self.pos += 1;
                        text.push('<');
                    }
                },
                Some('&') => {
                    text.push_str(&consume_char_ref(&self.input, &mut self.pos));
                }
                Some(c) => {
                    self.pos += 1;
                    text.push(if c == '\0' { '\u{FFFD}' } else { c });
                }
            }
        }
    }

    fn flush(&mut self, text: &mut String) {
        if !text.is_empty() {
            let s = std::mem::take(text);
            self.emit(Token::Characters(s));
        }
    }

    /// Read a tag whose name starts at the current position (the `<` or `</` has
    /// already been consumed).
    fn read_tag(&mut self, is_end: bool) {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if is_html_ws(c) || c == '/' || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            self.bump();
        }

        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some('>') => {
                    self.bump();
                    break;
                }
                Some('/') => {
                    self.bump();
                    if self.peek() == Some('>') {
                        self.bump();
                        self_closing = true;
                        break;
                    }
                    // stray slash: ignore and continue reading attributes
                }
                Some(_) => {
                    let (n, v) = self.read_attribute();
                    if !n.is_empty() && !attrs.iter().any(|(an, _)| an == &n) {
                        attrs.push((n, v));
                    }
                }
            }
        }

        if is_end {
            self.emit(Token::EndTag { name });
        } else {
            self.emit(Token::StartTag {
                name: name.clone(),
                attrs,
                self_closing,
            });
            if !self_closing {
                if let Some(process_refs) = rawtext_kind(&name) {
                    self.consume_rawtext(&name, process_refs);
                }
            }
        }
    }

    fn read_attribute(&mut self) -> (String, String) {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if is_html_ws(c) || c == '=' || c == '/' || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            self.bump();
        }
        self.skip_ws();

        let mut value = String::new();
        if self.peek() == Some('=') {
            self.bump();
            self.skip_ws();
            match self.peek() {
                Some('"') => {
                    self.bump();
                    value = self.read_quoted_value('"');
                }
                Some('\'') => {
                    self.bump();
                    value = self.read_quoted_value('\'');
                }
                Some('>') | None => {}
                Some(_) => value = self.read_unquoted_value(),
            }
        }
        (name, value)
    }

    fn read_quoted_value(&mut self, quote: char) -> String {
        let mut s = String::new();
        loop {
            match self.peek() {
                None => break,
                Some(c) if c == quote => {
                    self.bump();
                    break;
                }
                Some('&') => s.push_str(&consume_char_ref(&self.input, &mut self.pos)),
                Some(c) => {
                    self.bump();
                    s.push(c);
                }
            }
        }
        s
    }

    fn read_unquoted_value(&mut self) -> String {
        let mut s = String::new();
        loop {
            match self.peek() {
                None | Some('>') => break,
                Some(c) if is_html_ws(c) => break,
                Some('&') => s.push_str(&consume_char_ref(&self.input, &mut self.pos)),
                Some(c) => {
                    self.bump();
                    s.push(c);
                }
            }
        }
        s
    }

    fn end_tag_open(&mut self) {
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() => self.read_tag(true),
            Some('>') => {
                self.bump(); // </> — ignore
            }
            None => self.emit(Token::Characters("</".to_string())),
            Some(_) => self.bogus_comment(),
        }
    }

    /// Positioned just after `<!`.
    fn markup_declaration(&mut self) {
        if self.matches("--", false) {
            self.pos += 2;
            self.comment();
        } else if self.matches("doctype", true) {
            self.pos += 7;
            self.doctype();
        } else if self.matches("[CDATA[", false) {
            self.pos += 7;
            self.bogus_comment(); // Phase 1: treat CDATA as a bogus comment
        } else {
            self.bogus_comment();
        }
    }

    /// Positioned just after `<!--`.
    fn comment(&mut self) {
        let mut s = String::new();
        loop {
            if self.matches("-->", false) {
                self.pos += 3;
                break;
            }
            match self.bump() {
                None => break,
                Some(c) => s.push(c),
            }
        }
        self.emit(Token::Comment(s));
    }

    /// Positioned just after `<!doctype`.
    fn doctype(&mut self) {
        self.skip_ws();
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if is_html_ws(c) || c == '>' {
                break;
            }
            name.push(c.to_ascii_lowercase());
            self.bump();
        }
        // Skip any public/system identifiers up to '>'.
        while let Some(c) = self.bump() {
            if c == '>' {
                break;
            }
        }
        self.emit(Token::Doctype {
            name: if name.is_empty() { None } else { Some(name) },
        });
    }

    fn bogus_comment(&mut self) {
        let mut s = String::new();
        loop {
            match self.bump() {
                None => break,
                Some('>') => break,
                Some(c) => s.push(c),
            }
        }
        self.emit(Token::Comment(s));
    }

    fn consume_rawtext(&mut self, name: &str, process_refs: bool) {
        let mut text = String::new();
        loop {
            if self.peek() == Some('<') && self.peek_at(1) == Some('/') && self.at_end_tag(name, 2)
            {
                if !text.is_empty() {
                    let s = std::mem::take(&mut text);
                    self.emit(Token::Characters(s));
                }
                self.pos += 2;
                self.read_tag(true);
                return;
            }
            match self.peek() {
                None => {
                    if !text.is_empty() {
                        self.emit(Token::Characters(text));
                    }
                    return;
                }
                Some('&') if process_refs => {
                    text.push_str(&consume_char_ref(&self.input, &mut self.pos))
                }
                Some(c) => {
                    self.bump();
                    text.push(c);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::StartTag {
            name: name.to_string(),
            attrs: attrs
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
            self_closing: false,
        }
    }
    fn end(name: &str) -> Token {
        Token::EndTag {
            name: name.to_string(),
        }
    }
    fn chars(s: &str) -> Token {
        Token::Characters(s.to_string())
    }

    #[test]
    fn tags_attrs_text_and_entities() {
        let toks = tokenize(r#"<p class="x" hidden>Hi&amp;bye</p>"#);
        assert_eq!(
            toks,
            vec![
                start("p", &[("class", "x"), ("hidden", "")]),
                chars("Hi&bye"),
                end("p"),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn doctype_comment_and_void() {
        let toks = tokenize("<!DOCTYPE html><!-- hi --><br>x");
        assert_eq!(
            toks,
            vec![
                Token::Doctype {
                    name: Some("html".to_string())
                },
                Token::Comment(" hi ".to_string()),
                Token::StartTag {
                    name: "br".to_string(),
                    attrs: vec![],
                    self_closing: false,
                },
                chars("x"),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn unquoted_value_keeps_trailing_slash() {
        // Per spec, `/` is an ordinary character in an unquoted attribute value,
        // so this is src="a.png/" and NOT self-closing.
        let toks = tokenize("<img src=a.png/>");
        assert_eq!(
            toks,
            vec![
                Token::StartTag {
                    name: "img".to_string(),
                    attrs: vec![("src".to_string(), "a.png/".to_string())],
                    self_closing: false,
                },
                Token::Eof,
            ]
        );
    }

    #[test]
    fn self_closing_after_quote_or_space() {
        let toks = tokenize(r#"<input type="text" />"#);
        assert_eq!(
            toks,
            vec![
                Token::StartTag {
                    name: "input".to_string(),
                    attrs: vec![("type".to_string(), "text".to_string())],
                    self_closing: true,
                },
                Token::Eof,
            ]
        );
    }

    #[test]
    fn rawtext_script_is_not_parsed_as_tags() {
        let toks = tokenize("<script>if (a < b) x()</script>");
        assert_eq!(
            toks,
            vec![
                start("script", &[]),
                chars("if (a < b) x()"),
                end("script"),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn stray_less_than_is_text() {
        let toks = tokenize("a < b");
        assert_eq!(toks, vec![chars("a < b"), Token::Eof]);
    }
}
