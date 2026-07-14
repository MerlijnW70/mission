//! Stage 1 of the parser: the HTML **tokenizer** and character-reference decoding.
//!
//! Turns raw HTML into a flat stream of [`Token`]s — text, start tags (name + parsed
//! attributes), and end tags — decoding named/decimal/hex entities in text and attribute values.
//! Comments, `<script>`/`<style>` raw text, and declarations are skipped here. This module knows
//! nothing of the tree or the network; it is the innermost, self-contained layer.

use std::collections::HashSet;
use std::iter::Peekable;
use std::str::Chars;

use super::Attrs;

/// One lexical token produced by [`tokenize`].
///
/// `#[non_exhaustive]`: token kinds may be added (e.g. comments, doctype) later, so downstream
/// `match`es must include a wildcard arm.
#[derive(Debug, PartialEq, Eq, Clone)]
#[non_exhaustive]
pub enum Token {
    /// A run of character data between tags.
    Text(String),
    /// An opening tag with its name and parsed attributes, e.g. `<a href="x">`. `self_closing`
    /// is set for an explicit `<br/>` form or an HTML void element (see [`is_void`]) — such a
    /// tag takes no children and needs no matching end tag.
    StartTag {
        name: String,
        attrs: Attrs,
        self_closing: bool,
    },
    /// A closing tag, e.g. `</p>` → `EndTag("p")`. Any attributes on an end tag are discarded.
    EndTag(String),
}

/// Split `input` into a flat stream of [`Token`]s.
///
/// The scanner hops between `'<'` delimiters with [`str::find`] rather than stepping character
/// by character, so its edges are explicit: the `lt + 1` / `end + 1` cursor
/// advances, the empty-text guards, and the `starts_with('/')` start-vs-end decision. The tag
/// interior is handed to [`parse_tag`], which separates the name from the attributes.
pub fn tokenize(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut rest = input;

    while let Some(lt) = rest.find('<') {
        // Everything before the '<' is character data.
        text.push_str(&rest[..lt]);
        if !text.is_empty() {
            tokens.push(Token::Text(decode_entities(&std::mem::take(&mut text))));
        }

        rest = &rest[lt + 1..]; // step past the '<'

        // A comment `<!-- ... -->` is skipped entirely, including any '<' or '>' inside it.
        if let Some(body) = rest.strip_prefix("!--") {
            rest = match body.find("-->") {
                Some(i) => &body[i + 3..],
                None => "", // unterminated comment: consume to end of input
            };
            continue;
        }

        // A CDATA section `<![CDATA[ ... ]]>` is literal character data: its content (which may
        // contain '<' and '>') is emitted verbatim, NOT entity-decoded, and runs to `]]>`.
        if let Some(body) = rest.strip_prefix("![CDATA[") {
            let (content, after) = match body.find("]]>") {
                Some(i) => (&body[..i], &body[i + 3..]),
                None => (body, ""), // unterminated CDATA: the rest is content
            };
            if !content.is_empty() {
                tokens.push(Token::Text(content.to_string()));
            }
            rest = after;
            continue;
        }

        // A declaration such as `<!doctype html>` carries no content: skip to the closing '>'.
        // (Comments and CDATA were handled above, so any remaining `<!…>` is a declaration.)
        if rest.starts_with('!') {
            rest = match rest.find('>') {
                Some(i) => &rest[i + 1..],
                None => "", // unterminated declaration: consume to end of input
            };
            continue;
        }

        let is_end = rest.starts_with('/');
        if is_end {
            rest = &rest[1..]; // step past the '/'
        }

        // The tag interior runs up to the next '>' that is *not* inside a quoted attribute value
        // (or end of input, for an unterminated tag): a '>' inside `"…"`/`'…'` is literal, so
        // `<img alt="a>b" src=x>` is one tag, not a truncated one plus leaked text.
        let end = find_tag_end(rest);
        // A trailing '/' marks an explicit self-closing tag (`<br/>`, `<img .../>`); strip it
        // before parsing the interior so it does not leak into the name or an attribute.
        let (interior, explicit_close) = match rest[..end].trim_end().strip_suffix('/') {
            Some(without) => (without, true),
            None => (&rest[..end], false),
        };
        let (name, attrs) = parse_tag(interior);
        rest = rest.get(end + 1..).unwrap_or(""); // step past the '>'

        if is_end {
            tokens.push(Token::EndTag(name));
            continue;
        }
        let self_closing = explicit_close || is_void(&name);
        if !self_closing && is_raw_text(&name) {
            // `<script>` / `<style>` hold raw text, not markup: skip their content and end tag
            // entirely, so a '<' inside them is not mis-parsed and no JS/CSS reaches the output.
            rest = skip_raw_text(rest, &name);
            continue;
        }
        if !self_closing && is_rcdata(&name) {
            // `<title>` / `<textarea>` hold RCDATA: their content is character data (entities are
            // decoded) but any '<' is literal — tags inside never become elements. Emit the element
            // with a single decoded text child and resume after the end tag.
            let (content, after) = take_rcdata(rest, &name);
            tokens.push(Token::StartTag { name: name.clone(), attrs, self_closing: false });
            if !content.is_empty() {
                tokens.push(Token::Text(decode_entities(content)));
            }
            tokens.push(Token::EndTag(name));
            rest = after;
            continue;
        }
        tokens.push(Token::StartTag { name, attrs, self_closing });
    }

    // Trailing character data after the last tag (or the whole input, if tag-free).
    text.push_str(rest);
    if !text.is_empty() {
        tokens.push(Token::Text(decode_entities(&text)));
    }
    tokens
}

