//! The **parser** layer of the Mission browser — the "translator".
//!
//! Constitutional boundary: the parser receives already-fetched data and turns it into
//! structure. It must never make network calls itself (no reaching into the network module)
//! — an architectural boundary of the design.
//!
//! Three pure, zero-dependency stages, each in its own submodule:
//!   1. [`tokenizer`] — raw HTML → a flat stream of [`Token`]s, entities decoded.
//!   2. [`dom`]       — tokens folded into a nested [`Node`] tree, plus the `find_*` queries.
//!   3. [`css`]       — the [`select`] CSS selector engine over that tree.
//!
//! The submodules layer strictly downward (css → dom → tokenizer); this module owns only the
//! shared [`Attrs`] vocabulary and re-exports the public surface, so `mission::parser::X` is
//! unchanged by the split. It carries no logic of its own.

mod css;
mod dom;
mod tokenizer;

/// An element's attributes, as ordered `(name, value)` pairs in source order. Only the first
/// occurrence of a repeated name is kept (per HTML5). A boolean attribute (`<input disabled>`)
/// has an empty value.
pub type Attrs = Vec<(String, String)>;

pub use self::css::select;
pub use self::dom::{Node, find, find_all, find_by_id, find_by_class, parse};
pub use self::tokenizer::{Token, attr, tokenize};
