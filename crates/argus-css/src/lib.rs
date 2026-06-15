//! CSS parsing, selectors, and values (Layer 2).
//!
//! Parses stylesheets into qualified rules (a selector list plus a declaration
//! block), matches selectors against the DOM, and parses the value types layout
//! needs (colors, lengths). The cascade itself lives in `argus-style`, which
//! consumes this crate. See `docs/subsystems/style.md`.

pub mod selector;
pub mod tokenizer;
pub mod value;

pub use selector::{matches, Combinator, Compound, PseudoElement, Selector, Specificity};
pub use value::{parse_color, parse_length, Length};

use tokenizer::Token;

/// One `name: value` declaration.
#[derive(Clone, PartialEq, Debug)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

/// A qualified rule: a selector list and its declarations. `media`, if set, is
/// the `@media` condition the rule is gated behind (evaluated against the viewport).
#[derive(Clone, PartialEq, Debug)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
    pub media: Option<String>,
}

/// A parsed stylesheet.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

impl Stylesheet {
    /// A copy keeping only rules whose `@media` condition matches a viewport
    /// `viewport_width` px wide (un-gated rules are always kept). Source order
    /// is preserved so the cascade is unchanged.
    pub fn matching_media(&self, viewport_width: f32) -> Stylesheet {
        Stylesheet {
            rules: self
                .rules
                .iter()
                .filter(|r| match &r.media {
                    None => true,
                    Some(q) => media_query_matches(q, viewport_width),
                })
                .cloned()
                .collect(),
        }
    }
}

/// Parse a stylesheet. Malformed rules are skipped; `@`-rules are skipped (their
/// blocks too) for now.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    let tokens = tokenizer::tokenize(css);
    let mut rules = Vec::new();
    parse_rules_into(&tokens, None, &mut rules);
    let mut sheet = Stylesheet { rules };
    resolve_custom_properties(&mut sheet);
    sheet
}

