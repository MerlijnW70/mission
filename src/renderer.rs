//! The **renderer** layer of the Mission browser — it draws.
//!
//! Constitutional boundary: the renderer paints output from parsed structure. It must never
//! open the network (no backdoor to the internet) — an architectural boundary of the design.
//! Consuming the parser's tree is allowed and expected: the renderer sits
//! downstream of the parser in the pipeline (network → parser → renderer).
//!
//! First real capability: text rendering. [`render_text`] lays out the visible character data
//! of a document tree as readable lines — block-level elements (paragraphs, headings, `div`…)
//! break onto their own line, inline elements flow with the surrounding text. The core of a
//! text-mode browser. Geometric (pixel) layout comes in later steps.

use crate::parser::Node;

/// Render the visible text of a document `tree` as newline-separated lines.
///
/// Character data flows in document order with its whitespace collapsed (see [`collapse_spaces`]);
/// a block-level element (see [`is_block`]) forces a line break around its content, while inline
/// elements do not. The raw pass emits generous `'\n'` boundaries which [`normalize`] then folds
/// and trims — small, independently tested helpers, so the layout rules stay pinned by tests.
pub fn render_text(tree: &[Node]) -> String {
    let mut raw = String::new();
    collect_text(tree, &mut raw, Ctx::default());
    // Preformatted whitespace and layout indentation rode through `normalize` disguised as
    // reserved marker chars (they are not whitespace, so `normalize` left them alone); restore
    // them now that folding is done.
    unescape_pre(&normalize(&raw))
}

/// Render `tree` to text, then wrap each line to at most `width` columns at word boundaries.
pub fn render_text_wrapped(tree: &[Node], width: usize) -> String {
    wrap_lines(&render_text(tree), width)
}

/// Render a borrowed selection (as returned by [`crate::parser::select`] and the `find_*` queries)
/// to text — the `&[&Node]` counterpart of [`render_text`], mirroring [`crate::json::selection`].
pub fn render_nodes(nodes: &[&Node]) -> String {
    // Rendering only reads the tree, so the borrowed selection is rendered in place: no cloning of
    // each selected node's whole subtree. `collect_text` accepts any iterator of `&Node`, so the
    // `&[&Node]` is fed straight through as `&Node`s.
    let mut raw = String::new();
    collect_text(nodes.iter().copied(), &mut raw, Ctx::default());
    unescape_pre(&normalize(&raw))
}

/// Rendering context threaded through the walk: whether we are inside a `<pre>` (whitespace is
/// preserved) and how many lists deep we are (for nested-list indentation).
#[derive(Clone, Copy, Default)]
struct Ctx {
    pre: bool,
    list_depth: usize,
}

/// Walk `tree`, appending rendered text to `out` under context `ctx`. Accepts any iterator of
/// `&Node` (a slice, a `&Vec`, or a borrowed selection), so a `&[&Node]` renders without cloning.
fn collect_text<'a, I: IntoIterator<Item = &'a Node>>(tree: I, out: &mut String, ctx: Ctx) {
    for node in tree {
        match node {
            Node::Text(t) => {
                if ctx.pre {
                    out.push_str(&escape_pre(t));
                } else {
                    out.push_str(&collapse_spaces(t));
                }
            }
            Node::Element {
                tag,
                attrs,
                children,
            } => {
                if tag == "head" || tag == "title" || tag == "template" {
                    // Metadata (<head>, <title>) and inert <template> content are parsed (so they
                    // remain queryable) but never rendered as body text.
                } else if tag == "ul" || tag == "ol" {
                    collect_list(tag, children, out, ctx.list_depth);
                } else if tag == "hr" {
                    out.push_str("\n────────\n"); // a horizontal rule on its own line
                } else if tag == "dd" {
                    // A definition description is a block indented one level under its <dt> term.
                    out.push('\n');
                    indent(out, 1);
                    collect_text(children, out, ctx);
                    out.push('\n');
                } else if let Some(level) = heading_level(tag) {
                    // Mark headings markdown-style: <h2> → "## text".
                    out.push('\n');
                    for _ in 0..level {
                        out.push('#');
                    }
                    out.push(' ');
                    collect_text(children, out, ctx);
                    out.push('\n');
                } else if let Some(marker) = emphasis_marker(tag) {
                    // Inline markers, markdown-style: <b> → *text*, <code> → `text`, <del> → ~~…~~.
                    out.push_str(marker);
                    collect_text(children, out, ctx);
                    out.push_str(marker);
                } else if tag == "tr" {
                    collect_row(children, out);
                } else if tag == "blockquote" {
                    collect_blockquote(children, out);
                } else if tag == "pre" {
                    // Preformatted block: break onto its own line(s) and preserve inner whitespace.
                    out.push('\n');
                    collect_text(children, out, Ctx { pre: true, ..ctx });
                    out.push('\n');
                } else if tag == "img" {
                    // A hyperlink-less image renders its alt text as `[alt]`, if any.
                    if let Some(alt) = crate::parser::attr(attrs, "alt") {
                        out.push('[');
                        out.push_str(&collapse_spaces(alt));
                        out.push(']');
                    }
                } else if is_block(tag) {
                    out.push('\n');
                    collect_text(children, out, ctx);
                    out.push('\n');
                } else {
                    collect_text(children, out, ctx);
                }
                // Show a hyperlink's destination inline, e.g. `docs [/about]`.
                if tag == "a"
                    && let Some(href) = crate::parser::attr(attrs, "href")
                {
                    out.push_str(" [");
                    // Strip reserved markers (a URL can contain decoded control chars): unlike the
                    // text paths, href is written straight into `out`, so it must be sanitized too
                    // or a stray marker would survive `normalize` and unescape into whitespace.
                    out.extend(href.chars().filter(|c| !is_pre_marker(*c)));
                    out.push(']');
                }
            }
        }
    }
}

