//! Phase 2 DOM bindings via a JS-side shim + post-execution reconciliation.
//!
//! kataan has no host-callback API, but it supports enough JavaScript (ES6
//! `Proxy` get/set traps, `Object.defineProperty`, `JSON`, closures) to model
//! `document`/`window` *entirely in JS*: a prelude defines proxies whose traps
//! record DOM mutations into an array. We seed that prelude with the real DOM's
//! id'd elements, run prelude + page scripts through kataan once, then read the
//! recorded ops back and apply them to the real [`Document`] before layout.
//!
//! This is a pragmatic subset — no live reflow, events, or timers — but it makes
//! `document.getElementById(id).textContent = …`, `.innerHTML`, `.style.x`,
//! `.className`, and `setAttribute` actually change the rendered page.

use argus_dom::{Attribute, Document, NodeData, NodeId, QualName};

/// The JS prelude defining `document`, `window`, and proxy element handles.
/// `__SEED__` is replaced with a JSON object of `{ id: { textContent, ... } }`.
const PRELUDE: &str = r#"
var __argus_ops = [];
var __seed = __SEED__;
// `tgt` is {kind:"id"|"sel", val:"..."}. Seeded reads are only available for ids.
function __argus_el(tgt) {
  var seed = (tgt.kind === "id") ? __seed[tgt.val] : null;
  return new Proxy({}, {
    set: function(t, k, v) {
      __argus_ops.push({op: "prop", tgt: tgt, key: k, value: "" + v});
      return true;
    },
    get: function(t, k) {
      if (k === "style") {
        return new Proxy({}, {
          set: function(t2, k2, v2) {
            __argus_ops.push({op: "style", tgt: tgt, key: k2, value: "" + v2});
            return true;
          },
          get: function(t2, k2) { return ""; }
        });
      }
      if (k === "__tgt") return tgt;
      if (k === "setAttribute") {
        return function(name, val) {
          __argus_ops.push({op: "attr", tgt: tgt, key: "" + name, value: "" + val});
        };
      }
      if (k === "getAttribute") {
        return function(name) { return (seed && seed[name] != null) ? seed[name] : null; };
      }
      if (k === "appendChild" || k === "append") {
        return function(child) {
          __argus_ops.push({op: "append", tgt: tgt, child: child ? child.__tgt : null});
          return child;
        };
      }
      if (k === "remove") {
        return function() { __argus_ops.push({op: "remove", tgt: tgt}); };
      }
      if (seed && seed[k] != null) return seed[k];
      return "";
    }
  });
}
var __newCount = 0;
var document = {
  getElementById: function(id) { return __argus_el({kind: "id", val: "" + id}); },
  querySelector: function(sel) { return __argus_el({kind: "sel", val: "" + sel}); },
  createElement: function(tag) {
    var nid = "n" + (__newCount++);
    __argus_ops.push({op: "create", nid: nid, tag: "" + tag});
    return __argus_el({kind: "new", val: nid});
  },
  write: function(s) { __argus_ops.push({op: "write", value: "" + s}); }
};
document.body = __argus_el({kind: "sel", val: "body"});
document.documentElement = __argus_el({kind: "sel", val: "html"});
var window = document.window = document;
"#;

/// Run a document's inline scripts and apply their DOM mutations in place.
/// Returns the console output (minus the internal ops line) for logging.
pub(crate) fn apply_scripts(doc: &mut Document) -> Option<String> {
    let scripts = collect_scripts(doc);
    if scripts.is_empty() {
        return None;
    }

    let seed = seed_json(doc);
    let mut src = PRELUDE.replace("__SEED__", &seed);
    for s in &scripts {
        src.push('\n');
        src.push_str(s);
    }
    // Emit the recorded ops on a sentinel line we can pick out of the console.
    src.push_str("\nconsole.log(\"\\u0001ARGUSOPS\" + JSON.stringify(__argus_ops));\n");

    let result = argus_script::run_script(&src).ok()?;
    let mut console = String::new();
    for line in result.console.lines() {
        if let Some(json) = line
            .strip_prefix('\u{1}')
            .and_then(|l| l.strip_prefix("ARGUSOPS"))
        {
            apply_ops(doc, json);
        } else {
            console.push_str(line);
            console.push('\n');
        }
    }
    Some(console)
}

