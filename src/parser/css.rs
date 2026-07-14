//! Stage 3 of the parser: the **CSS selector engine** (`select`).
//!
//! Parses a selector string — compounds, combinators, structural/of-type pseudo-classes,
//! `:not()`, attribute operators, and comma-separated groups — and matches it against the
//! [`Node`] tree built by the `dom` module. The outermost parser layer; nothing depends on it.

use super::dom::Node;

/// One attribute constraint inside a compound: `[href]` present; `[type="text"]` equals;
/// `[href^="/a"]` prefix; `[src$=".png"]` suffix; `[class*="col"]` substring; `[rel~="me"]`
/// whitespace-list membership; `[lang|="en"]` exact-or-dash-prefix.
enum AttrConstraint {
    Present(String),
    Equals(String, String),
    Prefix(String, String),
    Suffix(String, String),
    Substring(String, String),
    /// `~=`: the attribute is a whitespace-separated list that contains `value` as a whole token.
    Includes(String, String),
    /// `|=`: the attribute equals `value` exactly, or begins with `value` immediately followed by
    /// `-` (the CSS language-subtag match, e.g. `[lang|="en"]` matches `en` and `en-US`).
    DashMatch(String, String),
}

/// How a compound relates to the one on its left: descendant (space, `a b`), direct child
/// (`a > b`), adjacent sibling (`a + b`), or general sibling (`a ~ b`).
enum Combinator {
    Descendant,
    Child,
    AdjacentSibling,
    GeneralSibling,
}

/// The `An+B` formula of `:nth-child(...)`. A plain position `k` is `A=0, B=k`; `odd` is `2n+1`;
/// `even` is `2n`.
struct Nth {
    a: i64,
    b: i64,
}

impl Nth {
    /// Whether a 1-based element `position` satisfies the formula: is there a non-negative integer
    /// `n` with `A*n + B == position`? With `A = 0` that is just `position == B`; otherwise `q`
    /// must be a multiple of `A` and `n = q / A` non-negative — which, for exact division, means
    /// `q` shares `A`'s sign (or is zero).
    fn matches_position(&self, position: usize) -> bool {
        // `b` can be any parsed i64 (down to i64::MIN), so the subtraction must be checked — an
        // out-of-range formula that no small 1-based position could satisfy simply matches nothing.
        let Some(q) = (position as i64).checked_sub(self.b) else {
            return false;
        };
        if self.a == 0 {
            q == 0
        } else if self.a.is_positive() {
            q % self.a == 0 && q >= 0
        } else {
            q % self.a == 0 && q <= 0
        }
    }
}

/// A pseudo-class constraint: structural position among all element siblings (`:first-child`,
/// `:last-child`, `:nth-child(...)`) or among same-type siblings (`:first-of-type`,
/// `:last-of-type`, `:nth-of-type(...)`), or the negation `:not(<selector list>)`.
enum PseudoClass {
    First,
    Last,
    Nth(Nth),
    FirstOfType,
    LastOfType,
    NthOfType(Nth),
    /// `:only-child` — the sole element child of its parent (no element siblings either side).
    OnlyChild,
    /// `:only-of-type` — the sole element of its tag among its siblings.
    OnlyOfType,
    /// `:nth-last-child(...)` — like `:nth-child` but counting from the last element sibling.
    NthLast(Nth),
    /// `:nth-last-of-type(...)` — like `:nth-of-type` but counting same-type siblings from the end.
    NthLastOfType(Nth),
    /// `:has(a, b, …)` — the element has a relative match: some descendant (or, with a leading `>`,
    /// direct child) is the subject of one of these relative selectors, anchored at this element.
    /// The members are full complex selectors, so internal combinators work (`:has(.card > img)`).
    Has(Vec<Vec<Step>>),
    /// `:not(a, b, …)` — the element must match *none* of these selectors. Each member is a full
    /// complex selector (a `Vec<Step>`), so combinators work inside the negation (`:not(ul li)`).
    /// The `Vec` also breaks the recursive type cycle the boxed compound used to.
    Not(Vec<Vec<Step>>),
}

/// One level of an element's location: the sibling list it belongs to and its index within it.
/// The chain of frames from root to the element gives the matcher both ancestors and siblings.
/// `Copy` so `:has(...)` can snapshot the path to its scope element and extend it independently.
#[derive(Clone, Copy)]
struct Frame<'a> {
    siblings: &'a [Node],
    index: usize,
}

/// A compound selector — the constraints on a single element: an optional tag name, an optional
/// id, required classes, attribute constraints, and structural pseudo-classes. `*` or an empty
/// tag means "any element".
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<AttrConstraint>,
    pseudos: Vec<PseudoClass>,
}

/// A compound plus the combinator linking it to the previous compound (the first step's
/// combinator is unused).
struct Step {
    combinator: Combinator,
    compound: Compound,
}

impl Compound {
    /// Parse one compound like `li.item:nth-child(2)[data-x]` (no combinators). The tag and
    /// attribute names are lowercased to match the DOM; ids, classes, and attribute values keep
    /// their case.
    fn parse(s: &str) -> Compound {
        const MARKERS: [char; 4] = ['.', '#', '[', ':'];
        let mut compound =
            Compound { tag: None, id: None, classes: Vec::new(), attrs: Vec::new(), pseudos: Vec::new() };
        let split = s.find(MARKERS).unwrap_or(s.len());
        let name = &s[..split];
        if !name.is_empty() && name != "*" {
            compound.tag = Some(name.to_ascii_lowercase());
        }
        let mut rest = &s[split..];
        while let Some(marker) = rest.as_bytes().first().copied() {
            if marker == b'[' {
                let close = rest.find(']').unwrap_or(rest.len());
                compound.attrs.push(parse_attr_constraint(&rest[1..close]));
                rest = rest.get(close + 1..).unwrap_or("");
            } else if marker == b':' {
                rest = &rest[1..]; // step past ':'
                let end = rest.find([MARKERS[0], MARKERS[1], MARKERS[2], MARKERS[3], '(']).unwrap_or(rest.len());
                // Pseudo-class names are case-insensitive (`:FIRST-CHILD` == `:first-child`), so
                // lowercase before matching — otherwise a non-lowercase form parses to None and is
                // silently dropped, making the compound over-match instead of filtering.
                let name = rest[..end].to_ascii_lowercase();
                rest = &rest[end..];
                let arg = if rest.starts_with('(') {
                    // The matching ')' — not merely the first — so an argument may itself contain
                    // parens, e.g. `:not(:nth-child(1), h1)`.
                    let close = matching_paren(rest);
                    let a = &rest[1..close.min(rest.len())];
                    rest = rest.get(close + 1..).unwrap_or("");
                    Some(a)
                } else {
                    None
                };
                if let Some(pseudo) = parse_pseudo(&name, arg) {
                    compound.pseudos.push(pseudo);
                }
            } else {
                rest = &rest[1..]; // step past '.' or '#'
                let end = rest.find(MARKERS).unwrap_or(rest.len());
                let token = &rest[..end];
                rest = &rest[end..];
                if marker == b'#' {
                    compound.id = Some(token.to_string());
                } else if !token.is_empty() {
                    compound.classes.push(token.to_string());
                }
            }
        }
        compound
    }