/// Render a `<blockquote>` by rendering its content and prefixing every resulting line with
/// `> ` (nested quotes stack, e.g. `> > `).
fn collect_blockquote(children: &[Node], out: &mut String) {
    let mut inner = String::new();
    collect_text(children, &mut inner, Ctx::default());
    out.push('\n');
    // A <pre> inside carries its line breaks as PRE_NEWLINE markers (still un-decoded at this
    // point); turn them into real newlines first so every inner line gets the `> ` prefix.
    let body = normalize(&inner).replace(PRE_NEWLINE, "\n");
    for line in body.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
}

/// Render a table row (`<tr>`) on its own line: the text of each `<td>`/`<th>` cell joined by
/// ` | `. Non-cell children are ignored.
fn collect_row(cells: &[Node], out: &mut String) {
    out.push('\n');
    let mut first = true;
    for cell in cells {
        if cell.tag() == Some("td") || cell.tag() == Some("th") {
            if !first {
                out.push_str(" | ");
            }
            first = false;
            if let Node::Element { children, .. } = cell {
                // Render the cell in isolation, then flatten any internal block breaks to spaces:
                // a block-level child (e.g. a `<p>` or `<div>` inside a `<td>`) must not split the
                // row across lines or orphan the ` | ` separator onto a line of its own. A `<pre>`
                // carries its line breaks as the reserved PRE_NEWLINE marker (still un-decoded here),
                // so that must be flattened too — otherwise `unescape_pre` turns it back into a real
                // newline and the row splits anyway.
                let mut buf = String::new();
                collect_text(children, &mut buf, Ctx::default());
                let cell_text = normalize(&buf).replace(['\n', PRE_NEWLINE], " ");
                out.push_str(&cell_text);
            }
        }
    }
    out.push('\n');
}

/// The markdown-style wrap marker for an inline element (used as both prefix and suffix): `*` for
/// bold, `_` for italic, `` ` `` for code, `~~` for deleted, `++` for inserted, `~`/`^` for
/// sub/superscript; `None` for anything else.
fn emphasis_marker(tag: &str) -> Option<&'static str> {
    match tag {
        "b" | "strong" => Some("*"),
        "i" | "em" => Some("_"),
        "code" => Some("`"),
        "del" | "s" | "strike" => Some("~~"),
        "ins" => Some("++"),
        "sub" => Some("~"),
        "sup" => Some("^"),
        _ => None,
    }
}

/// Wrap each line of `text` to at most `width` columns, breaking only at spaces. A line already
/// within `width` is left as is, and a single word longer than `width` is not broken (it overflows
/// rather than being split mid-word). Existing line breaks are preserved.
fn wrap_lines(text: &str, width: usize) -> String {
    let mut out = String::new();
    for (i, line) in text.lines().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // A leading run of spaces / `>` (blockquote prefix, list indent) is a hanging prefix that
        // continuation lines repeat, and its width is charged against the wrap budget.
        let prefix_len = line.bytes().take_while(|&b| b == b' ' || b == b'>').count();
        let (prefix, content) = line.split_at(prefix_len);
        let avail = width.saturating_sub(prefix_len).max(1);
        out.push_str(prefix);
        let mut col = 0;
        let mut first = true;
        for word in content.split(' ') {
            let w = word.chars().count();
            if !first && col + 1 + w > avail {
                out.push('\n');
                out.push_str(prefix);
                col = 0;
                first = true;
            }
            if !first {
                out.push(' ');
                col += 1;
            }
            out.push_str(word);
            col += w;
            first = false;
        }
    }
    out
}

/// The heading level of `h1`…`h6` (`1`…`6`), or `None` for any other tag.
fn heading_level(tag: &str) -> Option<u32> {
    let level = tag.strip_prefix('h')?.parse::<u32>().ok()?;
    (1..=6).contains(&level).then_some(level)
}

