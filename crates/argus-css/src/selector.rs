//! Selector parsing, specificity, and matching against the DOM.
//!
//! Supports type, universal, class, id, and attribute (`[a]`/`[a=v]`/`~=`/`^=`/
//! `$=`/`*=`/`|=`, plus the `[a=v i]` ASCII case-insensitive flag) selectors in
//! compound selectors, joined by descendant (whitespace), child (`>`),
//! adjacent-sibling (`+`), and general-sibling (`~`) combinators. Evaluated
//! pseudo-classes: `:first-child`,
//! `:last-child`, `:only-child`, `:nth-child(an+b)`, `:nth-last-child`,
//! `:first/last/only-of-type`, `:nth-of-type`, `:nth-last-of-type`, `:not(...)`,
//! `:is(...)`/`:where(...)` (match any argument; `:is()` takes its most specific
//! argument's weight, `:where()` contributes zero), `:root`, `:empty`, and the
//! form-state `:checked`/`:disabled`/`:enabled`/`:required`/`:read-only`, and
//! `:link`/`:any-link`. Other pseudo-classes/elements are
//! parsed-and-ignored so they don't break matching.

use crate::tokenizer::Token;
use argus_dom::{Document, NodeData, NodeId};

/// A combinator between two compound selectors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Combinator {
    Descendant,
    Child,
    /// `A + B` — B's immediately preceding element sibling is `A`.
    NextSibling,
    /// `A ~ B` — some preceding element sibling of B is `A`.
    SubsequentSibling,
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
    /// `[a|=v]` — equals `v`, or begins with `v` immediately followed by `-`
    /// (the language-subtag match).
    DashMatch(String),
}

/// An attribute selector, e.g. `[type="text"]`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AttrSel {
    pub name: String,
    pub op: AttrMatch,
    /// ASCII case-insensitive value match (the `i` flag: `[a=v i]`).
    pub ci: bool,
}

/// A structural pseudo-class we evaluate (others are parsed and ignored).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PseudoClass {
    FirstChild,
    LastChild,
    /// `:nth-child(an+b)`, stored as `(a, b)`. `odd` = `(2, 1)`, `even` = `(2, 0)`.
    NthChild(i32, i32),
    /// `:nth-last-child(an+b)` — counted from the last element sibling.
    NthLastChild(i32, i32),
    /// `:nth-of-type(an+b)` — counted among same-type element siblings.
    NthOfType(i32, i32),
    /// `:nth-last-of-type(an+b)` — same-type, counted from the end.
    NthLastOfType(i32, i32),
    /// `:first-of-type` / `:last-of-type` — first/last same-type element sibling.
    FirstOfType,
    LastOfType,
    /// `:only-child` — the sole element child; `:only-of-type` — sole of its type.
    OnlyChild,
    OnlyOfType,
    /// Form-state pseudo-classes, backed by attribute presence.
    Checked,
    Disabled,
    Enabled,
    /// `:required` / `:read-only` — backed by attribute presence.
    Required,
    ReadOnly,
    /// `:optional` — a form control without `required`.
    Optional,
    /// `:read-write` — an editable control (input/textarea, not readonly/disabled).
    ReadWrite,
    /// `:link` / `:any-link` — a hyperlink (`<a>`/`<area>` with an `href`).
    AnyLink,
    /// `:focus` — the element currently holding keyboard focus (the content
    /// process marks it with the `__argus_focus` attribute before the cascade).
    Focus,
    /// `:focus-within` — the focused element or any of its ancestors.
    FocusWithin,
    /// `:placeholder-shown` — an empty `<input>`/`<textarea>` with a `placeholder`
    /// (so the placeholder text is currently visible).
    PlaceholderShown,
    /// `:target` — the element whose `id` matches the current URL fragment (the
    /// content process marks it with `__argus_target` before the cascade).
    Target,
    /// `:root` — the document's root element (`<html>`).
    Root,
    /// `:empty` — no element or (non-whitespace) text children.
    Empty,
}

