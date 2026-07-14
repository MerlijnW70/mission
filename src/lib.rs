//! **Mission** — a from-scratch, zero-dependency text-mode HTML browser and CSS query engine.
//!
//! Mission turns raw HTML into readable text or structured data, with no runtime dependencies
//! (the `[dependencies]` table is empty), no network, and no graphics stack. The pipeline is
//! `HTML → tokenize → parse (DOM) → query (CSS) → render`:
//!
//! - [`parser::parse`] builds a [`parser::Node`] tree from HTML (lenient, browser-like recovery).
//! - [`parser::select`] queries it with a near-complete CSS selector engine.
//! - [`renderer::render_text`] lays the tree out as readable text; [`json`] serializes a selection.
//!
//! ```
//! use mission::parser::{parse, select};
//! use mission::renderer::render_text;
//!
//! let dom = parse("<article><h1>Title</h1><p>See <a href=\"/x\">the docs</a>.</p></article>");
//! assert_eq!(render_text(&dom), "# Title\nSee the docs [/x].");
//!
//! let hrefs: Vec<&str> = select(&dom, "a[href]").iter().filter_map(|n| n.attr("href")).collect();
//! assert_eq!(hrefs, ["/x"]);
//! ```
//!
//! The crate is covered by an extensive test suite.
#![forbid(unsafe_code)]

// The Mission browser's constitutional layers. Their boundaries (network ⊥ renderer, parser ⊥
// network) are architectural invariants of the design. lib.rs is the only place allowed to name
// all three.
pub mod json;
pub mod parser;
pub mod renderer;

// Not part of the stable library API: the CLI internals back the `mission` binary, the MCP server
// backs the `mission-mcp` binary, and the network layer is a deliberate offline stub. Hidden from
// the docs so consumers don't build on them.
#[doc(hidden)]
pub mod cli;
#[doc(hidden)]
pub mod mcp;
#[doc(hidden)]
pub mod network;