/// Render a `<ul>`/`<ol>` as one line per `<li>`: a `•` bullet for an unordered list, an
/// incrementing `1. 2. 3.` number for an ordered one. `depth` is how many lists enclose this one;
/// each level indents two spaces so nested lists keep their shape. Non-`<li>` children render
/// normally, and an `<li>`'s own content is rendered one level deeper so a nested list indents.
fn collect_list(tag: &str, children: &[Node], out: &mut String, depth: usize) {
    let ordered = tag == "ol";
    let mut number = 0u32;
    let inner = Ctx {
        pre: false,
        list_depth: depth + 1,
    };
    out.push('\n'); // the list is a block
    for child in children {
        if child.tag() == Some("li") {
            number += 1;
            out.push('\n');
            indent(out, depth);
            if ordered {
                out.push_str(&number.to_string());
                out.push_str(". ");
            } else {
                out.push_str("• ");
            }
            if let Node::Element { children: item, .. } = child {
                collect_text(item, out, inner);
            }
            out.push('\n');
        } else {
            collect_text(std::slice::from_ref(child), out, inner);
        }
    }
    out.push('\n');
}

/// Push `level` levels of indentation — two reserved space markers per level, so `normalize` keeps
/// them (they are not whitespace) and `unescape_pre` turns them back into real leading spaces.
fn indent(out: &mut String, level: usize) {
    for _ in 0..level * 2 {
        out.push(PRE_SPACE);
    }
}

/// Whether `tag` is a block-level element that lays out on its own line(s). Everything else is
/// treated as inline and flows with the surrounding text.
fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "html"
            | "body"
            | "div"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "main"
            | "nav"
            | "p"
            | "ul"
            | "ol"
            | "li"
            | "dl"
            | "dt"
            | "br"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}

/// Collapse each run of whitespace in a text node to a single space, turning source newlines,
/// tabs, and indentation into ordinary inter-word spacing (they are not line breaks in HTML —
/// only block elements and `<br>` break lines). Called before the block-boundary `'\n'`s are
/// added, so [`normalize`] can tell a text space from a block break.
fn collapse_spaces(text: &str) -> String {
    let mut out = String::new();
    let mut in_ws = false;
    for c in text.chars() {
        if is_pre_marker(c) {
            continue; // reserved renderer markers never carry through from source text
        }
        if is_collapsible_ws(c) {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

// Preformatted whitespace is carried through `normalize` as these reserved control chars: they
// are not `char::is_whitespace`, so `normalize` copies them verbatim instead of folding them.
// `escape_pre` maps real whitespace onto them and `unescape_pre` maps them back; both the pre and
// the collapsing paths strip any that appear in source text, so the round-trip cannot collide.
const PRE_SPACE: char = '\u{1}';
const PRE_NEWLINE: char = '\u{2}';
const PRE_TAB: char = '\u{3}';

/// Whether `c` is one of the reserved preformatted-whitespace markers.
fn is_pre_marker(c: char) -> bool {
    matches!(c, PRE_SPACE | PRE_NEWLINE | PRE_TAB)
}

/// Whitespace that HTML collapses / breaks lines on. Excludes `U+00A0` (`&nbsp;`), which is a
/// *non-breaking* space: it is preserved verbatim, does not fold with neighbours, and is not a
/// wrap opportunity — matching browser behavior.
fn is_collapsible_ws(c: char) -> bool {
    c.is_whitespace() && c != '\u{a0}'
}

/// Encode a preformatted text node: whitespace is preserved by mapping it to reserved markers
/// (space, newline, and tab kept distinct; other whitespace becomes a space, `\r` is dropped),
/// so the later `normalize` pass leaves it untouched. Any stray marker in the source is dropped.
fn escape_pre(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            ' ' => out.push(PRE_SPACE),
            '\n' => out.push(PRE_NEWLINE),
            '\t' => out.push(PRE_TAB),
            '\r' => {}                  // CR is dropped; the '\n' carries the line break
            _ if is_pre_marker(c) => {} // reserved: never let a source marker survive
            _ if is_collapsible_ws(c) => out.push(PRE_SPACE), // (nbsp falls through, kept verbatim)
            _ => out.push(c),
        }
    }
    out
}

/// Restore the reserved markers back to the whitespace they stood for, after `normalize` has run.
fn unescape_pre(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            PRE_SPACE => ' ',
            PRE_NEWLINE => '\n',
            PRE_TAB => '\t',
            other => other,
        })
        .collect()
}

