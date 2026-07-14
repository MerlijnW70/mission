//! Stage 2 of the parser: the **DOM** tree builder and the tree-query helpers.
//!
//! Folds the tokenizer's flat [`Token`] stream into a nested [`Node`] forest, and offers the
//! `find_*` queries plus the node accessors (`tag`, `attr`, `attrs`, `text`). Depends on the
//! tokenizer below it; the CSS selector engine above it builds on the `Node` type defined here.

use super::Attrs;
use super::tokenizer::{Token, attr, tokenize};

/// Maximum element nesting the tree builder will create. Beyond this, further start tags are
/// flattened to childless leaves rather than nested deeper. Real documents nest a few dozen deep;
/// this cap (far above that, far below where a recursive walk overflows) turns a hostile
/// deeply-nested document from a stack-overflow crash into bounded, still-lossless output.
const MAX_DEPTH: usize = 512;

// ---------------------------------------------------------------------------------------------
// Stage 2 — tree builder
// ---------------------------------------------------------------------------------------------

/// A node in the parsed document tree.
///
/// `#[non_exhaustive]`: variants may be added (e.g. comments) as the DOM grows, so downstream
/// `match`es must include a wildcard arm.
#[derive(Debug, PartialEq, Eq, Clone)]
#[non_exhaustive]
pub enum Node {
    /// Character data.
    Text(String),
    /// An element with a tag name, attributes, and ordered children.
    Element {
        tag: String,
        attrs: Attrs,
        children: Vec<Node>,
    },
}

/// Parse `input` into a forest of [`Node`]s (top-level siblings).
///
/// A stack holds the chain of currently-open elements, each with its attributes and the children
/// gathered so far. Text and completed elements are appended to the innermost open element, or to
/// the root forest when the stack is empty. A start tag may imply the end of open elements it
/// cannot nest in (see [`implied_end`]); an end tag closes down to the nearest matching open
/// element, completing any descendants on the way; a name that is open nowhere is ignored.
pub fn parse(input: &str) -> Vec<Node> {
    let mut roots: Vec<Node> = Vec::new();
    let mut stack: Vec<(String, Attrs, Vec<Node>)> = Vec::new();

    for token in tokenize(input) {
        match token {
            Token::Text(t) => current_children(&mut stack, &mut roots).push(Node::Text(t)),
            Token::StartTag { name, attrs, self_closing } => {
                // Implied end tags: a start tag can implicitly close open elements it cannot be
                // nested in — an unclosed <p>, a previous <li>/<td>/<tr>, etc. Close them first.
                while stack.last().is_some_and(|(open, _, _)| implied_end(open, &name)) {
                    let (tag, attrs, children) = stack.pop().expect("guarded by is_some_and above");
                    current_children(&mut stack, &mut roots)
                        .push(Node::Element { tag, attrs, children });
                }
                if self_closing || stack.len() >= MAX_DEPTH {
                    // A void/self-closing tag is a complete, childless element right away. Past the
                    // depth cap, a nesting tag is also flattened to a childless leaf instead of
                    // pushed: `parse` stays iterative and safe, and — crucially — this bounds the
                    // tree depth so the *recursive* consumers (render, select, find, text) cannot
                    // overflow the stack on hostile deeply-nested input. Text still flows into the
                    // deepest open element, so nothing is lost, only re-parented.
                    current_children(&mut stack, &mut roots)
                        .push(Node::Element { tag: name, attrs, children: Vec::new() });
                } else {
                    stack.push((name, attrs, Vec::new()));
                }
            }
            Token::EndTag(name) => {
                // Close down to the nearest matching open element, completing any still-open
                // descendants on the way — so `</ul>` closes an unclosed `<li>` first. An end tag
                // whose name is not open anywhere is a stray tag and is ignored.
                if let Some(depth) = stack.iter().rposition(|(tag, _, _)| tag == &name) {
                    while stack.len() > depth {
                        let (tag, attrs, children) =
                            stack.pop().expect("rposition guarantees an element at depth");
                        current_children(&mut stack, &mut roots)
                            .push(Node::Element { tag, attrs, children });
                    }
                }
            }
        }
    }

    // Auto-close any elements still open at end of input, innermost first.
    while let Some((tag, attrs, children)) = stack.pop() {
        current_children(&mut stack, &mut roots).push(Node::Element { tag, attrs, children });
    }
    roots
}

