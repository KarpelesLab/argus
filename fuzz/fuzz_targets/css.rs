//! Fuzz target: the CSS tokenizer + parser + value parsers must never panic.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let css = String::from_utf8_lossy(data);
    let sheet = argus_css::parse_stylesheet(&css);
    for rule in &sheet.rules {
        for d in &rule.declarations {
            let _ = argus_css::parse_color(&d.value);
            let _ = argus_css::parse_length(&d.value);
        }
    }
});