/// Fold the buffer's whitespace into clean lines: a run of whitespace that contains a block
/// boundary (`'\n'`) becomes a single newline, any other run becomes a single space, and
/// leading/trailing whitespace is trimmed. (Text runs are already space-collapsed by
/// [`collapse_spaces`], so the only newlines here are block boundaries.)
///
/// The decision reads the last byte already emitted — no pre-loop state whose initial value the
/// first character would mask — so every branch is reachable and pinned by a test.
fn normalize(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if is_collapsible_ws(c) {
            match out.as_bytes().last() {
                None => {} // leading whitespace: drop it
                Some(b' ') if c == '\n' => {
                    out.pop(); // a block newline outranks a pending space
                    out.push('\n');
                }
                Some(b' ' | b'\n') => {} // already separated: collapse into the existing one
                Some(_) => out.push(if c == '\n' { '\n' } else { ' ' }),
            }
        } else {
            out.push(c);
        }
    }
    if matches!(out.as_bytes().last(), Some(b' ' | b'\n')) {
        out.pop(); // trailing separator
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Node::{Element, Text};

    fn elem(tag: &str, children: Vec<Node>) -> Node {
        Element {
            tag: tag.into(),
            attrs: vec![],
            children,
        }
    }

    #[test]
    fn an_empty_tree_renders_nothing() {
        assert_eq!(render_text(&[]), "");
    }

    #[test]
    fn a_bare_text_run_renders_verbatim() {
        // Pins the push_str: dropping it would render "".
        assert_eq!(render_text(&[Text("hi".into())]), "hi");
    }

    #[test]
    fn sibling_runs_concatenate_in_order() {
        assert_eq!(render_text(&[Text("a".into()), Text("b".into())]), "ab");
    }

    #[test]
    fn element_text_is_drawn_from_its_children() {
        // Pins the recursion: dropping it would lose text nested inside an element.
        assert_eq!(render_text(&[elem("p", vec![Text("hi".into())])]), "hi");
    }

    #[test]
    fn an_empty_element_contributes_nothing() {
        assert_eq!(render_text(&[elem("p", vec![])]), "");
    }

    #[test]
    fn nested_and_sibling_text_is_gathered_in_document_order() {
        // "b" is inline, so it does not break the line — it emphasises with *…* instead.
        let tree = vec![elem(
            "p",
            vec![
                Text("x".into()),
                elem("b", vec![Text("y".into())]),
                Text("z".into()),
            ],
        )];
        assert_eq!(render_text(&tree), "x*y*z");
    }

    #[test]
    fn block_siblings_render_on_separate_lines() {
        // Pins the collapse in normalize: without it the two paragraphs stack a blank line.
        let tree = vec![
            elem("p", vec![Text("a".into())]),
            elem("p", vec![Text("b".into())]),
        ];
        assert_eq!(render_text(&tree), "a\nb");
    }

    #[test]
    fn an_inline_element_stays_on_the_line() {
        // "span" is not block-level, so its text flows with the siblings: no break.
        let tree = vec![
            Text("a".into()),
            elem("span", vec![Text("b".into())]),
            Text("c".into()),
        ];
        assert_eq!(render_text(&tree), "abc");
    }

    #[test]
    fn a_block_after_text_starts_a_new_line() {
        // Pins the leading '\n' push in collect_text.
        let tree = vec![Text("a".into()), elem("p", vec![Text("b".into())])];
        assert_eq!(render_text(&tree), "a\nb");
    }

    #[test]
    fn text_after_a_block_starts_a_new_line() {
        // Pins the trailing '\n' push in collect_text.
        let tree = vec![elem("p", vec![Text("a".into())]), Text("b".into())];
        assert_eq!(render_text(&tree), "a\nb");
    }

    fn link(href: &str, children: Vec<Node>) -> Node {
        Element {
            tag: "a".into(),
            attrs: vec![("href".into(), href.into())],
            children,
        }
    }

    #[test]
    fn a_hyperlink_shows_its_destination() {
        // Pins the link branch and its push_str/push calls.
        assert_eq!(
            render_text(&[link("/about", vec![Text("docs".into())])]),
            "docs [/about]"
        );
    }

    #[test]
    fn an_anchor_without_href_shows_only_its_text() {
        let anchor = Element {
            tag: "a".into(),
            attrs: vec![],
            children: vec![Text("x".into())],
        };
        assert_eq!(render_text(&[anchor]), "x");
    }

    #[test]
    fn a_non_anchor_href_is_not_shown() {
        // Pins the `tag == "a"` guard: a block <p> carrying href must not render a link.
        let p = Element {
            tag: "p".into(),
            attrs: vec![("href".into(), "/x".into())],
            children: vec![Text("t".into())],
        };
        assert_eq!(render_text(&[p]), "t");
    }

    #[test]
    fn collapse_spaces_folds_whitespace_runs_to_one_space() {
        assert_eq!(collapse_spaces("a   b"), "a b");
        assert_eq!(collapse_spaces("a\n\tb"), "a b"); // newline and tab become a space
        assert_eq!(collapse_spaces("  x  "), " x "); // edge whitespace kept as a single space
        assert_eq!(collapse_spaces(""), "");
    }

    #[test]
    fn runs_of_spaces_in_text_collapse_to_one() {
        assert_eq!(
            render_text(&[elem("p", vec![Text("a     b".into())])]),
            "a b"
        );
    }

    #[test]
    fn a_newline_in_text_is_a_space_not_a_line_break() {
        // Source newlines inside text are inter-word spacing; only blocks/<br> break lines.
        assert_eq!(render_text(&[elem("p", vec![Text("a\nb".into())])]), "a b");
    }

    #[test]
    fn leading_and_trailing_text_whitespace_is_trimmed() {
        assert_eq!(
            render_text(&[elem("p", vec![Text("   hi   ".into())])]),
            "hi"
        );
    }

    #[test]
    fn whitespace_across_inline_boundaries_collapses_to_one_space() {
        let tree = vec![
            Text("a ".into()),
            elem("span", vec![Text(" b ".into())]),
            Text(" c".into()),
        ];
        assert_eq!(render_text(&tree), "a b c");
    }

    #[test]
    fn a_trailing_space_is_trimmed() {
        // Pins the final trailing-separator trim for a space (not just a newline).
        assert_eq!(render_text(&[Text("hi ".into())]), "hi");
    }

    #[test]
    fn a_block_boundary_outranks_a_preceding_space() {
        // Pins the "space then block newline" upgrade: the newline must win, giving "a\nb".
        let tree = vec![Text("a ".into()), elem("p", vec![Text("b".into())])];
        assert_eq!(render_text(&tree), "a\nb");
    }

    #[test]
    fn an_unordered_list_marks_each_item_with_a_bullet() {
        let tree = vec![elem(
            "ul",
            vec![
                elem("li", vec![Text("a".into())]),
                elem("li", vec![Text("b".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "• a\n• b");
    }

    #[test]
    fn a_list_ignores_whitespace_between_items() {
        // A whitespace text node between <li>s must not become a bullet. Pins the
        // `child.tag() == Some("li")` guard (a mutant that treats every child as an item).
        let tree = vec![elem(
            "ul",
            vec![
                elem("li", vec![Text("a".into())]),
                Text(" ".into()),
                elem("li", vec![Text("b".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "• a\n• b");
    }

    #[test]
    fn an_ordered_list_numbers_each_item() {
        // Pins `tag == "ol"` and the incrementing counter.
        let tree = vec![elem(
            "ol",
            vec![
                elem("li", vec![Text("x".into())]),
                elem("li", vec![Text("y".into())]),
                elem("li", vec![Text("z".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "1. x\n2. y\n3. z");
    }

    #[test]
    fn a_horizontal_rule_renders_on_its_own_line() {
        let tree = vec![Text("a".into()), elem("hr", vec![]), Text("b".into())];
        assert_eq!(render_text(&tree), "a\n────────\nb");
    }

    #[test]
    fn render_nodes_renders_a_borrowed_selection() {
        // The &[&Node] counterpart of render_text, so a select()/find() result renders directly.
        let dom = crate::parser::parse("<h1>A</h1><p>b</p>");
        let picked: Vec<&Node> = crate::parser::select(&dom, "h1");
        assert_eq!(render_nodes(&picked), "# A");
    }

    #[test]
    fn only_h1_through_h6_are_headings() {
        // Pins both outer bounds of heading_level's range: <h0>/<h7> are ordinary inline elements
        // (their text flows, no `#` markers), not headings.
        assert_eq!(render_text(&[elem("h0", vec![Text("x".into())])]), "x");
        assert_eq!(render_text(&[elem("h7", vec![Text("y".into())])]), "y");
    }

    #[test]
    fn a_heading_is_marked_by_its_level() {
        // Pins heading_level and the '#'-count loop across the range's bounds.
        assert_eq!(
            render_text(&[elem("h1", vec![Text("Title".into())])]),
            "# Title"
        );
        assert_eq!(
            render_text(&[elem("h3", vec![Text("Sub".into())])]),
            "### Sub"
        );
        assert_eq!(
            render_text(&[elem("h6", vec![Text("Deep".into())])]),
            "###### Deep"
        );
    }

    #[test]
    fn bold_elements_are_wrapped_in_asterisks() {
        assert_eq!(render_text(&[elem("b", vec![Text("x".into())])]), "*x*");
        assert_eq!(
            render_text(&[elem("strong", vec![Text("y".into())])]),
            "*y*"
        );
    }

    #[test]
    fn italic_elements_are_wrapped_in_underscores() {
        // Pins the `<i>/<em>` arm distinctly from the bold marker.
        assert_eq!(render_text(&[elem("i", vec![Text("x".into())])]), "_x_");
        assert_eq!(render_text(&[elem("em", vec![Text("y".into())])]), "_y_");
    }

    #[test]
    fn emphasis_flows_inline_within_a_sentence() {
        let tree = vec![elem(
            "p",
            vec![
                Text("a ".into()),
                elem("b", vec![Text("bold".into())]),
                Text(" c".into()),
            ],
        )];
        assert_eq!(render_text(&tree), "a *bold* c");
    }

    #[test]
    fn head_and_title_metadata_are_not_rendered() {
        // <head>/<title> hold document metadata, never body text.
        let tree = vec![
            elem("head", vec![elem("title", vec![Text("Page Title".into())])]),
            elem("body", vec![Text("visible".into())]),
        ];
        assert_eq!(render_text(&tree), "visible");
    }

    #[test]
    fn a_standalone_title_is_also_suppressed() {
        let tree = vec![
            elem("title", vec![Text("T".into())]),
            elem("p", vec![Text("body".into())]),
        ];
        assert_eq!(render_text(&tree), "body");
    }

    #[test]
    fn a_blockquote_prefixes_each_line() {
        let tree = vec![elem(
            "blockquote",
            vec![
                elem("p", vec![Text("a".into())]),
                elem("p", vec![Text("b".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "> a\n> b");
    }

    #[test]
    fn nested_blockquotes_stack_their_markers() {
        let tree = vec![elem(
            "blockquote",
            vec![elem("blockquote", vec![Text("deep".into())])],
        )];
        assert_eq!(render_text(&tree), "> > deep");
    }

    #[test]
    fn a_blockquote_sits_between_surrounding_text() {
        let tree = vec![
            Text("before".into()),
            elem("blockquote", vec![Text("quote".into())]),
            Text("after".into()),
        ];
        assert_eq!(render_text(&tree), "before\n> quote\nafter");
    }

    #[test]
    fn a_table_row_joins_its_cells_with_a_separator() {
        // Pins the `first` flag (separator only between cells) and the td/th cell test.
        let tree = vec![elem(
            "tr",
            vec![
                elem("td", vec![Text("a".into())]),
                elem("th", vec![Text("b".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "a | b");
    }

    #[test]
    fn a_row_ignores_whitespace_between_cells() {
        // A whitespace text node between cells must not add a separator. Pins the td/th cell test.
        let tree = vec![elem(
            "tr",
            vec![
                elem("td", vec![Text("a".into())]),
                Text(" ".into()),
                elem("td", vec![Text("b".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "a | b");
    }

    #[test]
    fn table_rows_render_on_separate_lines() {
        let tree = vec![elem(
            "table",
            vec![
                elem(
                    "tr",
                    vec![
                        elem("td", vec![Text("a".into())]),
                        elem("td", vec![Text("b".into())]),
                    ],
                ),
                elem(
                    "tr",
                    vec![
                        elem("td", vec![Text("c".into())]),
                        elem("td", vec![Text("d".into())]),
                    ],
                ),
            ],
        )];
        assert_eq!(render_text(&tree), "a | b\nc | d");
    }

    #[test]
    fn pre_preserves_runs_of_spaces() {
        // Pins the `pre` flag on the <pre> branch and the space marker round-trip: outside <pre>
        // this would collapse to "a b".
        assert_eq!(
            render_text(&[elem("pre", vec![Text("a   b".into())])]),
            "a   b"
        );
    }

    #[test]
    fn pre_preserves_newlines() {
        // A source newline inside <pre> is a real line break, not inter-word spacing.
        assert_eq!(
            render_text(&[elem("pre", vec![Text("a\nb".into())])]),
            "a\nb"
        );
    }

    #[test]
    fn pre_preserves_tabs() {
        // The tab marker is distinct from the space marker, so a tab survives as a tab.
        assert_eq!(
            render_text(&[elem("pre", vec![Text("a\tb".into())])]),
            "a\tb"
        );
    }

    #[test]
    fn pre_is_a_block() {
        // Pins the two boundary newlines around the <pre> branch.
        let tree = vec![
            Text("x".into()),
            elem("pre", vec![Text("y".into())]),
            Text("z".into()),
        ];
        assert_eq!(render_text(&tree), "x\ny\nz");
    }

    #[test]
    fn pre_carries_nested_inline_markup_with_its_whitespace() {
        // Preformatting only changes whitespace: nested emphasis still renders, and the double
        // space before it is kept.
        let tree = vec![elem(
            "pre",
            vec![Text("a  ".into()), elem("b", vec![Text("x".into())])],
        )];
        assert_eq!(render_text(&tree), "a  *x*");
    }

    #[test]
    fn pre_drops_carriage_returns() {
        // Pins the `'\r' => {}` arm: the '\n' carries the break, so "a\r\nb" is "a\nb", not "a \nb".
        assert_eq!(
            render_text(&[elem("pre", vec![Text("a\r\nb".into())])]),
            "a\nb"
        );
    }

    #[test]
    fn pre_maps_other_whitespace_to_a_space() {
        // A vertical tab is whitespace but not one of the kept forms, so it becomes a space.
        assert_eq!(
            render_text(&[elem("pre", vec![Text("a\u{b}b".into())])]),
            "a b"
        );
    }

    #[test]
    fn pre_strips_a_reserved_marker_from_source() {
        // A marker char present in the source must not survive to be decoded back into whitespace.
        assert_eq!(
            render_text(&[elem("pre", vec![Text("a\u{1}b".into())])]),
            "ab"
        );
    }

    #[test]
    fn ordinary_text_strips_reserved_markers() {
        // Outside <pre> the same reserved chars are dropped, so they can never round-trip into
        // spurious whitespace. Pins the collapse-path strip and all three arms of is_pre_marker.
        assert_eq!(render_text(&[Text("a\u{1}\u{2}\u{3}b".into())]), "ab");
    }

    #[test]
    fn outside_pre_whitespace_still_collapses() {
        // Contrast with the <pre> cases: a normal block collapses its whitespace as before.
        assert_eq!(
            render_text(&[elem("div", vec![Text("a   b".into())])]),
            "a b"
        );
    }

    #[test]
    fn a_blockquote_body_is_not_preformatted() {
        // Pins the non-preformatted context passed into the blockquote body: its whitespace
        // collapses, so "a   b" is "> a b" (a preformatted body would keep the run).
        assert_eq!(
            render_text(&[elem("blockquote", vec![Text("a   b".into())])]),
            "> a b"
        );
    }

    #[test]
    fn a_table_cell_is_not_preformatted() {
        // Pins the non-preformatted context passed into each cell.
        let tree = vec![elem("tr", vec![elem("td", vec![Text("a   b".into())])])];
        assert_eq!(render_text(&tree), "a b");
    }

    #[test]
    fn a_block_element_inside_a_cell_stays_on_the_row() {
        // A block-level child (here a <p>) inside a <td> must not break the row or orphan the
        // ` | ` separator onto its own line: the cell is flattened to a single line.
        let tree = vec![elem(
            "tr",
            vec![
                elem("td", vec![elem("p", vec![Text("a".into())])]),
                elem("td", vec![Text("b".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "a | b");
    }

    #[test]
    fn a_pre_with_a_newline_inside_a_cell_stays_on_the_row() {
        // A <pre> carries its newline as the reserved PRE_NEWLINE marker, not a real '\n'; the cell
        // flatten must collapse that marker too, or `unescape_pre` restores the newline and splits
        // the row. Pins the PRE_NEWLINE arm of the cell flatten.
        let tree = vec![elem(
            "tr",
            vec![
                elem("td", vec![elem("pre", vec![Text("a\nb".into())])]),
                elem("td", vec![Text("c".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "a b | c");
    }

    #[test]
    fn two_blocks_inside_a_cell_collapse_to_one_spaced_line() {
        // Multiple block children in a cell join with a space (their block break flattened), so the
        // row stays a single ` | `-joined line.
        let tree = vec![elem(
            "tr",
            vec![
                elem(
                    "td",
                    vec![
                        elem("p", vec![Text("a".into())]),
                        elem("p", vec![Text("b".into())]),
                    ],
                ),
                elem("td", vec![Text("c".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "a b | c");
    }

    #[test]
    fn a_list_item_is_not_preformatted() {
        // Pins the non-preformatted context passed into each <li>.
        let tree = vec![elem("ul", vec![elem("li", vec![Text("a   b".into())])])];
        assert_eq!(render_text(&tree), "• a b");
    }

    #[test]
    fn template_content_is_inert_and_not_rendered() {
        // <template> is parsed (so it stays queryable) but its content never reaches the output.
        let tree = vec![
            elem("template", vec![elem("p", vec![Text("hidden".into())])]),
            elem("p", vec![Text("shown".into())]),
        ];
        assert_eq!(render_text(&tree), "shown");
    }

    fn elem_attrs(tag: &str, attrs: &[(&str, &str)], children: Vec<Node>) -> Node {
        Element {
            tag: tag.into(),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            children,
        }
    }

    #[test]
    fn a_definition_list_indents_descriptions_under_terms() {
        let tree = vec![elem(
            "dl",
            vec![
                elem("dt", vec![Text("Term".into())]),
                elem("dd", vec![Text("Desc".into())]),
            ],
        )];
        assert_eq!(render_text(&tree), "Term\n  Desc");
    }

    #[test]
    fn an_image_renders_its_alt_text_in_brackets() {
        assert_eq!(
            render_text(&[elem_attrs("img", &[("alt", "a cat")], vec![])]),
            "[a cat]"
        );
    }

    #[test]
    fn an_image_without_alt_renders_nothing() {
        // Pins the `if let Some(alt)` guard: no alt → no output (not "[]").
        assert_eq!(render_text(&[elem("img", vec![])]), "");
    }

    #[test]
    fn inline_code_is_wrapped_in_backticks() {
        assert_eq!(
            render_text(&[elem("code", vec![Text("x=1".into())])]),
            "`x=1`"
        );
    }

    #[test]
    fn sub_sup_del_ins_carry_distinct_markers() {
        assert_eq!(render_text(&[elem("sub", vec![Text("2".into())])]), "~2~");
        assert_eq!(render_text(&[elem("sup", vec![Text("2".into())])]), "^2^");
        assert_eq!(render_text(&[elem("del", vec![Text("o".into())])]), "~~o~~");
        assert_eq!(render_text(&[elem("s", vec![Text("o".into())])]), "~~o~~");
        assert_eq!(
            render_text(&[elem("strike", vec![Text("o".into())])]),
            "~~o~~"
        );
        assert_eq!(render_text(&[elem("ins", vec![Text("n".into())])]), "++n++");
    }

    #[test]
    fn nested_lists_indent_by_depth() {
        let tree = vec![elem(
            "ul",
            vec![elem(
                "li",
                vec![
                    Text("a".into()),
                    elem("ul", vec![elem("li", vec![Text("b".into())])]),
                ],
            )],
        )];
        assert_eq!(render_text(&tree), "• a\n  • b");
    }

    #[test]
    fn render_text_wrapped_wraps_the_output() {
        let tree = vec![elem("p", vec![Text("the quick brown fox".into())])];
        assert_eq!(render_text_wrapped(&tree, 9), "the quick\nbrown fox");
    }

    #[test]
    fn wrap_lines_breaks_at_word_boundaries() {
        assert_eq!(wrap_lines("the quick brown fox", 9), "the quick\nbrown fox");
    }

    #[test]
    fn wrap_lines_leaves_a_short_line_untouched() {
        assert_eq!(wrap_lines("short enough", 20), "short enough");
    }

    #[test]
    fn wrap_lines_does_not_split_a_word_longer_than_the_width() {
        // A single over-long word overflows the line rather than being broken mid-word.
        assert_eq!(
            wrap_lines("supercalifragilistic go", 5),
            "supercalifragilistic\ngo"
        );
    }

    #[test]
    fn wrap_lines_preserves_existing_line_breaks() {
        assert_eq!(wrap_lines("a b\nc d", 10), "a b\nc d");
    }

    #[test]
    fn wrap_lines_counts_the_word_and_the_joining_space() {
        // "aaa bbb" is 7 columns, so at width 6 the second word must wrap. Pins the exact
        // `col + 1 + w` threshold (either `+` flipped to `-` would keep them on one line).
        assert_eq!(wrap_lines("aaa bbb", 6), "aaa\nbbb");
    }

    #[test]
    fn wrap_lines_accumulates_spaces_into_the_column() {
        // "a b c" fills width 5 exactly, so "d" wraps; only correct if each joining space advanced
        // the column. Pins the `col += 1` space accounting.
        assert_eq!(wrap_lines("a b c d", 5), "a b c\nd");
    }

    #[test]
    fn a_link_url_strips_reserved_markers() {
        // A decoded control char in href (here U+0002) must not leak — href is written raw.
        let tree = vec![elem_attrs(
            "a",
            &[("href", "a\u{2}b")],
            vec![Text("L".into())],
        )];
        assert_eq!(render_text(&tree), "L [ab]");
    }

    #[test]
    fn a_non_breaking_space_is_preserved_not_collapsed() {
        // U+00A0 is kept verbatim and does not fold with neighbours.
        assert_eq!(
            render_text(&[elem("p", vec![Text("a\u{a0}\u{a0}b".into())])]),
            "a\u{a0}\u{a0}b"
        );
        // Contrast: ordinary spaces still collapse — pins the `is_whitespace()` half of the guard.
        assert_eq!(render_text(&[elem("p", vec![Text("a  b".into())])]), "a b");
    }

    #[test]
    fn a_pre_inside_a_blockquote_prefixes_every_line() {
        let tree = vec![elem(
            "blockquote",
            vec![elem("pre", vec![Text("one\ntwo".into())])],
        )];
        assert_eq!(render_text(&tree), "> one\n> two");
    }

    #[test]
    fn wrap_repeats_a_blockquote_prefix_on_continuation_lines() {
        assert_eq!(
            wrap_lines("> alpha beta gamma", 9),
            "> alpha\n> beta\n> gamma"
        );
    }

    #[test]
    fn wrap_repeats_an_indent_prefix_on_continuation_lines() {
        assert_eq!(
            wrap_lines("  alpha beta gamma", 9),
            "  alpha\n  beta\n  gamma"
        );
    }
}
