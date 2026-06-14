//! The DOM tree (Layer 2).
//!
//! An arena-backed node tree: nodes live in a `Vec` keyed by [`NodeId`] (a stable
//! index), with parent/child/sibling links stored as ids rather than pointers.
//! This gives cache-friendly traversal, cheap links, and a single clear owner (the
//! [`Document`]) — see `docs/subsystems/dom.md`. Phase 1 covers construction and
//! traversal; mutation observers, ranges, and shadow trees arrive in Phase 2.

mod name;
mod serialize;

pub use name::{Attribute, Namespace, QualName};

use std::fmt;

/// A stable handle to a node within its [`Document`] arena.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Node#{}", self.0)
    }
}

/// The payload of a node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NodeData {
    /// The document root.
    Document,
    /// A `<!DOCTYPE …>`.
    Doctype {
        name: Box<str>,
        public_id: Box<str>,
        system_id: Box<str>,
    },
    /// An element.
    Element(ElementData),
    /// A run of character data.
    Text(String),
    /// A `<!-- … -->` comment.
    Comment(String),
}

/// An element's name and attributes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ElementData {
    pub name: QualName,
    pub attrs: Vec<Attribute>,
}

impl ElementData {
    /// The value of attribute `name` (case-sensitive local name), if present.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|a| &*a.name == name)
            .map(|a| a.value.as_str())
    }
}

/// One node plus its links. Links are `None` at the ends.
#[derive(Clone, Debug)]
pub struct Node {
    pub data: NodeData,
    parent: Option<NodeId>,
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev_sibling: Option<NodeId>,
    next_sibling: Option<NodeId>,
}

impl Node {
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }
    pub fn first_child(&self) -> Option<NodeId> {
        self.first_child
    }
    pub fn last_child(&self) -> Option<NodeId> {
        self.last_child
    }
    pub fn next_sibling(&self) -> Option<NodeId> {
        self.next_sibling
    }
    pub fn prev_sibling(&self) -> Option<NodeId> {
        self.prev_sibling
    }

    /// The element payload, if this node is an element.
    pub fn as_element(&self) -> Option<&ElementData> {
        match &self.data {
            NodeData::Element(e) => Some(e),
            _ => None,
        }
    }
}

/// An owned DOM tree. Node `0` is always the [`NodeData::Document`] root.
pub struct Document {
    nodes: Vec<Node>,
}

impl Document {
    /// A new, empty document containing only the document root.
    pub fn new() -> Document {
        Document {
            nodes: vec![Node {
                data: NodeData::Document,
                parent: None,
                first_child: None,
                last_child: None,
                prev_sibling: None,
                next_sibling: None,
            }],
        }
    }

    /// The document root id (always `Node#0`).
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// Borrow a node.
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    /// Mutably borrow a node's data (links are managed via the tree methods).
    pub fn data_mut(&mut self, id: NodeId) -> &mut NodeData {
        &mut self.nodes[id.index()].data
    }

    /// Total node count (including the document root).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Always false — a document always has at least its root node.
    pub fn is_empty(&self) -> bool {
        false
    }