/// The byte index of the `>` that closes a tag interior, honoring attribute-value quoting: a `>`
/// inside a quoted value is literal and does not end the tag. Returns `rest.len()` when no closing
/// `>` is found outside a quote (an unterminated tag).
///
/// A quote opens a value only when it *directly follows* `=` (the HTML attribute-value-start
/// position). A stray quote elsewhere — inside an unquoted value (`b=c"d`), a bare apostrophe
/// (`title=it's`), or in the tag name — is an ordinary character, so the real `>` still ends the
/// tag. (Without this guard, an odd stray quote would swallow the `>` and the rest of the input.)
/// The characters inspected (`"`, `'`, `>`, `=`) are ASCII, so byte-indexing stays on char boundaries.
fn find_tag_end(rest: &str) -> usize {
    let mut quote: Option<u8> = None;
    let mut after_eq = false; // the previous char was '=': a quote here opens a quoted value
    for (i, b) in rest.bytes().enumerate() {
        if let Some(q) = quote {
            if b == q {
                quote = None; // closing quote: leave the attribute-value state
            }
            continue;
        }
        match b {
            b'>' => return i,
            b'"' | b'\'' if after_eq => quote = Some(b), // opens a quoted attribute value
            _ => {}
        }
        after_eq = b == b'=';
    }
    rest.len()
}

/// Parse a tag interior (`a href="x" disabled`) into its name and attributes.
///
/// The name is the leading non-whitespace run; the remainder is a sequence of attributes, each
/// a bare name (boolean) or `name=value` with a `"`-, `'`-, or un-quoted value. Tag and attribute
/// **names are lowercased** (HTML treats them case-insensitively) while values keep their case.
fn parse_tag(interior: &str) -> (String, Attrs) {
    let mut chars = interior.chars().peekable();
    let name = take_while(&mut chars, |c| !c.is_whitespace()).to_ascii_lowercase();

    let mut attrs = Attrs::new();
    // `seen` keeps the duplicate check O(1) per attribute instead of an O(n) rescan of `attrs`, so a
    // tag carrying many attributes cannot make tokenization quadratic. `HashSet::new()` does not
    // allocate until the first insert, so an attribute-less tag pays nothing.
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        skip_whitespace(&mut chars);
        let key = take_while(&mut chars, |c| !c.is_whitespace() && c != '=');
        if key.is_empty() {
            break;
        }
        let value = if chars.peek() == Some(&'=') {
            chars.next(); // consume '='
            decode_entities(&read_value(&mut chars))
        } else {
            String::new() // boolean attribute
        };
        // The value is read (advancing the scanner) even for a duplicate, but HTML keeps only the
        // FIRST occurrence of a repeated attribute name — `seen.insert` is true only the first time.
        let key = key.to_ascii_lowercase();
        if seen.insert(key.clone()) {
            attrs.push((key, value));
        }
    }
    (name, attrs)
}

/// Whether `tag` is a raw-text element whose content is CDATA-like, not markup. Its body is
/// skipped rather than tokenized. (`tag` is already lowercased by [`parse_tag`].)
fn is_raw_text(tag: &str) -> bool {
    matches!(tag, "script" | "style")
}

/// Whether `tag` is an RCDATA element: its content is character data (entities decoded) but any
/// `<` inside is literal, so tags never become child elements. (`tag` is already lowercased.)
fn is_rcdata(tag: &str) -> bool {
    matches!(tag, "title" | "textarea")
}

/// Take an RCDATA element's content up to its `</name>` end tag: returns `(content, rest)` where
/// `rest` is the input after the end tag's `>`. The end tag is matched case-insensitively; with no
/// end tag, the whole remaining input is content.
///
/// Scans `</` boundaries and compares only the short tag name (via [`is_end_tag_match`]) — it never
/// lowercases the whole remaining input, which was O(L) per element and so quadratic over a document
/// full of `<title>`/`<textarea>`s.
fn take_rcdata<'a>(rest: &'a str, name: &str) -> (&'a str, &'a str) {
    let mut offset = 0;
    while let Some(lt) = rest[offset..].find("</") {
        let abs = offset + lt;
        let after_name = &rest[abs + 2..];
        if is_end_tag_match(after_name, name) {
            let content = &rest[..abs];
            let tail = &after_name[name.len()..];
            let after = match tail.find('>') {
                Some(j) => &tail[j + 1..],
                None => "",
            };
            return (content, after);
        }
        offset = abs + 2; // this `</` was not the closer; keep looking past it
    }
    (rest, "")
}

/// Whether `after` (the text right after a `</`) begins the end tag for `name`: the name matches
/// case-insensitively AND is followed by a tag-name terminator (`>`, `/`, whitespace, or end of
/// input). The terminator check is what stops `</scripty>` from falsely closing `<script>`.
fn is_end_tag_match(after: &str, name: &str) -> bool {
    let bytes = after.as_bytes();
    // Byte comparison via `get(..)` (not `&str[..]`) so a multibyte char right after `</` can't
    // panic and a too-short tail is simply `None`; once the ASCII name matches, `name.len()` is a
    // valid char boundary for the tail slice at the call sites.
    if !bytes.get(..name.len()).is_some_and(|b| b.eq_ignore_ascii_case(name.as_bytes())) {
        return false;
    }
    match bytes.get(name.len()) {
        None => true, // `</name` at end of input: treat as the (unterminated) closer
        Some(&b) => b == b'>' || b == b'/' || b.is_ascii_whitespace(),
    }
}