    /// Whether the element at `frames[f].siblings[idx]` satisfies every constraint in this
    /// compound. The full frame stack is threaded through so a `:not(...)` pseudo can re-run the
    /// matcher for a complex selector (which needs the ancestor and sibling context); `budget` is
    /// the shared backtracking allowance that a nested `:not(...)` match draws from.
    fn matches(&self, frames: &[Frame], f: usize, idx: usize, budget: &mut u32) -> bool {
        let node = &frames[f].siblings[idx];
        if let Some(tag) = &self.tag
            && node.tag() != Some(tag.as_str())
        {
            return false;
        }
        if let Some(id) = &self.id
            && node.attr("id") != Some(id.as_str())
        {
            return false;
        }
        let classes_ok = self.classes.iter().all(|class| {
            node.attr("class")
                .is_some_and(|list| list.split_whitespace().any(|c| c == class))
        });
        let attrs_ok = self.attrs.iter().all(|constraint| match constraint {
            AttrConstraint::Present(name) => node.attr(name).is_some(),
            AttrConstraint::Equals(name, value) => node.attr(name) == Some(value.as_str()),
            // Per Selectors L4, `[att^=""]`/`[att$=""]`/`[att*=""]` with an empty value match
            // nothing — but Rust's starts_with/ends_with/contains("") all return true, so guard it.
            AttrConstraint::Prefix(name, value) => {
                !value.is_empty() && node.attr(name).is_some_and(|v| v.starts_with(value.as_str()))
            }
            AttrConstraint::Suffix(name, value) => {
                !value.is_empty() && node.attr(name).is_some_and(|v| v.ends_with(value.as_str()))
            }
            AttrConstraint::Substring(name, value) => {
                !value.is_empty() && node.attr(name).is_some_and(|v| v.contains(value.as_str()))
            }
            // `[att~=""]` matches nothing (an empty token is not a member); a value with internal
            // whitespace can never be a single token, so it also matches nothing — both fall out of
            // the whole-token compare naturally.
            AttrConstraint::Includes(name, value) => {
                !value.is_empty()
                    && node.attr(name).is_some_and(|v| v.split_whitespace().any(|t| t == value))
            }
            // `[att|=v]`: exactly `v`, or `v` immediately followed by `-`. Stripping the prefix and
            // testing for a leading `-` on the remainder needs no length arithmetic or byte
            // indexing. An empty value degrades to "equals empty, or starts with `-`", the spec
            // behavior of the empty dash-match.
            AttrConstraint::DashMatch(name, value) => node.attr(name).is_some_and(|v| {
                v == value || v.strip_prefix(value.as_str()).is_some_and(|rest| rest.starts_with('-'))
            }),
        });
        classes_ok
            && attrs_ok
            && self.pseudos.iter().all(|pseudo| pseudo.matches(frames, f, idx, &mut *budget))
    }
}

impl PseudoClass {
    /// Whether the element at `frames[f].siblings[idx]` satisfies this pseudo-class. Positions
    /// count element siblings only (text nodes are ignored), 1-based. `budget` is the shared
    /// backtracking allowance a `:not(...)` re-run draws from.
    fn matches(&self, frames: &[Frame], f: usize, idx: usize, budget: &mut u32) -> bool {
        let siblings = frames[f].siblings;
        match self {
            PseudoClass::First => prev_element(siblings, idx).is_none(),
            PseudoClass::Last => next_element(siblings, idx).is_none(),
            PseudoClass::Nth(nth) => {
                let position = siblings[..=idx].iter().filter(|n| n.tag().is_some()).count();
                nth.matches_position(position)
            }
            PseudoClass::FirstOfType => prev_of_type(siblings, idx).is_none(),
            PseudoClass::LastOfType => next_of_type(siblings, idx).is_none(),
            PseudoClass::NthOfType(nth) => {
                let target = siblings[idx].tag();
                let position = siblings[..=idx].iter().filter(|n| n.tag() == target).count();
                nth.matches_position(position)
            }
            PseudoClass::OnlyChild => {
                prev_element(siblings, idx).is_none() && next_element(siblings, idx).is_none()
            }
            PseudoClass::OnlyOfType => {
                prev_of_type(siblings, idx).is_none() && next_of_type(siblings, idx).is_none()
            }
            PseudoClass::NthLast(nth) => {
                // Count element siblings from `idx` to the end (1-based from the last).
                let position = siblings[idx..].iter().filter(|n| n.tag().is_some()).count();
                nth.matches_position(position)
            }
            PseudoClass::NthLastOfType(nth) => {
                let target = siblings[idx].tag();
                let position = siblings[idx..].iter().filter(|n| n.tag() == target).count();
                nth.matches_position(position)
            }
            // `:has(...)` — scan this element's own subtree for a subject of any relative selector,
            // anchored so the leftmost compound relates to this element by its leading combinator.
            PseudoClass::Has(group) => {
                // A private copy of the path down to this element (the scope), with the top frame
                // pinned to the element itself, extended as the scan descends into the subtree —
                // so `matches` keeps its immutable `&[Frame]` while `:has` needs to push frames.
                // `children()` is empty for the (unreachable here) text-node case, so no dead branch.
                let mut scoped: Vec<Frame> = frames[..=f].to_vec();
                scoped[f].index = idx;
                has_scan(siblings[idx].children(), group, &mut scoped, f, budget)
            }
            // The element matches `:not(...)` when it is the subject of *none* of the inner complex
            // selectors — each re-run through the full matcher against the same frame context,
            // drawing from the shared budget so a nested `:not` cannot reset the DoS bound.
            PseudoClass::Not(inner) => !inner
                .iter()
                .any(|steps| matches_from(steps, steps.len() - 1, frames, f, idx, &mut *budget)),
        }
    }
}

/// Parse a pseudo-class name and optional parenthesised argument. Unknown or malformed
/// pseudo-classes return `None` and are ignored.
fn parse_pseudo(name: &str, arg: Option<&str>) -> Option<PseudoClass> {
    match name {
        "first-child" => Some(PseudoClass::First),
        "last-child" => Some(PseudoClass::Last),
        "nth-child" => Some(PseudoClass::Nth(parse_nth(arg?)?)),
        "first-of-type" => Some(PseudoClass::FirstOfType),
        "last-of-type" => Some(PseudoClass::LastOfType),
        "nth-of-type" => Some(PseudoClass::NthOfType(parse_nth(arg?)?)),
        "only-child" => Some(PseudoClass::OnlyChild),
        "only-of-type" => Some(PseudoClass::OnlyOfType),
        "nth-last-child" => Some(PseudoClass::NthLast(parse_nth(arg?)?)),
        "nth-last-of-type" => Some(PseudoClass::NthLastOfType(parse_nth(arg?)?)),
        "has" => Some(PseudoClass::Has(parse_selector_list(arg?))),
        "not" => Some(PseudoClass::Not(parse_selector_list(arg?))),
        _ => None,
    }
}

/// Split `s` on top-level commas, keeping commas inside `[...]` or `(...)` literal (so an
/// attribute value or a nested argument is not split). Used for the `:not(a, b)` selector list;
/// pieces may carry surrounding whitespace or be empty, which the caller trims and drops.
fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                pieces.push(&s[start..i]);
                start = i + 1; // a comma is one ASCII byte, so this stays on a char boundary
            }
            _ => {}
        }
    }
    pieces.push(&s[start..]);
    pieces
}

/// Given `rest` starting at an opening `'('`, the byte index of the matching `')'`, tracking
/// nesting so an inner `(...)` does not close it early. Returns `rest.len()` if it is unbalanced.
fn matching_paren(rest: &str) -> usize {
    let mut depth = 0i32;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
    }
    rest.len()
}

/// Parse the argument of `:nth-child(...)` — `odd`, `even`, a plain integer, or an `An+B` formula
/// like `2n`, `2n+1`, `-n+3`, or `n`. Returns `None` for anything else.
fn parse_nth(arg: &str) -> Option<Nth> {
    let s = arg.trim();
    match s {
        "odd" => Some(Nth { a: 2, b: 1 }),
        "even" => Some(Nth { a: 2, b: 0 }),
        _ => match s.find(['n', 'N']) {
            Some(pos) => {
                let a = match s[..pos].trim() {
                    "" | "+" => 1,
                    "-" => -1,
                    coeff => coeff.parse().ok()?,
                };
                let tail = s[pos + 1..].replace(' ', "");
                let b = if tail.is_empty() { 0 } else { tail.parse().ok()? };
                Some(Nth { a, b })
            }
            None => Some(Nth { a: 0, b: s.parse().ok()? }),
        },
    }
}

/// Parse a bracket body: `href` → present; `type="text"` → equals; `href^="/a"` → prefix,
/// `src$=".png"` → suffix, `class*="x"` → substring, `rel~="me"` → whitespace-list membership,
/// `lang|="en"` → dash-prefix match (the operator char sits before `=`). Surrounding quotes are
/// stripped; attribute names are lowercased; values keep their case.
fn parse_attr_constraint(inner: &str) -> AttrConstraint {
    let Some((lhs, raw)) = inner.split_once('=') else {
        return AttrConstraint::Present(inner.trim().to_ascii_lowercase());
    };
    let value = raw.trim().trim_matches(['"', '\'']).to_string();
    let lhs = lhs.trim();
    let (name, op) = match lhs.chars().last() {
        Some(c @ ('^' | '$' | '*' | '~' | '|')) => (&lhs[..lhs.len() - 1], c),
        _ => (lhs, '='),
    };
    let name = name.trim().to_ascii_lowercase();
    match op {
        '^' => AttrConstraint::Prefix(name, value),
        '$' => AttrConstraint::Suffix(name, value),
        '*' => AttrConstraint::Substring(name, value),
        '~' => AttrConstraint::Includes(name, value),
        '|' => AttrConstraint::DashMatch(name, value),
        _ => AttrConstraint::Equals(name, value),
    }
}

