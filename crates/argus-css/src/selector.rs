//! Selector parsing, specificity, and matching against the DOM.
//!
//! Supports type, universal, class, id, and attribute (`[a]`/`[a=v]`/`~=`/`^=`/
//! `$=`/`*=`) selectors in compound selectors, joined by descendant (whitespace)
//! and child (`>`) combinators. Evaluated pseudo-classes: `:first-child`,
//! `:last-child`, `:nth-child(an+b)`, `:not(...)`, `:is(...)`/`:where(...)` (match
//! any argument; `:is()` takes its most specific argument's weight, `:where()`
//! contributes zero), and the form-state `:checked`/`:disabled`/`:enabled`. Other
//! pseudo-classes/elements are parsed-and-ignored so they don't break matching.

use crate::tokenizer::Token;
use argus_dom::{Document, NodeData, NodeId};

/// A combinator between two compound selectors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Combinator {
    Descendant,
    Child,
}

/// How an attribute selector matches its value.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AttrMatch {
    /// `[a]` — the attribute is present.
    Exists,
    /// `[a=v]`
    Exact(String),
    /// `[a~=v]` — whitespace-separated list contains `v`.
    Includes(String),
    /// `[a^=v]`
    Prefix(String),
    /// `[a$=v]`
    Suffix(String),
    /// `[a*=v]`
    Substring(String),
}

/// An attribute selector, e.g. `[type="text"]`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AttrSel {
    pub name: String,
    pub op: AttrMatch,
}

/// A structural pseudo-class we evaluate (others are parsed and ignored).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PseudoClass {
    FirstChild,
    LastChild,
    /// `:nth-child(an+b)`, stored as `(a, b)`. `odd` = `(2, 1)`, `even` = `(2, 0)`.
    NthChild(i32, i32),
    /// Form-state pseudo-classes, backed by attribute presence.
    Checked,
    Disabled,
    Enabled,
}

/// A compound selector: an optional type plus id/classes/attrs/pseudo-classes.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Compound {
    /// `None` means "any type" (universal, or a class/id-only compound).
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
    pub attrs: Vec<AttrSel>,
    pub pseudos: Vec<PseudoClass>,
    /// `:not(...)` arguments — the compound matches only if none of these do.
    pub negations: Vec<Compound>,
    /// `:is(...)` groups — each group is a list of alternatives; the group matches
    /// if any alternative matches. Contributes the most specific argument's weight.
    pub is_groups: Vec<Vec<Compound>>,
    /// `:where(...)` groups — like `:is()`, but contribute **zero** specificity.
    pub where_groups: Vec<Vec<Compound>>,
}

/// A complex selector: compounds left-to-right, with `combinators[k]` linking
/// `compounds[k-1]` to `compounds[k]` (`combinators[0]` is unused).
#[derive(Clone, PartialEq, Debug)]
pub struct Selector {
    pub compounds: Vec<Compound>,
    pub combinators: Vec<Combinator>,
}

/// CSS specificity as `(ids, classes, types)`, compared lexicographically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Specificity(pub u32, pub u32, pub u32);

impl Selector {
    pub fn specificity(&self) -> Specificity {
        let mut s = Specificity::default();
        for c in &self.compounds {
            if c.id.is_some() {
                s.0 += 1;
            }
            // Classes, attribute selectors, and pseudo-classes share the b column.
            s.1 += (c.classes.len() + c.attrs.len() + c.pseudos.len()) as u32;
            if c.tag.is_some() {
                s.2 += 1;
            }
            // `:not()` contributes the specificity of its argument.
            for n in &c.negations {
                s = s.add(compound_specificity(n));
            }
            // `:is()` contributes the specificity of its most specific argument.
            for group in &c.is_groups {
                if let Some(max) = group.iter().map(compound_specificity).max() {
                    s = s.add(max);
                }
            }
            // `:where()` contributes nothing (zero specificity, by design).
        }
        s
    }
}

impl Specificity {
    fn add(self, o: Specificity) -> Specificity {
        Specificity(self.0 + o.0, self.1 + o.1, self.2 + o.2)
    }
}