/// Skip a raw-text element's body and its `</name>` end tag, returning the input that follows.
/// The end tag is matched case-insensitively (`</SCRIPT>` closes `<script>`). If no end tag is
/// present, the rest of the input is consumed.
///
/// Scans for `</` and compares only the short tag name case-insensitively — so it never allocates
/// or lowercases the whole remaining input (which for many `<script>`s was quadratic).
fn skip_raw_text<'a>(rest: &'a str, name: &str) -> &'a str {
    let mut search = rest;
    while let Some(lt) = search.find("</") {
        let after_name = &search[lt + 2..];
        if is_end_tag_match(after_name, name) {
            // Found `</name` followed by a tag terminator; resume after the '>' that closes the end
            // tag (tolerating a trailing space or attributes, e.g. `</script >`).
            let tail = &after_name[name.len()..];
            return match tail.find('>') {
                Some(j) => &tail[j + 1..],
                None => "",
            };
        }
        search = &search[lt + 2..]; // this `</` was not the closer; keep looking past it
    }
    ""
}

/// Whether `tag` is an HTML void element — one that never has children and needs no end tag,
/// so it is treated as self-closing even without a trailing `/`.
fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

/// Consume and return characters while `pred` holds, leaving the first failing char unconsumed.
fn take_while(chars: &mut Peekable<Chars>, pred: impl Fn(char) -> bool) -> String {
    let mut s = String::new();
    while let Some(&c) = chars.peek() {
        if pred(c) {
            s.push(c);
            chars.next();
        } else {
            break;
        }
    }
    s
}

