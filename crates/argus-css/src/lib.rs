//! CSS parsing, selectors, and values (Layer 2).
//!
//! Parses stylesheets into qualified rules (a selector list plus a declaration
//! block), matches selectors against the DOM, and parses the value types layout
//! needs (colors, lengths). The cascade itself lives in `argus-style`, which
//! consumes this crate. See `docs/subsystems/style.md`.

pub mod selector;
pub mod tokenizer;
pub mod value;

pub use selector::{matches, Combinator, Compound, Selector, Specificity};
pub use value::{parse_color, parse_length, Length};

use tokenizer::Token;

/// One `name: value` declaration.
#[derive(Clone, PartialEq, Debug)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

/// A qualified rule: a selector list and its declarations.
#[derive(Clone, PartialEq, Debug)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

/// A parsed stylesheet.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

/// Parse a stylesheet. Malformed rules are skipped; `@`-rules are skipped (their
/// blocks too) for now.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    let tokens = tokenizer::tokenize(css);
    let mut rules = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        skip_ws(&tokens, &mut i);
        if i >= tokens.len() {
            break;
        }

        // Skip at-rules: to the end of their block, or a top-level ';'.
        if matches!(tokens[i], Token::AtKeyword(_)) {
            while i < tokens.len() && !matches!(tokens[i], Token::LBrace | Token::Semicolon) {
                i += 1;
            }
            match tokens.get(i) {
                Some(Token::Semicolon) => i += 1,
                Some(Token::LBrace) => skip_block(&tokens, &mut i),
                _ => {}
            }
            continue;
        }

        // Qualified rule: prelude up to '{'.
        let prelude_start = i;
        while i < tokens.len() && tokens[i] != Token::LBrace {
            i += 1;
        }
        if i >= tokens.len() {
            break; // no block — drop the trailing prelude
        }
        let prelude = &tokens[prelude_start..i];
        i += 1; // '{'

        let block_start = i;
        let mut depth = 1;
        while i < tokens.len() && depth > 0 {
            match tokens[i] {
                Token::LBrace => depth += 1,
                Token::RBrace => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
            i += 1;
        }
        let block = &tokens[block_start..i];
        if i < tokens.len() {
            i += 1; // '}'
        }

        let selectors = selector::parse_selector_list(prelude);
        if selectors.is_empty() {
            continue;
        }
        rules.push(Rule {
            selectors,
            declarations: parse_declarations(block),
        });
    }

    Stylesheet { rules }
}

/// Parse a bare declaration block (e.g. the value of an inline `style` attribute).
pub fn parse_declaration_block(input: &str) -> Vec<Declaration> {
    parse_declarations(&tokenizer::tokenize(input))
}

fn skip_ws(tokens: &[Token], i: &mut usize) {
    while matches!(tokens.get(*i), Some(Token::Whitespace)) {
        *i += 1;
    }
}

fn skip_block(tokens: &[Token], i: &mut usize) {
    // assumes tokens[*i] == LBrace
    *i += 1;
    let mut depth = 1;
    while *i < tokens.len() && depth > 0 {
        match tokens[*i] {
            Token::LBrace => depth += 1,
            Token::RBrace => depth -= 1,
            _ => {}
        }
        *i += 1;
    }
}

fn parse_declarations(tokens: &[Token]) -> Vec<Declaration> {
    let mut out = Vec::new();
    for decl in tokens.split(|t| *t == Token::Semicolon) {
        let Some(colon) = decl.iter().position(|t| *t == Token::Colon) else {
            continue;
        };
        let Some(name) = decl[..colon].iter().find_map(|t| match t {
            Token::Ident(n) => Some(n.to_ascii_lowercase()),
            _ => None,
        }) else {
            continue;
        };

        let (value_tokens, important) = strip_important(&decl[colon + 1..]);
        let value = stringify(value_tokens).trim().to_string();
        if value.is_empty() {
            continue;
        }
        out.push(Declaration {
            name,
            value,
            important,
        });
    }
    out
}

/// Strip a trailing `!important` from a value token slice.
fn strip_important(tokens: &[Token]) -> (&[Token], bool) {
    if let Some(bang) = tokens.iter().rposition(|t| *t == Token::Delim('!')) {
        let after_is_important = tokens[bang + 1..]
            .iter()
            .find(|t| **t != Token::Whitespace)
            .is_some_and(|t| matches!(t, Token::Ident(s) if s.eq_ignore_ascii_case("important")));
        if after_is_important {
            return (&tokens[..bang], true);
        }
    }
    (tokens, false)
}

/// Reconstruct a value string from its tokens (good enough for the value parsers).
fn stringify(tokens: &[Token]) -> String {
    let mut s = String::new();
    for t in tokens {
        match t {
            Token::Ident(x) | Token::Str(x) => s.push_str(x),
            Token::Hash(x) => {
                s.push('#');
                s.push_str(x);
            }
            Token::AtKeyword(x) => {
                s.push('@');
                s.push_str(x);
            }
            Token::Function(x) => {
                s.push_str(x);
                s.push('(');
            }
            Token::Number(n) => s.push_str(&fmt_num(*n)),
            Token::Dimension(n, u) => {
                s.push_str(&fmt_num(*n));
                s.push_str(u);
            }
            Token::Percentage(n) => {
                s.push_str(&fmt_num(*n));
                s.push('%');
            }
            Token::Delim(c) => s.push(*c),
            Token::Whitespace => s.push(' '),
            Token::Colon => s.push(':'),
            Token::Comma => s.push(','),
            Token::LParen => s.push('('),
            Token::RParen => s.push(')'),
            Token::LBracket => s.push('['),
            Token::RBracket => s.push(']'),
            Token::Semicolon | Token::LBrace | Token::RBrace => {}
        }
    }
    s
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rules_and_declarations() {
        let css = "/* hi */ h1, .title { color: #ff0000; margin: 1.5em !important }\n\
                   div > p { background-color: rgb(0, 128, 255); }";
        let sheet = parse_stylesheet(css);
        assert_eq!(sheet.rules.len(), 2);

        let r0 = &sheet.rules[0];
        assert_eq!(r0.selectors.len(), 2);
        assert_eq!(r0.declarations.len(), 2);
        assert_eq!(r0.declarations[0].name, "color");
        assert_eq!(r0.declarations[0].value, "#ff0000");
        assert!(r0.declarations[1].important);
        assert_eq!(r0.declarations[1].value, "1.5em");

        let r1 = &sheet.rules[1];
        assert_eq!(r1.declarations[0].name, "background-color");
        assert_eq!(
            parse_color(&r1.declarations[0].value),
            Some(argus_geometry::Color::rgb(0, 128, 255))
        );
    }

    #[test]
    fn skips_at_rules() {
        let css = "@media screen { p { color: red } } h1 { color: blue }";
        let sheet = parse_stylesheet(css);
        // The @media block is skipped wholesale; only h1 survives at top level.
        assert_eq!(sheet.rules.len(), 1);
        assert_eq!(sheet.rules[0].declarations[0].value, "blue");
    }
}
