//! HTML parsing (Layer 2): bytes/text → [`argus_dom::Document`].
//!
//! The [`tokenizer`] turns text into a token stream; the tree builder (added next)
//! assembles those into a DOM. See `docs/subsystems/dom.md`.

mod entities;
pub mod tokenizer;
mod tree_builder;

pub use tokenizer::{tokenize, Token};
pub use tree_builder::parse;
