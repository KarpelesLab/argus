//! html5lib-format tree-construction conformance harness.
//!
//! This drives `argus_html::parse` against test cases written in the upstream
//! [html5lib-tests](https://github.com/html5lib/html5lib-tests) `.dat` format and
//! compares the resulting tree against the expected serialization. It is the
//! conformance backbone for Phase 1's "HTML parser passes the bulk of html5lib …
//! tests" exit criterion: the harness reports the pass rate over the full curated
//! set and hard-asserts the cases the parser is expected to handle today, so new
//! regressions fail CI while genuinely-unimplemented corners are tracked, not faked.
//!
//! The `.dat` format: each case is `#data` (the input), `#errors` (ignored here),
//! optionally `#document-fragment` (fragment context — skipped), then `#document`
//! (the expected tree, one node per `| `-prefixed line, two spaces per depth).

use argus_dom::{Document, Namespace, NodeData, NodeId};

/// Serialize a parsed document in the html5lib tree-construction format.
fn serialize_html5lib(doc: &Document) -> String {
    fn walk(doc: &Document, id: NodeId, depth: usize, out: &mut String) {
        let indent = "  ".repeat(depth);
        match &doc.node(id).data {
            NodeData::Document => {}
            NodeData::Doctype { name, .. } => {
                out.push_str(&format!("| {indent}<!DOCTYPE {name}>\n"));
            }
            NodeData::Element(e) => {
                let prefix = match e.name.ns {
                    Namespace::Html => "",
                    Namespace::Svg => "svg ",
                    Namespace::MathMl => "math ",
                };
                out.push_str(&format!("| {indent}<{prefix}{}>\n", e.name.local));
                // Attributes are serialized sorted by name, indented one level deeper.
                let mut attrs: Vec<_> = e.attrs.iter().collect();
                attrs.sort_by(|a, b| a.name.cmp(&b.name));
                for a in attrs {
                    out.push_str(&format!("| {indent}  {}=\"{}\"\n", a.name, a.value));
                }
            }
            NodeData::Text(t) => out.push_str(&format!("| {indent}\"{t}\"\n")),
            NodeData::Comment(t) => out.push_str(&format!("| {indent}<!-- {t} -->\n")),
        }
        for c in doc.children(id) {
            walk(doc, c, depth + 1, out);
        }
    }
    let mut out = String::new();
    // The document's element/text/etc. children render at depth 0.
    for c in doc.children(doc.root()) {
        walk(doc, c, 0, &mut out);
    }
    out
}

/// One parsed `.dat` case.
struct Case {
    data: String,
    expected: String,
    is_fragment: bool,
}

/// Parse the curated `.dat` blob into cases.
fn parse_dat(blob: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    let lines: Vec<&str> = blob.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i] != "#data" {
            i += 1;
            continue;
        }
        i += 1;
        // #data: everything up to the next section marker.
        let mut data = Vec::new();
        while i < lines.len() && !is_marker(lines[i]) {
            data.push(lines[i]);
            i += 1;
        }
        let mut is_fragment = false;
        let mut expected = Vec::new();
        // Consume sections until the next #data (or EOF).
        while i < lines.len() && lines[i] != "#data" {
            match lines[i] {
                "#document-fragment" => {
                    is_fragment = true;
                    i += 1;
                }
                "#document" => {
                    i += 1;
                    while i < lines.len() && lines[i] != "#data" && lines[i].starts_with("| ") {
                        expected.push(lines[i]);
                        i += 1;
                    }
                }
                _ => i += 1,
            }
        }
        cases.push(Case {
            data: data.join("\n"),
            expected: expected.join("\n"),
            is_fragment,
        });
    }
    cases
}

fn is_marker(line: &str) -> bool {
    matches!(
        line,
        "#errors" | "#document" | "#document-fragment" | "#new-errors" | "#script-off" | "#script-on"
    )
}

#[test]
fn html5lib_tree_construction_conformance() {
    let cases = parse_dat(CASES);
    let mut total = 0;
    let mut passed = 0;
    let mut failures: Vec<(String, String, String)> = Vec::new();

    for case in &cases {
        if case.is_fragment {
            continue; // fragment parsing (innerHTML context) not modeled here
        }
        total += 1;
        let doc = argus_html::parse(&case.data);
        let got = serialize_html5lib(&doc);
        let got_trimmed = got.trim_end_matches('\n');
        if got_trimmed == case.expected {
            passed += 1;
        } else {
            failures.push((case.data.clone(), case.expected.clone(), got_trimmed.to_string()));
        }
    }

    eprintln!("html5lib tree-construction: {passed}/{total} cases passed");
    for (data, want, got) in &failures {
        eprintln!("\n--- FAIL ---\ninput: {data:?}\nexpected:\n{want}\ngot:\n{got}");
    }

    // The parser passes the full curated set today; require it to stay at 100% so a
    // regression in any covered behavior fails CI. New aspirational cases that aren't
    // yet handled should be added with their own relaxed gate, not by lowering this.
    assert_eq!(
        passed, total,
        "html5lib pass rate {passed}/{total} regressed below 100%"
    );
}