/// `::before` / `::after` generated-content pseudo-element.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PseudoElement {
    Before,
    After,
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
    /// `::before` / `::after` — the rule targets a generated-content box, not the
    /// element itself.
    pub pseudo_element: Option<PseudoElement>,
    /// `:not(...)` groups — one per `:not()`, each a selector list. The compound
    /// matches only if it matches no alternative; each `:not()` contributes the
    /// specificity of its *most specific* argument (CSS4).
    pub negations: Vec<Vec<Compound>>,
    /// `:is(...)` groups — each group is a list of alternatives; the group matches
    /// if any alternative matches. Contributes the most specific argument's weight.
    pub is_groups: Vec<Vec<Compound>>,
    /// `:where(...)` groups — like `:is()`, but contribute **zero** specificity.
    pub where_groups: Vec<Vec<Compound>>,
    /// `:has(...)` groups — each a list of alternatives matched against the
    /// element's *descendants*; the group matches if some descendant matches some
    /// alternative. Every group must be satisfied. Contributes the most specific
    /// argument's weight (like `:is()`).
    pub has_groups: Vec<Vec<Compound>>,
    /// `:lang(...)` groups — one per `:lang()`, each an OR-list of primary subtags
    /// (lowercased). The compound matches only if the element's language (its own
    /// or nearest ancestor `lang`) matches at least one tag in **every** group.
    pub langs: Vec<Vec<String>>,
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
    /// The `::before`/`::after` pseudo-element this selector targets, if any (it
    /// lives on the rightmost compound).
    pub fn pseudo_element(&self) -> Option<PseudoElement> {
        self.compounds.last().and_then(|c| c.pseudo_element)
    }

    pub fn specificity(&self) -> Specificity {
        let mut s = Specificity::default();
        for c in &self.compounds {
            if c.id.is_some() {
                s.0 += 1;
            }
            // Classes, attribute selectors, and pseudo-classes share the b column.
            s.1 += (c.classes.len() + c.attrs.len() + c.pseudos.len() + c.langs.len()) as u32;
            if c.tag.is_some() {
                s.2 += 1;
            }
            // `:not()` contributes the specificity of its argument.
            for group in &c.negations {
                if let Some(max) = group.iter().map(compound_specificity).max() {
                    s = s.add(max);
                }
            }
            // `:is()` / `:has()` contribute their most specific argument's weight.
            for group in c.is_groups.iter().chain(&c.has_groups) {
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
    s.1 += (c.classes.len() + c.attrs.len() + c.pseudos.len() + c.langs.len()) as u32;
    if c.tag.is_some() {
        s.2 += 1;
    }
    for group in &c.negations {
        if let Some(max) = group.iter().map(compound_specificity).max() {
            s = s.add(max);
        }
    }
    for group in c.is_groups.iter().chain(&c.has_groups) {
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
            Some(Token::Delim('+')) => {
                left = Combinator::NextSibling;
                i += 1;
                skip_ws(tokens, &mut i);
            }
            Some(Token::Delim('~')) => {
                left = Combinator::SubsequentSibling;
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
                                "required" => c.pseudos.push(PseudoClass::Required),
                                "read-only" => c.pseudos.push(PseudoClass::ReadOnly),
                                "optional" => c.pseudos.push(PseudoClass::Optional),
                                "read-write" => c.pseudos.push(PseudoClass::ReadWrite),
                                "link" | "any-link" => c.pseudos.push(PseudoClass::AnyLink),
                                // `:focus-visible` (keyboard-focus ring) — Argus
                                // focus is always "visible", so treat it as `:focus`.
                                "focus" | "focus-visible" => c.pseudos.push(PseudoClass::Focus),
                                "focus-within" => c.pseudos.push(PseudoClass::FocusWithin),
                                "placeholder-shown" => {
                                    c.pseudos.push(PseudoClass::PlaceholderShown)
                                }
                                "target" => c.pseudos.push(PseudoClass::Target),
                                "root" => c.pseudos.push(PseudoClass::Root),
                                "empty" => c.pseudos.push(PseudoClass::Empty),
                                "first-of-type" => c.pseudos.push(PseudoClass::FirstOfType),
                                "last-of-type" => c.pseudos.push(PseudoClass::LastOfType),
                                "only-child" => c.pseudos.push(PseudoClass::OnlyChild),
                                "only-of-type" => c.pseudos.push(PseudoClass::OnlyOfType),
                                // `:before`/`:after` are also valid (legacy single-colon).
                                "before" => c.pseudo_element = Some(PseudoElement::Before),
                                "after" => c.pseudo_element = Some(PseudoElement::After),
                                _ => {}
                            }
                        } else {
                            match name.as_str() {
                                "before" => c.pseudo_element = Some(PseudoElement::Before),
                                "after" => c.pseudo_element = Some(PseudoElement::After),
                                _ => {}
                            }
                        }
                        *i += 1;
                    }
                    Some(Token::Function(fname)) => {
                        // Which `:nth-*(an+b)` family member, if any.
                        let nth_kind = if double {
                            None
                        } else {
                            ["nth-child", "nth-last-child", "nth-of-type", "nth-last-of-type"]
                                .iter()
                                .position(|n| fname.eq_ignore_ascii_case(n))
                        };
                        let is_not = !double && fname.eq_ignore_ascii_case("not");
                        let is_is = !double && fname.eq_ignore_ascii_case("is");
                        let is_where = !double && fname.eq_ignore_ascii_case("where");
                        let is_lang = !double && fname.eq_ignore_ascii_case("lang");
                        let is_has = !double && fname.eq_ignore_ascii_case("has");
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
                        if let Some(kind) = nth_kind {
                            if let Some((a, b)) = parse_nth(&args) {
                                c.pseudos.push(match kind {
                                    0 => PseudoClass::NthChild(a, b),
                                    1 => PseudoClass::NthLastChild(a, b),
                                    2 => PseudoClass::NthOfType(a, b),
                                    _ => PseudoClass::NthLastOfType(a, b),
                                });
                            }
                        } else if is_not {
                            // `:not(<list>)` — a comma-separated selector list (CSS4);
                            // the compound fails if it matches any alternative.
                            let group: Vec<Compound> = args
                                .split(|t| *t == Token::Comma)
                                .filter_map(|alt| {
                                    let mut j = 0;
                                    skip_ws(alt, &mut j);
                                    parse_compound(alt, &mut j)
                                })
                                .collect();
                            if !group.is_empty() {
                                c.negations.push(group);
                            }
                        } else if is_is || is_where || is_has {
                            // `:is(...)` / `:where(...)` / `:has(...)` — a comma-
                            // separated list of compound alternatives. is/where match
                            // the element itself; has matches a *descendant*. A
                            // leading child combinator (`:has(> x)`) is tolerated by
                            // skipping it (we match any descendant either way).
                            let group: Vec<Compound> = args
                                .split(|t| *t == Token::Comma)
                                .filter_map(|alt| {
                                    let mut j = 0;
                                    skip_ws(alt, &mut j);
                                    if alt.get(j) == Some(&Token::Delim('>')) {
                                        j += 1;
                                        skip_ws(alt, &mut j);
                                    }
                                    parse_compound(alt, &mut j)
                                })
                                .collect();
                            if !group.is_empty() {
                                if is_is {
                                    c.is_groups.push(group);
                                } else if is_where {
                                    c.where_groups.push(group);
                                } else {
                                    c.has_groups.push(group);
                                }
                            }
                        } else if is_lang {
                            // `:lang(en, fr)` — an OR-list of primary language subtags.
                            let group: Vec<String> = args
                                .split(|t| *t == Token::Comma)
                                .filter_map(|alt| {
                                    alt.iter().find_map(|t| match t {
                                        Token::Ident(s) | Token::Str(s) => {
                                            let tag = s.split('-').next().unwrap_or(s);
                                            (!tag.is_empty()).then(|| tag.to_ascii_lowercase())
                                        }
                                        _ => None,
                                    })
                                })
                                .collect();
                            if !group.is_empty() {
                                c.langs.push(group);
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
            ci: false,
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
    // Optional case-sensitivity flag (i = ASCII case-insensitive, s = sensitive),
    // then the closing bracket.
    let mut ci = false;
    if let Some(Token::Ident(f)) = tokens.get(*i) {
        if f.eq_ignore_ascii_case("i") {
            ci = true;
            *i += 1;
            skip_ws(tokens, i);
        } else if f.eq_ignore_ascii_case("s") {
            *i += 1;
            skip_ws(tokens, i);
        }
    }
    if tokens.get(*i) == Some(&Token::RBracket) {
        *i += 1;
    } else {
        return None;
    }
    let op = match op_char {
        '=' => AttrMatch::Exact(value),
        '|' => AttrMatch::DashMatch(value),
        '~' => AttrMatch::Includes(value),
        '^' => AttrMatch::Prefix(value),
        '$' => AttrMatch::Suffix(value),
        '*' => AttrMatch::Substring(value),
        _ => return None,
    };
    Some(AttrSel { name, op, ci })
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
            Combinator::NextSibling => match prev_element_sibling(doc, current) {
                Some(s) if matches_compound(doc, s, target) => current = s,
                _ => return false,
            },
            Combinator::SubsequentSibling => {
                let mut sib = prev_element_sibling(doc, current);
                loop {
                    match sib {
                        Some(s) if matches_compound(doc, s, target) => {
                            current = s;
                            break;
                        }
                        Some(s) => sib = prev_element_sibling(doc, s),
                        None => return false,
                    }
                }
            }
        }
        idx -= 1;
    }
    true
}

/// The nearest preceding sibling that is an element (skipping text/comments).
fn prev_element_sibling(doc: &Document, node: NodeId) -> Option<NodeId> {
    let mut sib = doc.node(node).prev_sibling();
    while let Some(id) = sib {
        if matches!(&doc.node(id).data, NodeData::Element(_)) {
            return Some(id);
        }
        sib = doc.node(id).prev_sibling();
    }
    None
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
    for group in &compound.negations {
        if group.iter().any(|n| matches_compound(doc, node, n)) {
            return false;
        }
    }
    // `:is(...)` / `:where(...)` — each group must have a matching alternative.
    for group in compound.is_groups.iter().chain(&compound.where_groups) {
        if !group.iter().any(|alt| matches_compound(doc, node, alt)) {
            return false;
        }
    }
    // `:has(...)` — each group must match some descendant of the element.
    for group in &compound.has_groups {
        if !descendant_matches_any(doc, node, group) {
            return false;
        }
    }
    // `:lang(...)` — the element's language must match at least one tag per group.
    if !compound.langs.is_empty() {
        let lang = element_language(doc, node);
        let primary = lang
            .as_deref()
            .map(|l| l.split('-').next().unwrap_or(l).to_ascii_lowercase());
        for group in &compound.langs {
            let ok = primary
                .as_deref()
                .is_some_and(|p| group.iter().any(|tag| tag == p));
            if !ok {
                return false;
            }
        }
    }
    true
}

/// The effective language of `node`: the nearest `lang` (or `xml:lang`) attribute
/// on the element or any ancestor, if any.
fn element_language(doc: &Document, node: NodeId) -> Option<String> {
    let mut cur = Some(node);
    while let Some(id) = cur {
        if let NodeData::Element(e) = &doc.node(id).data {
            if let Some(l) = e.attr("lang").or_else(|| e.attr("xml:lang")) {
                let l = l.trim();
                if !l.is_empty() {
                    return Some(l.to_string());
                }
            }
        }
        cur = doc.node(id).parent();
    }
    None
}

fn attr_matches(e: &argus_dom::ElementData, sel: &AttrSel) -> bool {
    let Some(val) = e.attr(&sel.name) else {
        return false;
    };
    // For the `i` flag, compare ASCII-lowercased copies of both sides.
    let (val, fold): (String, fn(&str) -> String) = if sel.ci {
        (val.to_ascii_lowercase(), |s| s.to_ascii_lowercase())
    } else {
        (val.to_string(), |s| s.to_string())
    };
    let val = val.as_str();
    match &sel.op {
        AttrMatch::Exists => true,
        AttrMatch::Exact(v) => val == fold(v),
        AttrMatch::Includes(v) => !v.is_empty() && val.split_whitespace().any(|w| w == fold(v)),
        AttrMatch::Prefix(v) => !v.is_empty() && val.starts_with(&fold(v)),
        AttrMatch::Suffix(v) => !v.is_empty() && val.ends_with(&fold(v)),
        AttrMatch::Substring(v) => !v.is_empty() && val.contains(&fold(v)),
        AttrMatch::DashMatch(v) => {
            let v = fold(v);
            val == v || val.starts_with(&format!("{v}-"))
        }
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
        PseudoClass::NthChild(a, b) => nth_matches(a, b, sibling_index(doc, node, false, false)),
        PseudoClass::NthLastChild(a, b) => {
            nth_matches(a, b, sibling_index(doc, node, true, false))
        }
        PseudoClass::NthOfType(a, b) => nth_matches(a, b, sibling_index(doc, node, false, true)),
        PseudoClass::NthLastOfType(a, b) => {
            nth_matches(a, b, sibling_index(doc, node, true, true))
        }
        // `:first/last-of-type` — index 1 among same-type siblings (from start/end).
        PseudoClass::FirstOfType => sibling_index(doc, node, false, true) == 1,
        PseudoClass::LastOfType => sibling_index(doc, node, true, true) == 1,
        // `:only-child` — no element siblings either side.
        PseudoClass::OnlyChild => {
            sibling_index(doc, node, false, false) == 1 && sibling_index(doc, node, true, false) == 1
        }
        // `:only-of-type` — the sole same-type element among its siblings.
        PseudoClass::OnlyOfType => {
            sibling_index(doc, node, false, true) == 1 && sibling_index(doc, node, true, true) == 1
        }
        PseudoClass::Checked => element_has_attr(doc, node, "checked"),
        PseudoClass::Disabled => element_has_attr(doc, node, "disabled"),
        PseudoClass::Enabled => !element_has_attr(doc, node, "disabled"),
        PseudoClass::Required => element_has_attr(doc, node, "required"),
        PseudoClass::ReadOnly => element_has_attr(doc, node, "readonly"),
        // `:optional` — a form control (input/select/textarea) without `required`.
        PseudoClass::Optional => matches!(
            &doc.node(node).data,
            NodeData::Element(e)
                if (e.name.is_html("input") || e.name.is_html("select") || e.name.is_html("textarea"))
                    && e.attr("required").is_none()
        ),
        // `:read-write` — an editable input/textarea (not readonly/disabled), or an
        // element with `contenteditable` other than `false`.
        PseudoClass::ReadWrite => matches!(
            &doc.node(node).data,
            NodeData::Element(e)
                if ((e.name.is_html("input") || e.name.is_html("textarea"))
                        && e.attr("readonly").is_none() && e.attr("disabled").is_none())
                    || e.attr("contenteditable").is_some_and(|v| v != "false")
        ),
        PseudoClass::AnyLink => matches!(
            &doc.node(node).data,
            NodeData::Element(e)
                if (e.name.is_html("a") || e.name.is_html("area")) && e.attr("href").is_some()
        ),
        // `:focus` — the focused element, marked by the content process with the
        // `__argus_focus` attribute (see argus-content `apply_focus`).
        PseudoClass::Focus => element_has_attr(doc, node, "__argus_focus"),
        // `:focus-within` — this element or any descendant holds focus.
        PseudoClass::FocusWithin => subtree_has_focus(doc, node),
        // `:placeholder-shown` — an empty input/textarea that has a placeholder.
        PseudoClass::PlaceholderShown => matches!(
            &doc.node(node).data,
            NodeData::Element(e)
                if (e.name.is_html("input") || e.name.is_html("textarea"))
                    && e.attr("placeholder").is_some()
                    && e.attr("value").is_none_or(|v| v.is_empty())
        ),
        // `:target` — the element matching the current URL fragment (marked by the
        // content process).
        PseudoClass::Target => element_has_attr(doc, node, "__argus_target"),
        // `:root` — an element whose parent is the document node.
        PseudoClass::Root => doc
            .node(node)
            .parent()
            .is_some_and(|p| matches!(doc.node(p).data, NodeData::Document)),
        // `:empty` — no element children and no non-whitespace text.
        PseudoClass::Empty => doc.children(node).all(|c| match &doc.node(c).data {
            NodeData::Element(_) => false,
            NodeData::Text(t) => t.chars().all(|ch| ch.is_whitespace()),
            _ => true,
        }),
    }
}

/// Whether `node` is an element carrying attribute `name`.
fn element_has_attr(doc: &Document, node: NodeId, name: &str) -> bool {
    matches!(&doc.node(node).data, NodeData::Element(e) if e.attr(name).is_some())
}

/// Whether any descendant of `node` matches some alternative in a `:has()` group.
fn descendant_matches_any(doc: &Document, node: NodeId, group: &[Compound]) -> bool {
    doc.children(node).any(|c| {
        group.iter().any(|alt| matches_compound(doc, c, alt))
            || descendant_matches_any(doc, c, group)
    })
}

/// Whether `node` or any descendant carries the focus marker (`:focus-within`).
fn subtree_has_focus(doc: &Document, node: NodeId) -> bool {
    if element_has_attr(doc, node, "__argus_focus") {
        return true;
    }
    doc.children(node).any(|c| subtree_has_focus(doc, c))
}

/// The 1-based position of `node` among its element siblings, counted from the end
/// when `from_end`, and restricted to siblings sharing `node`'s tag when `same_type`.
fn sibling_index(doc: &Document, node: NodeId, from_end: bool, same_type: bool) -> i32 {
    let my_tag = doc.node(node).as_element().map(|e| e.name.local.clone());
    let step = |id: NodeId| {
        if from_end {
            doc.node(id).next_sibling()
        } else {
            doc.node(id).prev_sibling()
        }
    };
    let counts = |id: NodeId| match &doc.node(id).data {
        NodeData::Element(e) => !same_type || Some(&e.name.local) == my_tag.as_ref(),
        _ => false,
    };
    let mut index = 1i32;
    let mut sib = step(node);
    while let Some(id) = sib {
        if counts(id) {
            index += 1;
        }
        sib = step(id);
    }
    index
}

/// Whether a 1-based `index` satisfies `an + b` for some integer `n >= 0`.
fn nth_matches(a: i32, b: i32, index: i32) -> bool {
    if a == 0 {
        index == b
    } else {
        let diff = index - b;
        diff % a == 0 && diff / a >= 0
    }
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
        // `:not(<list>)` takes the most specific argument (max), not the sum:
        // :not(.a, #b) contributes (1,0,0) — the #b — not (1,1,0).
        assert_eq!(sel(":not(.a, #b)").specificity(), Specificity(1, 0, 0));
        // Two separate :not()s each contribute their arg.
        assert_eq!(sel(":not(.a):not(.b)").specificity(), Specificity(0, 2, 0));
    }

    #[test]
    fn lang_pseudo_matches_inherited_language() {
        // <html lang="en-US"><p/></html> — :lang(en) matches via the inherited lang.
        let mut doc = Document::new();
        let root = doc.root();
        let html =
            doc.create_element(QualName::html("html"), vec![Attribute::new("lang", "en-US")]);
        doc.append(root, html);
        let p = doc.create_element(QualName::html("p"), vec![]);
        doc.append(html, p);
        assert!(matches(&doc, p, &sel("p:lang(en)")), "primary subtag match");
        assert!(matches(&doc, p, &sel(":lang(fr, en-US)")), "OR-list match");
        assert!(!matches(&doc, p, &sel("p:lang(fr)")), "wrong language");
        // `:lang()` counts as a pseudo-class for specificity.
        assert_eq!(sel(":lang(en)").specificity(), Specificity(0, 1, 0));
    }

    #[test]
    fn parses_combinators() {
        let s = sel("div > p .note");
        assert_eq!(s.compounds.len(), 3);
        assert_eq!(s.combinators[1], Combinator::Child);
        assert_eq!(s.combinators[2], Combinator::Descendant);
    }

    #[test]
    fn sibling_combinators() {
        // <div><h2/><p id=p1/><p id=p2/><span/></div>
        let mut doc = Document::new();
        let root = doc.root();
        let div = doc.create_element(QualName::html("div"), vec![]);
        doc.append(root, div);
        let h2 = doc.create_element(QualName::html("h2"), vec![]);
        doc.append(div, h2);
        let p1 = doc.create_element(QualName::html("p"), vec![Attribute::new("id", "p1")]);
        doc.append(div, p1);
        let p2 = doc.create_element(QualName::html("p"), vec![Attribute::new("id", "p2")]);
        doc.append(div, p2);
        let span = doc.create_element(QualName::html("span"), vec![]);
        doc.append(div, span);

        // Adjacent sibling `+`: only the p immediately after the h2.
        assert!(matches(&doc, p1, &sel("h2 + p")));
        assert!(!matches(&doc, p2, &sel("h2 + p")), "p2 is not adjacent to h2");
        // General sibling `~`: any p following the h2.
        assert!(matches(&doc, p1, &sel("h2 ~ p")));
        assert!(matches(&doc, p2, &sel("h2 ~ p")));
        assert!(!matches(&doc, h2, &sel("p ~ h2")), "no p precedes the h2");
        // Chained with another combinator.
        assert!(matches(&doc, span, &sel("h2 ~ span")));
        assert!(matches(&doc, p2, &sel("#p1 + p")));
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
        // `:not(<compound>)` and the CSS4 `:not(<list>)`.
        assert!(matches(&doc, span, &sel("span:not(.note)")), "span isn't .note");
        assert!(!matches(&doc, p, &sel("p:not(.note)")), "p IS .note");
        assert!(matches(&doc, span, &sel(":not(p, div)")), "span is neither p nor div");
        assert!(!matches(&doc, p, &sel(":not(p, div)")), "p is excluded by the list");
        assert!(!matches(&doc, div, &sel(":not(p, div)")), "div is excluded by the list");
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
        // |= dash-match: lang="en-US" matches [lang|=en], plain "en" too, but not "english".
        let en_us = doc.create_element(QualName::html("p"), vec![Attribute::new("lang", "en-US")]);
        doc.append(root, en_us);
        let en = doc.create_element(QualName::html("p"), vec![Attribute::new("lang", "en")]);
        doc.append(root, en);
        let english = doc.create_element(QualName::html("p"), vec![Attribute::new("lang", "english")]);
        doc.append(root, english);
        assert!(matches(&doc, en_us, &sel("[lang|=en]")));
        assert!(matches(&doc, en, &sel("[lang|=en]")));
        assert!(!matches(&doc, english, &sel("[lang|=en]")));
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

        // :required and :read-only (attribute-backed).
        let required = doc.create_element(
            QualName::html("input"),
            vec![Attribute::new("required", "")],
        );
        doc.append(root, required);
        let readonly = doc.create_element(
            QualName::html("input"),
            vec![Attribute::new("readonly", "")],
        );
        doc.append(root, readonly);
        assert!(matches(&doc, required, &sel("input:required")));
        assert!(!matches(&doc, plain, &sel(":required")));
        assert!(matches(&doc, readonly, &sel(":read-only")));
        assert!(!matches(&doc, plain, &sel("input:read-only")));

        // :optional — a control without `required`; :read-write — an editable
        // input (not readonly/disabled).
        assert!(matches(&doc, plain, &sel("input:optional")));
        assert!(!matches(&doc, required, &sel(":optional")));
        assert!(matches(&doc, plain, &sel("input:read-write")));
        assert!(!matches(&doc, readonly, &sel(":read-write")));
        assert!(!matches(&doc, disabled, &sel(":read-write")));

        // :link / :any-link — <a href>, not a bare <a>.
        let link = doc.create_element(QualName::html("a"), vec![Attribute::new("href", "/x")]);
        doc.append(root, link);
        let anchor = doc.create_element(QualName::html("a"), vec![]);
        doc.append(root, anchor);
        assert!(matches(&doc, link, &sel("a:link")));
        assert!(matches(&doc, link, &sel(":any-link")));
        assert!(!matches(&doc, anchor, &sel("a:link")));
    }

    #[test]
    fn focus_and_focus_within_pseudo_classes() {
        // The content process marks the focused element with `__argus_focus`.
        let mut doc = Document::new();
        let root = doc.root();
        let wrapper = doc.create_element(QualName::html("div"), vec![]);
        doc.append(root, wrapper);
        let focused = doc.create_element(
            QualName::html("input"),
            vec![Attribute::new("__argus_focus", "")],
        );
        doc.append(wrapper, focused);
        let other = doc.create_element(QualName::html("input"), vec![]);
        doc.append(root, other);

        // :focus matches only the marked element.
        assert!(matches(&doc, focused, &sel("input:focus")));
        assert!(!matches(&doc, other, &sel("input:focus")));
        assert!(!matches(&doc, wrapper, &sel(":focus")), "ancestor isn't :focus");

        // :focus-within matches the focused element and its ancestors.
        assert!(matches(&doc, focused, &sel(":focus-within")));
        assert!(matches(&doc, wrapper, &sel("div:focus-within")));
        assert!(!matches(&doc, other, &sel(":focus-within")), "sibling subtree");

        // :focus counts as a pseudo-class for specificity (> a bare type).
        let a = sel("input:focus").specificity();
        let b = sel("input").specificity();
        assert!(a > b, "{a:?} should outrank {b:?}");
        // :focus-visible is treated as :focus.
        assert!(matches(&doc, focused, &sel("input:focus-visible")));
        assert!(!matches(&doc, other, &sel(":focus-visible")));
    }

    #[test]
    fn placeholder_shown_pseudo_class() {
        let mut doc = Document::new();
        let root = doc.root();
        // Empty input with a placeholder → placeholder shown.
        let empty = doc.create_element(
            QualName::html("input"),
            vec![Attribute::new("placeholder", "Search")],
        );
        doc.append(root, empty);
        // Same input but with a value → placeholder hidden.
        let filled = doc.create_element(
            QualName::html("input"),
            vec![
                Attribute::new("placeholder", "Search"),
                Attribute::new("value", "hi"),
            ],
        );
        doc.append(root, filled);
        // No placeholder attribute → never matches.
        let bare = doc.create_element(QualName::html("input"), vec![]);
        doc.append(root, bare);

        assert!(matches(&doc, empty, &sel("input:placeholder-shown")));
        assert!(!matches(&doc, filled, &sel(":placeholder-shown")));
        assert!(!matches(&doc, bare, &sel(":placeholder-shown")));
    }

    #[test]
    fn target_pseudo_class() {
        let mut doc = Document::new();
        let root = doc.root();
        // The content process marks the URL-fragment element with `__argus_target`.
        let hit = doc.create_element(
            QualName::html("section"),
            vec![
                Attribute::new("id", "sec"),
                Attribute::new("__argus_target", ""),
            ],
        );
        doc.append(root, hit);
        let miss = doc.create_element(QualName::html("section"), vec![Attribute::new("id", "x")]);
        doc.append(root, miss);
        assert!(matches(&doc, hit, &sel("section:target")));
        assert!(matches(&doc, hit, &sel(":target")));
        assert!(!matches(&doc, miss, &sel(":target")));
    }

    #[test]
    fn has_relational_selector() {
        // <div id=a><img></div> <div id=b><span>hi</span></div>
        let mut doc = Document::new();
        let root = doc.root();
        let a = doc.create_element(QualName::html("div"), vec![Attribute::new("id", "a")]);
        doc.append(root, a);
        let img = doc.create_element(QualName::html("img"), vec![]);
        doc.append(a, img);
        let b = doc.create_element(QualName::html("div"), vec![Attribute::new("id", "b")]);
        doc.append(root, b);
        let span = doc.create_element(QualName::html("span"), vec![Attribute::new("class", "x")]);
        doc.append(b, span);

        // div:has(img) matches the div containing an image, not the other.
        assert!(matches(&doc, a, &sel("div:has(img)")));
        assert!(!matches(&doc, b, &sel("div:has(img)")));
        // A leading child combinator is tolerated (matched as descendant).
        assert!(matches(&doc, b, &sel("div:has(> .x)")));
        // Class/compound argument and a non-matching case.
        assert!(matches(&doc, b, &sel("div:has(span.x)")));
        assert!(!matches(&doc, a, &sel("div:has(.x)")));
        // :has() adds the argument's specificity (a class here) over a bare type.
        assert!(sel("div:has(.x)").specificity() > sel("div").specificity());
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
    fn of_type_and_only_and_nth_last_pseudo_classes() {
        // <div><h2 id="h"/><p id="p1"/><span id="s"/><p id="p2"/><p id="p3"/></div>
        let mut doc = Document::new();
        let root = doc.root();
        let div = doc.create_element(QualName::html("div"), vec![]);
        doc.append(root, div);
        let mk = |doc: &mut Document, tag: &str, id: &str| {
            let n = doc.create_element(QualName::html(tag), vec![Attribute::new("id", id)]);
            doc.append(div, n);
            n
        };
        let h = mk(&mut doc, "h2", "h");
        let p1 = mk(&mut doc, "p", "p1");
        let s = mk(&mut doc, "span", "s");
        let p2 = mk(&mut doc, "p", "p2");
        let p3 = mk(&mut doc, "p", "p3");

        // :first-of-type / :last-of-type among <p> siblings.
        assert!(matches(&doc, p1, &sel("p:first-of-type")));
        assert!(!matches(&doc, p2, &sel("p:first-of-type")));
        assert!(matches(&doc, p3, &sel("p:last-of-type")));
        assert!(!matches(&doc, p2, &sel("p:last-of-type")));
        // :nth-of-type counts only same-type siblings: p2 is the 2nd <p>.
        assert!(matches(&doc, p2, &sel("p:nth-of-type(2)")));
        assert!(!matches(&doc, p2, &sel("p:nth-of-type(3)")));
        // :nth-last-child counts from the end: p3 is the last child.
        assert!(matches(&doc, p3, &sel(":nth-last-child(1)")));
        assert!(matches(&doc, p2, &sel(":nth-last-child(2)")));
        // :nth-last-of-type: p3 is the last <p>, p2 the 2nd-from-last <p>.
        assert!(matches(&doc, p3, &sel("p:nth-last-of-type(1)")));
        assert!(matches(&doc, p2, &sel("p:nth-last-of-type(2)")));
        // :only-of-type — h2 and span are each the sole one of their type.
        assert!(matches(&doc, h, &sel("h2:only-of-type")));
        assert!(matches(&doc, s, &sel("span:only-of-type")));
        assert!(!matches(&doc, p1, &sel("p:only-of-type")));
        // :only-child fails for everything here (div has 5 children).
        assert!(!matches(&doc, h, &sel(":only-child")));
    }

    #[test]
    fn case_insensitive_attribute_flag() {
        // <input type="TEXT"><input type="text">
        let mut doc = Document::new();
        let root = doc.root();
        let a = doc.create_element(QualName::html("input"), vec![Attribute::new("type", "TEXT")]);
        doc.append(root, a);
        let b = doc.create_element(QualName::html("input"), vec![Attribute::new("type", "text")]);
        doc.append(root, b);

        // Default match is case-sensitive.
        assert!(!matches(&doc, a, &sel("[type=text]")));
        assert!(matches(&doc, b, &sel("[type=text]")));
        // The `i` flag folds ASCII case on both sides.
        assert!(matches(&doc, a, &sel("[type=text i]")));
        assert!(matches(&doc, b, &sel("[type=TEXT i]")));
        // Works with substring/prefix operators too.
        assert!(matches(&doc, a, &sel("[type^=te i]")));
        assert!(matches(&doc, a, &sel("[type*=EX i]")));
        // An explicit `s` flag keeps it case-sensitive.
        assert!(!matches(&doc, a, &sel("[type=text s]")));
    }

    #[test]
    fn only_child_matches_sole_element() {
        // <ul><li id="solo"/></ul>
        let mut doc = Document::new();
        let root = doc.root();
        let ul = doc.create_element(QualName::html("ul"), vec![]);
        doc.append(root, ul);
        let li = doc.create_element(QualName::html("li"), vec![Attribute::new("id", "solo")]);
        doc.append(ul, li);
        assert!(matches(&doc, li, &sel("li:only-child")));
        assert!(matches(&doc, li, &sel("li:only-of-type")));
    }

    #[test]
    fn root_and_empty_pseudo_classes() {
        // <html><body><p id="e"></p><p id="ws"> </p><p id="f">x</p><div id="d"><span/></div></body></html>
        let mut doc = Document::new();
        let root = doc.root();
        let html = doc.create_element(QualName::html("html"), vec![]);
        doc.append(root, html);
        let body = doc.create_element(QualName::html("body"), vec![]);
        doc.append(html, body);
        let empty = doc.create_element(QualName::html("p"), vec![Attribute::new("id", "e")]);
        doc.append(body, empty);
        let ws = doc.create_element(QualName::html("p"), vec![Attribute::new("id", "ws")]);
        doc.append(body, ws);
        let t = doc.create_text(" ");
        doc.append(ws, t);
        let full = doc.create_element(QualName::html("p"), vec![Attribute::new("id", "f")]);
        doc.append(body, full);
        let txt = doc.create_text("x");
        doc.append(full, txt);
        let parent = doc.create_element(QualName::html("div"), vec![Attribute::new("id", "d")]);
        doc.append(body, parent);
        let child = doc.create_element(QualName::html("span"), vec![]);
        doc.append(parent, child);

        // :root is the <html> element only.
        assert!(matches(&doc, html, &sel(":root")));
        assert!(!matches(&doc, body, &sel(":root")));
        // :empty — no children, or whitespace-only text (Selectors-4 behavior).
        assert!(matches(&doc, empty, &sel("p:empty")));
        assert!(matches(&doc, ws, &sel(":empty")));
        assert!(!matches(&doc, full, &sel(":empty"))); // has text
        assert!(!matches(&doc, parent, &sel(":empty"))); // has an element child
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
