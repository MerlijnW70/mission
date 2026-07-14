//! End-to-end pipeline: raw HTML text → tokens → DOM tree → rendered text.
//!
//! An integration test — it exercises the public API rather than defining
//! new logic), it proves the three layers compose into a working text-mode browser core.

use mission::parser::{find, find_all, parse, select};
use mission::renderer::render_text;

#[test]
fn renders_the_visible_text_of_a_small_document() {
    let html = "<html><body>Hello <b>bold</b> world</body></html>";
    assert_eq!(render_text(&parse(html)), "Hello *bold* world");
}

#[test]
fn block_paragraphs_render_on_separate_lines() {
    let html = "<p>one</p><p>two</p>";
    assert_eq!(render_text(&parse(html)), "one\ntwo");
}

#[test]
fn survives_malformed_markup_without_losing_text() {
    // Unclosed <i> (emphasis), stray </x> — the pipeline recovers and keeps the text; the
    // still-open <i> emphasises to end of input.
    let html = "start<i>mid</x>end";
    assert_eq!(render_text(&parse(html)), "start_midend_");
}

#[test]
fn decodes_entities_and_shows_link_targets() {
    let html = "<p>Rock &amp; roll &#8212; see <a href=\"/faq\">the FAQ</a></p>";
    assert_eq!(
        render_text(&parse(html)),
        "Rock & roll — see the FAQ [/faq]"
    );
}

#[test]
fn comments_are_stripped_from_the_output() {
    let html = "<p>before<!-- hidden <b>markup</b> --> after</p>";
    assert_eq!(render_text(&parse(html)), "before after");
}

#[test]
fn a_void_br_splits_a_line_and_needs_no_close() {
    let html = "<p>line one<br>line two</p>";
    assert_eq!(render_text(&parse(html)), "line one\nline two");
}

#[test]
fn script_and_style_content_never_reaches_the_output() {
    let html = "<style>p{color:red}</style><p>visible<script>alert(1 < 2)</script></p>";
    assert_eq!(render_text(&parse(html)), "visible");
}

#[test]
fn a_doctype_prefixed_document_renders_cleanly() {
    let html = "<!DOCTYPE html><html><body><h1>Hi</h1></body></html>";
    assert_eq!(render_text(&parse(html)), "# Hi");
}

#[test]
fn uppercase_and_mixed_case_markup_renders_the_same_as_lowercase() {
    let html = "<BODY><H1>Title</H1><P>text<BR>more</P></BODY>";
    assert_eq!(render_text(&parse(html)), "# Title\ntext\nmore");
}

#[test]
fn indented_multiline_source_renders_as_clean_lines() {
    let html = "\
<body>
    <h1>Title</h1>
    <p>Some    text
       wrapped across   lines.</p>
</body>";
    assert_eq!(
        render_text(&parse(html)),
        "# Title\nSome text wrapped across lines."
    );
}

#[test]
fn the_dom_can_be_queried_after_parsing() {
    let html = "<article><h1>Headline</h1>\
                <p>See <a href=\"/a\">first</a> and <a href=\"/b\">second</a>.</p></article>";
    let dom = parse(html);

    // Pull the heading's text out of the tree.
    assert_eq!(
        find(&dom, "h1").map(|n| n.text()),
        Some("Headline".to_string())
    );

    // Collect every link's destination.
    let hrefs: Vec<&str> = find_all(&dom, "a")
        .iter()
        .filter_map(|n| n.attr("href"))
        .collect();
    assert_eq!(hrefs, vec!["/a", "/b"]);
}

#[test]
fn a_css_like_selector_extracts_nested_matches() {
    let html = "<nav><a href=\"/home\">Home</a></nav>\
                <article class=\"post\">\
                  <p>Intro with a <a href=\"/inline\">link</a>.</p>\
                  <ul><li class=\"tag\">rust</li><li class=\"tag\">html</li></ul>\
                </article>";
    let dom = parse(html);

    // Links inside the article only (not the nav link).
    let article_links: Vec<&str> = select(&dom, "article a")
        .iter()
        .filter_map(|n| n.attr("href"))
        .collect();
    assert_eq!(article_links, vec!["/inline"]);

    // Every tagged list item, by class.
    let tags: Vec<String> = select(&dom, "li.tag").iter().map(|n| n.text()).collect();
    assert_eq!(tags, vec!["rust", "html"]);
}

#[test]
fn child_and_attribute_selectors_target_precisely() {
    let html = "<nav><ul><li><a href=\"/home\">Home</a></li>\
                <li><a href=\"/docs\">Docs</a></li></ul></nav>\
                <footer><a href=\"/legal\">Legal</a></footer>";
    let dom = parse(html);

    // Direct-child <li> of a <ul>, then any link inside — nav items only, not the footer link.
    let items: Vec<&str> = select(&dom, "ul > li a[href]")
        .iter()
        .filter_map(|n| n.attr("href"))
        .collect();
    assert_eq!(items, vec!["/home", "/docs"]);

    // An attribute-equals selector pinpoints one link.
    let docs: Vec<String> = select(&dom, "a[href=\"/docs\"]")
        .iter()
        .map(|n| n.text())
        .collect();
    assert_eq!(docs, vec!["Docs"]);
}

#[test]
fn sibling_selectors_pick_out_related_elements() {
    let html = "<article><h2>Intro</h2><p>first</p><p>second</p>\
                <h2>Next</h2><p>third</p></article>";
    let dom = parse(html);

    // The paragraph immediately after each heading.
    let leads: Vec<String> = select(&dom, "h2 + p").iter().map(|n| n.text()).collect();
    assert_eq!(leads, vec!["first", "third"]);

    // Every paragraph following the first heading (same parent).
    let after_first: Vec<String> = select(&dom, "h2 ~ p").iter().map(|n| n.text()).collect();
    assert_eq!(after_first, vec!["first", "second", "third"]);
}

#[test]
fn structural_pseudo_classes_select_by_position() {
    let html = "<table><tr><td>a1</td><td>a2</td></tr>\
                <tr><td>b1</td><td>b2</td></tr>\
                <tr><td>c1</td><td>c2</td></tr></table>";
    let dom = parse(html);

    // The first cell of every row.
    let firsts: Vec<String> = select(&dom, "td:first-child")
        .iter()
        .map(|n| n.text())
        .collect();
    assert_eq!(firsts, vec!["a1", "b1", "c1"]);

    // Odd rows.
    let odd_rows = select(&dom, "tr:nth-child(odd)");
    assert_eq!(
        odd_rows.iter().map(|n| n.text()).collect::<Vec<_>>(),
        vec!["a1a2", "c1c2"]
    );
}

#[test]
fn advanced_selectors_pinpoint_elements() {
    let html = "<table><tr><td>h1</td><td>h2</td></tr>\
                <tr><td>a1</td><td>a2</td></tr>\
                <tr><td>b1</td><td>b2</td></tr></table>\
                <a href=\"/docs/intro\">docs</a><a href=\"/blog\">blog</a>";
    let dom = parse(html);

    // Every second table row (An+B formula).
    let even_rows: Vec<String> = select(&dom, "tr:nth-child(2n)")
        .iter()
        .map(|n| n.text())
        .collect();
    assert_eq!(even_rows, vec!["a1a2"]);

    // Links whose href starts with /docs (prefix operator).
    let docs: Vec<String> = select(&dom, "a[href^=\"/docs\"]")
        .iter()
        .map(|n| n.text())
        .collect();
    assert_eq!(docs, vec!["docs"]);
}
