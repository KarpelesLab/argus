//! Fuzz target: the HTML tokenizer + tree builder must never panic, and the DOM
//! it produces must always serialize cleanly.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let html = String::from_utf8_lossy(data);
    let doc = argus_html::parse(&html);
    let _ = doc.serialize();
});