/// Curated html5lib-format tree-construction cases (core behaviors of the
/// tokenizer + tree builder). Drawn from the canonical upstream suite's coverage.
const CASES: &str = r####"#data
<p>Hello</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "Hello"

#data
Just text
#errors
#document
| <html>
|   <head>
|   <body>
|     "Just text"

#data
<!DOCTYPE html><html><head></head><body><p>x</p></body></html>
#errors
#document
| <!DOCTYPE html>
| <html>
|   <head>
|   <body>
|     <p>
|       "x"

#data
<div id="a" class="b">content</div>
#errors
#document
| <html>
|   <head>
|   <body>
|     <div>
|       class="b"
|       id="a"
|       "content"

#data
<p>One<p>Two
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "One"
|     <p>
|       "Two"

#data
<ul><li>a<li>b</ul>
#errors
#document
| <html>
|   <head>
|   <body>
|     <ul>
|       <li>
|         "a"
|       <li>
|         "b"

#data
<b>bold</b> and <i>italic</i>
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       "bold"
|     " and "
|     <i>
|       "italic"

#data
<br>
#errors
#document
| <html>
|   <head>
|   <body>
|     <br>

#data
<img src="x.png" alt="pic">
#errors
#document
| <html>
|   <head>
|   <body>
|     <img>
|       alt="pic"
|       src="x.png"

#data
<!-- a comment --><p>after</p>
#errors
#document
| <!--  a comment  -->
| <html>
|   <head>
|   <body>
|     <p>
|       "after"

#data
<a href="/x">link</a>
#errors
#document
| <html>
|   <head>
|   <body>
|     <a>
|       href="/x"
|       "link"

#data
<head><title>T</title></head><body>B</body>
#errors
#document
| <html>
|   <head>
|     <title>
|       "T"
|   <body>
|     "B"

#data
<span>a<span>b</span>c</span>
#errors
#document
| <html>
|   <head>
|   <body>
|     <span>
|       "a"
|       <span>
|         "b"
|       "c"

#data
<p>a&amp;b</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "a&b"

#data
<div><p>x</div>y
#errors
#document
| <html>
|   <head>
|   <body>
|     <div>
|       <p>
|         "x"
|     "y"

#data
<h1>Title</h1><h2>Sub</h2>
#errors
#document
| <html>
|   <head>
|   <body>
|     <h1>
|       "Title"
|     <h2>
|       "Sub"

#data
<input type="text" value="v">
#errors
#document
| <html>
|   <head>
|   <body>
|     <input>
|       type="text"
|       value="v"

#data
<p>line1<br>line2</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "line1"
|       <br>
|       "line2"

#data
<section><article>x</article></section>
#errors
#document
| <html>
|   <head>
|   <body>
|     <section>
|       <article>
|         "x"

#data
<p>café</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "café"

#data
<table><tr><td>cell</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "cell"

#data
<p><div>x</div>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|     <div>
|       "x"

#data
<title><b>not bold</b></title>
#errors
#document
| <html>
|   <head>
|     <title>
|       "<b>not bold</b>"
|   <body>

#data
<script>if (a<b) {}</script>
#errors
#document
| <html>
|   <head>
|     <script>
|       "if (a<b) {}"
|   <body>

#data
<hr><hr>
#errors
#document
| <html>
|   <head>
|   <body>
|     <hr>
|     <hr>

#data
<style>.a{color:red}</style>
#errors
#document
| <html>
|   <head>
|     <style>
|       ".a{color:red}"
|   <body>

#data
<dl><dt>term<dd>def</dl>
#errors
#document
| <html>
|   <head>
|   <body>
|     <dl>
|       <dt>
|         "term"
|       <dd>
|         "def"

#data
<table><caption>cap</caption><tr><td>c</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <caption>
|         "cap"
|       <tbody>
|         <tr>
|           <td>
|             "c"

#data
<p>a</p><!--c-->
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "a"
|     <!-- c -->

#data
<table><td>x</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "x"

#data
<select><option>a<option>b</select>
#errors
#document
| <html>
|   <head>
|   <body>
|     <select>
|       <option>
|         "a"
|       <option>
|         "b"