/// The index of the nearest element sibling before `idx` in `siblings` (text nodes are skipped,
/// as CSS sibling combinators ignore them).
fn prev_element(siblings: &[Node], idx: usize) -> Option<usize> {
    (0..idx).rev().find(|&j| siblings[j].tag().is_some())
}

/// The index of the nearest element sibling after `idx` in `siblings`.
fn next_element(siblings: &[Node], idx: usize) -> Option<usize> {
    (idx + 1..siblings.len()).find(|&j| siblings[j].tag().is_some())
}

/// The nearest sibling before `idx` with the same tag as the element at `idx`.
fn prev_of_type(siblings: &[Node], idx: usize) -> Option<usize> {
    let tag = siblings[idx].tag();
    (0..idx).rev().find(|&j| siblings[j].tag() == tag)
}

/// The nearest sibling after `idx` with the same tag as the element at `idx`.
fn next_of_type(siblings: &[Node], idx: usize) -> Option<usize> {
    let tag = siblings[idx].tag();
    (idx + 1..siblings.len()).find(|&j| siblings[j].tag() == tag)
}

/// Whether the element at `frames[f].siblings[idx]` satisfies `steps[si]` and the earlier steps
/// match, following each step's combinator. Recursion walks left through the compounds and, per
/// combinator, up through ancestor frames or sideways to preceding element siblings — with
/// backtracking for the descendant and general-sibling combinators.
fn matches_from(steps: &[Step], si: usize, frames: &[Frame], f: usize, idx: usize, budget: &mut u32) -> bool {
    // The descendant and general-sibling arms backtrack over every ancestor/left-sibling with no
    // memoization, so a hostile deep/wide tree against a multi-step selector can explore
    // combinatorially many paths. `budget` caps that exploration: it is far above any real
    // selector's cost, and once exhausted the match conservatively fails rather than hanging.
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    if !steps[si].compound.matches(frames, f, idx, budget) {
        return false;
    }
    if si == 0 {
        return true; // the leftmost compound matched — the whole chain is satisfied
    }
    match steps[si].combinator {
        Combinator::Descendant => {
            (0..f).any(|af| matches_from(steps, si - 1, frames, af, frames[af].index, budget))
        }
        Combinator::Child => {
            f > 0 && matches_from(steps, si - 1, frames, f - 1, frames[f - 1].index, budget)
        }
        Combinator::AdjacentSibling => match prev_element(frames[f].siblings, idx) {
            Some(j) => matches_from(steps, si - 1, frames, f, j, budget),
            None => false,
        },
        Combinator::GeneralSibling => (0..idx)
            .filter(|&j| frames[f].siblings[j].tag().is_some())
            .any(|j| matches_from(steps, si - 1, frames, f, j, budget)),
    }
}

/// Scan a `:has(...)` scope element's subtree for any element that is the subject of any relative
/// selector in `group`, anchored to the scope at frame `scope_f`. Frames are pushed as the walk
/// descends so the relative selector's combinators see the real ancestor/sibling context; the scan
/// stops (and unwinds) at the first hit.
fn has_scan<'a>(
    tree: &'a [Node],
    group: &[Vec<Step>],
    frames: &mut Vec<Frame<'a>>,
    scope_f: usize,
    budget: &mut u32,
) -> bool {
    for (index, node) in tree.iter().enumerate() {
        if let Node::Element { children, .. } = node {
            frames.push(Frame { siblings: tree, index });
            let f = frames.len() - 1;
            let hit = {
                let fr: &[Frame] = frames;
                group.iter().any(|steps| {
                    matches_scoped(steps, steps.len() - 1, fr, f, index, scope_f, &mut *budget)
                })
            };
            if hit || has_scan(children, group, frames, scope_f, budget) {
                frames.pop();
                return true;
            }
            frames.pop();
        }
    }
    false
}

/// Like [`matches_from`], but the leftmost compound must land in the right position relative to a
/// `:has(...)` scope element at frame `scope_f` (see [`scope_reached`]) instead of simply being the
/// chain's start. Ancestor walks are bounded to the scope's subtree, so a relative selector never
/// escapes above the scope.
fn matches_scoped(
    steps: &[Step],
    si: usize,
    frames: &[Frame],
    f: usize,
    idx: usize,
    scope_f: usize,
    budget: &mut u32,
) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    if !steps[si].compound.matches(frames, f, idx, budget) {
        return false;
    }
    if si == 0 {
        // The leftmost compound matched; it must sit correctly relative to the scope element.
        return scope_reached(&steps[0].combinator, f, scope_f);
    }
    match steps[si].combinator {
        // Ancestors within the scope's subtree only (frames strictly between the scope and here).
        Combinator::Descendant => ((scope_f + 1)..f)
            .any(|af| matches_scoped(steps, si - 1, frames, af, frames[af].index, scope_f, budget)),
        Combinator::Child => {
            f > scope_f && matches_scoped(steps, si - 1, frames, f - 1, frames[f - 1].index, scope_f, budget)
        }
        Combinator::AdjacentSibling => match prev_element(frames[f].siblings, idx) {
            Some(j) => matches_scoped(steps, si - 1, frames, f, j, scope_f, budget),
            None => false,
        },
        Combinator::GeneralSibling => (0..idx)
            .filter(|&j| frames[f].siblings[j].tag().is_some())
            .any(|j| matches_scoped(steps, si - 1, frames, f, j, scope_f, budget)),
    }
}

/// Whether the leftmost matched compound (at frame `f`) sits in the right position relative to a
/// `:has(...)` scope element at frame `scope_f`, per the relative selector's leading combinator: a
/// descendant leading combinator (the default) accepts any element inside the scope's subtree; a
/// child combinator (`>`) accepts only the scope's direct children. Leading sibling combinators
/// (`:has(+ x)`, `:has(~ x)`) are not supported and never match.
fn scope_reached(combinator: &Combinator, f: usize, scope_f: usize) -> bool {
    match combinator {
        Combinator::Descendant => f > scope_f,
        Combinator::Child => f == scope_f + 1,
        Combinator::AdjacentSibling | Combinator::GeneralSibling => false,
    }
}

/// Base backtracking budget for one whole [`select`] call. A real selector uses a handful of steps
/// per node; a million is unreachable in practice but bounds an adversarial nesting/sibling bomb.
const MATCH_BUDGET: u32 = 1_000_000;

/// Extra backtracking allowance granted per element node in the tree. The whole-tree budget scales
/// with the document size, so a legitimate page of any size matches fully while the total work
/// stays linear in the node count — a hostile tree can no longer multiply a per-node explosion by
/// the number of nodes (the old per-node reset made a failing descendant selector `O(nodes × 1M)`).
const PER_ELEMENT_BUDGET: u32 = 4_096;

/// Count the element nodes in `tree` (text nodes excluded), recursing to the same bounded depth the
/// `select` walk itself uses.
fn count_elements(tree: &[Node]) -> usize {
    tree.iter()
        .map(|n| match n {
            Node::Element { children, .. } => 1 + count_elements(children),
            _ => 0,
        })
        .sum()
}

/// The single backtracking budget for one [`select`] over `tree`: a base plus a per-element share,
/// saturating at `u32::MAX` for absurdly large trees. Shared across the whole tree walk (not reset
/// per node), so it bounds total matcher work rather than per-node work.
fn element_budget(tree: &[Node]) -> u32 {
    let total =
        MATCH_BUDGET as u64 + (count_elements(tree) as u64).saturating_mul(PER_ELEMENT_BUDGET as u64);
    total.min(u32::MAX as u64) as u32
}