    fn push(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            data,
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
        });
        id
    }

    /// Create a detached element node.
    pub fn create_element(&mut self, name: QualName, attrs: Vec<Attribute>) -> NodeId {
        self.push(NodeData::Element(ElementData { name, attrs }))
    }

    /// Create a detached text node.
    pub fn create_text(&mut self, text: impl Into<String>) -> NodeId {
        self.push(NodeData::Text(text.into()))
    }

    /// Create a detached comment node.
    pub fn create_comment(&mut self, text: impl Into<String>) -> NodeId {
        self.push(NodeData::Comment(text.into()))
    }

    /// Create a detached doctype node.
    pub fn create_doctype(
        &mut self,
        name: impl Into<Box<str>>,
        public_id: impl Into<Box<str>>,
        system_id: impl Into<Box<str>>,
    ) -> NodeId {
        self.push(NodeData::Doctype {
            name: name.into(),
            public_id: public_id.into(),
            system_id: system_id.into(),
        })
    }

    /// Append `child` as the last child of `parent`, detaching it first if needed.
    pub fn append(&mut self, parent: NodeId, child: NodeId) {
        self.detach(child);
        let last = self.nodes[parent.index()].last_child;
        self.nodes[child.index()].parent = Some(parent);
        self.nodes[child.index()].prev_sibling = last;
        match last {
            Some(prev) => self.nodes[prev.index()].next_sibling = Some(child),
            None => self.nodes[parent.index()].first_child = Some(child),
        }
        self.nodes[parent.index()].last_child = Some(child);
    }

    /// Insert `child` immediately before `sibling` (which must have a parent).
    pub fn insert_before(&mut self, sibling: NodeId, child: NodeId) {
        self.detach(child);
        let parent = self.nodes[sibling.index()]
            .parent
            .expect("insert_before: reference node has no parent");
        let prev = self.nodes[sibling.index()].prev_sibling;

        self.nodes[child.index()].parent = Some(parent);
        self.nodes[child.index()].prev_sibling = prev;
        self.nodes[child.index()].next_sibling = Some(sibling);
        self.nodes[sibling.index()].prev_sibling = Some(child);
        match prev {
            Some(p) => self.nodes[p.index()].next_sibling = Some(child),
            None => self.nodes[parent.index()].first_child = Some(child),
        }
    }

    /// Remove `child` from its parent, leaving it detached (still allocated).
    pub fn detach(&mut self, child: NodeId) {
        let (parent, prev, next) = {
            let n = &self.nodes[child.index()];
            (n.parent, n.prev_sibling, n.next_sibling)
        };
        if let Some(parent) = parent {
            match prev {
                Some(p) => self.nodes[p.index()].next_sibling = next,
                None => self.nodes[parent.index()].first_child = next,
            }
            match next {
                Some(n) => self.nodes[n.index()].prev_sibling = prev,
                None => self.nodes[parent.index()].last_child = prev,
            }
        }
        let n = &mut self.nodes[child.index()];
        n.parent = None;
        n.prev_sibling = None;
        n.next_sibling = None;
    }

    /// Iterate the children of `parent` in order.
    pub fn children(&self, parent: NodeId) -> Children<'_> {
        Children {
            doc: self,
            next: self.nodes[parent.index()].first_child,
        }
    }

    /// Serialize the tree under the document root in the html5lib `#document`
    /// format (used by tests). See [`serialize`].
    pub fn serialize(&self) -> String {
        serialize::serialize(self)
    }
}

impl Default for Document {
    fn default() -> Self {
        Document::new()
    }
}

/// Iterator over a node's children, yielded in order.
pub struct Children<'a> {
    doc: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let id = self.next?;
        self.next = self.doc.nodes[id.index()].next_sibling;
        Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(doc: &mut Document, name: &str) -> NodeId {
        doc.create_element(QualName::html(name), Vec::new())
    }

    #[test]
    fn append_builds_ordered_children() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = el(&mut doc, "a");
        let b = el(&mut doc, "b");
        let c = el(&mut doc, "c");
        doc.append(root, a);
        doc.append(root, b);
        doc.append(root, c);

        let kids: Vec<_> = doc.children(root).collect();
        assert_eq!(kids, vec![a, b, c]);
        assert_eq!(doc.node(a).parent(), Some(root));
        assert_eq!(doc.node(c).next_sibling(), None);
        assert_eq!(doc.node(b).prev_sibling(), Some(a));
    }

    #[test]
    fn insert_before_and_detach() {
        let mut doc = Document::new();
        let root = doc.root();
        let a = el(&mut doc, "a");
        let c = el(&mut doc, "c");
        doc.append(root, a);
        doc.append(root, c);

        let b = el(&mut doc, "b");
        doc.insert_before(c, b);
        assert_eq!(doc.children(root).collect::<Vec<_>>(), vec![a, b, c]);

        doc.detach(b);
        assert_eq!(doc.children(root).collect::<Vec<_>>(), vec![a, c]);
        assert_eq!(doc.node(b).parent(), None);
    }

    #[test]
    fn append_moves_an_attached_node() {
        let mut doc = Document::new();
        let root = doc.root();
        let p = el(&mut doc, "p");
        let span = el(&mut doc, "span");
        doc.append(root, p);
        doc.append(root, span);
        // Re-appending span under p moves it out of root.
        doc.append(p, span);
        assert_eq!(doc.children(root).collect::<Vec<_>>(), vec![p]);
        assert_eq!(doc.children(p).collect::<Vec<_>>(), vec![span]);
    }
}
