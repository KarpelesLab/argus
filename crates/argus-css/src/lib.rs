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

/// Reserved [`family_key`] values: `0` = default/proportional, `1` = the
/// `monospace` generic. Web-font family hashes are always `>= 2`.
pub const FONT_KEY_DEFAULT: u32 = 0;
pub const FONT_KEY_MONOSPACE: u32 = 1;

/// A stable key for a font-family name, shared by the style engine (which records
/// the used family) and the font registry (which registers `@font-face` faces), so
/// both agree on a face. Lowercased FNV-1a, biased into `>= 2` to avoid the
/// reserved default/monospace keys.
pub fn family_key(name: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.trim().trim_matches(|c| c == '"' || c == '\'').bytes() {
        h ^= b.to_ascii_lowercase() as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    // Keep clear of the reserved 0/1 keys.
    if h < 2 {
        h.wrapping_add(2)
    } else {
        h
    }
}

/// Fold a face's bold/italic into a family [`family_key`] base so that each
/// weight/style variant of a web family gets a distinct registry key. The style
/// engine (using the run's resolved bold/italic) and the registry (using each
/// `@font-face`'s declared weight/style) compute the same key, so a bold run
/// selects the bold face when one exists. Always `>= 2` (clear of default/mono).
pub fn style_variant(base: u32, bold: bool, italic: bool) -> u32 {
    let mut v = base.rotate_left(2);
    if bold {
        v ^= 0x9e37_79b9;
    }
    if italic {
        v ^= 0x85eb_ca6b;
    }
    if v < 2 {
        v.wrapping_add(2)
    } else {
        v
    }
}

/// An `@font-face` rule: a web-font `family` name bound to a downloadable `src`,
/// with the face's declared weight/style (defaulting to normal 400).
#[derive(Clone, PartialEq, Debug)]
pub struct FontFace {
    /// The declared `font-family` name, lowercased (matched against used families).
    pub family: String,
    /// The first `url(...)` from `src` (relative to the stylesheet's base URL).
    pub src_url: String,
    /// `font-weight: bold`/`>= 600` declared on the face.
    pub bold: bool,
    /// `font-style: italic`/`oblique` declared on the face.
    pub italic: bool,
}

/// A parsed stylesheet.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// `@font-face` rules collected from the sheet (used to fetch web fonts).
    pub font_faces: Vec<FontFace>,
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
            font_faces: self.font_faces.clone(),
        }
    }
}