/// Every element in `tree` matching the CSS-like `selector`, in document order. Supported: type
/// (`div`), class (`.card`), id (`#main`), the universal `*`, attribute selectors (`[href]`,
/// `[type="text"]`), the structural pseudo-classes `:first-child`, `:last-child`, `:nth-child(...)`,
/// `:only-child`, `:nth-last-child(...)` and the same-type `:first-of-type`/`:last-of-type`/
/// `:nth-of-type(...)`/`:only-of-type`/`:nth-last-of-type(...)` (with `odd`/`even`/an integer/an
/// `An+B` formula), the relational `:has(<relative selector list>)` (a descendant match, or a
/// direct-child match with a leading `>`), the negation `:not(<selector list>)` (matching none of a
/// comma-separated list),
/// the attribute operators `^=` (prefix), `$=` (suffix), `*=` (substring), `~=` (whitespace-list
/// membership) and `|=` (dash-prefix), compounds of these (`a[href^="/docs"]:not(.ext)`), and the
/// descendant (space), child (`>`), adjacent-sibling (`+`),
/// and general-sibling (`~`) combinators. A top-level comma forms a selector group (`h1, h2`): a
/// node is returned if it matches *any* selector in the group, once, in document order.
/// Separators — commas included — are recognised only at the top level, so whitespace, commas, and
/// combinator characters inside `[...]` or `(...)` are taken literally.
///
/// A selector list (comma-separated complex selectors), with empty members dropped. Shared by the
/// top-level `select` group and the `:not(...)` argument, so both understand combinators.
fn parse_selector_list(s: &str) -> Vec<Vec<Step>> {
    split_top_level_commas(s)
        .into_iter()
        .map(parse_complex)
        .filter(|steps| !steps.is_empty())
        .collect()
}

/// Parse one complex selector — a chain of compounds joined by combinators (descendant space,
/// child `>`, adjacent `+`, general `~`) — into its ordered [`Step`]s. Separators inside `[...]`
/// or `(...)` are literal. There are no commas here; the caller has already split the list.
fn parse_complex(s: &str) -> Vec<Step> {
    let mut steps: Vec<Step> = Vec::new();
    let mut combinator = Combinator::Descendant; // links the next compound to the previous one
    let mut token = String::new(); // the compound being accumulated
    let mut depth = 0i32; // nesting inside `(...)` / `[...]`, where separators are literal

    for c in s.chars() {
        if c == '(' || c == '[' {
            depth += 1;
            token.push(c);
        } else if c == ')' || c == ']' {
            depth -= 1;
            token.push(c);
        } else if depth > 0 {
            token.push(c);
        } else if c.is_whitespace() || c == '>' || c == '+' || c == '~' {
            // A separator: close the pending compound, then note the combinator to the next one.
            if !token.is_empty() {
                steps.push(Step { combinator, compound: Compound::parse(&token) });
                token.clear();
                combinator = Combinator::Descendant;
            }
            combinator = match c {
                '>' => Combinator::Child,
                '+' => Combinator::AdjacentSibling,
                '~' => Combinator::GeneralSibling,
                _ => combinator, // whitespace keeps the descendant relation just set
            };
        } else {
            token.push(c);
        }
    }
    if !token.is_empty() {
        steps.push(Step { combinator, compound: Compound::parse(&token) });
    }
    steps
}

/// Deepest `:not()`/pseudo nesting `select` will parse; well past any real selector, well below
/// where the recursive parse/match would overflow the stack.
const MAX_SELECTOR_NESTING: i32 = 32;

/// Whether `s` ever nests parentheses deeper than `limit` — a cheap, iterative pre-check that
/// bounds the later recursive `:not()` parse and match.
fn paren_depth_exceeds(s: &str, limit: i32) -> bool {
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                if depth > limit {
                    return true;
                }
            }
            ')' => depth -= 1,
            _ => {}
        }
    }
    false
}

pub fn select<'a>(tree: &'a [Node], selector: &str) -> Vec<&'a Node> {
    // `:not()` nesting is parsed and matched recursively, so a pathologically nested selector like
    // `:not(:not(:not(…)))` would overflow the stack. Real selectors nest a level or two; reject
    // anything past the cap (as matching nothing) before recursing.
    if paren_depth_exceeds(selector, MAX_SELECTOR_NESTING) {
        return Vec::new();
    }
    // A group is the comma-separated selectors, matched as "any of"; each member is one complex
    // selector. Empty members (from stray commas) are dropped.
    let group: Vec<Vec<Step>> = parse_selector_list(selector);

    // No empty-group guard is needed: `select_into`'s `any` over an empty group matches nothing,
    // so an empty selector walks the tree and returns nothing without the special case.
    let mut out = Vec::new();
    let mut frames = Vec::new();
    // One backtracking budget for the whole walk, sized to the tree, so total matcher work is
    // bounded linearly in the node count instead of per-node (which multiplied by the node count).
    let mut budget = element_budget(tree);
    select_into(tree, &group, &mut frames, &mut budget, &mut out);
    out
}

fn select_into<'a>(
    tree: &'a [Node],
    group: &[Vec<Step>],
    frames: &mut Vec<Frame<'a>>,
    budget: &mut u32,
    out: &mut Vec<&'a Node>,
) {
    for (index, node) in tree.iter().enumerate() {
        if let Node::Element { children, .. } = node {
            frames.push(Frame { siblings: tree, index });
            let f = frames.len() - 1;
            // Walking the tree once and pushing a node the first time any selector in the group
            // matches gives document order and no duplicates, whatever the selectors' order. The
            // shared budget is drawn down across every node (never reset), bounding total work.
            if group
                .iter()
                .any(|steps| matches_from(steps, steps.len() - 1, frames, f, index, &mut *budget))
            {
                out.push(node);
            }
            select_into(children, group, frames, budget, out);
            frames.pop();
        }
    }
}


#[cfg(test)]
mod selector_tests {
    use super::*;
    use crate::parser::{Node, parse};

    fn texts(nodes: Vec<&Node>) -> Vec<String> {
        nodes.iter().map(|n| n.text()).collect()
    }

    #[test]
    fn a_type_selector_matches_by_tag() {
        let tree = parse("<div>a</div><p>b</p><div>c</div>");
        assert_eq!(texts(select(&tree, "div")), vec!["a", "c"]);
    }

    #[test]
    fn a_class_selector_matches_by_class() {
        let tree = parse("<p class=\"lead\">a</p><p>b</p><span class=\"lead\">c</span>");
        assert_eq!(texts(select(&tree, ".lead")), vec!["a", "c"]);
    }

    #[test]
    fn an_id_selector_matches_by_id() {
        let tree = parse("<p id=\"one\">a</p><p id=\"two\">b</p>");
        assert_eq!(texts(select(&tree, "#two")), vec!["b"]);
    }

    #[test]
    fn a_compound_selector_requires_every_part() {
        let tree = parse("<div class=\"card\">a</div><span class=\"card\">b</span><div>c</div>");
        // Only the <div> that also has class "card".
        assert_eq!(texts(select(&tree, "div.card")), vec!["a"]);
    }

    #[test]
    fn a_compound_can_require_several_classes() {
        let tree = parse("<div class=\"card wide\">a</div><div class=\"card\">b</div>");
        assert_eq!(texts(select(&tree, "div.card.wide")), vec!["a"]);
    }

    #[test]
    fn the_universal_selector_matches_every_element() {
        let tree = parse("<div><p>x</p></div>");
        // div and p — two elements.
        assert_eq!(select(&tree, "*").len(), 2);
    }

    #[test]
    fn a_type_selector_is_case_insensitive() {
        // Selector tag is lowercased to match the DOM's normalized names.
        let tree = parse("<SECTION>hi</SECTION>");
        assert_eq!(texts(select(&tree, "SECTION")), vec!["hi"]);
    }

    #[test]
    fn a_descendant_combinator_matches_only_inside_the_ancestor() {
        let tree = parse("<article><a href=\"/in\">in</a></article><a href=\"/out\">out</a>");
        assert_eq!(texts(select(&tree, "article a")), vec!["in"]);
    }

    #[test]
    fn a_descendant_combinator_reaches_through_intermediate_elements() {
        let tree = parse("<article><div><p><a>deep</a></p></div></article>");
        assert_eq!(texts(select(&tree, "article a")), vec!["deep"]);
    }

    #[test]
    fn a_three_step_descendant_chain_matches_in_order() {
        let tree = parse(
            "<article class=\"post\"><div class=\"card\"><a>hit</a></div></article>\
             <div class=\"card\"><a>miss</a></div>",
        );
        // <a> inside a .card inside a .post — the second <a> lacks the .post ancestor.
        assert_eq!(texts(select(&tree, ".post .card a")), vec!["hit"]);
    }

    #[test]
    fn an_empty_selector_matches_nothing() {
        let tree = parse("<div>a</div>");
        assert!(select(&tree, "   ").is_empty());
    }

    #[test]
    fn an_empty_class_token_is_ignored() {
        // A trailing '.' yields no class constraint, so "p." still matches any <p>. Pins the
        // `!token.is_empty()` guard: pushing an empty class would require an unmatchable "" class.
        let tree = parse("<p>hi</p>");
        assert_eq!(texts(select(&tree, "p.")), vec!["hi"]);
    }