/// Collect inline (`src`-less) `<script>` text in document order.
fn collect_scripts(doc: &Document) -> Vec<String> {
    fn walk(doc: &Document, id: NodeId, out: &mut Vec<String>) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if e.name.is_html("script") && e.attr("src").is_none() {
                let mut src = String::new();
                for c in doc.children(id) {
                    if let NodeData::Text(t) = &doc.node(c).data {
                        src.push_str(t);
                    }
                }
                if !src.trim().is_empty() {
                    out.push(src);
                }
            }
        }
        for c in doc.children(id) {
            walk(doc, c, out);
        }
    }
    let mut out = Vec::new();
    walk(doc, doc.root(), &mut out);
    out
}

/// Build the `__seed` JSON: each id'd element's text content and attributes,
/// so read-modify-write scripts see initial values.
fn seed_json(doc: &Document) -> String {
    let mut entries: Vec<String> = Vec::new();
    fn walk(doc: &Document, id: NodeId, entries: &mut Vec<String>) {
        if let NodeData::Element(e) = &doc.node(id).data {
            if let Some(eid) = e.attr("id") {
                let mut fields = vec![format!(
                    "\"textContent\":{}",
                    json_string(&text_content(doc, id))
                )];
                for a in &e.attrs {
                    fields.push(format!(
                        "{}:{}",
                        json_string(&a.name),
                        json_string(&a.value)
                    ));
                }
                entries.push(format!("{}:{{{}}}", json_string(eid), fields.join(",")));
            }
        }
        for c in doc.children(id) {
            walk(doc, c, entries);
        }
    }
    walk(doc, doc.root(), &mut entries);
    format!("{{{}}}", entries.join(","))
}

/// The concatenated text of an element's descendants.
fn text_content(doc: &Document, id: NodeId) -> String {
    let mut out = String::new();
    fn walk(doc: &Document, id: NodeId, out: &mut String) {
        match &doc.node(id).data {
            NodeData::Text(t) => out.push_str(t),
            _ => {
                for c in doc.children(id) {
                    walk(doc, c, out);
                }
            }
        }
    }
    walk(doc, id, &mut out);
    out
}

/// Apply the ops array (parsed from JSON) to the document.
fn apply_ops(doc: &mut Document, json: &str) {
    use std::collections::HashMap;
    let Some(Json::Arr(ops)) = parse_json(json) else {
        return;
    };
    // Detached elements created by `document.createElement`, keyed by synthetic id.
    let mut created: HashMap<String, NodeId> = HashMap::new();

    // Resolve a `tgt` descriptor ({kind, val}) to a node (created nodes included).
    let resolve =
        |doc: &Document, created: &HashMap<String, NodeId>, tgt: Option<&Json>| -> Option<NodeId> {
            let Some(Json::Obj(t)) = tgt else { return None };
            let f = |k: &str| t.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.as_str());
            match (f("kind"), f("val")) {
                (Some("id"), Some(v)) => find_by_id(doc, v),
                (Some("sel"), Some(v)) => find_by_selector(doc, v),
                (Some("new"), Some(v)) => created.get(v).copied(),
                _ => None,
            }
        };

    for op in ops {
        let Json::Obj(fields) = op else { continue };
        let get =
            |k: &str| -> Option<&Json> { fields.iter().find(|(n, _)| n == k).map(|(_, v)| v) };
        let op_kind = get("op").and_then(Json::as_str).unwrap_or("");
        let value = get("value")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let key = get("key").and_then(Json::as_str).unwrap_or("").to_string();

        if op_kind == "create" {
            let nid = get("nid").and_then(Json::as_str).unwrap_or("");
            let tag = get("tag").and_then(Json::as_str).unwrap_or("div");
            let node = doc.create_element(QualName::html(tag.to_ascii_lowercase()), vec![]);
            if !nid.is_empty() {
                created.insert(nid.to_string(), node);
            }
            continue;
        }

        let Some(node) = resolve(doc, &created, get("tgt")) else {
            continue;
        };
        match op_kind {
            "prop" => apply_prop(doc, node, &key, &value),
            "style" => merge_style(doc, node, &key, &value),
            "attr" => set_attribute(doc, node, &key, &value),
            "remove" => doc.detach(node),
            "append" => {
                if let Some(child) = resolve(doc, &created, get("child")) {
                    doc.append(node, child);
                }
            }
            _ => {}
        }
    }
}

