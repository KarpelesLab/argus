//! Tree serialization in the html5lib `#document` format.
//!
//! This is the format the html5lib-tests corpus uses to express expected trees,
//! so emitting it lets tests read naturally and lines us up for running that
//! corpus later. Each line is `| ` then two spaces per depth, then the node:
//! `<tag>` for elements (with `svg`/`math` prefix for foreign namespaces),
//! `name="value"` for attributes (sorted, indented one level under the element),
//! `"text"` for character data, `<!-- … -->` for comments, `<!DOCTYPE …>` for the
//! doctype.

use crate::{Document, Namespace, NodeData, NodeId};

pub(crate) fn serialize(doc: &Document) -> String {
    let mut out = String::new();
    for child in doc.children(doc.root()) {
        write_node(doc, child, 0, &mut out);
    }
    out
}

fn indent(depth: usize, out: &mut String) {
    out.push_str("| ");
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_node(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
    indent(depth, out);
    match &doc.node(id).data {
        NodeData::Document => {} // never nested
        NodeData::Doctype { name, .. } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push_str(">\n");
        }
        NodeData::Element(e) => {
            out.push('<');
            match e.name.ns {
                Namespace::Html => {}
                Namespace::Svg => out.push_str("svg "),
                Namespace::MathMl => out.push_str("math "),
            }
            out.push_str(&e.name.local);
            out.push_str(">\n");

            let mut attrs: Vec<_> = e.attrs.iter().collect();
            attrs.sort_by(|a, b| a.name.cmp(&b.name));
            for attr in attrs {
                indent(depth + 1, out);
                out.push_str(&attr.name);
                out.push_str("=\"");
                out.push_str(&attr.value);
                out.push_str("\"\n");
            }

            for child in doc.children(id) {
                write_node(doc, child, depth + 1, out);
            }
        }
        NodeData::Text(t) => {
            out.push('"');
            out.push_str(t);
            out.push_str("\"\n");
        }
        NodeData::Comment(c) => {
            out.push_str("<!-- ");
            out.push_str(c);
            out.push_str(" -->\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{Attribute, Document, QualName};

    #[test]
    fn serializes_elements_attrs_and_text() {
        let mut doc = Document::new();
        let root = doc.root();
        let html = doc.create_element(QualName::html("html"), Vec::new());
        doc.append(root, html);
        let body = doc.create_element(QualName::html("body"), Vec::new());
        doc.append(html, body);
        let p = doc.create_element(
            QualName::html("p"),
            vec![Attribute::new("class", "lead"), Attribute::new("id", "x")],
        );
        doc.append(body, p);
        let text = doc.create_text("hi");
        doc.append(p, text);

        let expected = "\
| <html>
|   <body>
|     <p>
|       class=\"lead\"
|       id=\"x\"
|       \"hi\"
";
        assert_eq!(doc.serialize(), expected);
    }
}