/// The specificity weight a single compound contributes (no combinators).
fn compound_specificity(c: &Compound) -> Specificity {
    let mut s = Specificity::default();
    if c.id.is_some() {
        s.0 += 1;
    }
    s.1 += (c.classes.len() + c.attrs.len() + c.pseudos.len()) as u32;
    if c.tag.is_some() {
        s.2 += 1;
    }
    for n in &c.negations {
        s = s.add(compound_specificity(n));
    }
    for group in &c.is_groups {
        if let Some(max) = group.iter().map(compound_specificity).max() {
            s = s.add(max);
        }
    }
    s
}

/// Parse a selector list (comma-separated complex selectors) from a token slice.
/// Selectors that fail to parse are skipped.
pub fn parse_selector_list(tokens: &[Token]) -> Vec<Selector> {
    // Split on commas at parenthesis depth 0 only, so commas inside a functional
    // pseudo-class argument (`:is(a, b)`, `:where(...)`, `:not(...)`) don't split the
    // list. The tokenizer folds the opening `(` into the `Function` token, so a
    // `Function` opens a level just like `LParen`.
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (idx, t) in tokens.iter().enumerate() {
        match t {
            Token::Function(_) | Token::LParen => depth += 1,
            Token::RParen => depth -= 1,
            Token::Comma if depth == 0 => {
                if let Some(s) = parse_complex(&tokens[start..idx]) {
                    out.push(s);
                }
                start = idx + 1;
            }
            _ => {}
        }
    }
    if let Some(s) = parse_complex(&tokens[start..]) {
        out.push(s);
    }
    out
}

fn parse_complex(tokens: &[Token]) -> Option<Selector> {
    let mut compounds = Vec::new();
    let mut combinators = Vec::new();
    let mut i = 0;
    let mut left = Combinator::Descendant; // combinator preceding the next compound

    skip_ws(tokens, &mut i);
    while i < tokens.len() {
        let compound = parse_compound(tokens, &mut i)?;
        compounds.push(compound);
        combinators.push(left);

        // Determine the separator to the next compound, if any.
        let mut saw_ws = false;
        while matches!(tokens.get(i), Some(Token::Whitespace)) {
            saw_ws = true;
            i += 1;
        }
        match tokens.get(i) {
            None => break,
            Some(Token::Delim('>')) => {
                left = Combinator::Child;
                i += 1;
                skip_ws(tokens, &mut i);
            }
            Some(_) if saw_ws => left = Combinator::Descendant,
            Some(_) => return None, // unexpected token with no separator
        }
    }

    if compounds.is_empty() {
        None
    } else {
        Some(Selector {
            compounds,
            combinators,
        })
    }
}