    #[test]
    fn a_descendant_combinator_rejects_a_wrong_type_ancestor() {
        // <a> sits in a <div>, not an <article> — "article a" must not match it. Pins the
        // ancestor-match test in matches_from (a mutant that "matches" any ancestor would hit).
        let tree = parse("<div><a>x</a></div>");
        assert!(select(&tree, "article a").is_empty());
    }

    #[test]
    fn a_child_combinator_matches_only_direct_children() {
        let tree = parse("<ul><li>a</li><li>b</li></ul>");
        assert_eq!(texts(select(&tree, "ul > li")), vec!["a", "b"]);
    }

    #[test]
    fn a_child_combinator_rejects_a_deeper_descendant() {
        // The <li> is a grandchild of <ul>, so "ul > li" must not match, but "ul li" must.
        let tree = parse("<ul><div><li>deep</li></div></ul>");
        assert!(select(&tree, "ul > li").is_empty());
        assert_eq!(texts(select(&tree, "ul li")), vec!["deep"]);
    }

    #[test]
    fn a_child_combinator_rejects_a_top_level_target_with_no_parent() {
        // A top-level <li> has no ancestors, so "ul > li" must not match. Pins the `pi > 0` guard:
        // a `pi >= 0` mutant would underflow the ancestor index and panic.
        let tree = parse("<li>top</li>");
        assert!(select(&tree, "ul > li").is_empty());
    }

    #[test]
    fn a_child_combinator_can_chain_with_a_descendant() {
        // "ul > li a": an <a> anywhere inside an <li> that is a direct child of a <ul>.
        let tree = parse("<ul><li><span><a>hit</a></span></li></ul>");
        assert_eq!(texts(select(&tree, "ul > li a")), vec!["hit"]);
    }

    #[test]
    fn an_attribute_presence_selector_matches_elements_that_have_it() {
        let tree = parse("<a href=\"/x\">yes</a><a>no</a>");
        assert_eq!(texts(select(&tree, "[href]")), vec!["yes"]);
    }

    #[test]
    fn an_attribute_equals_selector_matches_the_value() {
        let tree = parse("<p data-role=\"main\">a</p><p data-role=\"aside\">b</p>");
        assert_eq!(texts(select(&tree, "[data-role=\"main\"]")), vec!["a"]);
    }

    #[test]
    fn an_attribute_value_may_be_unquoted() {
        let tree = parse("<p data-role=\"main\">a</p><p data-role=\"aside\">b</p>");
        assert_eq!(texts(select(&tree, "[data-role=main]")), vec!["a"]);
    }

    #[test]
    fn an_attribute_constraint_combines_with_the_rest_of_the_compound() {
        // Only an <a> that also carries href.
        let tree = parse("<a href=\"/x\">a</a><a>b</a><span href=\"/y\">c</span>");
        assert_eq!(texts(select(&tree, "a[href]")), vec!["a"]);
    }

    #[test]
    fn an_attribute_equals_selector_rejects_a_different_value() {
        let tree = parse("<p data-role=\"main\">a</p>");
        assert!(select(&tree, "[data-role=\"other\"]").is_empty());
    }

    #[test]
    fn a_separator_inside_brackets_is_taken_literally() {
        // A space inside an attribute value must not split the selector. Pins depth tracking.
        let tree = parse("<p data-role=\"top bar\">a</p><p data-role=\"top\">b</p>");
        assert_eq!(texts(select(&tree, "[data-role=\"top bar\"]")), vec!["a"]);
    }

    #[test]
    fn attribute_prefix_suffix_and_substring_operators_match() {
        let tree = parse("<a href=\"/docs/intro\">a</a><a href=\"pic.png\">b</a><a href=\"/x/api/y\">c</a>");
        assert_eq!(texts(select(&tree, "[href^=\"/docs\"]")), vec!["a"]); // starts with
        assert_eq!(texts(select(&tree, "[href$=\".png\"]")), vec!["b"]); // ends with
        assert_eq!(texts(select(&tree, "[href*=\"api\"]")), vec!["c"]); // contains
    }

    #[test]
    fn attribute_operators_are_distinct_from_each_other() {
        // "/docs" starts with "/doc" but does not end with or equal it.
        let tree = parse("<a href=\"/docs\">a</a>");
        assert_eq!(texts(select(&tree, "[href^=\"/doc\"]")), vec!["a"]);
        assert!(select(&tree, "[href$=\"/doc\"]").is_empty());
        assert!(select(&tree, "[href=\"/doc\"]").is_empty());
    }

    #[test]
    fn a_bracket_compound_can_be_followed_by_a_combinator() {
        // ']' must close the bracket depth so the following space is a descendant combinator.
        let tree = parse("<div data-x=\"y\"><a>hit</a></div><a>miss</a>");
        assert_eq!(texts(select(&tree, "[data-x=\"y\"] a")), vec!["hit"]);
    }

    #[test]
    fn a_paren_compound_can_be_followed_by_a_combinator() {
        // ')' must close the paren depth so the following space is a descendant combinator.
        let tree = parse("<ul><li>a<span>s</span></li><li>b<span>t</span></li></ul>");
        assert_eq!(texts(select(&tree, "li:nth-child(1) span")), vec!["s"]);
    }

    #[test]
    fn an_adjacent_sibling_matches_the_immediately_following_element() {
        let tree = parse("<h2>a</h2><p>b</p><p>c</p>");
        // Only the first <p> immediately follows the <h2>.
        assert_eq!(texts(select(&tree, "h2 + p")), vec!["b"]);
    }

    #[test]
    fn an_adjacent_sibling_rejects_a_gap() {
        // A <div> sits between, so "h2 + p" must not match — but "h2 ~ p" (general) must.
        let tree = parse("<h2>a</h2><div>x</div><p>b</p>");
        assert!(select(&tree, "h2 + p").is_empty());
        assert_eq!(texts(select(&tree, "h2 ~ p")), vec!["b"]);
    }

    #[test]
    fn an_adjacent_sibling_skips_intervening_text() {
        // The whitespace text node between the tags is ignored by the combinator.
        let tree = parse("<h2>a</h2> <p>b</p>");
        assert_eq!(texts(select(&tree, "h2 + p")), vec!["b"]);
    }

    #[test]
    fn a_general_sibling_matches_every_following_sibling() {
        let tree = parse("<h2>a</h2><p>b</p><span>x</span><p>c</p>");
        assert_eq!(texts(select(&tree, "h2 ~ p")), vec!["b", "c"]);
    }

    #[test]
    fn a_general_sibling_ignores_elements_before_the_reference() {
        // The first <p> comes before the <h2>, so only the trailing <p> qualifies.
        let tree = parse("<p>b</p><h2>a</h2><p>c</p>");
        assert_eq!(texts(select(&tree, "h2 ~ p")), vec!["c"]);
    }

    #[test]
    fn a_sibling_combinator_requires_a_shared_parent() {
        // The <h2> is nested in a <div>, so the top-level <p> is not its sibling.
        let tree = parse("<div><h2>a</h2></div><p>b</p>");
        assert!(select(&tree, "h2 + p").is_empty());
        assert!(select(&tree, "h2 ~ p").is_empty());
    }

    #[test]
    fn a_sibling_combinator_needs_a_matching_reference_sibling() {
        let tree = parse("<span>x</span><p>b</p>");
        // No <h2> precedes the <p>.
        assert!(select(&tree, "h2 + p").is_empty());
    }

    #[test]
    fn an_adjacent_sibling_rejects_a_first_element_with_no_predecessor() {
        // The <p> is first, so there is no preceding sibling at all. Pins the `None => false`
        // arm: a `None => true` mutant would match it.
        let tree = parse("<p>b</p>");
        assert!(select(&tree, "h2 + p").is_empty());
    }

    #[test]
    fn first_child_matches_only_the_first_element_child() {
        let tree = parse("<ul><li>a</li><li>b</li></ul>");
        assert_eq!(texts(select(&tree, "li:first-child")), vec!["a"]);
    }

    #[test]
    fn last_child_matches_only_the_last_element_child() {
        let tree = parse("<ul><li>a</li><li>b</li></ul>");
        assert_eq!(texts(select(&tree, "li:last-child")), vec!["b"]);
    }

    #[test]
    fn nth_child_matches_a_one_based_position() {
        let tree = parse("<ul><li>a</li><li>b</li><li>c</li></ul>");
        assert_eq!(texts(select(&tree, "li:nth-child(2)")), vec!["b"]);
    }