/// The first element matching a CSS selector, in document order (`None` if the
/// selector fails to parse or nothing matches).
fn find_by_selector(doc: &Document, sel: &str) -> Option<NodeId> {
    let selectors = argus_css::selector::parse_selector_list(&argus_css::tokenizer::tokenize(sel));
    let selector = selectors.into_iter().next()?;
    fn walk(doc: &Document, n: NodeId, sel: &argus_css::Selector) -> Option<NodeId> {
        if matches!(&doc.node(n).data, NodeData::Element(_)) && argus_css::matches(doc, n, sel) {
            return Some(n);
        }
        for c in doc.children(n) {
            if let Some(found) = walk(doc, c, sel) {
                return Some(found);
            }
        }
        None
    }
    walk(doc, doc.root(), &selector)
}

/// Interpret a JS property assignment (`el.<key> = value`).
fn apply_prop(doc: &mut Document, node: NodeId, key: &str, value: &str) {
    match key {
        "textContent" | "innerText" => set_text_content(doc, node, value),
        "innerHTML" => set_inner_html(doc, node, value),
        "className" => set_attribute(doc, node, "class", value),
        _ => set_attribute(doc, node, key, value),
    }
}

/// The first element with `id` in document order.
fn find_by_id(doc: &Document, id: &str) -> Option<NodeId> {
    fn walk(doc: &Document, n: NodeId, id: &str) -> Option<NodeId> {
        if let NodeData::Element(e) = &doc.node(n).data {
            if e.attr("id") == Some(id) {
                return Some(n);
            }
        }
        for c in doc.children(n) {
            if let Some(found) = walk(doc, c, id) {
                return Some(found);
            }
        }
        None
    }
    walk(doc, doc.root(), id)
}

/// Detach every child of `node`.
fn clear_children(doc: &mut Document, node: NodeId) {
    let kids: Vec<NodeId> = doc.children(node).collect();
    for c in kids {
        doc.detach(c);
    }
}

fn set_text_content(doc: &mut Document, node: NodeId, text: &str) {
    clear_children(doc, node);
    let t = doc.create_text(text);
    doc.append(node, t);
}

fn set_inner_html(doc: &mut Document, node: NodeId, html: &str) {
    clear_children(doc, node);
    // Parse the fragment as a document and import its <body> children.
    let frag = argus_html::parse(html);
    let Some(body) = find_body(&frag) else { return };
    let kids: Vec<NodeId> = frag.children(body).collect();
    for c in kids {
        import_subtree(&frag, c, doc, node);
    }
}

fn find_body(doc: &Document) -> Option<NodeId> {
    fn walk(doc: &Document, n: NodeId) -> Option<NodeId> {
        if let NodeData::Element(e) = &doc.node(n).data {
            if e.name.is_html("body") {
                return Some(n);
            }
        }
        for c in doc.children(n) {
            if let Some(b) = walk(doc, c) {
                return Some(b);
            }
        }
        None
    }
    walk(doc, doc.root())
}

/// Deep-copy `src_node` from `src` into `dst` as a new child of `dst_parent`.
fn import_subtree(src: &Document, src_node: NodeId, dst: &mut Document, dst_parent: NodeId) {
    let new = match &src.node(src_node).data {
        NodeData::Element(e) => dst.create_element(
            QualName::html(e.name.local.clone()),
            e.attrs
                .iter()
                .map(|a| Attribute::new(a.name.clone(), a.value.clone()))
                .collect(),
        ),
        NodeData::Text(t) => dst.create_text(t.clone()),
        NodeData::Comment(t) => dst.create_comment(t.clone()),
        _ => return,
    };
    dst.append(dst_parent, new);
    let kids: Vec<NodeId> = src.children(src_node).collect();
    for c in kids {
        import_subtree(src, c, dst, new);
    }
}

/// Set or replace an attribute on an element.
fn set_attribute(doc: &mut Document, node: NodeId, name: &str, value: &str) {
    if let NodeData::Element(e) = doc.data_mut(node) {
        if let Some(a) = e.attrs.iter_mut().find(|a| &*a.name == name) {
            a.value = value.to_string();
        } else {
            e.attrs.push(Attribute::new(name, value));
        }
    }
}