#data
<textarea>
hi</textarea>
#errors
#document
| <html>
|   <head>
|   <body>
|     <textarea>
|       "hi"

#data
<button>a<button>b
#errors
#document
| <html>
|   <head>
|   <body>
|     <button>
|       "a"
|     <button>
|       "b"

#data
<table>stray<tr><td>c</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     "stray"
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "c"

#data
<meta charset="utf-8"><title>T</title>
#errors
#document
| <html>
|   <head>
|     <meta>
|       charset="utf-8"
|     <title>
|       "T"
|   <body>

#data
<ul><li>a<ul><li>b</ul></li></ul>
#errors
#document
| <html>
|   <head>
|   <body>
|     <ul>
|       <li>
|         "a"
|         <ul>
|           <li>
|             "b"

#data
<table><thead><tr><th>H</th></thead><tbody><tr><td>D</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <thead>
|         <tr>
|           <th>
|             "H"
|       <tbody>
|         <tr>
|           <td>
|             "D"

#data
<image src="x.png">
#errors
#document
| <html>
|   <head>
|   <body>
|     <img>
|       src="x.png"

#data
<div id="a" id="b">x</div>
#errors
#document
| <html>
|   <head>
|   <body>
|     <div>
|       id="a"
|       "x"

#data
<p><b><i>deep</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       <b>
|         <i>
|           "deep"

#data
</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>

#data
a</br>b
#errors
#document
| <html>
|   <head>
|   <body>
|     "a"
|     <br>
|     "b"

#data
<table><colgroup><col><col></colgroup><tr><td>c</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <colgroup>
|         <col>
|         <col>
|       <tbody>
|         <tr>
|           <td>
|             "c"

#data
<svg><circle/><rect/></svg>
#errors
#document
| <html>
|   <head>
|   <body>
|     <svg svg>
|       <svg circle>
|       <svg rect>

#data
<p>before<svg><g><text>t</text></g></svg>after</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "before"
|       <svg svg>
|         <svg g>
|           <svg text>
|             "t"
|       "after"

#data
<table><tr><td>a<td>b<tr><td>c</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "a"
|           <td>
|             "b"
|         <tr>
|           <td>
|             "c"

#data
<table><caption>cap<tr><td>c</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <caption>
|         "cap"
|       <tbody>
|         <tr>
|           <td>
|             "c"

#data
<p>x<search>y</search>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "x"
|     <search>
|       "y"

#data
<body class="a"><body id="x">hi
#errors
#document
| <html>
|   <head>
|   <body>
|     class="a"
|     id="x"
|     "hi"

#data
<p>a<table>b</table>c
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "a"
|     "b"
|     <table>
|     "c"

#data
<p>1<b>2</p>3
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "1"
|       <b>
|         "2"
|     <b>
|       "3"

#data
<p>1<b>2</p><i>3</i>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "1"
|       <b>
|         "2"
|     <b>
|       <i>
|         "3"

#data
<h1>a<h2>b</h2>
#errors
#document
| <html>
|   <head>
|   <body>
|     <h1>
|       "a"
|     <h2>
|       "b"

#data
<table><tr><th>h<td>d</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <tbody>
|         <tr>
|           <th>
|             "h"
|           <td>
|             "d"

#data
<b>1<p>2</b>3
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       "1"
|     <p>
|       <b>
|         "2"
|       "3"

#data
<b><i>1</b>2
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       <i>
|         "1"
|     <i>
|       "2"

#data
<a>1<div>2<div>3</a>4
#errors
#document
| <html>
|   <head>
|   <body>
|     <a>
|       "1"
|     <div>
|       <a>
|         "2"
|       <div>
|         <a>
|           "3"
|         "4"

#data
<font><p>x</font>y
#errors
#document
| <html>
|   <head>
|   <body>
|     <font>
|     <p>
|       <font>
|         "x"
|       "y"

#data
<b><b>1</b>2
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       <b>
|         "1"
|       "2"

#data
<a href="1">x<a href="2">y
#errors
#document
| <html>
|   <head>
|   <body>
|     <a>
|       href="1"
|       "x"
|     <a>
|       href="2"
|       "y"

#data
<table><td>1<tr><td>2</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "1"
|         <tr>
|           <td>
|             "2"

#data
<table><tbody><tr><td>1</tbody><tbody><tr><td>2</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "1"
|       <tbody>
|         <tr>
|           <td>
|             "2"

#data
<a href="https://x">link</a>
#errors
#document
| <html>
|   <head>
|   <body>
|     <a>
|       href="https://x"
|       "link"

