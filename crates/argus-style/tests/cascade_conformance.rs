//! CSS cascade conformance harness.
//!
//! Exercises the resolution order of the cascade (Phase 1's "CSS parser + selector
//! matching + cascade … for that subset" criterion): specificity ordering, source
//! order tiebreaks, `!important`, inline styles, the universal selector, and
//! inheritance. Each case asserts the winning computed value, so a cascade
//! regression fails CI.

use argus_css::parse_stylesheet;
use argus_dom::{Attribute, Document, NodeId, QualName};
use argus_geometry::Color;
use argus_style::{computed_style, ComputedStyle};

/// Append an element with attributes under `parent`, return its id.
fn el(doc: &mut Document, parent: NodeId, tag: &str, attrs: &[(&str, &str)]) -> NodeId {
    let a = attrs.iter().map(|(n, v)| Attribute::new(*n, *v)).collect();
    let id = doc.create_element(QualName::html(tag), a);
    doc.append(parent, id);
    id
}

const GREEN: Color = Color { r: 0, g: 128, b: 0, a: 255 };
const BLUE: Color = Color { r: 0, g: 0, b: 255, a: 255 };

#[test]
fn specificity_id_beats_class_beats_tag() {
    let mut doc = Document::new();
    let root = doc.root();
    let p = el(&mut doc, root, "p", &[("id", "x"), ("class", "c")]);
    // Authored in tag/class/id order; the id rule must win despite coming first-ish.
    let css = parse_stylesheet("#x { color: blue } .c { color: green } p { color: red }");
    let cs = computed_style(&doc, p, &ComputedStyle::initial(), &css);
    assert_eq!(cs.color, BLUE, "id selector should win");
}

#[test]
fn important_beats_higher_specificity() {
    let mut doc = Document::new();
    let root = doc.root();
    let p = el(&mut doc, root, "p", &[("id", "x"), ("class", "c")]);
    let css = parse_stylesheet("#x { color: blue } .c { color: green !important }");
    let cs = computed_style(&doc, p, &ComputedStyle::initial(), &css);
    assert_eq!(cs.color, GREEN, "!important should beat a more specific normal rule");
}

#[test]
fn inline_style_beats_author_rule() {
    let mut doc = Document::new();
    let root = doc.root();
    let p = el(&mut doc, root, "p", &[("id", "x"), ("style", "color: green")]);
    let css = parse_stylesheet("#x { color: blue }");
    let cs = computed_style(&doc, p, &ComputedStyle::initial(), &css);
    assert_eq!(cs.color, GREEN, "a normal inline style should beat a normal author rule");
}

#[test]
fn important_author_beats_inline() {
    let mut doc = Document::new();
    let root = doc.root();
    let p = el(&mut doc, root, "p", &[("id", "x"), ("style", "color: green")]);
    let css = parse_stylesheet("#x { color: blue !important }");
    let cs = computed_style(&doc, p, &ComputedStyle::initial(), &css);
    assert_eq!(cs.color, BLUE, "an !important author rule should beat a normal inline style");
}

#[test]
fn source_order_breaks_equal_specificity() {
    let mut doc = Document::new();
    let root = doc.root();
    let p = el(&mut doc, root, "p", &[("class", "a b")]);
    // Two single-class rules of equal specificity: the later one wins.
    let css = parse_stylesheet(".a { color: red } .b { color: green }");
    let cs = computed_style(&doc, p, &ComputedStyle::initial(), &css);
    assert_eq!(cs.color, GREEN, "equal specificity → last declared wins");
}

#[test]
fn universal_selector_has_lowest_specificity() {
    let mut doc = Document::new();
    let root = doc.root();
    let p = el(&mut doc, root, "p", &[]);
    let css = parse_stylesheet("* { color: red } p { color: blue }");
    let cs = computed_style(&doc, p, &ComputedStyle::initial(), &css);
    assert_eq!(cs.color, BLUE, "a type selector should beat the universal selector");
}

#[test]
fn color_inherits_but_border_does_not() {
    let mut doc = Document::new();
    let root = doc.root();
    let parent = el(&mut doc, root, "div", &[("id", "p")]);
    let child = el(&mut doc, parent, "span", &[]);
    let css = parse_stylesheet("#p { color: green; border: 5px solid red }");
    let parent_cs = computed_style(&doc, parent, &ComputedStyle::initial(), &css);
    let child_cs = computed_style(&doc, child, &parent_cs, &css);
    // color is inherited; the border (not inherited) stays at its initial 0.
    assert_eq!(child_cs.color, GREEN, "color should inherit to the child");
    assert_eq!(child_cs.border.top, 0.0, "border must not inherit");
    assert_eq!(parent_cs.border.top, 5.0, "parent keeps its own border");
}
