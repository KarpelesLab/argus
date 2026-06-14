//! Selector parsing, specificity, and matching against the DOM.
//!
//! Supports type, universal, class, and id selectors in compound selectors, joined
//! by descendant (whitespace) and child (`>`) combinators. Pseudo-classes/elements
//! are parsed-and-ignored for now (they don't affect matching but are skipped so
//! they don't break parsing). Attribute and sibling selectors come later.

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
        }
        s
    }
}

/// Parse a selector list (comma-separated complex selectors) from a token slice.
/// Selectors that fail to parse are skipped.
pub fn parse_selector_list(tokens: &[Token]) -> Vec<Selector> {
    tokens
        .split(|t| *t == Token::Comma)
        .filter_map(parse_complex)
        .collect()
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
                                _ => {}
                            }
                        }
                        *i += 1;
                    }
                    Some(Token::Function(_)) => {
                        *i += 1;
                        // skip to matching ')'
                        let mut depth = 1;
                        while *i < tokens.len() && depth > 0 {
                            match tokens.get(*i) {
                                Some(Token::LParen) => depth += 1,
                                Some(Token::RParen) => depth -= 1,
                                _ => {}
                            }
                            *i += 1;
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
    let mut sib = match p {
        PseudoClass::FirstChild => doc.node(node).prev_sibling(),
        PseudoClass::LastChild => doc.node(node).next_sibling(),
    };
    while let Some(id) = sib {
        if matches!(doc.node(id).data, NodeData::Element(_)) {
            return false; // an element sibling on that side → not first/last
        }
        sib = match p {
            PseudoClass::FirstChild => doc.node(id).prev_sibling(),
            PseudoClass::LastChild => doc.node(id).next_sibling(),
        };
    }
    true
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
}