/// Consume any leading whitespace.
fn skip_whitespace(chars: &mut Peekable<Chars>) {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

/// Read an attribute value: a `"`- or `'`-quoted run (quotes stripped), else an unquoted run up
/// to the next whitespace.
fn read_value(chars: &mut Peekable<Chars>) -> String {
    match chars.peek() {
        Some(&quote) if quote == '"' || quote == '\'' => {
            chars.next(); // opening quote
            let value = take_while(chars, |c| c != quote);
            chars.next(); // closing quote (if present)
            value
        }
        _ => take_while(chars, |c| !c.is_whitespace()),
    }
}

/// Decode HTML character references in `input`: named (`&amp;`, `&lt;`, …), decimal (`&#65;`),
/// and hex (`&#x41;`). Anything that is not a recognized reference — a bare `&`, an unknown
/// name, a malformed number — is left exactly as written.
///
/// The scanner hops between `&` markers with [`str::find`]; each `&…;` span is offered to
/// [`decode_one`], and only a recognized reference advances past the `;`. The edges —
/// the `amp + 1` cursor, the `;` lookup, and the recognized-vs-literal branch — are pinned by
/// the tests below.
fn decode_entities(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';')
            && let Some(ch) = decode_one(&after[..semi])
        {
            out.push(ch);
            rest = &after[semi + 1..];
        } else {
            // Not a recognized reference: keep the '&' literally, resume just after it.
            out.push('&');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Decode the body of a single reference (the text between `&` and `;`), or `None` if it is not
/// a reference this parser knows. A named reference is looked up in [`NAMED_ENTITIES`]; otherwise
/// a `#NN` / `#xHH` numeric reference is parsed.
fn decode_one(body: &str) -> Option<char> {
    if let Some(&(_, ch)) = NAMED_ENTITIES.iter().find(|(name, _)| *name == body) {
        return Some(ch);
    }
    let number = body.strip_prefix('#')?;
    let code = match number.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => number.parse::<u32>().ok()?,
    };
    Some(numeric_char(code))
}

/// Map a numeric character reference's code point to a character, applying the HTML fix-ups:
/// the `0x80..=0x9F` range is re-mapped through Windows-1252 (so `&#151;` is an em dash, not a
/// C1 control), and null / surrogate / out-of-range values become the replacement character.
fn numeric_char(code: u32) -> char {
    #[rustfmt::skip]
    const WINDOWS_1252: [char; 32] = [
        '\u{20ac}', '\u{81}',   '\u{201a}', '\u{192}',  '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
        '\u{2c6}',  '\u{2030}', '\u{160}',  '\u{2039}', '\u{152}',  '\u{8d}',   '\u{17d}',  '\u{8f}',
        '\u{90}',   '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
        '\u{2dc}',  '\u{2122}', '\u{161}',  '\u{203a}', '\u{153}',  '\u{9d}',   '\u{17e}',  '\u{178}',
    ];
    match code {
        0 => '\u{fffd}',
        0x80..=0x9f => WINDOWS_1252[(code - 0x80) as usize],
        _ => char::from_u32(code).unwrap_or('\u{fffd}'),
    }
}

/// The named character references Mission decodes, `(name, character)`. Names are case-sensitive
/// and given without the surrounding `&`/`;`. A practical subset of the HTML5 set: the markup
/// basics, the full Latin-1 supplement, the common typographic punctuation and symbols, and a
/// handful of math/Greek/arrow references that show up in real prose.
#[rustfmt::skip]
static NAMED_ENTITIES: &[(&str, char)] = &[
    // Markup basics
    ("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\''),
    // Latin-1 symbols
    ("nbsp", '\u{a0}'), ("iexcl", '¡'), ("cent", '¢'), ("pound", '£'), ("curren", '¤'),
    ("yen", '¥'), ("brvbar", '¦'), ("sect", '§'), ("uml", '¨'), ("copy", '©'), ("ordf", 'ª'),
    ("laquo", '«'), ("not", '¬'), ("shy", '\u{ad}'), ("reg", '®'), ("macr", '¯'), ("deg", '°'),
    ("plusmn", '±'), ("sup2", '²'), ("sup3", '³'), ("acute", '´'), ("micro", 'µ'), ("para", '¶'),
    ("middot", '·'), ("cedil", '¸'), ("sup1", '¹'), ("ordm", 'º'), ("raquo", '»'),
    ("frac14", '¼'), ("frac12", '½'), ("frac34", '¾'), ("iquest", '¿'), ("times", '×'), ("divide", '÷'),
    // Latin-1 letters
    ("Agrave", 'À'), ("Aacute", 'Á'), ("Acirc", 'Â'), ("Atilde", 'Ã'), ("Auml", 'Ä'), ("Aring", 'Å'),
    ("AElig", 'Æ'), ("Ccedil", 'Ç'), ("Egrave", 'È'), ("Eacute", 'É'), ("Ecirc", 'Ê'), ("Euml", 'Ë'),
    ("Igrave", 'Ì'), ("Iacute", 'Í'), ("Icirc", 'Î'), ("Iuml", 'Ï'), ("ETH", 'Ð'), ("Ntilde", 'Ñ'),
    ("Ograve", 'Ò'), ("Oacute", 'Ó'), ("Ocirc", 'Ô'), ("Otilde", 'Õ'), ("Ouml", 'Ö'), ("Oslash", 'Ø'),
    ("Ugrave", 'Ù'), ("Uacute", 'Ú'), ("Ucirc", 'Û'), ("Uuml", 'Ü'), ("Yacute", 'Ý'), ("THORN", 'Þ'),
    ("szlig", 'ß'), ("agrave", 'à'), ("aacute", 'á'), ("acirc", 'â'), ("atilde", 'ã'), ("auml", 'ä'),
    ("aring", 'å'), ("aelig", 'æ'), ("ccedil", 'ç'), ("egrave", 'è'), ("eacute", 'é'), ("ecirc", 'ê'),
    ("euml", 'ë'), ("igrave", 'ì'), ("iacute", 'í'), ("icirc", 'î'), ("iuml", 'ï'), ("eth", 'ð'),
    ("ntilde", 'ñ'), ("ograve", 'ò'), ("oacute", 'ó'), ("ocirc", 'ô'), ("otilde", 'õ'), ("ouml", 'ö'),
    ("oslash", 'ø'), ("ugrave", 'ù'), ("uacute", 'ú'), ("ucirc", 'û'), ("uuml", 'ü'), ("yacute", 'ý'),
    ("thorn", 'þ'), ("yuml", 'ÿ'),
    // Typographic punctuation
    ("ndash", '–'), ("mdash", '—'), ("lsquo", '‘'), ("rsquo", '’'), ("sbquo", '‚'), ("ldquo", '“'),
    ("rdquo", '”'), ("bdquo", '„'), ("dagger", '†'), ("Dagger", '‡'), ("bull", '•'), ("hellip", '…'),
    ("permil", '‰'), ("prime", '′'), ("Prime", '″'), ("lsaquo", '‹'), ("rsaquo", '›'), ("oline", '‾'),
    ("frasl", '⁄'), ("euro", '€'), ("trade", '™'),
    // Math and misc
    ("minus", '−'), ("lowast", '∗'), ("radic", '√'), ("infin", '∞'), ("cap", '∩'), ("cup", '∪'),
    ("int", '∫'), ("asymp", '≈'), ("ne", '≠'), ("equiv", '≡'), ("le", '≤'), ("ge", '≥'),
    ("larr", '←'), ("uarr", '↑'), ("rarr", '→'), ("darr", '↓'), ("harr", '↔'),
    ("spades", '♠'), ("clubs", '♣'), ("hearts", '♥'), ("diams", '♦'),
    // A few common Greek letters
    ("alpha", 'α'), ("beta", 'β'), ("gamma", 'γ'), ("delta", 'δ'), ("pi", 'π'), ("sigma", 'σ'),
    ("omega", 'ω'), ("mu", 'μ'), ("lambda", 'λ'),
];

/// The value of the first attribute named `name`, if present.
pub fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tokenizer_tests {
    use super::Token::{EndTag, Text};
    use super::*;

    fn start(name: &str) -> Token {
        Token::StartTag { name: name.into(), attrs: vec![], self_closing: false }
    }

    fn start_attrs(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::StartTag {
            name: name.into(),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            self_closing: false,
        }
    }

    fn self_closed(name: &str) -> Token {
        Token::StartTag { name: name.into(), attrs: vec![], self_closing: true }
    }

    fn self_closed_attrs(name: &str, attrs: &[(&str, &str)]) -> Token {
        Token::StartTag {
            name: name.into(),
            attrs: attrs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            self_closing: true,
        }
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        assert_eq!(tokenize(""), vec![]);
    }

    #[test]
    fn tag_free_text_is_a_single_run() {
        assert_eq!(tokenize("hello"), vec![Text("hello".into())]);
    }

    #[test]
    fn a_start_tag_alone_has_no_surrounding_text() {
        // Pins both empty-text guards: a flipped guard would emit Text("") before or after.
        assert_eq!(tokenize("<a>"), vec![start("a")]);
    }

    #[test]
    fn a_slash_marks_an_end_tag() {
        // Pins starts_with('/'): flipping it would turn this into a StartTag("a").
        assert_eq!(tokenize("</a>"), vec![EndTag("a".into())]);
    }

    #[test]
    fn multi_character_tag_names_are_kept_whole() {
        assert_eq!(tokenize("<div>"), vec![start("div")]);
    }

    #[test]
    fn text_between_tags_is_its_own_token() {
        assert_eq!(
            tokenize("<a>hi</a>"),
            vec![start("a"), Text("hi".into()), EndTag("a".into())]
        );
    }

    #[test]
    fn leading_text_precedes_the_tag() {
        assert_eq!(tokenize("hi<a>"), vec![Text("hi".into()), start("a")]);
    }

    #[test]
    fn text_after_a_tag_is_captured() {
        assert_eq!(tokenize("<a>x"), vec![start("a"), Text("x".into())]);
    }

    #[test]
    fn an_unterminated_tag_runs_to_end_of_input() {
        assert_eq!(tokenize("<a"), vec![start("a")]);
    }

    #[test]
    fn a_double_quoted_attribute_is_parsed() {
        assert_eq!(tokenize("<a href=\"x\">"), vec![start_attrs("a", &[("href", "x")])]);
    }

    #[test]
    fn a_single_quoted_attribute_is_parsed() {
        // Pins the `quote == '\''` alternative in read_value.
        assert_eq!(tokenize("<a href='x'>"), vec![start_attrs("a", &[("href", "x")])]);
    }

    #[test]
    fn an_unquoted_attribute_runs_to_whitespace() {
        assert_eq!(tokenize("<a href=x>"), vec![start_attrs("a", &[("href", "x")])]);
    }

    #[test]
    fn a_quoted_value_may_contain_spaces() {
        // Pins the quoted branch: an unquoted reader would stop at the space.
        assert_eq!(
            tokenize("<p class=\"a b\">"),
            vec![start_attrs("p", &[("class", "a b")])]
        );
    }

    #[test]
    fn a_boolean_attribute_has_an_empty_value() {
        // `input` is a void element, hence self-closing.
        assert_eq!(tokenize("<input disabled>"), vec![self_closed_attrs("input", &[("disabled", "")])]);
    }

    #[test]
    fn multiple_attributes_keep_source_order() {
        assert_eq!(
            tokenize("<a href=\"x\" class=\"y\">"),
            vec![start_attrs("a", &[("href", "x"), ("class", "y")])]
        );
    }

    #[test]
    fn a_boolean_attribute_before_a_valued_one_is_distinguished() {
        // Pins the `peek() == Some('=')` decision between boolean and valued attributes.
        assert_eq!(
            tokenize("<input disabled name=\"x\">"),
            vec![self_closed_attrs("input", &[("disabled", ""), ("name", "x")])]
        );
    }

    #[test]
    fn extra_whitespace_between_attributes_is_skipped() {
        assert_eq!(
            tokenize("<a   href=\"x\"   >"),
            vec![start_attrs("a", &[("href", "x")])]
        );
    }

    #[test]
    fn a_comment_produces_no_token() {
        // Pins the strip_prefix("!--") branch: otherwise the body becomes a garbage tag.
        assert_eq!(tokenize("<!-- hello -->"), vec![]);
    }

    #[test]
    fn text_around_a_comment_survives_and_scanning_resumes_after_it() {
        // Pins the `i + 3` resume past "-->": a wrong offset would leak "-->" into "b".
        assert_eq!(tokenize("a<!-- c -->b"), vec![Text("a".into()), Text("b".into())]);
    }

    #[test]
    fn angle_brackets_inside_a_comment_do_not_end_it() {
        assert_eq!(tokenize("<!-- a > b < c -->x"), vec![Text("x".into())]);
    }

    #[test]
    fn an_unterminated_comment_consumes_the_rest_of_input() {
        // Pins the None branch: without it, "lost" would leak back out as a tag/text.
        assert_eq!(tokenize("keep<!-- lost"), vec![Text("keep".into())]);
    }

    #[test]
    fn an_explicit_self_closing_tag_drops_the_slash() {
        // Pins strip_suffix('/'): without it the name would be "span/".
        assert_eq!(tokenize("<span/>"), vec![self_closed("span")]);
    }

    #[test]
    fn a_self_closing_tag_may_have_a_space_and_attributes() {
        assert_eq!(
            tokenize("<img src=\"x\" />"),
            vec![self_closed_attrs("img", &[("src", "x")])]
        );
    }

    #[test]
    fn a_void_element_is_self_closing_without_a_slash() {
        // Pins is_void: <br> carries no slash yet must still be self-closing.
        assert_eq!(tokenize("<br>"), vec![self_closed("br")]);
    }

    #[test]
    fn a_non_void_tag_is_not_self_closing() {
        assert_eq!(tokenize("<div>"), vec![start("div")]);
    }

    #[test]
    fn a_script_element_is_skipped_including_stray_angle_brackets() {
        // The `1 < 2` inside must NOT be mis-parsed as a tag; the whole element vanishes.
        assert_eq!(tokenize("<script>if (1 < 2) {}</script>"), vec![]);
    }

    #[test]
    fn text_around_a_script_survives_and_scanning_resumes_after_it() {
        // Pins skip_raw_text's resume offsets: a wrong one would leak "</script>" into "b".
        assert_eq!(tokenize("a<script>x</script>b"), vec![Text("a".into()), Text("b".into())]);
    }

    #[test]
    fn a_style_element_is_skipped() {
        assert_eq!(tokenize("<style>.a{color:red}</style>after"), vec![Text("after".into())]);
    }

    #[test]
    fn an_unterminated_script_consumes_the_rest_of_input() {
        assert_eq!(tokenize("keep<script>alert(1)"), vec![Text("keep".into())]);
    }

    #[test]
    fn a_tag_name_is_lowercased() {
        // Pins the name lowercasing: without it the name would be "DIV".
        assert_eq!(tokenize("<DIV>"), vec![start("div")]);
    }

    #[test]
    fn an_attribute_name_is_lowercased_but_its_value_is_not() {
        assert_eq!(tokenize("<a HREF=\"X\">"), vec![start_attrs("a", &[("href", "X")])]);
    }

    #[test]
    fn a_void_element_is_recognized_regardless_of_case() {
        // <BR> must be void too — is_void sees the lowercased name.
        assert_eq!(tokenize("<BR>"), vec![self_closed("br")]);
    }

    #[test]
    fn a_raw_text_element_is_matched_case_insensitively() {
        // <SCRIPT> ... </SCRIPT> is skipped even in uppercase, stray '<' and all.
        assert_eq!(tokenize("<SCRIPT>x < y</SCRIPT>keep"), vec![Text("keep".into())]);
    }

    #[test]
    fn raw_text_skips_a_non_matching_end_tag_before_the_real_one() {
        // A `</b>` inside <script> is not the closer; the scan must advance past that `</` and keep
        // looking for `</script>`. Pins the loop-advance in skip_raw_text.
        assert_eq!(tokenize("<script>a</b>c</script>keep"), vec![Text("keep".into())]);
    }

    #[test]
    fn a_doctype_declaration_produces_no_token() {
        // Pins the starts_with('!') skip: otherwise it becomes a garbage "!doctype" tag.
        assert_eq!(tokenize("<!DOCTYPE html>"), vec![]);
    }

    #[test]
    fn content_after_a_doctype_is_kept_and_scanning_resumes() {
        // Pins the `i + 1` resume past the declaration's '>'.
        assert_eq!(tokenize("<!doctype html><p>hi"), vec![start("p"), Text("hi".into())]);
    }

    #[test]
    fn an_unterminated_declaration_consumes_the_rest_of_input() {
        assert_eq!(tokenize("keep<!DOCTYPE"), vec![Text("keep".into())]);
    }

    #[test]
    fn a_cdata_section_yields_its_content_verbatim() {
        // Content is literal character data: the inner '<'/'>' do not start tags, and it runs to
        // `]]>` (not the first '>'). Pins the `]]>` scan and the `i + 3` resume.
        assert_eq!(
            tokenize("a<![CDATA[x<b>y]]>z"),
            vec![Text("a".into()), Text("x<b>y".into()), Text("z".into())]
        );
    }

    #[test]
    fn cdata_content_is_not_entity_decoded() {
        // Unlike ordinary text, a reference inside CDATA stays literal.
        assert_eq!(tokenize("<![CDATA[&amp;]]>"), vec![Text("&amp;".into())]);
    }

    #[test]
    fn an_empty_cdata_section_yields_no_token() {
        // Pins the non-empty guard so `<![CDATA[]]>` does not emit an empty text token.
        assert_eq!(tokenize("<![CDATA[]]>"), vec![]);
    }

    #[test]
    fn an_unterminated_cdata_consumes_the_rest_as_content() {
        assert_eq!(tokenize("<![CDATA[tail"), vec![Text("tail".into())]);
    }

    #[test]
    fn title_and_textarea_are_rcdata_not_markup() {
        // Tags inside are literal (no child elements); entities are still decoded.
        assert_eq!(
            tokenize("<title>a<b>c &amp; d</title>"),
            vec![start("title"), Text("a<b>c & d".into()), EndTag("title".into())]
        );
        assert_eq!(
            tokenize("<textarea><p>x</textarea>"),
            vec![start("textarea"), Text("<p>x".into()), EndTag("textarea".into())]
        );
    }

    #[test]
    fn an_unterminated_rcdata_element_takes_the_rest_as_content() {
        assert_eq!(
            tokenize("<title>tail"),
            vec![start("title"), Text("tail".into()), EndTag("title".into())]
        );
    }

    #[test]
    fn an_empty_rcdata_element_emits_no_text_token() {
        // Pins the non-empty guard: `<title></title>` is just the two tags, no empty Text between.
        assert_eq!(tokenize("<title></title>"), vec![start("title"), EndTag("title".into())]);
    }

    #[test]
    fn a_repeated_attribute_keeps_the_first_value() {
        // HTML5 drops later duplicates; the DOM stores only the first, and it is the one looked up.
        assert_eq!(tokenize("<a href=\"1\" href=\"2\">"), vec![start_attrs("a", &[("href", "1")])]);
    }

    #[test]
    fn a_duplicate_attribute_is_detected_case_insensitively() {
        // Names are lowercased, so `HREF` and `href` are the same attribute — the first wins.
        assert_eq!(tokenize("<a HREF=\"1\" href=\"2\">"), vec![start_attrs("a", &[("href", "1")])]);
    }

    #[test]
    fn three_duplicate_attributes_all_collapse_to_the_first() {
        // Pins the seen-set dedup across more than two occurrences: only the first value survives.
        assert_eq!(
            tokenize("<a id=\"1\" id=\"2\" id=\"3\">"),
            vec![start_attrs("a", &[("id", "1")])]
        );
    }

    #[test]
    fn a_greater_than_inside_a_quoted_attribute_does_not_end_the_tag() {
        // A '>' inside a double-quoted value is literal: the tag runs to the real '>', so no
        // attribute is lost and nothing leaks into the text stream. Pins find_tag_end's quote state.
        assert_eq!(
            tokenize("<img alt=\"a>b\" src=x>"),
            vec![self_closed_attrs("img", &[("alt", "a>b"), ("src", "x")])]
        );
    }

    #[test]
    fn a_greater_than_inside_a_single_quoted_attribute_does_not_end_the_tag() {
        // The single-quote arm of the quote state, and a following element proving the tag closed at
        // the correct '>'.
        assert_eq!(
            tokenize("<a title='1 > 0'>x"),
            vec![start_attrs("a", &[("title", "1 > 0")]), Text("x".into())]
        );
    }

    #[test]
    fn a_stray_quote_in_an_unquoted_value_does_not_capture_the_tag_end() {
        // A quote NOT directly after '=' is an ordinary char: the real '>' still ends the tag, and
        // trailing content is not swallowed. Pins find_tag_end's `after_eq` guard (a regression the
        // naive quote toggle caused: it entered quote state on the stray '"' and lost `>e`).
        assert_eq!(
            tokenize("<a b=c\"d>e"),
            vec![start_attrs("a", &[("b", "c\"d")]), Text("e".into())]
        );
    }

    #[test]
    fn a_bare_apostrophe_in_an_unquoted_value_does_not_capture_the_tag_end() {
        assert_eq!(
            tokenize("<a title=it's>x"),
            vec![start_attrs("a", &[("title", "it's")]), Text("x".into())]
        );
    }

    #[test]
    fn a_tag_interior_starting_with_a_quote_ends_at_the_first_bare_greater_than() {
        // The first char of the interior is a quote that does not follow '=', so it must not open a
        // quoted value. Pins the initial `after_eq = false` (a `true` start would enter quote state
        // on this leading quote and swallow the '>').
        assert_eq!(tokenize("<\">x"), vec![start("\""), Text("x".into())]);
    }

    #[test]
    fn a_stray_quote_does_not_swallow_following_markup() {
        // The worst case: without the guard the whole `text</div>` is absorbed into the attribute
        // and vanishes. The `>` must still close the tag so the content survives.
        assert_eq!(
            tokenize("<div data=x'y>text</div>"),
            vec![start_attrs("div", &[("data", "x'y")]), Text("text".into()), EndTag("div".into())]
        );
    }

    #[test]
    fn a_raw_text_end_tag_must_be_a_whole_name_not_a_prefix() {
        // `</scripty>` is NOT `</script>`: the raw-text region continues until the real closer, so
        // the stray `</scripty>` and the text around it stay swallowed. Pins is_end_tag_match's
        // terminator check (without it, `scripty` would falsely close the script early).
        assert_eq!(
            tokenize("a<script>x</scripty>y</script>z"),
            vec![Text("a".into()), Text("z".into())]
        );
    }

    #[test]
    fn an_rcdata_end_tag_must_be_a_whole_name_not_a_prefix() {
        // Likewise for RCDATA: `</titlee>` does not close `<title>`; it stays literal content until
        // the real `</title>`.
        assert_eq!(
            tokenize("<title>x</titlee>y</title>z"),
            vec![start("title"), Text("x</titlee>y".into()), EndTag("title".into()), Text("z".into())]
        );
    }

    #[test]
    fn a_raw_text_end_tag_with_a_same_length_wrong_name_does_not_close() {
        // `</abcdef>` is the same length as `script` but a different name: the name check must reject
        // it even though the following `>` is a tag terminator, so the region runs to `</script>`.
        // Pins is_end_tag_match's name comparison and its `return false` value.
        assert_eq!(
            tokenize("a<script>x</abcdef>y</script>z"),
            vec![Text("a".into()), Text("z".into())]
        );
    }

    #[test]
    fn a_raw_text_end_tag_may_be_terminated_by_whitespace() {
        // `</script >` closes the element: whitespace right after the name is a valid terminator.
        assert_eq!(
            tokenize("a<script>x</script >keep"),
            vec![Text("a".into()), Text("keep".into())]
        );
    }

    #[test]
    fn a_raw_text_end_tag_may_be_terminated_by_a_slash() {
        // `</script/>` also closes it: a `/` right after the name is a terminator too.
        assert_eq!(
            tokenize("a<script>x</script/>keep"),
            vec![Text("a".into()), Text("keep".into())]
        );
    }

    #[test]
    fn an_rcdata_end_tag_at_end_of_input_still_closes() {
        // `</title` with no closing `>` at EOF is the (unterminated) closer: content ends before it.
        // Pins the `None => true` arm of is_end_tag_match's terminator check.
        assert_eq!(
            tokenize("<title>x</title"),
            vec![start("title"), Text("x".into()), EndTag("title".into())]
        );
    }
}

#[cfg(test)]
mod attr_tests {
    use super::*;
    use crate::parser::Attrs;

    fn attrs(pairs: &[(&str, &str)]) -> Attrs {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn returns_the_value_of_a_present_attribute() {
        assert_eq!(attr(&attrs(&[("href", "x")]), "href"), Some("x"));
    }

    #[test]
    fn returns_none_for_an_absent_attribute() {
        assert_eq!(attr(&attrs(&[("href", "x")]), "class"), None);
    }

    #[test]
    fn finds_the_named_attribute_among_several() {
        // Pins the `key == name` match: flipping it would return the first non-match.
        assert_eq!(attr(&attrs(&[("a", "1"), ("b", "2")]), "b"), Some("2"));
    }
}

#[cfg(test)]
mod entity_tests {
    use super::*;

    #[test]
    fn text_without_references_is_unchanged() {
        assert_eq!(decode_entities("plain text"), "plain text");
    }

    #[test]
    fn named_references_decode() {
        assert_eq!(decode_entities("a&amp;b"), "a&b");
        assert_eq!(decode_entities("&lt;&gt;"), "<>");
        assert_eq!(decode_entities("&quot;&apos;"), "\"'");
        assert_eq!(decode_entities("&nbsp;"), "\u{a0}");
    }

    #[test]
    fn extended_named_references_decode() {
        // A representative slice of the extended table: a Latin-1 letter, three symbols, currencies.
        assert_eq!(decode_entities("caf&eacute;"), "café");
        assert_eq!(decode_entities("&copy;&mdash;&trade;"), "©—™");
        assert_eq!(decode_entities("&pound;&euro;"), "£€");
    }

    #[test]
    fn numeric_references_apply_windows_1252_and_replacement_fixups() {
        assert_eq!(decode_entities("&#151;"), "\u{2014}"); // 0x97 → em dash (not a C1 control)
        assert_eq!(decode_entities("&#128;"), "\u{20ac}"); // 0x80 → euro (lower bound of the map)
        assert_eq!(decode_entities("&#159;"), "\u{178}"); // 0x9f → Ÿ (upper bound of the map)
        assert_eq!(decode_entities("&#0;"), "\u{fffd}"); // null → replacement char
        assert_eq!(decode_entities("&#xFFFFFFFF;"), "\u{fffd}"); // out of range → replacement char
        assert_eq!(decode_entities("&#65;"), "A"); // ordinary code points unaffected
    }

    #[test]
    fn a_named_reference_requires_an_exact_case_sensitive_name() {
        // Pins the `*name == body` equality in the table lookup: a near-miss or wrong case must
        // NOT match (a `starts_with` or case-fold would wrongly resolve these).
        assert_eq!(decode_entities("&mdas;"), "&mdas;"); // not "mdash"
        assert_eq!(decode_entities("&COPY;"), "&COPY;"); // names are case-sensitive
    }

    #[test]
    fn decimal_references_decode() {
        assert_eq!(decode_entities("&#65;"), "A");
    }

    #[test]
    fn hex_references_decode_in_either_case() {
        // Pins radix 16 (vs decimal) and the 'x'/'X' prefix: "41" as decimal would be ')'.
        assert_eq!(decode_entities("&#x41;"), "A");
        assert_eq!(decode_entities("&#X41;"), "A");
    }

    #[test]
    fn an_unknown_reference_is_left_literal() {
        assert_eq!(decode_entities("&unknown;"), "&unknown;");
    }

    #[test]
    fn a_bare_ampersand_is_kept() {
        // Pins the literal-'&' fallback and the `amp + 1` resume.
        assert_eq!(decode_entities("a & b"), "a & b");
    }

    #[test]
    fn a_malformed_numeric_reference_is_left_literal() {
        assert_eq!(decode_entities("&#zz;"), "&#zz;");
    }

    #[test]
    fn a_reference_is_decoded_between_surrounding_text() {
        // Pins the push_str of text before '&' and the resume past ';'.
        assert_eq!(decode_entities("5 &lt; 6 &amp; 7"), "5 < 6 & 7");
    }

    #[test]
    fn entities_in_text_are_decoded_through_tokenize() {
        assert_eq!(tokenize("Tom &amp; Jerry"), vec![Token::Text("Tom & Jerry".into())]);
    }

    #[test]
    fn entities_in_attribute_values_are_decoded() {
        assert_eq!(
            tokenize("<a title=\"Tom &amp; Jerry\">"),
            vec![Token::StartTag {
                name: "a".into(),
                attrs: vec![("title".into(), "Tom & Jerry".into())],
                self_closing: false,
            }]
        );
    }
}