#data
<!DOCTYPE html><p>x</p>
#errors
#document
| <!DOCTYPE html>
| <html>
|   <head>
|   <body>
|     <p>
|       "x"

#data
<div><p>1<p>2</div>
#errors
#document
| <html>
|   <head>
|   <body>
|     <div>
|       <p>
|         "1"
|       <p>
|         "2"

#data
<b>1<i>2</i>3</b>
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       "1"
|       <i>
|         "2"
|       "3"

#data
<select><option>a<option>b</select>
#errors
#document
| <html>
|   <head>
|   <body>
|     <select>
|       <option>
|         "a"
|       <option>
|         "b"

#data
<svg><circle></svg>
#errors
#document
| <html>
|   <head>
|   <body>
|     <svg svg>
|       <svg circle>

#data
<table>x<td>y</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     "x"
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "y"

#data
<ul><li>1<li>2</ul>
#errors
#document
| <html>
|   <head>
|   <body>
|     <ul>
|       <li>
|         "1"
|       <li>
|         "2"

#data
<dl><dt>a<dd>b</dl>
#errors
#document
| <html>
|   <head>
|   <body>
|     <dl>
|       <dt>
|         "a"
|       <dd>
|         "b"

#data
<b>1<p>2</b>3</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       "1"
|     <p>
|       <b>
|         "2"
|       "3"

#data
<p>a<!--c-->b</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "a"
|       <!-- c -->
|       "b"

#data
<ul><li>1<li>2</ul>
#errors
#document
| <html>
|   <head>
|   <body>
|     <ul>
|       <li>
|         "1"
|       <li>
|         "2"

#data
caf&eacute; &amp; tea
#errors
#document
| <html>
|   <head>
|   <body>
|     "café & tea"

#data
<table><tr><td>x</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "x"

#data
<a href="/x"><b>link</b></a>
#errors
#document
| <html>
|   <head>
|   <body>
|     <a>
|       href="/x"
|       <b>
|         "link"

#data
<p>a<br>b</p>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "a"
|       <br>
|       "b"

#data
<div id=main>x</div>
#errors
#document
| <html>
|   <head>
|   <body>
|     <div>
|       id="main"
|       "x"

#data
<select><option>a<option>b</select>
#errors
#document
| <html>
|   <head>
|   <body>
|     <select>
|       <option>
|         "a"
|       <option>
|         "b"

#data
<title>a<b>c</title>
#errors
#document
| <html>
|   <head>
|     <title>
|       "a<b>c"
|   <body>

#data
<p>a<div>b</div>
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "a"
|     <div>
|       "b"

#data
<textarea>
x</textarea>
#errors
#document
| <html>
|   <head>
|   <body>
|     <textarea>
|       "x"

#data
<pre>
hi</pre>
#errors
#document
| <html>
|   <head>
|   <body>
|     <pre>
|       "hi"

#data
<head><meta charset="utf-8"></head><body>y</body>
#errors
#document
| <html>
|   <head>
|     <meta>
|       charset="utf-8"
|   <body>
|     "y"

#data
<form action="/post"><input name="q"></form>
#errors
#document
| <html>
|   <head>
|   <body>
|     <form>
|       action="/post"
|       <input>
|         name="q"

#data
<table><caption>C</caption><tr><td>x</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <caption>
|         "C"
|       <tbody>
|         <tr>
|           <td>
|             "x"

#data
<table><colgroup><col></colgroup><tr><td>x</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     <table>
|       <colgroup>
|         <col>
|       <tbody>
|         <tr>
|           <td>
|             "x"

#data
<div class="a&amp;b">x</div>
#errors
#document
| <html>
|   <head>
|   <body>
|     <div>
|       class="a&b"
|       "x"

#data
<script>var x = 1 < 2;</script>
#errors
#document
| <html>
|   <head>
|     <script>
|       "var x = 1 < 2;"
|   <body>

#data
<table>text<tr><td>x</table>
#errors
#document
| <html>
|   <head>
|   <body>
|     "text"
|     <table>
|       <tbody>
|         <tr>
|           <td>
|             "x"

#data
<!DOCTYPE html>
#errors
#document
| <!DOCTYPE html>
| <html>
|   <head>
|   <body>

#data
<p>1<p>2<p>3
#errors
#document
| <html>
|   <head>
|   <body>
|     <p>
|       "1"
|     <p>
|       "2"
|     <p>
|       "3"

#data
<b>bold <i>both</i> still</b>
#errors
#document
| <html>
|   <head>
|   <body>
|     <b>
|       "bold "
|       <i>
|         "both"
|       " still"
"####;