fn parse_compound(tokens: &[Token], i: &mut usize) -> Option<Compound> {
    let mut c = Compound::default();
    let mut started = false;
    loop {
        match tokens.get(*i) {
            Some(Token::Ident(name)) => {
                c.tag = Some(name.to_ascii_lowercase());
                started = true;
                *i += 1;
            }
            Some(Token::Delim('*')) => {
                started = true; // universal: no constraint
                *i += 1;
            }
            Some(Token::Hash(id)) => {
                c.id = Some(id.clone());
                started = true;
                *i += 1;
            }
            Some(Token::Delim('.')) => {
                *i += 1;
                if let Some(Token::Ident(cls)) = tokens.get(*i) {
                    c.classes.push(cls.clone());
                    started = true;
                    *i += 1;
                } else {
                    return None;
                }
            }
            // Attribute selectors: [name], [name=val], [name~=val], etc.
            Some(Token::LBracket) => {
                *i += 1;
                if let Some(attr) = parse_attr(tokens, i) {
                    c.attrs.push(attr);
                    started = true;
                } else {
                    return None;
                }
            }
            // Pseudo-classes / pseudo-elements: capture the structural ones we
            // evaluate; parse and ignore the rest.
            Some(Token::Colon) => {
                *i += 1;
                let double = matches!(tokens.get(*i), Some(Token::Colon));
                if double {
                    *i += 1; // ::
                }
                match tokens.get(*i) {
                    Some(Token::Ident(name)) => {
                        if !double {
                            match name.as_str() {
                                "first-child" => c.pseudos.push(PseudoClass::FirstChild),
                                "last-child" => c.pseudos.push(PseudoClass::LastChild),
                                "checked" => c.pseudos.push(PseudoClass::Checked),
                                "disabled" => c.pseudos.push(PseudoClass::Disabled),
                                "enabled" => c.pseudos.push(PseudoClass::Enabled),
                                _ => {}
                            }
                        }
                        *i += 1;
                    }
                    Some(Token::Function(fname)) => {
                        let is_nth = !double && fname.eq_ignore_ascii_case("nth-child");
                        let is_not = !double && fname.eq_ignore_ascii_case("not");
                        let is_is = !double && fname.eq_ignore_ascii_case("is");
                        let is_where = !double && fname.eq_ignore_ascii_case("where");
                        *i += 1;
                        // Capture the argument tokens up to the matching ')'.
                        let mut args = Vec::new();
                        let mut depth = 1;
                        while *i < tokens.len() && depth > 0 {
                            match tokens.get(*i) {
                                Some(Token::LParen) => depth += 1,
                                Some(Token::RParen) => {
                                    depth -= 1;
                                    if depth == 0 {
                                        *i += 1;
                                        break;
                                    }
                                }
                                Some(t) => args.push(t.clone()),
                                None => {}
                            }
                            *i += 1;
                        }
                        if is_nth {
                            if let Some((a, b)) = parse_nth(&args) {
                                c.pseudos.push(PseudoClass::NthChild(a, b));
                            }
                        } else if is_not {
                            // `:not(<compound>)` — parse the inner simple selector.
                            let mut j = 0;
                            if let Some(inner) = parse_compound(&args, &mut j) {
                                c.negations.push(inner);
                            }
                        } else if is_is || is_where {
                            // `:is(...)` / `:where(...)` — a comma-separated list of
                            // compound alternatives; the group matches if any does.
                            let group: Vec<Compound> = args
                                .split(|t| *t == Token::Comma)
                                .filter_map(|alt| {
                                    let mut j = 0;
                                    skip_ws(alt, &mut j);
                                    parse_compound(alt, &mut j)
                                })
                                .collect();
                            if !group.is_empty() {
                                if is_is {
                                    c.is_groups.push(group);
                                } else {
                                    c.where_groups.push(group);
                                }
                            }
                        }
                    }
                    _ => return None,
                }
                started = true;
            }
            _ => break,
        }
    }
    if started {
        Some(c)
    } else {
        None
    }
}

/// Parse the body of an attribute selector (positioned just after `[`).
fn parse_attr(tokens: &[Token], i: &mut usize) -> Option<AttrSel> {
    skip_ws(tokens, i);
    let name = match tokens.get(*i) {
        Some(Token::Ident(n)) => {
            *i += 1;
            n.to_ascii_lowercase()
        }
        _ => return None,
    };
    skip_ws(tokens, i);
    if tokens.get(*i) == Some(&Token::RBracket) {
        *i += 1;
        return Some(AttrSel {
            name,
            op: AttrMatch::Exists,
        });
    }
    let op_char = match tokens.get(*i) {
        Some(Token::Delim('=')) => {
            *i += 1;
            '='
        }
        Some(Token::Delim(c)) if matches!(*c, '~' | '^' | '$' | '*' | '|') => {
            let c = *c;
            *i += 1;
            if tokens.get(*i) != Some(&Token::Delim('=')) {
                return None;
            }
            *i += 1;
            c
        }
        _ => return None,
    };
    skip_ws(tokens, i);
    let value = match tokens.get(*i) {
        Some(Token::Ident(s)) | Some(Token::Str(s)) => {
            *i += 1;
            s.clone()
        }
        _ => return None,
    };
    skip_ws(tokens, i);
    // Optional case-sensitivity flag (i/s), then the closing bracket.
    if matches!(tokens.get(*i), Some(Token::Ident(f)) if f == "i" || f == "s") {
        *i += 1;
        skip_ws(tokens, i);
    }
    if tokens.get(*i) == Some(&Token::RBracket) {
        *i += 1;
    } else {
        return None;
    }
    let op = match op_char {
        '=' | '|' => AttrMatch::Exact(value),
        '~' => AttrMatch::Includes(value),
        '^' => AttrMatch::Prefix(value),
        '$' => AttrMatch::Suffix(value),
        '*' => AttrMatch::Substring(value),
        _ => return None,
    };
    Some(AttrSel { name, op })
}