/// The list that the next node should be appended to: the innermost open element's children,
/// or the root forest when nothing is open.
fn current_children<'a>(
    stack: &'a mut [(String, Attrs, Vec<Node>)],
    roots: &'a mut Vec<Node>,
) -> &'a mut Vec<Node> {
    match stack.last_mut() {
        Some((_, _, children)) => children,
        None => roots,
    }
}

/// Whether an open `<open>` element is implicitly closed by a new `<new>` start tag — the common
/// HTML "optional end tag" rules. Applied in a loop, so `<td>a<tr>` closes the cell *and* the row.
fn implied_end(open: &str, new: &str) -> bool {
    match open {
        "p" => is_p_closer(new),
        "li" => new == "li",
        "dt" | "dd" => new == "dt" || new == "dd",
        "td" | "th" => matches!(new, "td" | "th" | "tr" | "thead" | "tbody" | "tfoot"),
        "tr" => matches!(new, "tr" | "thead" | "tbody" | "tfoot"),
        "thead" | "tbody" | "tfoot" => matches!(new, "thead" | "tbody" | "tfoot"),
        "option" => matches!(new, "option" | "optgroup"),
        "optgroup" => new == "optgroup",
        _ => false,
    }
}

/// Whether a `<p>` is closed by a following `<tag>` start — a `<p>` cannot contain these block
/// elements, so opening one ends the paragraph.
fn is_p_closer(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "div"
            | "ul" | "ol" | "dl" | "menu"
            | "table" | "section" | "article" | "header" | "footer" | "nav" | "main" | "aside"
            | "blockquote" | "pre" | "hr" | "form" | "fieldset" | "figure" | "figcaption"
            | "address" | "details" | "hgroup"
            | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
    )
}

// ---------------------------------------------------------------------------------------------
// DOM queries
// ---------------------------------------------------------------------------------------------

impl Node {
    /// The tag name, if this node is an element (`None` for text).
    pub fn tag(&self) -> Option<&str> {
        match self {
            Node::Element { tag, .. } => Some(tag.as_str()),
            Node::Text(_) => None,
        }
    }

    /// The value of attribute `name`, if this node is an element that carries it.
    pub fn attr(&self, name: &str) -> Option<&str> {
        match self {
            Node::Element { attrs, .. } => attr(attrs, name),
            Node::Text(_) => None,
        }
    }

    /// This node's child nodes in document order (an empty slice for a text node).
    pub fn children(&self) -> &[Node] {
        match self {
            Node::Element { children, .. } => children,
            Node::Text(_) => &[],
        }
    }

    /// All of this node's attributes as `(name, value)` pairs (empty for a text node).
    pub fn attrs(&self) -> &[(String, String)] {
        match self {
            Node::Element { attrs, .. } => attrs,
            Node::Text(_) => &[],
        }
    }

    /// Whether this element's `class` attribute lists `class` as a whole token. Classes are a
    /// whitespace-separated set, so `class="a b"` has `"a"` and `"b"` but not `"ab"`. The single
    /// definition shared by [`find_by_class`] and the CSS selector engine's class matching.
    pub fn has_class(&self, class: &str) -> bool {
        self.attr("class")
            .is_some_and(|list| list.split_whitespace().any(|c| c == class))
    }

    /// The concatenated text of this node and all its descendants, in document order — the
    /// element's plain-text content, with no layout (unlike [`crate::renderer::render_text`]).
    pub fn text(&self) -> String {
        match self {
            Node::Text(t) => t.clone(),
            Node::Element { children, .. } => {
                let mut out = String::new();
                for child in children {
                    out.push_str(&child.text());
                }
                out
            }
        }
    }
}

/// The first element in `tree` matching `pred`, searched depth-first in document (pre-)order, so
/// an ancestor is found before its descendants. The shared core of the `find_*` queries.
fn find_where<'a, F: Fn(&Node) -> bool>(tree: &'a [Node], pred: &F) -> Option<&'a Node> {
    for node in tree {
        if pred(node) {
            return Some(node);
        }
        if let Node::Element { children, .. } = node
            && let Some(found) = find_where(children, pred)
        {
            return Some(found);
        }
    }
    None
}