/// Resolve CSS custom properties (`--name`) and substitute `var(--name, fallback)`
/// references in all declaration values. Variables are gathered globally
/// (last declaration wins) — an approximation of `:root`-scoped design tokens that
/// covers the common case without per-element inheritance.
fn resolve_custom_properties(sheet: &mut Stylesheet) {
    use std::collections::HashMap;
    // Gather, in source order, every custom property declaration.
    let mut vars: HashMap<String, String> = HashMap::new();
    let mut any = false;
    for rule in &sheet.rules {
        for d in &rule.declarations {
            if d.name.starts_with("--") {
                vars.insert(d.name.clone(), d.value.clone());
                any = true;
            }
        }
    }
    if !any {
        return;
    }
    // Resolve variables that reference other variables (bounded passes).
    for _ in 0..10 {
        let snapshot = vars.clone();
        let mut changed = false;
        for v in vars.values_mut() {
            if v.contains("var(") {
                let resolved = substitute_vars(v, &snapshot, 0);
                if resolved != *v {
                    *v = resolved;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Substitute into every non-custom declaration value.
    for rule in &mut sheet.rules {
        for d in &mut rule.declarations {
            if !d.name.starts_with("--") && d.value.contains("var(") {
                d.value = substitute_vars(&d.value, &vars, 0);
            }
        }
    }
}

/// Replace `var(--name)` / `var(--name, fallback)` occurrences in `value`.
fn substitute_vars(
    value: &str,
    vars: &std::collections::HashMap<String, String>,
    depth: u32,
) -> String {
    if depth > 16 {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(pos) = rest.find("var(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        // Find the matching ')' for this var( accounting for nested parens.
        let mut depth_p = 1;
        let mut end = None;
        for (k, ch) in after.char_indices() {
            match ch {
                '(' => depth_p += 1,
                ')' => {
                    depth_p -= 1;
                    if depth_p == 0 {
                        end = Some(k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            // Unbalanced — emit the rest verbatim and stop.
            out.push_str(rest);
            return out;
        };
        let inner = &after[..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let replacement = match vars.get(name) {
            Some(v) => v.clone(),
            None => fallback.unwrap_or("").to_string(),
        };
        // Resolve nested var() inside the replacement/fallback.
        out.push_str(&substitute_vars(&replacement, vars, depth + 1));
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Parse qualified rules from `tokens`, tagging each with `media` (the enclosing
/// `@media` condition, if any). `@media` blocks recurse; other at-rules are skipped.
fn parse_rules_into(tokens: &[Token], media: Option<&str>, rules: &mut Vec<Rule>) {
    let mut i = 0;
    while i < tokens.len() {
        skip_ws(tokens, &mut i);
        if i >= tokens.len() {
            break;
        }

        if let Token::AtKeyword(name) = &tokens[i] {
            let is_media = name.eq_ignore_ascii_case("media");
            let is_supports = name.eq_ignore_ascii_case("supports");
            i += 1;
            // Capture the prelude (the media query) up to '{' or ';'.
            let prelude_start = i;
            while i < tokens.len() && !matches!(tokens[i], Token::LBrace | Token::Semicolon) {
                i += 1;
            }
            let query = stringify(&tokens[prelude_start..i]).trim().to_string();
            match tokens.get(i) {
                Some(Token::Semicolon) => i += 1,
                Some(Token::LBrace) => {
                    let block_start = i + 1;
                    skip_block(tokens, &mut i); // advances past the matching '}'
                    let block = &tokens[block_start..i.saturating_sub(1).max(block_start)];
                    if is_media {
                        // Nested media combines conservatively to the inner query.
                        parse_rules_into(block, Some(&query), rules);
                    } else if is_supports {
                        // Include the block only if we support the feature query.
                        if supports_condition(&query) {
                            parse_rules_into(block, media, rules);
                        }
                    }
                }
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
            media: media.map(|m| m.to_string()),
        });
    }
}

/// Whether an `@supports` condition is satisfied. Handles `(prop: value)` feature
/// tests (supported when the property is one the cascade applies), `not`, and
/// top-level `and`/`or` chains. Conservatively treats a known property as supported
/// regardless of the value (we don't fully validate values).
pub fn supports_condition(cond: &str) -> bool {
    let c = cond.trim();
    if let Some(rest) = c.strip_prefix("not ") {
        return !supports_condition(rest.trim());
    }
    // Top-level `or`/`and` (we don't handle deeply nested groups).
    if c.to_ascii_lowercase().contains(" or ") {
        return c.split(" or ").any(supports_feature_group);
    }
    if c.to_ascii_lowercase().contains(" and ") {
        return c.split(" and ").all(supports_feature_group);
    }
    supports_feature_group(c)
}

/// Evaluate a single parenthesized `(prop: value)` feature.
fn supports_feature_group(s: &str) -> bool {
    let s = s.trim().trim_start_matches('(').trim_end_matches(')');
    match s.split_once(':') {
        Some((prop, _val)) => supports_property(prop.trim()),
        None => false,
    }
}

/// Whether the cascade applies the named property (so a feature test for it is
/// reported as supported).
fn supports_property(prop: &str) -> bool {
    const KNOWN: &[&str] = &[
        "display",
        "color",
        "background-color",
        "background",
        "margin",
        "padding",
        "border",
        "border-color",
        "border-radius",
        "width",
        "min-width",
        "max-width",
        "height",
        "min-height",
        "aspect-ratio",
        "font-size",
        "font-weight",
        "font",
        "text-align",
        "text-decoration",
        "text-transform",
        "line-height",
        "list-style-type",
        "box-sizing",
        "opacity",
        "white-space",
        "vertical-align",
        "gap",
        "visibility",
        "outline",
        "position",
        "top",
        "right",
        "bottom",
        "left",
        "inset",
        "flex-direction",
        "grid-template-columns",
    ];
    KNOWN.contains(&prop.to_ascii_lowercase().as_str())
}

/// Whether a `@media` query matches a viewport `viewport_width` px wide. Supports
/// `screen`/`all`/`print`, `(min-width)`/`(max-width)`/`(width)` in px/em, the
/// keyword features `prefers-color-scheme`/`prefers-reduced-motion`/`hover`/
/// `pointer`/`orientation` (answered for a typical desktop screen), comma lists
/// (OR), `and` (AND), and a leading `not` (negation). Unknown features are
/// treated as non-matching.
pub fn media_query_matches(query: &str, viewport_width: f32) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return true;
    }
    // A comma-separated media query list matches if any branch matches.
    q.split(',')
        .any(|branch| media_branch_matches(branch, viewport_width))
}

fn media_branch_matches(branch: &str, vw: f32) -> bool {
    let branch = branch.trim().to_ascii_lowercase();
    if branch.is_empty() {
        return true;
    }
    // A leading `not` negates the whole branch (e.g. `not screen`, `not all`,
    // `not (min-width: 600px)`).
    let (negate, branch) = match branch.strip_prefix("not ") {
        Some(rest) => (true, rest.trim().to_string()),
        None => (false, branch),
    };
    // Every `and`-joined condition must hold. Split on the ` and ` keyword (with
    // surrounding spaces) so a value like `landscape` isn't split on its "and".
    let matched = branch.split(" and ").all(|cond| {
        let cond = cond.trim().trim_start_matches("only ").trim();
        if cond.is_empty() || cond == "screen" || cond == "all" {
            return true;
        }
        if cond == "print" || cond == "speech" {
            return false;
        }
        if let Some(inner) = cond.strip_prefix('(').and_then(|s| s.strip_suffix(')')) {
            let Some((feat, val)) = inner.split_once(':') else {
                return false;
            };
            let feat = feat.trim();
            let val = val.trim();
            // Keyword features answered for a typical desktop screen (no dark mode,
            // a fine pointer that can hover, landscape, no reduced-motion).
            match feat {
                "prefers-color-scheme" => return matches!(val, "light" | "no-preference"),
                "prefers-reduced-motion" => return val == "no-preference",
                "prefers-contrast" => return matches!(val, "no-preference" | "standard"),
                "hover" | "any-hover" => return val == "hover",
                "pointer" | "any-pointer" => return val == "fine",
                "orientation" => return val == "landscape",
                _ => {}
            }
            let px = parse_length(val).map(|l| l.to_px(16.0, vw));
            return match (feat, px) {
                ("min-width", Some(p)) => vw >= p,
                ("max-width", Some(p)) => vw <= p,
                ("width", Some(p)) => (vw - p).abs() < 0.5,
                _ => false,
            };
        }
        false
    });
    matched != negate
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
    fn parses_media_rules_and_other_at_rules_skipped() {
        let css = "@import url(x.css); @media screen { p { color: red } } h1 { color: blue }";
        let sheet = parse_stylesheet(css);
        // @import is skipped; the @media rule is kept (tagged) plus the top-level h1.
        assert_eq!(sheet.rules.len(), 2);
        let p = sheet.rules.iter().find(|r| r.media.is_some()).unwrap();
        assert_eq!(p.media.as_deref(), Some("screen"));
        assert_eq!(p.declarations[0].value, "red");
        let h1 = sheet.rules.iter().find(|r| r.media.is_none()).unwrap();
        assert_eq!(h1.declarations[0].value, "blue");
    }

    #[test]
    fn supports_rule_is_gated_on_the_condition() {
        // Supported feature (display is applied) → the block's rule is kept.
        let s1 = parse_stylesheet("@supports (display: grid) { .x { color: red } }");
        assert_eq!(s1.rules.len(), 1);
        assert_eq!(s1.rules[0].declarations[0].value, "red");
        // Unknown property → the block is dropped.
        let s2 = parse_stylesheet("@supports (rotate: 5deg) { .x { color: red } }");
        assert_eq!(s2.rules.len(), 0);
        // `not (unknown)` is supported; `and`/`or` combine.
        assert!(supports_condition("not (rotate: 5deg)"));
        assert!(supports_condition("(display: grid) and (gap: 1px)"));
        assert!(!supports_condition("(display: grid) and (rotate: 1deg)"));
        assert!(supports_condition("(rotate: 1deg) or (color: red)"));
    }

    #[test]
    fn custom_properties_substitution() {
        let css = ":root { --brand: #ff0000; --pad: 8px; --accent: var(--brand) }\
                   .a { color: var(--brand); padding: var(--pad) }\
                   .b { color: var(--missing, #00ff00); border-color: var(--accent) }";
        let sheet = parse_stylesheet(css);
        let decl = |sel_idx: usize, name: &str| -> String {
            sheet.rules[sel_idx]
                .declarations
                .iter()
                .find(|d| d.name == name)
                .map(|d| d.value.clone())
                .unwrap_or_default()
        };
        // .a is the second rule (index 1).
        assert_eq!(decl(1, "color"), "#ff0000");
        assert_eq!(decl(1, "padding"), "8px");
        // Fallback used when the variable is undefined.
        assert_eq!(decl(2, "color"), "#00ff00");
        // Variable referencing another variable resolves transitively.
        assert_eq!(decl(2, "border-color"), "#ff0000");
    }

    #[test]
    fn media_query_evaluation() {
        assert!(media_query_matches("screen", 800.0));
        assert!(media_query_matches("", 800.0));
        assert!(!media_query_matches("print", 800.0));
        assert!(media_query_matches("(max-width: 600px)", 500.0));
        assert!(!media_query_matches("(max-width: 600px)", 700.0));
        assert!(media_query_matches("(min-width: 600px)", 700.0));
        assert!(media_query_matches("screen and (min-width: 400px)", 500.0));
        assert!(!media_query_matches("screen and (min-width: 800px)", 500.0));
        // Comma list is OR.
        assert!(media_query_matches("print, (max-width: 600px)", 500.0));

        // `not` negates the whole branch.
        assert!(!media_query_matches("not screen", 800.0)); // we render as screen
        assert!(media_query_matches("not print", 800.0));
        assert!(!media_query_matches("not all", 800.0));
        assert!(!media_query_matches("not (min-width: 600px)", 700.0)); // width matches → negated
        assert!(media_query_matches("not (min-width: 600px)", 500.0)); // doesn't match → negated true
        // Exact `(width:)`.
        assert!(media_query_matches("(width: 800px)", 800.0));
        assert!(!media_query_matches("(width: 800px)", 700.0));

        // Keyword features: desktop-screen defaults (light, fine pointer, hover,
        // landscape, no reduced motion).
        assert!(media_query_matches("(prefers-color-scheme: light)", 800.0));
        assert!(!media_query_matches("(prefers-color-scheme: dark)", 800.0));
        assert!(media_query_matches("(hover: hover)", 800.0));
        assert!(!media_query_matches("(hover: none)", 800.0));
        assert!(media_query_matches("(pointer: fine)", 800.0));
        assert!(media_query_matches("(orientation: landscape)", 800.0));
        assert!(!media_query_matches("(orientation: portrait)", 800.0));
        assert!(media_query_matches("(prefers-reduced-motion: no-preference)", 800.0));
        // Combined with width.
        assert!(media_query_matches("(prefers-color-scheme: light) and (min-width: 600px)", 700.0));

        // A matching media rule overrides a base rule via matching_media().
        let sheet =
            parse_stylesheet("p { color: blue } @media (max-width: 600px) { p { color: red } }");
        assert_eq!(sheet.matching_media(500.0).rules.len(), 2); // both apply
        assert_eq!(sheet.matching_media(900.0).rules.len(), 1); // media rule dropped
    }

    /// Robustness (a lightweight fuzz): the tokenizer + parser + selector engine +
    /// value parsers must never panic on arbitrary byte input.
    #[test]
    fn css_parser_survives_arbitrary_input() {
        let mut seed = 0xD1B54A32D192ED03u64;
        let mut byte = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed & 0xff) as u8
        };
        // Bias toward selector + value + at-rule + pseudo bytes for deep coverage.
        const BIAS: &[u8] =
            b"{}()[]:;,.#*>~+=\"' \n%/-0123abcdivpxemrgb()repeat,@medianotandfrhslvarisheronth";
        for _ in 0..4000 {
            let len = (byte() as usize) * 3;
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    if byte() < 150 {
                        BIAS[byte() as usize % BIAS.len()]
                    } else {
                        byte()
                    }
                })
                .collect();
            let css = String::from_utf8_lossy(&bytes);
            let sheet = parse_stylesheet(&css);
            // Exercise selector machinery + value parsing on whatever parsed.
            for rule in &sheet.rules {
                for sel in &rule.selectors {
                    let _ = sel.specificity();
                    let _ = sel.pseudo_element();
                }
                for d in &rule.declarations {
                    let _ = parse_color(&d.value);
                    let _ = parse_length(&d.value);
                }
            }
            // The inline-style declaration-block path is a separate parser.
            let _ = parse_declaration_block(&css);
        }
    }
}
