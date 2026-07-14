# Mission

A from-scratch, **zero-dependency** text-mode web browser and HTML/CSS query engine, written in
Rust. Mission takes raw HTML and turns it into readable text or structured data. It has no runtime
dependencies (the `[dependencies]` table is empty), needs no network or graphics stack, and compiles
to a single small executable.

```text
$ mission page.html
# Rust (programming language)
Rust is a general-purpose programming language [/wiki/...] which emphasizes performance,
type safety, concurrency, and memory safety.
...

$ mission page.html --select 'a[href^="https"]' --attr href
https://www.rust-lang.org/
https://doc.rust-lang.org/

$ mission page.html --select "h2" --attr id --json
["History","Safety","Ecosystem","Performance","Adoption"]
```

## What it does

Mission is a complete little pipeline — raw bytes in, readable text or JSON out:

```text
HTML  →  tokenize  →  parse (DOM)  →  query (CSS)  →  render
```

- **Renders** HTML to clean, readable text — headings, lists, tables, blockquotes, emphasis, and
  links with their destinations.
- **Queries** the parsed document with a near-complete CSS selector engine.
- **Extracts** structured data — attribute values or JSON — for pipelines, agents, and databases.

There is **no `mission fetch`** — Mission has no network layer by design. Feed it HTML from wherever:
a file, a pipe, `curl`.

## Quick start

Building and running Mission needs **only Rust** — nothing else.

```sh
git clone https://github.com/MerlijnW70/mission
cd mission
cargo build --release
./target/release/mission                      # render the built-in demo page
./target/release/mission page.html            # render an HTML file to text
./target/release/mission page.html --select "h1"   # query it
cargo test                                     # run the test suite
```

### CLI

| Invocation | Effect |
| --- | --- |
| `mission` | render the built-in demo page |
| `mission <file>...` | render one or more HTML files (`-` reads stdin, e.g. `curl … \| mission -`) |
| `mission [<file>] --select <css>` | print the elements matching a CSS selector |
| `… --attr <name>` | print that attribute of each match instead of its text |
| `… --json` | emit machine-readable JSON (element objects, or values with `--attr`) |
| `… --jsonl` | emit one JSON object per match, per line — each flushed as produced (NDJSON) |
| `… --pretty` | emit indented JSON |
| `… --count` | print the number of matches (grep-style) |
| `… --fail-on-empty` | exit non-zero if the selector matches nothing (for CI pipelines) |
| `… --width <n>` | wrap rendered text to at most `n` columns at word boundaries |
| `mission --filter` | NDJSON pipe mode: read one `{"html","selector"}` job per stdin line, emit one `{"matches":[…]}` result line each (flushed) — a stream-in / stream-out slicer |
| `mission --help` / `--version` | usage / version |

## Capabilities

### Parser / tokenizer

Turns HTML into a DOM (`Vec<Node>`, where a `Node` is `Text` or `Element { tag, attrs, children }`).

- **Tags & attributes** — double-quoted, single-quoted, unquoted, and boolean attributes. A repeated
  attribute keeps its **first** value (per HTML5).
- **Character references** decoded in text *and* attribute values: a broad **named** set (markup
  basics, the full Latin-1 supplement, common typographic / symbol / Greek references), plus decimal
  (`&#65;`) and hex (`&#x41;`), with the HTML numeric fix-ups (`&#151;` → em dash via Windows-1252;
  null / surrogate / out-of-range → U+FFFD).
- **Comments** `<!-- … -->` and **CDATA** `<![CDATA[ … ]]>` handled whole.
- **Void & self-closing** elements — `<br>`, `<img>`, `<hr>`, `<input>`, … and the `<br/>` form.
- **Raw-text & RCDATA elements** — `<script>`/`<style>` content is skipped; `<title>`/`<textarea>`
  are RCDATA (content is text with entities decoded, but a `<` inside is literal).
- **Declarations** — `<!doctype html>` and friends are skipped.
- **Case-insensitive** tag and attribute names (`<DIV>` == `<div>`), normalized to lowercase.
- **Lenient recovery** — implied end tags close unclosed elements the way browsers do; an end tag
  closes down to its nearest matching open element; a name open nowhere is ignored; anything still
  open auto-closes at end of input.

### CSS selector engine

`select(&dom, "…")` supports a near-complete CSS subset:

| Category | Selectors |
| --- | --- |
| Simple | `div` · `.card` · `#main` · `*` |
| Attribute | `[href]` · `[type="text"]` · `[href^="/a"]` · `[src$=".png"]` · `[class*="col"]` · `[rel~="me"]` · `[lang\|="en"]` |
| Structural pseudo | `:first-child` · `:last-child` · `:nth-child(An+B \| odd \| even)` · `:only-child` · `:nth-last-child(…)` |
| Of-type pseudo | `:first-of-type` · `:last-of-type` · `:nth-of-type(…)` · `:only-of-type` · `:nth-last-of-type(…)` |
| Relational | `:has(<relative selector>)` — a descendant match, or a direct-child match with a leading `>` |
| Negation | `:not(<selector list>)` — matches none of a comma-separated list of full complex selectors |
| Combinators | descendant (space) · child `>` · adjacent sibling `+` · general sibling `~` |
| Groups | `h1, h2` — matches any member, once, in document order |

### Renderer & output

- **Text rendering** — headings (`## …`), lists (bulleted / numbered, nested), tables (` | `-joined
  rows), blockquotes (`> `), emphasis (`*bold*`, `_italic_`, `` `code` ``), `<pre>`, image alt text,
  hyperlink destinations, and word-boundary width wrapping.
- **JSON** — zero-dependency serialization of element objects, attribute-value arrays, and a
  streaming NDJSON mode (one flushed record per match, near-zero time-to-first-line).
- **MCP server** — a `mission-mcp` binary exposes select / render / attributes over a line-delimited
  JSON-RPC stdio interface, for tool-using agents.

## Design

- **Zero runtime dependencies.** The whole parser, CSS engine, renderer, and JSON layer are written
  from scratch; the `[dependencies]` table stays empty.
- **`#![forbid(unsafe_code)]`** throughout.
- **No network, by design.** The parser and renderer never open a socket — Mission converts HTML you
  already have; it does not fetch. This keeps the attack surface small and the layers independent
  (network ⊥ renderer, parser ⊥ network).
- **Robust against hostile input.** Depth caps and bounded work guard against adversarial HTML; the
  behaviour is pinned by an extensive test suite.

## Library use

```rust
use mission::parser::{parse, select};
use mission::renderer::render_text;

let dom = parse("<article><h1>Title</h1><p>See <a href=\"/x\">the docs</a>.</p></article>");
assert_eq!(render_text(&dom), "# Title\nSee the docs [/x].");

let hrefs: Vec<&str> = select(&dom, "a[href]").iter().filter_map(|n| n.attr("href")).collect();
assert_eq!(hrefs, ["/x"]);
```

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT) at your option.
