//! CSS tokenizer.
//!
//! A pragmatic subset of CSS Syntax Level 3 §4: enough to tokenize selectors and
//! declaration blocks — identifiers, hashes, strings, numbers/dimensions/
//! percentages, at-keywords, functions, delimiters, blocks, and comments. Escapes,
//! unicode-ranges, and `url()` specifics are simplified. See
//! `docs/subsystems/style.md`.

/// A CSS token.
#[derive(Clone, PartialEq, Debug)]
pub enum Token {
    Ident(String),
    /// `#name` (id selectors, hex colors).
    Hash(String),
    /// `@media`, `@import`, …
    AtKeyword(String),
    /// `name(` — an identifier immediately followed by `(`.
    Function(String),
    Str(String),
    Number(f64),
    /// number + unit, e.g. `12px`.
    Dimension(f64, String),
    /// number + `%`.
    Percentage(f64),
    /// Any other single character (`.`, `>`, `*`, `+`, `~`, `=`, `|`, `/`, …).
    Delim(char),
    Whitespace,
    Colon,
    Semicolon,
    Comma,
    LBrace,
    RBrace,
    LParen,
    RParen,
    LBracket,
    RBracket,
}

/// Tokenize `input` into a flat token stream (no trailing EOF token).
pub fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let mut t = Lexer { chars, pos: 0 };
    let mut out = Vec::new();
    while let Some(tok) = t.next_token() {
        out.push(tok);
    }
    out
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '-' || !c.is_ascii()
}
fn is_ident(c: char) -> bool {
    is_ident_start(c) || c.is_ascii_digit()
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn next_token(&mut self) -> Option<Token> {
        // Skip comments first (they may appear anywhere).
        loop {
            if self.peek() == Some('/') && self.peek_at(1) == Some('*') {
                self.pos += 2;
                while self.pos < self.chars.len()
                    && !(self.peek() == Some('*') && self.peek_at(1) == Some('/'))
                {
                    self.pos += 1;
                }
                self.pos += 2; // consume "*/" (clamped by get())
                continue;
            }
            break;
        }

        let c = self.peek()?;
        Some(match c {
            c if c.is_whitespace() => {
                while matches!(self.peek(), Some(c) if c.is_whitespace()) {
                    self.pos += 1;
                }
                Token::Whitespace
            }
            ':' => {
                self.pos += 1;
                Token::Colon
            }
            ';' => {
                self.pos += 1;
                Token::Semicolon
            }
            ',' => {
                self.pos += 1;
                Token::Comma
            }
            '{' => {
                self.pos += 1;
                Token::LBrace
            }
            '}' => {
                self.pos += 1;
                Token::RBrace
            }
            '(' => {
                self.pos += 1;
                Token::LParen
            }
            ')' => {
                self.pos += 1;
                Token::RParen
            }
            '[' => {
                self.pos += 1;
                Token::LBracket
            }
            ']' => {
                self.pos += 1;
                Token::RBracket
            }
            '"' | '\'' => self.string(c),
            '#' => {
                self.pos += 1;
                let name = self.consume_name();
                Token::Hash(name)
            }
            '@' => {
                self.pos += 1;
                Token::AtKeyword(self.consume_name())
            }
            '0'..='9' => self.numeric(),
            '+' | '.' | '-' if self.starts_number() => self.numeric(),
            c if is_ident_start(c) => {
                let name = self.consume_name();
                if self.peek() == Some('(') {
                    self.pos += 1;
                    Token::Function(name)
                } else {
                    Token::Ident(name)
                }
            }
            other => {
                self.pos += 1;
                Token::Delim(other)
            }
        })
    }

    /// Whether the input at the current sign/dot begins a number.
    fn starts_number(&self) -> bool {
        match self.peek() {
            Some('+') | Some('-') => {
                matches!(self.peek_at(1), Some(c) if c.is_ascii_digit())
                    || (self.peek_at(1) == Some('.')
                        && matches!(self.peek_at(2), Some(c) if c.is_ascii_digit()))
            }
            Some('.') => matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()),
            _ => false,
        }
    }

    fn consume_name(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if is_ident(c) {
                s.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        s
    }

    fn string(&mut self, quote: char) -> Token {
        self.pos += 1; // opening quote
        let mut s = String::new();
        while let Some(c) = self.bump() {
            if c == quote {
                break;
            }
            if c == '\\' {
                // CSS escape: `\<1-6 hex>` (optional trailing space) is a code point;
                // any other `\<char>` is that literal character.
                match self.peek() {
                    Some(h) if h.is_ascii_hexdigit() => {
                        let mut hex = String::new();
                        while hex.len() < 6 && self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                            hex.push(self.bump().unwrap());
                        }
                        if self.peek() == Some(' ') {
                            self.bump();
                        }
                        if let Some(cp) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                        {
                            s.push(cp);
                        }
                    }
                    Some(_) => s.push(self.bump().unwrap()),
                    None => {}
                }
            } else {
                s.push(c);
            }
        }
        Token::Str(s)
    }

    fn numeric(&mut self) -> Token {
        let start = self.pos;
        if matches!(self.peek(), Some('+') | Some('-')) {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if self.peek() == Some('.') && matches!(self.peek_at(1), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let num: f64 = self.chars[start..self.pos]
            .iter()
            .collect::<String>()
            .parse()
            .unwrap_or(0.0);

        if self.peek() == Some('%') {
            self.pos += 1;
            Token::Percentage(num)
        } else if matches!(self.peek(), Some(c) if is_ident_start(c)) {
            let unit = self.consume_name();
            Token::Dimension(num, unit)
        } else {
            Token::Number(num)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_a_rule() {
        let toks = tokenize("div.box > p#x { color: #fff; margin: 1.5em; }");
        use Token::*;
        assert_eq!(toks[0], Ident("div".into()));
        assert_eq!(toks[1], Delim('.'));
        assert_eq!(toks[2], Ident("box".into()));
        assert!(toks.contains(&Hash("x".into())));
        assert!(toks.contains(&Dimension(1.5, "em".into())));
        assert!(toks.contains(&Hash("fff".into())));
    }

    #[test]
    fn comments_and_strings_and_functions() {
        let toks = tokenize("a /* c */ b rgb(1,2,3) \"hi\"");
        assert!(toks.contains(&Token::Function("rgb".into())));
        assert!(toks.contains(&Token::Str("hi".into())));
        // The comment leaves no token.
        let idents: Vec<_> = toks
            .iter()
            .filter(|t| matches!(t, Token::Ident(_)))
            .collect();
        assert_eq!(idents.len(), 2);
    }

    #[test]
    fn signed_and_dotted_numbers() {
        assert_eq!(tokenize("-3"), vec![Token::Number(-3.0)]);
        assert_eq!(tokenize(".5em"), vec![Token::Dimension(0.5, "em".into())]);
        assert_eq!(tokenize("50%"), vec![Token::Percentage(50.0)]);
    }
}
