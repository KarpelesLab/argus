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

/// A compound selector: an optional type plus id/classes, with no combinator.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Compound {
    /// `None` means "any type" (universal, or a class/id-only compound).
    pub tag: Option<String>,
    pub id: Option<String>,
    pub classes: Vec<String>,
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
            s.1 += c.classes.len() as u32;
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
            // Pseudo-classes / pseudo-elements: parse and ignore.
            Some(Token::Colon) => {
                *i += 1;
                if matches!(tokens.get(*i), Some(Token::Colon)) {
                    *i += 1; // ::
                }
                match tokens.get(*i) {
                    Some(Token::Ident(_)) => *i += 1,
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
}