    #[test]
    fn nth_child_odd_and_even_select_alternating_elements() {
        let tree = parse("<ul><li>1</li><li>2</li><li>3</li><li>4</li></ul>");
        assert_eq!(texts(select(&tree, "li:nth-child(odd)")), vec!["1", "3"]);
        assert_eq!(texts(select(&tree, "li:nth-child(even)")), vec!["2", "4"]);
    }

    #[test]
    fn nth_child_supports_an_plus_b_formulas() {
        let tree = parse("<ul><li>1</li><li>2</li><li>3</li><li>4</li><li>5</li></ul>");
        assert_eq!(texts(select(&tree, "li:nth-child(2n)")), vec!["2", "4"]); // A=2, B=0
        assert_eq!(texts(select(&tree, "li:nth-child(2n+1)")), vec!["1", "3", "5"]); // A=2, B=1
        assert_eq!(texts(select(&tree, "li:nth-child(n+3)")), vec!["3", "4", "5"]); // from 3 on
        assert_eq!(texts(select(&tree, "li:nth-child(-n+2)")), vec!["1", "2"]); // first 2
    }

    #[test]
    fn structural_position_ignores_text_siblings() {
        // Leading whitespace text does not make the first <li> stop being the first child.
        let tree = parse("<ul> <li>a</li> <li>b</li> </ul>");
        assert_eq!(texts(select(&tree, "li:first-child")), vec!["a"]);
        assert_eq!(texts(select(&tree, "li:nth-child(1)")), vec!["a"]);
    }

    #[test]
    fn a_pseudo_class_combines_with_the_tag() {
        // The first child is a <span>, so "p:first-child" matches nothing but "span:first-child"
        // does — the position and the tag must both hold.
        let tree = parse("<div><span>x</span><p>y</p></div>");
        assert!(select(&tree, "p:first-child").is_empty());
        assert_eq!(texts(select(&tree, "span:first-child")), vec!["x"]);
    }

    #[test]
    fn a_pseudo_class_composes_with_a_combinator() {
        let tree = parse("<ul><li><a>first</a></li><li><a>second</a></li></ul>");
        // A link inside the first <li> only.
        assert_eq!(texts(select(&tree, "li:first-child a")), vec!["first"]);
    }

    #[test]
    fn not_negates_a_compound() {
        let tree = parse("<a class=\"external\">a</a><a>b</a>");
        // Every <a> that does NOT carry the "external" class.
        assert_eq!(texts(select(&tree, "a:not(.external)")), vec!["b"]);
    }

    #[test]
    fn not_can_negate_an_attribute() {
        let tree = parse("<a href=\"/x\">a</a><a>b</a>");
        assert_eq!(texts(select(&tree, "a:not([href])")), vec!["b"]);
    }

    #[test]
    fn not_with_a_selector_list_excludes_every_member() {
        // `:not(h1, h2)` matches an element that is neither. Pins the list parse (a dropped final
        // member would leak h2 through) and the `any`-of-none semantics (`all` would leak h1).
        let tree = parse("<h1>a</h1><h2>b</h2><p>c</p>");
        assert_eq!(texts(select(&tree, "*:not(h1, h2)")), vec!["c"]);
    }

    #[test]
    fn not_list_keeps_a_comma_inside_brackets_literal() {
        // The comma sits inside an attribute value (depth > 0), so the list is one member, not two.
        let tree = parse("<p data-x=\"a,b\">x</p><p>y</p>");
        assert_eq!(texts(select(&tree, "p:not([data-x=\"a,b\"])")), vec!["y"]);
    }

    #[test]
    fn not_list_keeps_a_comma_inside_nested_parens_literal() {
        // The list separator must be found at depth 0, past a balanced `(...)`. Pins the paren
        // arm of the depth counter: `:nth-child(1)` and `h1` are two members here.
        let tree = parse("<h1>a</h1><p>b</p><span>c</span>");
        assert_eq!(texts(select(&tree, "*:not(:nth-child(1), h1)")), vec!["b", "c"]);
    }

    #[test]
    fn not_list_ignores_empty_members() {
        // Leading/trailing/doubled commas yield empty members that must be dropped — kept, an
        // empty member would parse as the universal `*` and make `:not` match nothing.
        let tree = parse("<h1>a</h1><p>b</p>");
        assert_eq!(texts(select(&tree, "*:not(h1,,)")), vec!["b"]);
    }

    #[test]
    fn not_list_trims_member_whitespace() {
        // Members carry the spaces around the commas; without trimming, " h1 " matches no tag and
        // the negation would exclude nothing.
        let tree = parse("<h1>a</h1><p>b</p>");
        assert_eq!(texts(select(&tree, "*:not( h1 , h2 )")), vec!["b"]);
    }

    #[test]
    fn not_with_a_descendant_combinator() {
        // `:not(ul li)` excludes an li that descends from a ul; the li under <ol> survives. The
        // negation runs the full matcher, so the ancestor walk works inside it.
        let tree = parse("<ul><li>a</li></ul><ol><li>b</li></ol>");
        assert_eq!(texts(select(&tree, "li:not(ul li)")), vec!["b"]);
    }

    #[test]
    fn not_with_a_child_combinator() {
        // The direct-child `>` inside the negation: only the <p> that is a child of <div> is out.
        let tree = parse("<div><p>a</p></div><section><p>b</p></section>");
        assert_eq!(texts(select(&tree, "p:not(div > p)")), vec!["b"]);
    }

    #[test]
    fn not_with_an_adjacent_sibling_combinator() {
        // `h2 + p` inside the negation excludes only the <p> immediately after the <h2>.
        let tree = parse("<h2>x</h2><p>a</p><p>b</p>");
        assert_eq!(texts(select(&tree, "p:not(h2 + p)")), vec!["b"]);
    }

    #[test]
    fn not_with_a_general_sibling_combinator() {
        // `h2 ~ p` inside the negation excludes every <p> that follows the <h2>; the one before it
        // survives.
        let tree = parse("<p>a</p><h2>x</h2><p>b</p>");
        assert_eq!(texts(select(&tree, "p:not(h2 ~ p)")), vec!["a"]);
    }

    #[test]
    fn not_list_mixes_a_combinator_member_and_a_simple_member() {
        // A list where one member has a combinator and another is a class: an element is excluded
        // if it matches either.
        let tree = parse("<div><p>a</p></div><p class=\"skip\">b</p><p>c</p>");
        assert_eq!(texts(select(&tree, "p:not(div p, .skip)")), vec!["c"]);
    }

    #[test]
    fn nested_negation_cancels_out() {
        // `:not(:not(p))` is `p`: the inner negation is re-run through the matcher recursively.
        let tree = parse("<p>a</p><div>b</div>");
        assert_eq!(texts(select(&tree, "*:not(:not(p))")), vec!["a"]);
    }

    #[test]
    fn not_composes_with_another_pseudo_class() {
        let tree = parse("<ul><li>a</li><li>b</li><li>c</li></ul>");
        // Every <li> except the first.
        assert_eq!(texts(select(&tree, "li:not(:first-child)")), vec!["b", "c"]);
    }

    #[test]
    fn first_of_type_matches_the_first_element_of_its_tag() {
        let tree = parse("<div><h2>h</h2><p>a</p><p>b</p></div>");
        // <p> is the 2nd child but the 1st <p>: :first-child misses, :first-of-type hits.
        assert!(select(&tree, "p:first-child").is_empty());
        assert_eq!(texts(select(&tree, "p:first-of-type")), vec!["a"]);
    }

    #[test]
    fn last_of_type_matches_the_last_element_of_its_tag() {
        let tree = parse("<div><p>a</p><p>b</p><span>s</span></div>");
        assert_eq!(texts(select(&tree, "p:last-of-type")), vec!["b"]);
    }

    #[test]
    fn nth_of_type_counts_only_same_tag_siblings() {
        let tree = parse("<div><p>a</p><span>x</span><p>b</p><p>c</p></div>");
        // The 2nd <p>, skipping the intervening <span>.
        assert_eq!(texts(select(&tree, "p:nth-of-type(2)")), vec!["b"]);
        assert_eq!(texts(select(&tree, "p:nth-of-type(odd)")), vec!["a", "c"]);
    }

    #[test]
    fn a_group_matches_any_of_its_selectors() {
        // A comma is a group separator: an element matches if it satisfies either member. A
        // dropped final push, or `any` weakened to `all`, would lose one of the two tags.
        let tree = parse("<h1>a</h1><p>b</p><h2>c</h2>");
        assert_eq!(texts(select(&tree, "h1, h2")), vec!["a", "c"]);
    }