/// Every element in `tree` matching `pred`, in document order — a match is still descended into,
/// so nested matches are all returned. The shared core of the `find_all_*` queries.
fn collect_where<'a, F: Fn(&Node) -> bool>(tree: &'a [Node], pred: &F, out: &mut Vec<&'a Node>) {
    for node in tree {
        if pred(node) {
            out.push(node);
        }
        if let Node::Element { children, .. } = node {
            collect_where(children, pred, out);
        }
    }
}

/// The first element in `tree` with the tag name `tag`.
pub fn find<'a>(tree: &'a [Node], tag: &str) -> Option<&'a Node> {
    find_where(tree, &|node| node.tag() == Some(tag))
}

/// Every element in `tree` with the tag name `tag`, in document order.
pub fn find_all<'a>(tree: &'a [Node], tag: &str) -> Vec<&'a Node> {
    let mut out = Vec::new();
    collect_where(tree, &|node| node.tag() == Some(tag), &mut out);
    out
}

/// The first element in `tree` whose `id` attribute equals `id`.
pub fn find_by_id<'a>(tree: &'a [Node], id: &str) -> Option<&'a Node> {
    find_where(tree, &|node| node.attr("id") == Some(id))
}

/// Every element in `tree` whose `class` attribute lists `class`. Classes are a
/// whitespace-separated set, so `class="a b"` matches `"a"` and `"b"` but not `"ab"`.
pub fn find_by_class<'a>(tree: &'a [Node], class: &str) -> Vec<&'a Node> {
    let mut out = Vec::new();
    collect_where(tree, &|node| node.has_class(class), &mut out);
    out
}

#[cfg(test)]
mod tree_tests {
    use super::Node::{Element, Text};
    use super::*;

    fn elem(tag: &str, children: Vec<Node>) -> Node {
        Element { tag: tag.into(), attrs: vec![], children }
    }

    fn elem_attrs(tag: &str, attrs: &[(&str, &str)], children: Vec<Node>) -> Node {
        Element {
            tag: tag.into(),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            children,
        }
    }

    #[test]
    fn empty_input_is_an_empty_forest() {
        assert_eq!(parse(""), vec![]);
    }

    #[test]
    fn bare_text_sits_at_the_root() {
        assert_eq!(parse("hi"), vec![Text("hi".into())]);
    }

    #[test]
    fn an_empty_element_has_no_children() {
        assert_eq!(parse("<a></a>"), vec![elem("a", vec![])]);
    }

    #[test]
    fn text_becomes_a_child_of_its_element() {
        assert_eq!(parse("<a>hi</a>"), vec![elem("a", vec![Text("hi".into())])]);
    }

    #[test]
    fn elements_nest() {
        assert_eq!(
            parse("<a><b></b></a>"),
            vec![elem("a", vec![elem("b", vec![])])]
        );
    }

    #[test]
    fn an_unclosed_element_is_auto_closed_at_end() {
        assert_eq!(parse("<a>"), vec![elem("a", vec![])]);
    }

    #[test]
    fn a_stray_end_tag_is_ignored_without_panicking() {
        assert_eq!(parse("</a>"), vec![]);
    }

    #[test]
    fn a_mismatched_end_tag_does_not_close_the_element() {
        assert_eq!(parse("<a></b>c"), vec![elem("a", vec![Text("c".into())])]);
    }

    #[test]
    fn an_end_tag_closes_down_to_the_matching_open_element() {
        // The inner </ul> closes the still-open <li> first, so the following <li> lands in the
        // OUTER list — not nested one level too deep.
        assert_eq!(
            parse("<ul><li>a<ul><li>b</ul><li>c</ul>"),
            vec![elem(
                "ul",
                vec![
                    elem(
                        "li",
                        vec![
                            Text("a".into()),
                            elem("ul", vec![elem("li", vec![Text("b".into())])]),
                        ]
                    ),
                    elem("li", vec![Text("c".into())]),
                ]
            )]
        );
    }

    #[test]
    fn a_mismatched_end_tag_closes_the_named_ancestor() {
        // </b> closes <b>, completing the open <i> nested inside it; text after lands at the root.
        assert_eq!(
            parse("<b><i>x</b>y"),
            vec![elem("b", vec![elem("i", vec![Text("x".into())])]), Text("y".into())]
        );
    }