/// Merge `prop: value` into the element's inline `style` attribute, converting a
/// camelCase JS property (e.g. `backgroundColor`) to its CSS form.
fn merge_style(doc: &mut Document, node: NodeId, prop: &str, value: &str) {
    let css_name = camel_to_kebab(prop);
    let existing = doc
        .node(node)
        .as_element()
        .and_then(|e| e.attr("style"))
        .unwrap_or("")
        .to_string();
    let mut decls: Vec<(String, String)> = Vec::new();
    for decl in existing.split(';') {
        if let Some((k, v)) = decl.split_once(':') {
            let k = k.trim();
            if !k.is_empty() && k != css_name {
                decls.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    decls.push((css_name, value.trim().to_string()));
    let merged = decls
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("; ");
    set_attribute(doc, node, "style", &merged);
}

fn camel_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for ch in s.chars() {
        if ch.is_ascii_uppercase() {
            out.push('-');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

// ---- Minimal JSON --------------------------------------------------------

/// A parsed JSON value (only what the ops payload needs). `Bool`/`Num` are parsed
/// for completeness but the ops payload only reads strings.
#[allow(dead_code)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Escape a string as a JSON string literal (with surrounding quotes).
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn parse_json(s: &str) -> Option<Json> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let v = parse_value(&chars, &mut i)?;
    Some(v)
}

fn skip_ws(c: &[char], i: &mut usize) {
    while *i < c.len() && c[*i].is_whitespace() {
        *i += 1;
    }
}

fn parse_value(c: &[char], i: &mut usize) -> Option<Json> {
    skip_ws(c, i);
    match c.get(*i)? {
        '"' => parse_string(c, i).map(Json::Str),
        '[' => parse_array(c, i),
        '{' => parse_object(c, i),
        't' => parse_literal(c, i, "true", Json::Bool(true)),
        'f' => parse_literal(c, i, "false", Json::Bool(false)),
        'n' => parse_literal(c, i, "null", Json::Null),
        _ => parse_number(c, i),
    }
}

fn parse_literal(c: &[char], i: &mut usize, word: &str, val: Json) -> Option<Json> {
    for ch in word.chars() {
        if c.get(*i) != Some(&ch) {
            return None;
        }
        *i += 1;
    }
    Some(val)
}

fn parse_string(c: &[char], i: &mut usize) -> Option<String> {
    if c.get(*i) != Some(&'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    while let Some(&ch) = c.get(*i) {
        *i += 1;
        match ch {
            '"' => return Some(out),
            '\\' => {
                let esc = *c.get(*i)?;
                *i += 1;
                match esc {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            code = code * 16 + c.get(*i)?.to_digit(16)?;
                            *i += 1;
                        }
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return None,
                }
            }
            _ => out.push(ch),
        }
    }
    None
}

fn parse_array(c: &[char], i: &mut usize) -> Option<Json> {
    *i += 1; // '['
    let mut out = Vec::new();
    skip_ws(c, i);
    if c.get(*i) == Some(&']') {
        *i += 1;
        return Some(Json::Arr(out));
    }
    loop {
        out.push(parse_value(c, i)?);
        skip_ws(c, i);
        match c.get(*i) {
            Some(',') => {
                *i += 1;
            }
            Some(']') => {
                *i += 1;
                return Some(Json::Arr(out));
            }
            _ => return None,
        }
    }
}

fn parse_object(c: &[char], i: &mut usize) -> Option<Json> {
    *i += 1; // '{'
    let mut out = Vec::new();
    skip_ws(c, i);
    if c.get(*i) == Some(&'}') {
        *i += 1;
        return Some(Json::Obj(out));
    }
    loop {
        skip_ws(c, i);
        let key = parse_string(c, i)?;
        skip_ws(c, i);
        if c.get(*i) != Some(&':') {
            return None;
        }
        *i += 1;
        let val = parse_value(c, i)?;
        out.push((key, val));
        skip_ws(c, i);
        match c.get(*i) {
            Some(',') => {
                *i += 1;
            }
            Some('}') => {
                *i += 1;
                return Some(Json::Obj(out));
            }
            _ => return None,
        }
    }
}

fn parse_number(c: &[char], i: &mut usize) -> Option<Json> {
    let start = *i;
    while let Some(&ch) = c.get(*i) {
        if ch.is_ascii_digit() || matches!(ch, '-' | '+' | '.' | 'e' | 'E') {
            *i += 1;
        } else {
            break;
        }
    }
    let s: String = c[start..*i].iter().collect();
    s.parse().ok().map(Json::Num)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(doc: &Document, id: &str) -> String {
        let node = find_by_id(doc, id).expect("element");
        text_content(doc, node)
    }
    fn attr_of(doc: &Document, id: &str, name: &str) -> Option<String> {
        find_by_id(doc, id).and_then(|n| {
            doc.node(n)
                .as_element()
                .and_then(|e| e.attr(name))
                .map(String::from)
        })
    }

    #[test]
    fn set_text_content_via_get_element_by_id() {
        let mut doc = argus_html::parse(
            "<div id=out>old</div>\
             <script>document.getElementById('out').textContent = 'new text';</script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "out"), "new text");
    }

    #[test]
    fn read_modify_write_uses_seeded_value() {
        // The seeded textContent lets a script read the old value and append.
        let mut doc = argus_html::parse(
            "<span id=c>5</span>\
             <script>var e=document.getElementById('c'); e.textContent = e.textContent + '0';</script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "c"), "50");
    }

    #[test]
    fn style_and_attribute_and_class_mutations() {
        let mut doc = argus_html::parse(
            "<div id=b>x</div>\
             <script>\
               var e=document.getElementById('b');\
               e.style.color = 'red';\
               e.style.backgroundColor = 'blue';\
               e.className = 'active';\
               e.setAttribute('data-k', 'v');\
             </script>",
        );
        apply_scripts(&mut doc);
        let style = attr_of(&doc, "b", "style").unwrap_or_default();
        assert!(style.contains("color: red"), "style: {style}");
        assert!(style.contains("background-color: blue"), "style: {style}");
        assert_eq!(attr_of(&doc, "b", "class").as_deref(), Some("active"));
        assert_eq!(attr_of(&doc, "b", "data-k").as_deref(), Some("v"));
    }

    #[test]
    fn inner_html_replaces_children() {
        let mut doc = argus_html::parse(
            "<div id=h>old</div>\
             <script>document.getElementById('h').innerHTML = '<b>bold</b> and <i>italic</i>';</script>",
        );
        apply_scripts(&mut doc);
        let node = find_by_id(&doc, "h").unwrap();
        // The text content is now the fragment's text; a <b> child exists.
        assert_eq!(text_of(&doc, "h"), "bold and italic");
        let has_b = doc
            .children(node)
            .any(|c| matches!(&doc.node(c).data, NodeData::Element(e) if e.name.is_html("b")));
        assert!(has_b, "innerHTML should create a <b> element");
    }

    #[test]
    fn query_selector_by_class_and_tag() {
        let mut doc = argus_html::parse(
            "<p class=\"intro\" id=\"p1\">a</p><p id=\"p2\">b</p>\
             <script>\
               document.querySelector('.intro').textContent = 'X';\
               document.querySelector('p:nth-child(2)').textContent = 'Y';\
             </script>",
        );
        apply_scripts(&mut doc);
        assert_eq!(text_of(&doc, "p1"), "X");
        // The 2nd <p> in document order is p2.
        assert_eq!(text_of(&doc, "p2"), "Y");
    }

    #[test]
    fn create_element_and_append_child() {
        let mut doc = argus_html::parse(
            "<div id=\"root\"></div>\
             <script>\
               var p = document.createElement('p');\
               p.textContent = 'created node';\
               p.className = 'made';\
               document.getElementById('root').appendChild(p);\
             </script>",
        );
        apply_scripts(&mut doc);
        let root = find_by_id(&doc, "root").unwrap();
        let child = doc.children(root).next().expect("appended child");
        let e = doc.node(child).as_element().expect("element");
        assert!(e.name.is_html("p"));
        assert_eq!(e.attr("class"), Some("made"));
        assert_eq!(text_content(&doc, child), "created node");
    }

    #[test]
    fn no_scripts_is_noop() {
        let mut doc = argus_html::parse("<p id=p>hi</p>");
        assert!(apply_scripts(&mut doc).is_none());
        assert_eq!(text_of(&doc, "p"), "hi");
    }
}
