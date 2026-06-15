//! Fuzz target: the CSS tokenizer + parser + value parsers must never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let css = String::from_utf8_lossy(data);
    let sheet = argus_css::parse_stylesheet(&css);
    for rule in &sheet.rules {
        // Selector machinery (specificity / pseudo-element) must not panic.
        for sel in &rule.selectors {
            let _ = sel.specificity();
            let _ = sel.pseudo_element();
        }
        for d in &rule.declarations {
            let _ = argus_css::parse_color(&d.value);
            let _ = argus_css::parse_length(&d.value);
        }
    }
    // The inline-style declaration-block path is a separate parser.
    let _ = argus_css::parse_declaration_block(&css);
});