    #[test]
    fn siblings_line_up_at_the_root() {
        assert_eq!(
            parse("<a></a><b></b>"),
            vec![elem("a", vec![]), elem("b", vec![])]
        );
    }

    #[test]
    fn text_can_surround_an_element() {
        assert_eq!(
            parse("x<a>y</a>z"),
            vec![
                Text("x".into()),
                elem("a", vec![Text("y".into())]),
                Text("z".into())
            ]
        );
    }

    #[test]
    fn attributes_are_carried_into_the_element() {
        // The whole point of this step: href survives from tag interior to DOM node.
        assert_eq!(
            parse("<a href=\"/about\">about</a>"),
            vec![elem_attrs("a", &[("href", "/about")], vec![Text("about".into())])]
        );
    }

    #[test]
    fn an_explicit_self_closing_tag_takes_no_children() {
        // Text after <span/> is a sibling, not a child — pins the self-closing branch and the
        // explicit side of `explicit_close || is_void`.
        assert_eq!(
            parse("<span/>after"),
            vec![elem("span", vec![]), Text("after".into())]
        );
    }

    #[test]
    fn a_void_element_takes_no_children() {
        // <br> is void, so "after" is a sibling — pins the is_void side of the same test.
        assert_eq!(
            parse("<br>after"),
            vec![elem("br", vec![]), Text("after".into())]
        );
    }

    #[test]
    fn a_non_void_element_still_adopts_following_text() {
        // Contrast: a normal element keeps text as a child, so the self-closing branch matters.
        assert_eq!(
            parse("<div>after"),
            vec![elem("div", vec![Text("after".into())])]
        );
    }

    #[test]
    fn an_end_tag_matches_its_start_tag_regardless_of_case() {
        // <Div> ... </DIV> must close cleanly — both names lowercase to "div".
        assert_eq!(
            parse("<Div>x</DIV>"),
            vec![elem("div", vec![Text("x".into())])]
        );
    }

    #[test]
    fn a_void_element_between_text_breaks_a_paragraph() {
        // <br> nested in a paragraph closes immediately; its siblings surround it.
        assert_eq!(
            parse("<p>a<br>b</p>"),
            vec![elem(
                "p",
                vec![Text("a".into()), elem("br", vec![]), Text("b".into())]
            )]
        );
    }

    #[test]
    fn a_new_paragraph_implicitly_closes_an_open_one() {
        assert_eq!(
            parse("<p>a<p>b"),
            vec![elem("p", vec![Text("a".into())]), elem("p", vec![Text("b".into())])]
        );
    }

    #[test]
    fn a_block_start_closes_an_open_paragraph() {
        // Every p-closer must end the paragraph rather than nest inside it. One loop pins every
        // alternative of is_p_closer: drop any, and the <p> would absorb that block's content.
        for closer in [
            "p", "div", "ul", "ol", "dl", "menu", "table", "section", "article", "header",
            "footer", "nav", "main", "aside", "blockquote", "pre", "hr", "form", "fieldset",
            "figure", "figcaption", "address", "details", "hgroup", "h1", "h2", "h3", "h4", "h5",
            "h6",
        ] {
            let dom = parse(&format!("<p>a<{closer}>b"));
            assert_eq!(dom[0].tag(), Some("p"), "closer <{closer}>");
            assert_eq!(dom[0].text(), "a", "<p> must not absorb <{closer}>");
        }
    }

    #[test]
    fn a_non_closer_element_stays_inside_the_paragraph() {
        // An inline <span> is NOT a p-closer, so it nests; pins is_p_closer's `false` default (a
        // mutant returning true everywhere would wrongly split this paragraph).
        assert_eq!(
            parse("<p>a<span>b</span>c</p>"),
            vec![elem(
                "p",
                vec![Text("a".into()), elem("span", vec![Text("b".into())]), Text("c".into())]
            )]
        );
    }

    #[test]
    fn a_new_list_item_closes_the_previous_one() {
        assert_eq!(
            parse("<ul><li>a<li>b</ul>"),
            vec![elem(
                "ul",
                vec![elem("li", vec![Text("a".into())]), elem("li", vec![Text("b".into())])]
            )]
        );
    }