    #[test]
    fn a_group_returns_matches_in_document_order_not_selector_order() {
        // The tree is walked once, so the <h1> comes before the <h2> even though the group lists
        // h2 first. A per-selector-then-concatenate approach would return them h2, h1.
        let tree = parse("<h1>a</h1><h2>c</h2>");
        assert_eq!(texts(select(&tree, "h2, h1")), vec!["a", "c"]);
    }

    #[test]
    fn a_group_reports_a_node_matching_both_selectors_only_once() {
        // The <div class="x"> satisfies both members; walking once and pushing on first match
        // dedupes it. A concatenation of per-selector results would list it twice.
        let tree = parse("<div class=\"x\">hi</div>");
        assert_eq!(texts(select(&tree, "div, .x")), vec!["hi"]);
    }

    #[test]
    fn each_member_of_a_group_carries_its_own_combinators() {
        // The members are full complex selectors, not bare tags: "ul > li" and "span" are parsed
        // independently, so the group is not confused by one member's combinator.
        let tree = parse("<ul><li>a</li></ul><span>s</span><li>loose</li>");
        assert_eq!(texts(select(&tree, "ul > li, span")), vec!["a", "s"]);
    }

    #[test]
    fn a_comma_inside_brackets_is_not_a_group_separator() {
        // The comma sits inside an attribute value at depth > 0, so it must stay literal and the
        // whole thing remains one selector. Pins the depth guard for the comma branch.
        let tree = parse("<p data-x=\"a,b\">hit</p><p data-x=\"a\">miss</p>");
        assert_eq!(texts(select(&tree, "[data-x=\"a,b\"]")), vec!["hit"]);
    }

    #[test]
    fn a_comma_inside_parens_is_not_a_group_separator() {
        // A comma inside `:not(...)` is at depth > 0 and must not split the group. Here the inner
        // argument never matches a real tag, so `:not(...)` keeps the <p>; a wrongful split would
        // instead treat the fragments as separate selectors.
        let tree = parse("<p>hit</p>");
        assert_eq!(texts(select(&tree, "p:not(x, y)")), vec!["hit"]);
    }

    #[test]
    fn empty_members_in_a_group_are_ignored() {
        // Leading, trailing, and doubled commas produce empty selectors that must be skipped, not
        // pushed as empty step lists (which would underflow in matching). The <p> is a decoy: were
        // an empty member kept, `Compound::parse("")` would be a universal `*` and wrongly match it.
        let tree = parse("<h1>a</h1><p>x</p><h2>b</h2>");
        assert_eq!(texts(select(&tree, ", h1 ,, h2 ,")), vec!["a", "b"]);
    }

    #[test]
    fn an_empty_operator_value_matches_nothing() {
        // Per Selectors L4, [att^=""]/[att$=""]/[att*=""] match nothing — not every element with
        // the attribute (Rust's starts_with/ends_with/contains("") would otherwise all be true).
        let tree = parse("<a href=\"/x\">A</a><p>B</p>");
        assert!(select(&tree, "[href^=\"\"]").is_empty());
        assert!(select(&tree, "[href$=\"\"]").is_empty());
        assert!(select(&tree, "[href*=\"\"]").is_empty());
        // A non-empty value still matches.
        assert_eq!(texts(select(&tree, "[href^=\"/\"]")), vec!["A"]);
    }

    #[test]
    fn nth_child_with_an_out_of_range_offset_matches_nothing_without_panicking() {
        // `b` = i64::MIN would overflow `position - b`; checked_sub makes it match nothing.
        let tree = parse("<ul><li>a</li><li>b</li></ul>");
        assert!(select(&tree, "li:nth-child(-9223372036854775808)").is_empty());
        // A normal formula still works alongside the guard.
        assert_eq!(texts(select(&tree, "li:nth-child(2)")), vec!["b"]);
    }

    #[test]
    fn the_backtracking_budget_fails_closed_when_exhausted() {
        // A zero budget must return false regardless of the compound; a fresh budget matches. Pins
        // the `*budget == 0` guard and the decrement (private matcher, called directly).
        let tree = parse("<a>x</a>");
        let frames = vec![Frame { siblings: &tree, index: 0 }];
        let steps = parse_complex("a");
        let mut exhausted = 0u32;
        assert!(!matches_from(&steps, 0, &frames, 0, 0, &mut exhausted));
        let mut fresh = MATCH_BUDGET;
        assert!(matches_from(&steps, 0, &frames, 0, 0, &mut fresh));
        assert_eq!(fresh, MATCH_BUDGET - 1); // exactly one step consumed
    }

    #[test]
    fn paren_depth_guard_bounds_recursive_selector_nesting() {
        assert!(!paren_depth_exceeds("a:not(b)", MAX_SELECTOR_NESTING)); // depth 1
        assert!(!paren_depth_exceeds(&"(".repeat(MAX_SELECTOR_NESTING as usize), MAX_SELECTOR_NESTING));
        assert!(paren_depth_exceeds(&"(".repeat(MAX_SELECTOR_NESTING as usize + 1), MAX_SELECTOR_NESTING));
        // Many flat `()` pairs stay at depth 1 — pins the `)` decrement (without it, depth would
        // accumulate and falsely trip the guard).
        assert!(!paren_depth_exceeds(&"()".repeat(MAX_SELECTOR_NESTING as usize + 10), MAX_SELECTOR_NESTING));
    }

    #[test]
    fn a_selector_nested_past_the_cap_matches_nothing_without_overflowing() {
        // Just past the nesting cap → guarded to an empty result (kept small: with the guard
        // disabled this still parses without a crash, so it stays a fast, deterministic kill).
        let tree = parse("<p>hi</p>");
        let over = MAX_SELECTOR_NESTING as usize + 8;
        let bomb = format!("{}p{}", ":not(".repeat(over), ")".repeat(over));
        assert!(select(&tree, &bomb).is_empty());
        // A legitimately nested :not still works.
        assert_eq!(texts(select(&tree, "p:not(:not(p))")), vec!["hi"]);
    }

    #[test]
    fn a_pseudo_class_name_is_matched_case_insensitively() {
        // `:FIRST-CHILD` must behave exactly like `:first-child`; before the lowercasing fix an
        // upper-case pseudo parsed to nothing and was silently dropped, so `li:FIRST-CHILD` wrongly
        // matched every <li>.
        let tree = parse("<ul><li>a</li><li>b</li></ul>");
        assert_eq!(texts(select(&tree, "li:FIRST-CHILD")), vec!["a"]);
        assert_eq!(texts(select(&tree, "li:Nth-Child(2)")), vec!["b"]);
    }

    #[test]
    fn an_attribute_includes_operator_matches_a_whitespace_token() {
        // `~=` matches a whole token in a whitespace-separated list, not a substring.
        let tree = parse("<a rel=\"nofollow me author\">a</a><a rel=\"nofollowme\">b</a>");
        assert_eq!(texts(select(&tree, "[rel~=\"me\"]")), vec!["a"]);
        // A substring of a token does not match; an empty value matches nothing.
        assert!(select(&tree, "[rel~=\"follow\"]").is_empty());
        assert!(select(&tree, "[rel~=\"\"]").is_empty());
    }

    #[test]
    fn an_attribute_dash_match_operator_matches_a_value_or_its_dash_prefix() {
        // `|=` matches the exact value or the value followed by `-` (the language-subtag rule).
        let tree = parse("<p lang=\"en\">a</p><p lang=\"en-US\">b</p><p lang=\"english\">c</p>");
        assert_eq!(texts(select(&tree, "[lang|=\"en\"]")), vec!["a", "b"]);
        // `english` starts with `en` but not `en-`, so it must not match.
        assert!(select(&tree, "[lang|=\"fr\"]").is_empty());
    }

    #[test]
    fn only_child_matches_the_sole_element_child() {
        let tree = parse("<div><p>alone</p></div><section><p>a</p><p>b</p></section>");
        // The <p> in <div> is an only child; the two in <section> are not.
        assert_eq!(texts(select(&tree, "p:only-child")), vec!["alone"]);
    }

    #[test]
    fn only_of_type_matches_the_sole_element_of_its_tag() {
        // The <p> is the only <p> among its siblings even though a <span> sits beside it.
        let tree = parse("<div><span>x</span><p>solo</p></div><div><p>a</p><p>b</p></div>");
        assert_eq!(texts(select(&tree, "p:only-of-type")), vec!["solo"]);
    }

