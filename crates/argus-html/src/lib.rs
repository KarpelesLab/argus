//! HTML parsing (Layer 2): bytes/text → [`argus_dom::Document`].
//!
//! The [`tokenizer`] turns text into a token stream; the tree builder (added next)
//! assembles those into a DOM. See `docs/subsystems/dom.md`.

mod entities;
pub mod tokenizer;

pub use tokenizer::{tokenize, Token};
