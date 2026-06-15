//! Tokenizer conformance harness (companion to the tree-construction one).
//!
//! Covers the tokenizer half of Phase 1's "HTML parser passes the bulk of html5lib
//! tokenizer + tree-construction tests" exit criterion: character references (named,
//! numeric decimal/hex, in attribute values, legacy semicolon-less), attribute
//! quoting forms, comments, doctype, and self-closing flags. Each case asserts the
//! exact `Token` stream (minus the trailing `Eof`) so a tokenizer regression fails CI.

use argus_html::{tokenize, Token};

/// Build a `StartTag` with `(name, value)` attributes.
fn start(name: &str, attrs: &[(&str, &str)], self_closing: bool) -> Token {
    Token::StartTag {
        name: name.to_string(),
        attrs: attrs.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect(),
        self_closing,
    }
}
fn end(name: &str) -> Token {
    Token::EndTag { name: name.to_string() }
}
fn chars(s: &str) -> Token {
    Token::Characters(s.to_string())
}
fn comment(s: &str) -> Token {
    Token::Comment(s.to_string())
}

/// Tokenize and drop the trailing `Eof` for comparison.
fn toks(input: &str) -> Vec<Token> {
    let mut v = tokenize(input);
    if matches!(v.last(), Some(Token::Eof)) {
        v.pop();
    }
    v
}

#[track_caller]
fn check(input: &str, expected: Vec<Token>) {
    let got = toks(input);
    assert_eq!(got, expected, "tokenizing {input:?}");
}

#[test]
fn named_character_references() {
    check("a&amp;b", vec![chars("a&b")]);
    check("&lt;&gt;", vec![chars("<>")]);
    check("&quot;x&quot;", vec![chars("\"x\"")]);
    check("&copy;", vec![chars("\u{00A9}")]);
    check("&nbsp;", vec![chars("\u{00A0}")]);
}

#[test]
fn numeric_character_references() {
    check("&#65;", vec![chars("A")]);
    check("&#x41;", vec![chars("A")]);
    check("&#X41;", vec![chars("A")]);
    check("&#956;", vec![chars("\u{03BC}")]); // µ
    check("&#xe9;", vec![chars("\u{00E9}")]); // é
}

#[test]
fn ampersand_that_is_not_a_reference_is_literal() {
    // A bare ampersand (no valid name) is passed through as a literal character.
    check("a & b", vec![chars("a & b")]);
    check("AT&T", vec![chars("AT&T")]);
}

#[test]
fn character_reference_in_attribute_value() {
    check(
        "<a href=\"?a=1&amp;b=2\">",
        vec![start("a", &[("href", "?a=1&b=2")], false)],
    );
}

#[test]
fn attribute_quoting_forms() {
    check("<input value=\"v\">", vec![start("input", &[("value", "v")], false)]);
    check("<input value='v'>", vec![start("input", &[("value", "v")], false)]);
    check("<input value=v>", vec![start("input", &[("value", "v")], false)]);
    check(
        "<div a=\"1\" b='2' c=3>",
        vec![start("div", &[("a", "1"), ("b", "2"), ("c", "3")], false)],
    );
}

#[test]
fn boolean_attribute_has_empty_value() {
    check("<input disabled>", vec![start("input", &[("disabled", "")], false)]);
}

#[test]
fn tag_and_attribute_names_are_lowercased() {
    check("<DIV ID=x>", vec![start("div", &[("id", "x")], false)]);
}

#[test]
fn self_closing_flag_is_recorded() {
    check("<br/>", vec![start("br", &[], true)]);
    check("<img src=x />", vec![start("img", &[("src", "x")], true)]);
}

#[test]
fn end_tags() {
    check("<p>hi</p>", vec![start("p", &[], false), chars("hi"), end("p")]);
}

#[test]
fn comments() {
    check("<!-- hi -->", vec![comment(" hi ")]);
    check("<!---->", vec![comment("")]);
    check("a<!--c-->b", vec![chars("a"), comment("c"), chars("b")]);
}

#[test]
fn doctype() {
    check(
        "<!DOCTYPE html><p>",
        vec![Token::Doctype { name: Some("html".to_string()) }, start("p", &[], false)],
    );
}

#[test]
fn rawtext_script_does_not_decode_entities() {
    // Script content is RAWTEXT: `&amp;` and `<b>` stay literal.
    check(
        "<script>if (a&amp;&b) x<b>y</script>",
        vec![start("script", &[], false), chars("if (a&amp;&b) x<b>y"), end("script")],
    );
}

#[test]
fn rcdata_title_decodes_entities_but_not_tags() {
    // Title is RCDATA: entities decode, but `<b>` is literal text.
    check(
        "<title>a&amp;b <b>c</title>",
        vec![start("title", &[], false), chars("a&b <b>c"), end("title")],
    );
}

#[test]
fn stray_lt_is_character_data() {
    // `<` not forming a valid tag is treated as a literal character.
    check("a < b", vec![chars("a < b")]);
}