    #[test]
    fn nth_last_child_counts_from_the_end() {
        let tree = parse("<ul><li>1</li><li>2</li><li>3</li></ul>");
        assert_eq!(texts(select(&tree, "li:nth-last-child(1)")), vec!["3"]); // last
        assert_eq!(texts(select(&tree, "li:nth-last-child(2)")), vec!["2"]);
        assert_eq!(texts(select(&tree, "li:nth-last-child(odd)")), vec!["1", "3"]);
    }

    #[test]
    fn nth_last_of_type_counts_same_type_siblings_from_the_end() {
        let tree = parse("<div><p>a</p><span>x</span><p>b</p><p>c</p></div>");
        // Counting <p>s from the end: c=1, b=2, a=3 (the <span> is skipped).
        assert_eq!(texts(select(&tree, "p:nth-last-of-type(1)")), vec!["c"]);
        assert_eq!(texts(select(&tree, "p:nth-last-of-type(3)")), vec!["a"]);
    }

    #[test]
    fn count_elements_counts_only_elements_across_the_tree() {
        // div > (p, span > b) plus a text node = 4 elements. Pins the element budget's scaling input.
        let tree = parse("<div><p>x</p><span><b>y</b></span>text</div>");
        assert_eq!(count_elements(&tree), 4);
    }

    #[test]
    fn has_matches_an_element_with_a_matching_descendant() {
        let tree = parse("<section><p><a>x</a></p></section><section><p>y</p></section>");
        // Only the first <section> contains an <a> somewhere in its subtree.
        assert_eq!(texts(select(&tree, "section:has(a)")), vec!["x"]);
    }

    #[test]
    fn has_with_no_relative_match_selects_nothing() {
        let tree = parse("<div><p>x</p></div>");
        assert!(select(&tree, "div:has(a)").is_empty());
    }

    #[test]
    fn has_child_combinator_requires_a_direct_child() {
        // The first <ul> has a direct <li>; the second's <li> is nested under a <div>. `:has(> li)`
        // matches only the first, while the descendant form `:has(li)` matches both. Pins
        // scope_reached's Child (direct-child) vs Descendant (any-descendant) distinction.
        let tree = parse("<ul>a<li>A</li></ul><ul><div><li>B</li></div></ul>");
        assert_eq!(texts(select(&tree, "ul:has(> li)")), vec!["aA"]);
        assert_eq!(select(&tree, "ul:has(li)").len(), 2);
    }

    #[test]
    fn has_supports_an_internal_descendant_combinator() {
        // `:has(div a)`: the container must hold an <a> that descends from a <div> inside it — the
        // bare <a> in the second article does not qualify.
        let tree = parse("<article><div><a>hit</a></div></article><article><a>miss</a></article>");
        assert_eq!(texts(select(&tree, "article:has(div a)")), vec!["hit"]);
    }

    #[test]
    fn has_child_chain_matches_nested_direct_children() {
        // `:has(> div > p)`: a direct-child <div> that itself has a direct-child <p>. The second
        // section's <p> is a grandchild of its <div>, so it does not match.
        let tree = parse(
            "<section><div><p>x</p></div></section>\
             <section><div><span><p>y</p></span></div></section>",
        );
        assert_eq!(texts(select(&tree, "section:has(> div > p)")), vec!["x"]);
    }

    #[test]
    fn has_composes_with_the_tag_and_a_class() {
        // A `.card` that also has an <img> descendant: the plain <div> (no class) and the empty
        // `.card` are both excluded.
        let tree = parse("<div class=\"card\"><img></div><div><img></div><div class=\"card\">no</div>");
        assert_eq!(select(&tree, "div.card:has(img)").len(), 1);
    }

    #[test]
    fn has_can_be_negated() {
        // `:not(:has(a))` — elements that do NOT contain an <a>. The relative match runs through the
        // full matcher inside the negation.
        let tree = parse("<li><a>x</a></li><li>plain</li>");
        assert_eq!(texts(select(&tree, "li:not(:has(a))")), vec!["plain"]);
    }

    #[test]
    fn has_with_a_leading_sibling_combinator_matches_nothing() {
        // Leading `+`/`~` inside `:has` are unsupported: such a selector matches nothing rather than
        // misbehaving. Pins scope_reached's sibling arm.
        let tree = parse("<h2>a</h2><p>b</p>");
        assert!(select(&tree, "h2:has(+ p)").is_empty());
    }

    #[test]
    fn has_internal_adjacent_sibling_needs_a_preceding_element() {
        // `:has(h2 + p)`: a section with an <h2> immediately before a <p>. A lone <p> (no preceding
        // element sibling) must NOT match — pins the `None => false` arm of the adjacent combinator.
        let tree = parse("<section><h2>t</h2><p>x</p></section><section><p>y</p></section>");
        assert_eq!(texts(select(&tree, "section:has(h2 + p)")), vec!["tx"]);
    }

    #[test]
    fn has_internal_general_sibling_matches_a_later_sibling() {
        // `:has(h2 ~ p)`: an <h2> with a following <p> sibling (a <div> may sit between). The second
        // section has no <p> at all, so it does not match.
        let tree = parse(
            "<section><h2>t</h2><div>d</div><p>x</p></section>\
             <section><h2>t</h2><div>y</div></section>",
        );
        assert_eq!(select(&tree, "section:has(h2 ~ p)").len(), 1);
    }

    #[test]
    fn scope_reached_enforces_the_leading_combinator_position() {
        // Descendant accepts any frame strictly below the scope (pins `>` vs `>=`); Child only the
        // frame exactly one below; leading sibling combinators never match.
        assert!(scope_reached(&Combinator::Descendant, 2, 1));
        assert!(!scope_reached(&Combinator::Descendant, 1, 1)); // same frame as scope: not below
        assert!(scope_reached(&Combinator::Child, 2, 1));
        assert!(!scope_reached(&Combinator::Child, 3, 1));
        assert!(!scope_reached(&Combinator::AdjacentSibling, 2, 1));
        assert!(!scope_reached(&Combinator::GeneralSibling, 2, 1));
    }

    #[test]
    fn matches_scoped_fails_closed_on_an_exhausted_budget() {
        // The scoped matcher's budget guard, mirroring matches_from: a zero budget fails closed
        // (no underflow, no match); a fresh budget matches and consumes exactly one step.
        let tree = parse("<div><a>x</a></div>");
        let Node::Element { children, .. } = &tree[0] else { unreachable!() };
        let frames = vec![
            Frame { siblings: &tree, index: 0 },    // <div> — the scope, frame 0
            Frame { siblings: children, index: 0 }, // <a> — frame 1
        ];
        let steps = parse_complex("a");
        let mut exhausted = 0u32;
        assert!(!matches_scoped(&steps, 0, &frames, 1, 0, 0, &mut exhausted));
        let mut fresh = MATCH_BUDGET;
        assert!(matches_scoped(&steps, 0, &frames, 1, 0, 0, &mut fresh));
        assert_eq!(fresh, MATCH_BUDGET - 1);
    }

    #[test]
    fn matches_scoped_child_chain_stops_at_the_scope_frame() {
        // A Child chain longer than the scoped depth must stop at the scope frame rather than
        // recursing below it. Pins the `f > scope_f` guard in the Child arm (a `>=` would underflow
        // the frame index past 0).
        let tree = parse("<a><a>x</a></a>"); // outer <a> (scope, frame 0) > inner <a> (frame 1)
        let Node::Element { children, .. } = &tree[0] else { unreachable!() };
        let frames = vec![
            Frame { siblings: &tree, index: 0 },    // outer <a> — scope, frame 0
            Frame { siblings: children, index: 0 }, // inner <a> — frame 1
        ];
        // `a > a > a` is one compound deeper than the two available frames, so the Child recursion
        // reaches the scope frame and must stop there — matching nothing, without panicking.
        let steps = parse_complex("a > a > a");
        let mut budget = MATCH_BUDGET;
        assert!(!matches_scoped(&steps, 2, &frames, 1, 0, 0, &mut budget));
    }

    #[test]
    fn the_match_budget_scales_with_the_tree_so_a_large_document_still_matches_fully() {
        // A single shared budget must not fail-close on a legitimately large, wide document: every
        // one of many siblings under a structural pseudo must still be selected. (With a fixed
        // whole-tree budget this would truncate; the per-element scaling keeps it complete.)
        let items = "<li>x</li>".repeat(500);
        let tree = parse(&format!("<ul>{items}</ul>"));
        assert_eq!(select(&tree, "li:nth-child(odd)").len(), 250);
        assert_eq!(select(&tree, "li").len(), 500);
    }
}