fn skip_ws(tokens: &[Token], i: &mut usize) {
    while matches!(tokens.get(*i), Some(Token::Whitespace)) {
        *i += 1;
    }
}

/// Whether `node` (an element) matches `selector` within `doc`.
pub fn matches(doc: &Document, node: NodeId, selector: &Selector) -> bool {
    let Some(mut idx) = selector.compounds.len().checked_sub(1) else {
        return false;
    };
    if !matches_compound(doc, node, &selector.compounds[idx]) {
        return false;
    }
    let mut current = node;
    while idx > 0 {
        let comb = selector.combinators[idx];
        let target = &selector.compounds[idx - 1];
        match comb {
            Combinator::Child => match element_parent(doc, current) {
                Some(p) if matches_compound(doc, p, target) => current = p,
                _ => return false,
            },
            Combinator::Descendant => {
                let mut anc = element_parent(doc, current);
                loop {
                    match anc {
                        Some(a) if matches_compound(doc, a, target) => {
                            current = a;
                            break;
                        }
                        Some(a) => anc = element_parent(doc, a),
                        None => return false,
                    }
                }
            }
        }
        idx -= 1;
    }
    true
}

fn matches_compound(doc: &Document, node: NodeId, compound: &Compound) -> bool {
    let NodeData::Element(e) = &doc.node(node).data else {
        return false;
    };
    if let Some(tag) = &compound.tag {
        if !e.name.is_html(tag) {
            return false;
        }
    }
    if let Some(id) = &compound.id {
        if e.attr("id") != Some(id) {
            return false;
        }
    }
    if !compound.classes.is_empty() {
        let class_attr = e.attr("class").unwrap_or("");
        let present: Vec<&str> = class_attr.split_whitespace().collect();
        if !compound
            .classes
            .iter()
            .all(|c| present.contains(&c.as_str()))
        {
            return false;
        }
    }
    for attr in &compound.attrs {
        if !attr_matches(e, attr) {
            return false;
        }
    }
    for &p in &compound.pseudos {
        if !pseudo_matches(doc, node, p) {
            return false;
        }
    }
    // `:not(...)` — the compound fails if any negated selector matches.
    for n in &compound.negations {
        if matches_compound(doc, node, n) {
            return false;
        }
    }
    // `:is(...)` / `:where(...)` — each group must have a matching alternative.
    for group in compound.is_groups.iter().chain(&compound.where_groups) {
        if !group.iter().any(|alt| matches_compound(doc, node, alt)) {
            return false;
        }
    }
    true
}

fn attr_matches(e: &argus_dom::ElementData, sel: &AttrSel) -> bool {
    let Some(val) = e.attr(&sel.name) else {
        return false;
    };
    match &sel.op {
        AttrMatch::Exists => true,
        AttrMatch::Exact(v) => val == v,
        AttrMatch::Includes(v) => !v.is_empty() && val.split_whitespace().any(|w| w == v),
        AttrMatch::Prefix(v) => !v.is_empty() && val.starts_with(v.as_str()),
        AttrMatch::Suffix(v) => !v.is_empty() && val.ends_with(v.as_str()),
        AttrMatch::Substring(v) => !v.is_empty() && val.contains(v.as_str()),
    }
}

