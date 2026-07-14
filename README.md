# Mission

[![CI](https://github.com/MerlijnW70/mission/actions/workflows/ci.yml/badge.svg)](https://github.com/MerlijnW70/mission/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/MerlijnW70/mission?sort=semver)](https://github.com/MerlijnW70/mission/releases/latest)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Zero dependencies](https://img.shields.io/badge/dependencies-0-success.svg)](Cargo.toml)

**A fast, robust HTML parser and CSS selector engine that won't crash on bad HTML.**
Zero dependencies, one small binary, no network — turn messy real-world HTML into clean text or
structured data. **Free and open source** (MIT / Apache-2.0).

> Mission is the open core of **[Mission Cloud](docs/mission-cloud.md)** — the managed platform for
> extraction that *heals itself* when sites change. The parser is yours to keep, forever. ☁️

---

## ⚡ Install in one line — no toolchain needed

```sh
# macOS / Linux:
curl -fsSL https://raw.githubusercontent.com/MerlijnW70/mission/main/install.sh | sh
```

Windows / other: grab a binary from the [latest release](https://github.com/MerlijnW70/mission/releases/latest)
(all platforms · x86-64 & arm64), or with Rust: `cargo install --git https://github.com/MerlijnW70/mission mission`.

## 🚀 60-second start

```sh
mission page.html                                  # render HTML → readable text
mission page.html --select 'a[href]' --attr href   # extract every link
mission page.html --select 'h2' --json             # structured data out
curl -s https://example.com | mission - --select h1 # slice a live page (pipe HTML in)
```

```text
$ curl -s https://en.wikipedia.org/wiki/Rust_(programming_language) | mission - --select '.infobox a[href^="http"]' --attr href
https://www.rust-lang.org/
https://github.com/rust-lang/rust
...
```

There is **no `mission fetch`** — Mission has no network layer by design. Feed it HTML from a file,
a pipe, or `curl`. That keeps it small, fast, and safe on untrusted input.

## 🤖 Use it as an MCP tool (give your agent HTML superpowers)

Installing Mission also gives you **`mission-mcp`**, a [Model Context Protocol](https://modelcontextprotocol.io)
server. Point any MCP-compatible editor or environment at it and your agent gets four tools —
`select`, `select_text`, `render`, `attributes` — to slice HTML with CSS selectors, deterministically
and locally.

Most MCP hosts take the same block — add it to your client's server config and restart:

```json
{
  "mcpServers": {
    "mission": { "command": "mission-mcp" }
  }
}
```

For CLI-based clients it's usually a one-liner, e.g. `<client> mcp add mission mission-mcp`.

Now your agent can do *"fetch this page and pull out the product prices"* without shipping the whole
HTML into the context window — Mission does the slicing and hands back just the matches.

---

## What it does

```text
HTML  →  tokenize  →  parse (DOM)  →  query (CSS)  →  render
```

- **Renders** HTML to clean, readable text — headings, lists, tables, blockquotes, emphasis, links.
- **Queries** with a near-complete CSS selector engine (below).
- **Extracts** structured data — attribute values, JSON, or a streaming NDJSON slicer.

### CLI

| Invocation | Effect |
| --- | --- |
| `mission <file>...` | render one or more HTML files (`-` reads stdin) |
| `… --select <css>` | print the elements matching a CSS selector |
| `… --attr <name>` | print that attribute of each match instead of its text |
| `… --json` / `--jsonl` / `--pretty` | machine-readable JSON (array, NDJSON per-line, or indented) |
| `… --count` | print the number of matches (grep-style) |
| `… --fail-on-empty` | exit non-zero if nothing matched (for CI pipelines) |
| `… --width <n>` | wrap rendered text to `n` columns at word boundaries |
| `mission --filter` | NDJSON pipe mode: one `{"html","selector"}` job per line in, one `{"matches":[…]}` line out |

### CSS selector engine

| Category | Selectors |
| --- | --- |
| Simple | `div` · `.card` · `#main` · `*` |
| Attribute | `[href]` · `[type="text"]` · `[href^="/a"]` · `[src$=".png"]` · `[class*="col"]` · `[rel~="me"]` · `[lang\|="en"]` |
| Structural | `:first-child` · `:last-child` · `:nth-child(An+B \| odd \| even)` · `:only-child` · `:nth-last-child(…)` |
| Of-type | `:first-of-type` · `:last-of-type` · `:nth-of-type(…)` · `:only-of-type` · `:nth-last-of-type(…)` |
| Relational | `:has(<relative selector>)` — the container that holds `X` |
| Negation | `:not(<selector list>)` |
| Combinators | descendant (space) · child `>` · adjacent `+` · general sibling `~` · groups `h1, h2` |

## Why it holds up

- **Won't crash on bad HTML.** Lenient, browser-like recovery plus depth caps and bounded matching
  mean adversarial or broken markup produces output, not a panic.
- **Zero runtime dependencies.** The tokenizer, DOM, CSS engine, renderer, and JSON are all
  from-scratch; the `[dependencies]` table is empty. `#![forbid(unsafe_code)]` throughout.
- **No network, by design.** The parser and renderer never open a socket — small attack surface,
  fully deterministic.
- **Mutation-tested.** Behaviour is pinned by an extensive, mutation-tested suite — a green build
  means the tested behaviour is genuinely exercised, not merely that the code compiles.

## Performance

Single core, zero dependencies, no SIMD — and the benchmark itself has **no dependencies either**
(a `std`-only harness). Reproduce any time with `cargo bench`:

| Stage | Throughput | 1 MB page |
| --- | --- | --- |
| Parse HTML → DOM | ~50 MB/s | ~20 ms |
| CSS `select` over the parsed tree | **> 1 GB/s** | < 1 ms |
| Render to text | ~175 MB/s | ~6 ms |

The shape that matters for extraction: **parse a page once, then query it as many times as you like
— selection runs at gigabytes per second.**

## Library use

```rust
use mission::parser::{parse, select};
use mission::renderer::render_text;

let dom = parse("<article><h1>Title</h1><p>See <a href=\"/x\">the docs</a>.</p></article>");
assert_eq!(render_text(&dom), "# Title\nSee the docs [/x].");

let hrefs: Vec<&str> = select(&dom, "a[href]").iter().filter_map(|n| n.attr("href")).collect();
assert_eq!(hrefs, ["/x"]);
```

---

## ☁️ Mission is one part of a bigger family — meet Mission Cloud

Mission (this repo) is the **extraction engine**: given HTML and a selector, it returns the data.
That's the hard, deterministic part — and it's **free forever.**

But real extraction at scale has a second, messier problem: **the web changes.** Selectors rot, pages
get redesigned, endpoints flake. That's what **[Mission Cloud](docs/mission-cloud.md)** solves — a
managed platform that wraps this engine with a **self-healing brain**:

| | **Mission** (free, open source) | **Mission Cloud** (managed) |
| --- | --- | --- |
| HTML parse · CSS query · render | ✅ | ✅ |
| Run locally / self-host | ✅ | ✅ |
| **Auto-retry with fallback selectors** when one breaks | — | ✅ |
| **Strategy escalation + circuit breaking** (no 3am pages) | — | ✅ |
| **High-throughput binary transport** for pipelines | — | ✅ |
| **Managed, distributed, monitored** extraction at scale | — | ✅ |

> ### ☁️ [Read about the platform →](docs/mission-cloud.md)  ·  [Join the early-access waitlist →](docs/mission-cloud.md#get-early-access)

The parser you're installing here is the genuine core of that platform — not a crippled demo. Use it
free, forever; reach for Mission Cloud when extraction becomes something you have to *keep running.*

## License

The Mission parser is licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at
your option. (Mission Cloud is a separate commercial offering.)