    #[test]
    fn definition_terms_and_descriptions_close_each_other() {
        // dt→dd and dd→dt.
        assert_eq!(
            parse("<dl><dt>a<dd>b<dt>c</dl>"),
            vec![elem(
                "dl",
                vec![
                    elem("dt", vec![Text("a".into())]),
                    elem("dd", vec![Text("b".into())]),
                    elem("dt", vec![Text("c".into())]),
                ]
            )]
        );
    }

    #[test]
    fn a_new_cell_closes_the_previous_cell() {
        // td→th (open=td) and th→td (open=th) — both sides of the cell arm.
        assert_eq!(
            parse("<table><tr><td>a<th>b<td>c</tr></table>"),
            vec![elem(
                "table",
                vec![elem(
                    "tr",
                    vec![
                        elem("td", vec![Text("a".into())]),
                        elem("th", vec![Text("b".into())]),
                        elem("td", vec![Text("c".into())]),
                    ]
                )]
            )]
        );
    }

    #[test]
    fn a_new_row_closes_the_open_cell_and_row() {
        // The loop walks up: a <tr> closes both the open <td> and the open <tr>.
        assert_eq!(
            parse("<table><tr><td>a<tr><td>b</table>"),
            vec![elem(
                "table",
                vec![
                    elem("tr", vec![elem("td", vec![Text("a".into())])]),
                    elem("tr", vec![elem("td", vec![Text("b".into())])]),
                ]
            )]
        );
    }

    #[test]
    fn a_table_section_closes_the_previous_section_row_and_cell() {
        // <tbody> after <thead> closes the thead and its open row/cell (three implied closes).
        assert_eq!(
            parse("<table><thead><tr><td>a<tbody><tr><td>b</table>"),
            vec![elem(
                "table",
                vec![
                    elem("thead", vec![elem("tr", vec![elem("td", vec![Text("a".into())])])]),
                    elem("tbody", vec![elem("tr", vec![elem("td", vec![Text("b".into())])])]),
                ]
            )]
        );
    }

    #[test]
    fn a_new_option_closes_the_previous_one() {
        assert_eq!(
            parse("<select><option>a<option>b</select>"),
            vec![elem(
                "select",
                vec![
                    elem("option", vec![Text("a".into())]),
                    elem("option", vec![Text("b".into())]),
                ]
            )]
        );
    }

    #[test]
    fn deep_nesting_is_capped_to_keep_recursive_consumers_safe() {
        fn depth(nodes: &[Node]) -> usize {
            nodes
                .iter()
                .map(|n| match n {
                    Node::Element { children, .. } => 1 + depth(children),
                    Node::Text(_) => 0,
                })
                .max()
                .unwrap_or(0)
        }
        // Exactly at the cap: everything nests, nothing flattened.
        assert_eq!(depth(&parse(&"<div>".repeat(MAX_DEPTH))), MAX_DEPTH);
        // Past the cap: depth stays bounded (deeper tags flatten to leaves at MAX_DEPTH + 1), so a
        // recursive walk cannot overflow no matter how deep the input.
        assert_eq!(depth(&parse(&"<div>".repeat(MAX_DEPTH + 5))), MAX_DEPTH + 1);
    }

    #[test]
    fn optgroups_close_options_and_each_other() {
        // option→optgroup (a new group closes the open option) and optgroup→optgroup.
        assert_eq!(
            parse("<select><option>a<optgroup>b<optgroup>c</select>"),
            vec![elem(
                "select",
                vec![
                    elem("option", vec![Text("a".into())]),
                    elem("optgroup", vec![Text("b".into())]),
                    elem("optgroup", vec![Text("c".into())]),
                ]
            )]
        );
    }
}

#[cfg(test)]
mod query_tests {
    use super::*;

    #[test]
    fn tag_names_an_element_and_is_none_for_text() {
        let tree = parse("<p>hi</p>");
        assert_eq!(tree[0].tag(), Some("p"));
        // The <p>'s child is the text node.
        if let Node::Element { children, .. } = &tree[0] {
            assert_eq!(children[0].tag(), None);
        } else {
            panic!("expected an element");
        }
    }

    #[test]
    fn attr_reads_an_elements_attribute_and_is_none_otherwise() {
        let tree = parse("<a href=\"/x\">go</a>");
        assert_eq!(tree[0].attr("href"), Some("/x"));
        assert_eq!(tree[0].attr("class"), None);
        assert_eq!(Node::Text("t".into()).attr("href"), None);
    }