fn pseudo_matches(doc: &Document, node: NodeId, p: PseudoClass) -> bool {
    match p {
        PseudoClass::FirstChild | PseudoClass::LastChild => {
            let mut sib = match p {
                PseudoClass::LastChild => doc.node(node).next_sibling(),
                _ => doc.node(node).prev_sibling(),
            };
            while let Some(id) = sib {
                if matches!(doc.node(id).data, NodeData::Element(_)) {
                    return false; // an element sibling on that side → not first/last
                }
                sib = match p {
                    PseudoClass::LastChild => doc.node(id).next_sibling(),
                    _ => doc.node(id).prev_sibling(),
                };
            }
            true
        }
        PseudoClass::NthChild(a, b) => {
            // 1-based index among element siblings.
            let mut index = 1i32;
            let mut sib = doc.node(node).prev_sibling();
            while let Some(id) = sib {
                if matches!(doc.node(id).data, NodeData::Element(_)) {
                    index += 1;
                }
                sib = doc.node(id).prev_sibling();
            }
            // Matches if index == a*n + b for some integer n >= 0.
            if a == 0 {
                index == b
            } else {
                let diff = index - b;
                diff % a == 0 && diff / a >= 0
            }
        }
        PseudoClass::Checked => element_has_attr(doc, node, "checked"),
        PseudoClass::Disabled => element_has_attr(doc, node, "disabled"),
        PseudoClass::Enabled => !element_has_attr(doc, node, "disabled"),
    }
}

/// Whether `node` is an element carrying attribute `name`.
fn element_has_attr(doc: &Document, node: NodeId, name: &str) -> bool {
    matches!(&doc.node(node).data, NodeData::Element(e) if e.attr(name).is_some())
}

/// Parse the argument of `:nth-child(...)` into `(a, b)` for `an+b`. Handles
/// `odd`, `even`, a bare integer `b`, `n`/`-n`, `an`, and `an±b`.
fn parse_nth(args: &[Token]) -> Option<(i32, i32)> {
    match args {
        [Token::Ident(k)] if k.eq_ignore_ascii_case("odd") => Some((2, 1)),
        [Token::Ident(k)] if k.eq_ignore_ascii_case("even") => Some((2, 0)),
        [Token::Number(b)] => Some((0, *b as i32)),
        // `n`, `-n`, optionally followed by `+b`/`-b` (a signed Number token).
        [Token::Ident(k)] if is_n_ident(k) => Some((n_coeff(k), 0)),
        [Token::Ident(k), Token::Number(b)] if is_n_ident(k) => Some((n_coeff(k), *b as i32)),
        // `an`, optionally followed by a signed `b`.
        [Token::Dimension(a, u)] if u.eq_ignore_ascii_case("n") => Some((*a as i32, 0)),
        [Token::Dimension(a, u), Token::Number(b)] if u.eq_ignore_ascii_case("n") => {
            Some((*a as i32, *b as i32))
        }
        _ => None,
    }
}

/// Whether `s` is the bare `n` coefficient ident (`n` or `-n`).
fn is_n_ident(s: &str) -> bool {
    s.eq_ignore_ascii_case("n") || s.eq_ignore_ascii_case("-n") || s.eq_ignore_ascii_case("+n")
}

fn n_coeff(s: &str) -> i32 {
    if s.starts_with('-') {
        -1
    } else {
        1
    }
}

