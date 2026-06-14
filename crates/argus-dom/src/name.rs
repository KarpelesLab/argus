//! Element/attribute names and attributes.
//!
//! Phase 1 keeps names as boxed strings with a small namespace enum. Interning
//! (atoms for O(1) comparison) is a later optimization noted in
//! `docs/subsystems/dom.md`; the API here is shaped so it can be swapped in.

/// The XML namespace an element or attribute belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Namespace {
    /// `http://www.w3.org/1999/xhtml`
    #[default]
    Html,
    /// `http://www.w3.org/2000/svg`
    Svg,
    /// `http://www.w3.org/1998/Math/MathML`
    MathMl,
}

/// A qualified name: a local name plus its namespace. (Prefixes are resolved
/// away; we keep only the namespace.)
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct QualName {
    pub ns: Namespace,
    pub local: Box<str>,
}

impl QualName {
    /// A name in the HTML namespace.
    pub fn html(local: impl Into<Box<str>>) -> QualName {
        QualName {
            ns: Namespace::Html,
            local: local.into(),
        }
    }

    /// A name in an explicit namespace.
    pub fn new(ns: Namespace, local: impl Into<Box<str>>) -> QualName {
        QualName {
            ns,
            local: local.into(),
        }
    }

    /// Whether this is an HTML-namespaced element with the given local name.
    pub fn is_html(&self, local: &str) -> bool {
        self.ns == Namespace::Html && &*self.local == local
    }
}

/// An element attribute. Phase 1 ignores attribute namespaces (all none).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Attribute {
    pub name: Box<str>,
    pub value: String,
}

impl Attribute {
    pub fn new(name: impl Into<Box<str>>, value: impl Into<String>) -> Attribute {
        Attribute {
            name: name.into(),
            value: value.into(),
        }
    }
}