    #[test]
    fn text_concatenates_all_descendant_text() {
        let tree = parse("<p>a<b>b</b>c</p>");
        // No layout, unlike render_text — just the raw text content.
        assert_eq!(tree[0].text(), "abc");
    }

    #[test]
    fn text_of_a_bare_text_node_is_itself() {
        assert_eq!(Node::Text("hello".into()).text(), "hello");
    }

    #[test]
    fn find_returns_the_first_matching_element_in_preorder() {
        let tree = parse("<div><p>one</p><p>two</p></div>");
        // Pins pre-order: the outer div is found before descending, and the first <p> wins.
        assert_eq!(find(&tree, "div").map(Node::text), Some("onetwo".into()));
        assert_eq!(find(&tree, "p").map(Node::text), Some("one".into()));
    }

    #[test]
    fn find_reaches_nested_elements_and_reports_absence() {
        let tree = parse("<div><span><b>deep</b></span></div>");
        assert_eq!(find(&tree, "b").map(Node::text), Some("deep".into()));
        assert_eq!(find(&tree, "table"), None);
    }

    #[test]
    fn find_all_returns_every_match_in_document_order() {
        let tree = parse("<ul><li>a</li><li>b</li><li>c</li></ul>");
        let texts: Vec<String> = find_all(&tree, "li").iter().map(|n| n.text()).collect();
        assert_eq!(texts, vec!["a", "b", "c"]);
    }

    #[test]
    fn find_all_includes_nested_matches_of_the_same_tag() {
        // A match is still descended into, so the inner <div> is found too.
        let tree = parse("<div><div>inner</div></div>");
        assert_eq!(find_all(&tree, "div").len(), 2);
    }

    #[test]
    fn find_all_is_empty_when_nothing_matches() {
        let tree = parse("<p>x</p>");
        assert!(find_all(&tree, "a").is_empty());
    }

    #[test]
    fn queries_compose_to_extract_link_targets() {
        let tree = parse("<p><a href=\"/one\">1</a> and <a href=\"/two\">2</a></p>");
        let hrefs: Vec<&str> = find_all(&tree, "a").iter().filter_map(|n| n.attr("href")).collect();
        assert_eq!(hrefs, vec!["/one", "/two"]);
    }

    #[test]
    fn find_by_id_returns_the_element_with_that_id() {
        let tree = parse("<div><p id=\"intro\">hi</p><p id=\"body\">bye</p></div>");
        assert_eq!(find_by_id(&tree, "body").map(Node::text), Some("bye".into()));
        assert_eq!(find_by_id(&tree, "missing"), None);
    }

    #[test]
    fn find_by_class_matches_a_whole_class_token() {
        let tree = parse("<p class=\"lead big\">a</p><p class=\"lead\">b</p><p class=\"small\">c</p>");
        let hits: Vec<String> = find_by_class(&tree, "lead").iter().map(|n| n.text()).collect();
        assert_eq!(hits, vec!["a", "b"]);
    }

    #[test]
    fn find_by_class_does_not_match_a_substring() {
        // "lead" must not match the class "leader" — classes are whole whitespace-separated tokens.
        let tree = parse("<p class=\"leader\">x</p>");
        assert!(find_by_class(&tree, "lead").is_empty());
    }

    #[test]
    fn children_returns_the_child_nodes_or_an_empty_slice() {
        let tree = parse("<div><p>x</p><p>y</p></div>");
        assert_eq!(tree[0].children().len(), 2); // the two <p> children
        assert!(Node::Text("t".into()).children().is_empty()); // a text node has no children
        assert!(parse("<br>")[0].children().is_empty()); // an empty element has none
    }

    #[test]
    fn has_class_matches_a_whole_token_only() {
        // The shared class-membership predicate: a whole token matches, a substring does not, and a
        // text node (or an element with no class) never does.
        let tree = parse("<p class=\"lead big\">x</p>");
        assert!(tree[0].has_class("lead"));
        assert!(tree[0].has_class("big"));
        assert!(!tree[0].has_class("lea")); // substring, not a token
        assert!(!Node::Text("t".into()).has_class("lead"));
        assert!(!parse("<p>y</p>")[0].has_class("lead"));
    }
}