/// The nearest ancestor that is an element (stops at the document).
fn element_parent(doc: &Document, node: NodeId) -> Option<NodeId> {
    let parent = doc.node(node).parent()?;
    matches!(doc.node(parent).data, NodeData::Element(_)).then_some(parent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokenizer::tokenize;
    use argus_dom::{Attribute, QualName};

    fn sel(s: &str) -> Selector {
        parse_selector_list(&tokenize(s)).remove(0)
    }

    #[test]
    fn specificity_ordering() {
        assert!(sel("#x").specificity() > sel(".c").specificity());
        assert!(sel(".c").specificity() > sel("div").specificity());
        assert_eq!(sel("div.c.d#x").specificity(), Specificity(1, 2, 1));
    }

    #[test]
    fn parses_combinators() {
        let s = sel("div > p .note");
        assert_eq!(s.compounds.len(), 3);
        assert_eq!(s.combinators[1], Combinator::Child);
        assert_eq!(s.combinators[2], Combinator::Descendant);
    }

    #[test]
    fn matching_against_dom() {
        // <div><p class="note"><span id="x"></span></p></div>
        let mut doc = Document::new();
        let root = doc.root();
        let div = doc.create_element(QualName::html("div"), vec![]);
        doc.append(root, div);
        let p = doc.create_element(
            QualName::html("p"),
            vec![Attribute::new("class", "note lead")],
        );
        doc.append(div, p);
        let span = doc.create_element(QualName::html("span"), vec![Attribute::new("id", "x")]);
        doc.append(p, span);

        assert!(matches(&doc, span, &sel("span")));
        assert!(matches(&doc, span, &sel("#x")));
        assert!(matches(&doc, span, &sel("div span")));
        assert!(matches(&doc, span, &sel("p.note > span")));
        assert!(matches(&doc, span, &sel("div .note span")));
        assert!(!matches(&doc, span, &sel("p > div"))); // wrong structure
        assert!(!matches(&doc, p, &sel("div > span"))); // p is not a span
        assert!(matches(&doc, p, &sel(".note")));
        assert!(!matches(&doc, p, &sel(".missing")));
    }

    #[test]
    fn attribute_and_structural_selectors() {
        // <ul><li>a</li><li id="x" data-k="v1 v2">b</li><li>c</li></ul>
        let mut doc = Document::new();
        let root = doc.root();
        let ul = doc.create_element(QualName::html("ul"), vec![]);
        doc.append(root, ul);
        let li1 = doc.create_element(QualName::html("li"), vec![]);
        doc.append(ul, li1);
        let li2 = doc.create_element(
            QualName::html("li"),
            vec![Attribute::new("id", "x"), Attribute::new("data-k", "v1 v2")],
        );
        doc.append(ul, li2);
        let li3 = doc.create_element(QualName::html("li"), vec![]);
        doc.append(ul, li3);

        assert!(matches(&doc, li2, &sel("li[id]")));
        assert!(matches(&doc, li2, &sel("[id=x]")));
        assert!(matches(&doc, li2, &sel("[data-k~=v2]")));
        assert!(!matches(&doc, li2, &sel("[data-k~=v3]")));
        assert!(matches(&doc, li2, &sel("[data-k^=v1]")));
        assert!(matches(&doc, li1, &sel("li:first-child")));
        assert!(!matches(&doc, li2, &sel("li:first-child")));
        assert!(matches(&doc, li3, &sel("li:last-child")));
        assert!(!matches(&doc, li2, &sel("li:last-child")));
        // Specificity: attribute selector counts in the class column.
        assert_eq!(sel("li[id]").specificity(), Specificity(0, 1, 1));
    }

    #[test]
    fn not_selector() {
        // <ul><li>a</li><li class="skip">b</li><li id="x">c</li></ul>
        let mut doc = Document::new();
        let root = doc.root();
        let ul = doc.create_element(QualName::html("ul"), vec![]);
        doc.append(root, ul);
        let li1 = doc.create_element(QualName::html("li"), vec![]);
        doc.append(ul, li1);
        let li2 = doc.create_element(QualName::html("li"), vec![Attribute::new("class", "skip")]);
        doc.append(ul, li2);
        let li3 = doc.create_element(QualName::html("li"), vec![Attribute::new("id", "x")]);
        doc.append(ul, li3);

        assert!(matches(&doc, li1, &sel("li:not(.skip)")));
        assert!(!matches(&doc, li2, &sel("li:not(.skip)")));
        assert!(matches(&doc, li3, &sel("li:not(.skip)")));
        assert!(!matches(&doc, li3, &sel("li:not(#x)")));
        assert!(matches(&doc, li1, &sel("li:not(#x)")));
        // `:not()` adds its argument's specificity (a class here).
        assert_eq!(sel("li:not(.skip)").specificity(), Specificity(0, 1, 1));
    }

    #[test]
    fn form_state_pseudo_classes() {
        // <input checked> and <input disabled>
        let mut doc = Document::new();
        let root = doc.root();
        let checked =
            doc.create_element(QualName::html("input"), vec![Attribute::new("checked", "")]);
        doc.append(root, checked);
        let disabled = doc.create_element(
            QualName::html("input"),
            vec![Attribute::new("disabled", "")],
        );
        doc.append(root, disabled);
        let plain = doc.create_element(QualName::html("input"), vec![]);
        doc.append(root, plain);

        assert!(matches(&doc, checked, &sel("input:checked")));
        assert!(!matches(&doc, plain, &sel("input:checked")));
        assert!(matches(&doc, disabled, &sel(":disabled")));
        assert!(matches(&doc, plain, &sel("input:enabled")));
        assert!(!matches(&doc, disabled, &sel(":enabled")));
    }

    #[test]
    fn nth_child_selectors() {
        // <ul> with five <li> children: li1..li5.
        let mut doc = Document::new();
        let root = doc.root();
        let ul = doc.create_element(QualName::html("ul"), vec![]);
        doc.append(root, ul);
        let lis: Vec<NodeId> = (0..5)
            .map(|_| {
                let li = doc.create_element(QualName::html("li"), vec![]);
                doc.append(ul, li);
                li
            })
            .collect();

        // odd → 1,3,5 ; even → 2,4
        assert!(matches(&doc, lis[0], &sel("li:nth-child(odd)")));
        assert!(!matches(&doc, lis[1], &sel("li:nth-child(odd)")));
        assert!(matches(&doc, lis[1], &sel("li:nth-child(even)")));
        // exact index
        assert!(matches(&doc, lis[2], &sel("li:nth-child(3)")));
        assert!(!matches(&doc, lis[3], &sel("li:nth-child(3)")));
        // an+b: 2n+1 = 1,3,5 ; 3n = 3 (n>=1) — n=0 gives 0 (no element)
        assert!(matches(&doc, lis[4], &sel("li:nth-child(2n+1)")));
        assert!(matches(&doc, lis[2], &sel("li:nth-child(3n)")));
        assert!(!matches(&doc, lis[0], &sel("li:nth-child(3n)")));
        // n+3 (a=1,b=3) matches index >= 3 → li3,li4,li5
        assert!(matches(&doc, lis[3], &sel("li:nth-child(n+3)")));
        assert!(!matches(&doc, lis[1], &sel("li:nth-child(n+3)")));
    }

    #[test]
    fn is_and_where_match_any_alternative() {
        // <section><h1 id="a"></h1><h2 class="c"></h2><p></p></section>
        let mut doc = Document::new();
        let root = doc.root();
        let sec = doc.create_element(QualName::html("section"), vec![]);
        doc.append(root, sec);
        let h1 = doc.create_element(QualName::html("h1"), vec![Attribute::new("id", "a")]);
        doc.append(sec, h1);
        let h2 = doc.create_element(QualName::html("h2"), vec![Attribute::new("class", "c")]);
        doc.append(sec, h2);
        let p = doc.create_element(QualName::html("p"), vec![]);
        doc.append(sec, p);

        // :is() matches if any alternative matches.
        assert!(matches(&doc, h1, &sel(":is(h1, h2)")));
        assert!(matches(&doc, h2, &sel(":is(h1, h2)")));
        assert!(!matches(&doc, p, &sel(":is(h1, h2)")));
        // Scoped + combined with a class / descendant combinator.
        assert!(matches(&doc, h2, &sel("section :is(.c, .d)")));
        assert!(!matches(&doc, h1, &sel("section :is(.c, .d)")));
        // :where() matches identically...
        assert!(matches(&doc, h1, &sel(":where(h1, h2)")));
        assert!(!matches(&doc, p, &sel(":where(h1, h2)")));
        // Two :is() groups must each match.
        assert!(matches(&doc, h1, &sel(":is(h1, h2):is(#a)")));
        assert!(!matches(&doc, h2, &sel(":is(h1, h2):is(#a)")));
    }

    #[test]
    fn is_adds_specificity_but_where_does_not() {
        // :is() takes the most specific argument's weight; :where() contributes zero.
        assert_eq!(sel(":is(#a, .b)").specificity(), Specificity(1, 0, 0));
        assert_eq!(sel(":where(#a, .b)").specificity(), Specificity(0, 0, 0));
        // A type selector plus :is(.class) → (0,1,1).
        assert_eq!(sel("div:is(.c)").specificity(), Specificity(0, 1, 1));
    }
}