/// Parse a stylesheet. Malformed rules are skipped; `@`-rules are skipped (their
/// blocks too) for now.
pub fn parse_stylesheet(css: &str) -> Stylesheet {
    let tokens = tokenizer::tokenize(css);
    let mut rules = Vec::new();
    let mut font_faces = Vec::new();
    parse_rules_into(&tokens, None, &mut rules, &mut font_faces);
    let mut sheet = Stylesheet { rules, font_faces };
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
fn parse_rules_into(
    tokens: &[Token],
    media: Option<&str>,
    rules: &mut Vec<Rule>,
    font_faces: &mut Vec<FontFace>,
) {
    let mut i = 0;
    while i < tokens.len() {
        skip_ws(tokens, &mut i);
        if i >= tokens.len() {
            break;
        }

        if let Token::AtKeyword(name) = &tokens[i] {
            let is_media = name.eq_ignore_ascii_case("media");
            let is_supports = name.eq_ignore_ascii_case("supports");
            let is_font_face = name.eq_ignore_ascii_case("font-face");
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
                        parse_rules_into(block, Some(&query), rules, font_faces);
                    } else if is_supports {
                        // Include the block only if we support the feature query.
                        if supports_condition(&query) {
                            parse_rules_into(block, media, rules, font_faces);
                        }
                    } else if is_font_face {
                        if let Some(face) = font_face_from_block(block) {
                            font_faces.push(face);
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

/// Build a [`FontFace`] from an `@font-face` block's declarations, requiring both a
/// `font-family` and a `src` with at least one `url(...)`. Returns `None` if either
/// is missing.
fn font_face_from_block(block: &[Token]) -> Option<FontFace> {
    let decls = parse_declarations(block);
    let family = decls
        .iter()
        .find(|d| d.name == "font-family")
        .map(|d| d.value.trim().trim_matches(|c| c == '"' || c == '\'').to_ascii_lowercase())
        .filter(|f| !f.is_empty())?;
    let src_url = decls
        .iter()
        .find(|d| d.name == "src")
        .and_then(|d| choose_font_src(&d.value))?;
    // The face's weight/style (default normal). `font-weight: bold` or a numeric
    // `>= 600` is treated as bold; a range like `400 700` takes its first value.
    let bold = decls.iter().find(|d| d.name == "font-weight").is_some_and(|d| {
        let first = d.value.split_whitespace().next().unwrap_or("");
        first.eq_ignore_ascii_case("bold")
            || first.parse::<u32>().map(|n| n >= 600).unwrap_or(false)
    });
    let italic = decls.iter().find(|d| d.name == "font-style").is_some_and(|d| {
        matches!(d.value.trim(), "italic" | "oblique")
    });
    Some(FontFace { family, src_url, bold, italic })
}

/// Choose the best downloadable `url()` from an `@font-face` `src` list. `local()`
/// sources are skipped (no local-font access in the sandbox). Among `url()`
/// entries the most-decodable format wins — raw sfnt (ttf/otf) over WOFF (which we
/// decompress); WOFF2/EOT/SVG are skipped (undecodable). Format is read from a
/// `format(...)` hint, falling back to the URL extension; ties keep source order.
fn choose_font_src(value: &str) -> Option<String> {
    let mut best: Option<(u8, String)> = None;
    for entry in split_top_commas(value) {
        let entry = entry.trim();
        let Some(url) = extract_first_url(entry) else {
            continue; // local(...) or a malformed entry
        };
        // Format hint, else the URL's extension.
        let fmt = entry
            .find("format(")
            .map(|p| {
                let r = &entry[p + 7..];
                r[..r.find(')').unwrap_or(r.len())]
                    .trim()
                    .trim_matches(|c| c == '"' || c == '\'')
                    .to_ascii_lowercase()
            })
            .filter(|f| !f.is_empty())
            .unwrap_or_else(|| {
                url.rsplit('.').next().unwrap_or("").split(['?', '#']).next().unwrap_or("")
                    .to_ascii_lowercase()
            });
        let rank = match fmt.as_str() {
            "truetype" | "opentype" | "ttf" | "otf" | "sfnt" => 0,
            "woff" => 1,
            // woff2/eot/svg or unknown: skip (can't decode).
            _ => continue,
        };
        if best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, url));
        }
    }
    best.map(|(_, u)| u)
}

/// Split a CSS value on commas that sit at paren depth zero (keeping `url(...)`,
/// `format(...)`, and `local(...)` arguments intact).
fn split_top_commas(value: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in value.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// Extract the first `url(...)` target from a CSS value, stripping quotes and
/// surrounding whitespace. Returns `None` if there is no `url(...)`.
fn extract_first_url(value: &str) -> Option<String> {
    let start = value.find("url(")? + 4;
    let rest = &value[start..];
    let end = rest.find(')')?;
    let url = rest[..end].trim().trim_matches(|c| c == '"' || c == '\'').trim();
    (!url.is_empty()).then(|| url.to_string())
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
    fn nested_brace_at_rules_are_skipped_cleanly() {
        // `@font-face` (one block) and `@keyframes` (nested from/to blocks) must be
        // skipped without swallowing the following real rule.
        let css = "@font-face { font-family: X; src: url(x.ttf) } \
                   @keyframes spin { from { opacity: 0 } to { opacity: 1 } } \
                   p { color: green }";
        let sheet = parse_stylesheet(css);
        assert_eq!(sheet.rules.len(), 1, "only the p rule survives");
        assert_eq!(sheet.rules[0].declarations[0].value, "green");
        // The @font-face rule is now captured (family + url), not just skipped.
        assert_eq!(sheet.font_faces.len(), 1);
        assert_eq!(sheet.font_faces[0].family, "x");
        assert_eq!(sheet.font_faces[0].src_url, "x.ttf");
    }

    #[test]
    fn family_key_is_stable_and_avoids_reserved() {
        // Case/quote-insensitive and deterministic.
        assert_eq!(family_key("Inter"), family_key("  inter  "));
        assert_eq!(family_key("\"Inter\""), family_key("inter"));
        // Distinct families get distinct keys, all clear of the reserved 0/1.
        assert_ne!(family_key("inter"), family_key("roboto"));
        for name in ["inter", "roboto", "a", "x", ""] {
            assert!(family_key(name) >= 2, "{name} key must be >= 2");
        }
    }

    #[test]
    fn font_face_parses_weight_and_style_into_distinct_keys() {
        let sheet = parse_stylesheet(
            "@font-face { font-family: Inter; src: url(a.ttf) } \
             @font-face { font-family: Inter; font-weight: 700; src: url(b.ttf) } \
             @font-face { font-family: Inter; font-style: italic; src: url(c.ttf) }",
        );
        assert_eq!(sheet.font_faces.len(), 3);
        let by_url = |u: &str| sheet.font_faces.iter().find(|f| f.src_url == u).unwrap();
        assert!(!by_url("a.ttf").bold && !by_url("a.ttf").italic, "regular");
        assert!(by_url("b.ttf").bold, "weight 700 → bold");
        assert!(by_url("c.ttf").italic, "italic");
        // Each variant maps to a distinct registry key off the same family base.
        let base = family_key("inter");
        let keys = [
            style_variant(base, false, false),
            style_variant(base, true, false),
            style_variant(base, false, true),
            style_variant(base, true, true),
        ];
        for (i, a) in keys.iter().enumerate() {
            assert!(*a >= 2, "key clear of reserved");
            for b in &keys[i + 1..] {
                assert_ne!(a, b, "weight/style variants are distinct");
            }
        }
    }

    #[test]
    fn font_face_picks_a_decodable_src() {
        // Real-world src list: local() and woff2 are skipped; the ttf wins over the
        // woff fallback (both decodable, ttf ranks higher).
        let css = "@font-face { font-family: \"Inter\"; \
                   src: local('Inter'), url('/f/inter.woff2') format('woff2'), \
                        url(inter.woff) format('woff'), url(inter.ttf) format('truetype') }";
        let sheet = parse_stylesheet(css);
        assert_eq!(sheet.font_faces.len(), 1);
        assert_eq!(sheet.font_faces[0].family, "inter", "quotes stripped, lowercased");
        assert_eq!(sheet.font_faces[0].src_url, "inter.ttf", "ttf preferred over woff/woff2");
        // woff is chosen when no raw sfnt is offered.
        let woff = parse_stylesheet(
            "@font-face { font-family: A; src: url(a.woff2), url(a.woff) format('woff') }",
        );
        assert_eq!(woff.font_faces[0].src_url, "a.woff");
        // A face with only undecodable sources (woff2-only) is dropped.
        let none = parse_stylesheet("@font-face { font-family: B; src: url(b.woff2) format('woff2') }");
        assert!(none.font_faces.is_empty());
        // A face missing src entirely is dropped.
        assert!(parse_stylesheet("@font-face { font-family: NoSrc }").font_faces.is_empty());
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
