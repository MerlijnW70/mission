//! Zero-dependency JSON for Mission: the query-result **serializers** ([`selection`], [`object`],
//! [`strings`]) plus a small general JSON **reader** ([`Value`] + [`from_str`]) used by the MCP
//! server to read JSON-RPC requests. Still not a full library — just what Mission emits and reads.
//!
//! This layer only reads the parsed tree; like the renderer it has no business with the network.

use crate::parser::Node;

/// Serialize a single element as a JSON object `{"tag":…,"attrs":{…},"text":…}`.
pub fn object(node: &Node) -> String {
    let mut out = String::from("{\"tag\":");
    push_string(node.tag().unwrap_or(""), &mut out);
    out.push_str(",\"attrs\":{");
    for (j, (key, value)) in node.attrs().iter().enumerate() {
        if j > 0 {
            out.push(',');
        }
        push_string(key, &mut out);
        out.push(':');
        push_string(value, &mut out);
    }
    out.push_str("},\"text\":");
    push_string(&node.text(), &mut out);
    out.push('}');
    out
}

/// Serialize a selection as a JSON array of [`object`]s.
pub fn selection(nodes: &[&Node]) -> String {
    let mut out = String::from("[");
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&object(node));
    }
    out.push(']');
    out
}

/// Serialize a list of strings as a JSON array (used for `--attr --json`).
pub fn strings(values: &[&str]) -> String {
    let mut out = String::from("[");
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        push_string(value, &mut out);
    }
    out.push(']');
    out
}

/// Append `s` to `out` as a quoted, escaped JSON string.
fn push_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Other control characters as \u00XX via
            // `format!`, which handles the escaping cleanly.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------------------------
// General JSON reader — a `Value` tree + a recursive-descent parser (for JSON-RPC requests).
// ---------------------------------------------------------------------------------------------

/// A parsed JSON value. Numbers are `f64` (JSON has one number type); objects keep source order.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    /// The value under `key` if this is an object that has it.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// This value as a string slice, if it is a JSON string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// This value as a number, if it is a JSON number.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// This value as a bool, if it is a JSON boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Serialize back to compact JSON.
    pub fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            // Rust's f64 Display already omits a trailing `.0`, so an integer id round-trips as `1`.
            // JSON has no infinity/NaN, and Display would emit the invalid tokens `inf`/`NaN`; a
            // non-finite number (e.g. from parsing an over-range literal like `1e999`) becomes
            // `null` instead, as JSON encoders conventionally do, keeping the output valid JSON.
            Value::Number(n) if n.is_finite() => out.push_str(&format!("{n}")),
            Value::Number(_) => out.push_str("null"),
            Value::String(s) => push_string(s, out),
            Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write(out);
                }
                out.push(']');
            }
            Value::Object(pairs) => {
                out.push('{');
                for (i, (key, value)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    push_string(key, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }

    /// Serialize back to a compact JSON string.
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }
}

/// The deepest array/object nesting [`from_str`] will accept, to bound its recursion against a
/// hostile deeply-nested document (the same defense the HTML parser and selector engine use).
const MAX_JSON_DEPTH: u32 = 128;

/// Parse a complete JSON document, or `None` if it is malformed, over-nested, or has trailing
/// data. Intended for the small, well-formed messages a JSON-RPC client sends. Named `from_str`
/// (serde-style) to avoid colliding with the HTML [`crate::parser::parse`].
pub fn from_str(input: &str) -> Option<Value> {
    let mut p = Parser { bytes: input.as_bytes(), pos: 0 };
    p.skip_ws();
    let value = p.value(0)?;
    p.skip_ws();
    if p.pos == p.bytes.len() { Some(value) } else { None }
}

/// A cursor over the input bytes. JSON's structural characters are all ASCII, so byte-indexing
/// stays on char boundaries; string contents are validated back into `&str` before use.
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    /// Parse one value at nesting `depth` (whitespace already skipped by the caller).
    fn value(&mut self, depth: u32) -> Option<Value> {
        match self.peek()? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Value::String),
            b't' => self.literal(b"true", Value::Bool(true)),
            b'f' => self.literal(b"false", Value::Bool(false)),
            b'n' => self.literal(b"null", Value::Null),
            _ => self.number(),
        }
    }

    /// Match an exact keyword (`true`/`false`/`null`) and yield `val`.
    fn literal(&mut self, word: &[u8], val: Value) -> Option<Value> {
        if self.bytes[self.pos..].starts_with(word) {
            self.pos += word.len();
            Some(val)
        } else {
            None
        }
    }

    fn object(&mut self, depth: u32) -> Option<Value> {
        if depth >= MAX_JSON_DEPTH {
            return None;
        }
        self.pos += 1; // consume '{'
        let mut pairs = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Some(Value::Object(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return None; // a key must be a string
            }
            let key = self.string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return None;
            }
            self.pos += 1; // consume ':'
            self.skip_ws();
            let value = self.value(depth + 1)?;
            pairs.push((key, value));
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b'}' => {
                    self.pos += 1;
                    return Some(Value::Object(pairs));
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: u32) -> Option<Value> {
        if depth >= MAX_JSON_DEPTH {
            return None;
        }
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Some(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.value(depth + 1)?);
            self.skip_ws();
            match self.peek()? {
                b',' => self.pos += 1,
                b']' => {
                    self.pos += 1;
                    return Some(Value::Array(items));
                }
                _ => return None,
            }
        }
    }

    /// Parse a string starting at the opening `"`, decoding escapes (including `\uXXXX` and
    /// surrogate pairs). Leaves `pos` just past the closing `"`. The scanning loop lives in
    /// [`Parser::continue_string`], which a `\u` escape re-enters.
    fn string(&mut self) -> Option<String> {
        self.pos += 1; // consume opening '"'
        self.continue_string(String::new())
    }

    /// Handle a `\u` escape (already consumed `\u`): read four hex digits, combining a high/low
    /// surrogate pair into one code point, then continue the surrounding string.
    fn finish_unicode_escape(&mut self, mut out: String) -> Option<String> {
        let code = self.hex4()?;
        if (0xD800..=0xDBFF).contains(&code) {
            // High surrogate: must be followed by a `\u<low>` low surrogate; combine the pair.
            let low = self.expect_low_surrogate()?;
            let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
            out.push(char::from_u32(combined)?);
            return self.continue_string(out);
        }
        out.push(char::from_u32(code)?);
        self.continue_string(out)
    }

    /// After a high surrogate, consume a following `\u` and four hex digits and return the value
    /// if it is a low surrogate (`0xDC00..=0xDFFF`); otherwise `None` (lone/invalid surrogate).
    fn expect_low_surrogate(&mut self) -> Option<u32> {
        if self.peek() != Some(b'\\') {
            return None;
        }
        self.pos += 1;
        if self.peek() != Some(b'u') {
            return None;
        }
        self.pos += 1;
        let low = self.hex4()?;
        (0xDC00..=0xDFFF).contains(&low).then_some(low)
    }

    /// Resume parsing string content after an escape was handled by a helper.
    fn continue_string(&mut self, mut out: String) -> Option<String> {
        loop {
            match self.peek()? {
                b'"' => {
                    self.pos += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.pos += 1;
                    let ch = match self.peek()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            self.pos += 1;
                            return self.finish_unicode_escape(out);
                        }
                        _ => return None,
                    };
                    out.push(ch);
                    self.pos += 1;
                }
                c if c < 0x20 => return None,
                _ => {
                    let start = self.pos;
                    while let Some(c) = self.peek() {
                        if c == b'"' || c == b'\\' || c < 0x20 {
                            break;
                        }
                        self.pos += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.bytes[start..self.pos]).ok()?);
                }
            }
        }
    }

    /// Read exactly four hex digits as a code point.
    fn hex4(&mut self) -> Option<u32> {
        let mut code = 0u32;
        for _ in 0..4 {
            let digit = (self.peek()? as char).to_digit(16)?;
            code = code * 16 + digit;
            self.pos += 1;
        }
        Some(code)
    }

    /// Parse a JSON number: collect the numeric span and hand it to the standard float parser,
    /// which validates the grammar and produces the `f64`.
    fn number(&mut self) -> Option<Value> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if matches!(c, b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let span = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        span.parse::<f64>().ok().map(Value::Number)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    #[test]
    fn an_empty_selection_is_an_empty_array() {
        assert_eq!(selection(&[]), "[]");
    }

    #[test]
    fn an_element_serializes_tag_attrs_and_text() {
        let dom = parse("<a href=\"/x\" class=\"ext\">go</a>");
        let nodes: Vec<&Node> = dom.iter().collect();
        assert_eq!(
            selection(&nodes),
            "[{\"tag\":\"a\",\"attrs\":{\"href\":\"/x\",\"class\":\"ext\"},\"text\":\"go\"}]"
        );
    }

    #[test]
    fn multiple_elements_are_comma_separated() {
        let dom = parse("<p>a</p><p>b</p>");
        let nodes: Vec<&Node> = dom.iter().collect();
        assert_eq!(
            selection(&nodes),
            "[{\"tag\":\"p\",\"attrs\":{},\"text\":\"a\"},{\"tag\":\"p\",\"attrs\":{},\"text\":\"b\"}]"
        );
    }

    #[test]
    fn special_characters_in_text_are_escaped() {
        let dom = parse("<p>say \"hi\"\tnow</p>");
        let nodes: Vec<&Node> = dom.iter().collect();
        // The quote and tab must be escaped so the output is valid JSON.
        assert_eq!(
            selection(&nodes),
            "[{\"tag\":\"p\",\"attrs\":{},\"text\":\"say \\\"hi\\\"\\tnow\"}]"
        );
    }

    #[test]
    fn a_control_character_becomes_a_unicode_escape() {
        let mut out = String::new();
        // 0x1b has a non-zero high nibble, so it also pins the `>> 4` shift.
        push_string("\u{1b}", &mut out);
        assert_eq!(out, "\"\\u001b\"");
        let mut low = String::new();
        push_string("\u{1}", &mut low);
        assert_eq!(low, "\"\\u0001\"");
    }

    #[test]
    fn a_backslash_is_escaped() {
        let mut out = String::new();
        push_string("a\\b", &mut out);
        assert_eq!(out, "\"a\\\\b\"");
    }

    #[test]
    fn strings_serializes_a_json_array() {
        assert_eq!(strings(&["/a", "/b"]), "[\"/a\",\"/b\"]");
        assert_eq!(strings(&[]), "[]");
    }

    // --- reader: primitives ------------------------------------------------------------------

    #[test]
    fn parses_the_three_literals() {
        assert_eq!(from_str("null"), Some(Value::Null));
        assert_eq!(from_str("true"), Some(Value::Bool(true)));
        assert_eq!(from_str("false"), Some(Value::Bool(false)));
    }

    #[test]
    fn a_truncated_or_wrong_literal_is_rejected() {
        // A prefix must not parse; and a same-length near-miss must not either (pins the
        // `starts_with(word)` check — `cond→true` would accept `trux`/`nxll` as true/null).
        assert_eq!(from_str("tru"), None);
        assert_eq!(from_str("nul"), None);
        assert_eq!(from_str("trux"), None);
        assert_eq!(from_str("nxll"), None);
        assert_eq!(from_str("falsy"), None);
    }

    #[test]
    fn parses_numbers_including_sign_fraction_and_exponent() {
        assert_eq!(from_str("0"), Some(Value::Number(0.0)));
        assert_eq!(from_str("42"), Some(Value::Number(42.0)));
        assert_eq!(from_str("-7"), Some(Value::Number(-7.0)));
        assert_eq!(from_str("3.5"), Some(Value::Number(3.5)));
        assert_eq!(from_str("1e3"), Some(Value::Number(1000.0)));
        assert_eq!(from_str("-2.5E-1"), Some(Value::Number(-0.25)));
    }

    #[test]
    fn a_non_numeric_token_is_rejected() {
        assert_eq!(from_str("abc"), None);
        assert_eq!(from_str("--"), None);
    }

    // --- reader: strings & escapes -----------------------------------------------------------

    #[test]
    fn parses_a_plain_string() {
        assert_eq!(from_str("\"hello\""), Some(Value::String("hello".into())));
    }

    #[test]
    fn decodes_every_short_escape() {
        assert_eq!(
            from_str(r#""a\"b\\c\/d\be\ff\ng\rh\ti""#).unwrap().as_str().unwrap(),
            "a\"b\\c/d\u{8}e\u{c}f\ng\rh\ti"
        );
    }

    #[test]
    fn an_unknown_escape_is_rejected() {
        assert_eq!(from_str(r#""\x""#), None);
    }

    #[test]
    fn a_raw_control_char_in_a_string_is_rejected() {
        assert_eq!(from_str("\"a\tb\""), None); // literal tab byte, not the \t escape
    }

    #[test]
    fn an_unterminated_string_is_rejected() {
        assert_eq!(from_str("\"abc"), None);
    }

    #[test]
    fn decodes_bmp_unicode_escapes_digit_by_digit() {
        // Non-raw literals with `\\u` so the parser actually receives `\uXXXX`. Distinct digits and
        // both hex cases pin hex4's *16 accumulation, its to_digit(16), and its position advance.
        assert_eq!(from_str("\"\\u0041\"").unwrap().as_str().unwrap(), "A"); // 0x41
        assert_eq!(from_str("\"\\u00e9\"").unwrap().as_str().unwrap(), "é"); // 0xE9 lowercase hex
        assert_eq!(from_str("\"\\u00E9\"").unwrap().as_str().unwrap(), "é"); // uppercase hex
        assert_eq!(from_str("\"\\u2014\"").unwrap().as_str().unwrap(), "—"); // 0x2014 em dash
    }

    #[test]
    fn decodes_surrogate_pairs_across_the_range() {
        // Three points pin the combine arithmetic 0x10000 + ((hi-0xD800)<<10) + (lo-0xDC00).
        assert_eq!(from_str("\"\\uD800\\uDC00\"").unwrap().as_str().unwrap(), "\u{10000}"); // min
        assert_eq!(from_str("\"\\uDBFF\\uDFFF\"").unwrap().as_str().unwrap(), "\u{10FFFF}"); // max
        assert_eq!(from_str("\"\\uD83D\\uDE00\"").unwrap().as_str().unwrap(), "\u{1F600}"); // 😀
        // Content after a spliced surrogate pair still parses (re-enters continue_string).
        assert_eq!(from_str("\"\\uD83D\\uDE00!\"").unwrap().as_str().unwrap(), "\u{1F600}!");
    }

    #[test]
    fn an_invalid_surrogate_follow_up_is_rejected() {
        assert_eq!(from_str("\"\\uD800\""), None); // lone high surrogate, nothing after
        assert_eq!(from_str("\"\\uD800A\""), None); // not followed by a backslash
        assert_eq!(from_str("\"\\uD800\\n\""), None); // backslash escape, but not \u
        assert_eq!(from_str("\"\\uD800\\u0041\""), None); // \u, but 0x41 is not a low surrogate
        // These pin the two `!=` guards: with either skipped, a stray char is consumed in place of
        // `\`/`u` and the trailing `DC00` would be read as a (wrongly-accepted) low surrogate.
        assert_eq!(from_str("\"\\uD800XuDC00\""), None); // no leading backslash
        assert_eq!(from_str("\"\\uD800\\XDC00\""), None); // backslash, but the next char isn't `u`
    }

    #[test]
    fn a_space_is_valid_string_content() {
        // Pins `c < 0x20` (not `<= 0x20`) in both the run-scan (352) and the top-level arm (348):
        // a space is ordinary content. The space right after the `\n` escape lands at the top of
        // the loop, exercising arm 348 specifically.
        assert_eq!(from_str("\"a b c\"").unwrap().as_str().unwrap(), "a b c");
        assert_eq!(from_str("\"\\n x\"").unwrap().as_str().unwrap(), "\n x");
    }

    #[test]
    fn a_bad_hex_digit_in_a_unicode_escape_is_rejected() {
        assert_eq!(from_str(r#""\u00zz""#), None);
    }

    #[test]
    fn multibyte_content_survives_after_an_escape() {
        // Re-enters the scanning loop via continue_string, then copies a multibyte run.
        assert_eq!(from_str(r#""\ncafé""#).unwrap().as_str().unwrap(), "\ncafé");
    }

    // --- reader: arrays & objects ------------------------------------------------------------

    #[test]
    fn parses_an_empty_and_a_nonempty_array() {
        assert_eq!(from_str("[]"), Some(Value::Array(vec![])));
        assert_eq!(
            from_str("[1, true, \"x\"]"),
            Some(Value::Array(vec![Value::Number(1.0), Value::Bool(true), Value::String("x".into())]))
        );
    }

    #[test]
    fn parses_an_empty_and_a_nonempty_object_and_get_finds_keys() {
        assert_eq!(from_str("{}"), Some(Value::Object(vec![])));
        let v = from_str(r#"{"a": 1, "b": "two"}"#).unwrap();
        assert_eq!(v.get("a").and_then(Value::as_f64), Some(1.0));
        assert_eq!(v.get("b").and_then(Value::as_str), Some("two"));
        assert_eq!(v.get("missing"), None);
    }

    #[test]
    fn get_on_a_non_object_is_none() {
        assert_eq!(from_str("[1]").unwrap().get("a"), None);
    }

    #[test]
    fn a_non_string_object_key_is_rejected() {
        assert_eq!(from_str("{1: 2}"), None);
        // Pins the key-must-be-a-string check: without it, `x"` is read as the empty key "" and a
        // valid `:1` follows, so `{x":1}` would be wrongly accepted.
        assert_eq!(from_str(r#"{x":1}"#), None);
    }

    #[test]
    fn a_missing_colon_or_bad_separator_is_rejected() {
        assert_eq!(from_str(r#"{"a" 1}"#), None);
        // Skipping the colon check would consume `x` as the separator and accept `{"a"x1}`.
        assert_eq!(from_str(r#"{"a"x1}"#), None);
        assert_eq!(from_str(r#"{"a": 1 "b": 2}"#), None); // missing comma
        assert_eq!(from_str("[1 2]"), None); // missing comma
    }

    #[test]
    fn nesting_and_whitespace_are_handled() {
        let v = from_str("  {\n \"k\" : [ { \"n\" : 5 } ] \n} ").unwrap();
        assert_eq!(v.get("k").unwrap().to_json(), r#"[{"n":5}]"#);
    }

    #[test]
    fn nesting_is_accepted_at_the_cap_and_rejected_one_past_it() {
        let d = MAX_JSON_DEPTH as usize;
        // Arrays: exactly at the cap parses; one deeper is rejected (pins the array-side `>=`).
        assert!(from_str(&format!("{}1{}", "[".repeat(d), "]".repeat(d))).is_some());
        assert!(from_str(&format!("{}1{}", "[".repeat(d + 1), "]".repeat(d + 1))).is_none());
        // Objects: same, pinning the object-side depth check independently.
        assert!(from_str(&format!("{}1{}", r#"{"a":"#.repeat(d), "}".repeat(d))).is_some());
        assert!(from_str(&format!("{}1{}", r#"{"a":"#.repeat(d + 1), "}".repeat(d + 1))).is_none());
    }

    // --- reader: document-level errors -------------------------------------------------------

    #[test]
    fn trailing_data_after_a_value_is_rejected() {
        assert_eq!(from_str("1 2"), None);
        assert_eq!(from_str("{} x"), None);
    }

    #[test]
    fn empty_or_whitespace_only_input_is_rejected() {
        assert_eq!(from_str(""), None);
        assert_eq!(from_str("   "), None);
    }

    // --- reader: serialization & accessors ---------------------------------------------------

    #[test]
    fn round_trips_a_document() {
        let src = r#"{"id":7,"ok":true,"nums":[1,-2,3.5],"s":"a\"b"}"#;
        assert_eq!(from_str(src).unwrap().to_json(), src);
    }

    #[test]
    fn integer_numbers_serialize_without_a_decimal_point() {
        // Pins format_number's integer branch — a JSON-RPC id must round-trip as `1`, not `1.0`.
        assert_eq!(Value::Number(1.0).to_json(), "1");
        assert_eq!(Value::Number(-42.0).to_json(), "-42");
        assert_eq!(Value::Number(2.5).to_json(), "2.5");
    }

    #[test]
    fn non_finite_numbers_serialize_as_null() {
        // f64 infinity/NaN have no JSON form; they must become valid `null`, never the invalid
        // `inf`/`NaN` tokens Rust's Display would produce. An over-range literal parses to infinity.
        assert_eq!(Value::Number(f64::INFINITY).to_json(), "null");
        assert_eq!(Value::Number(f64::NEG_INFINITY).to_json(), "null");
        assert_eq!(Value::Number(f64::NAN).to_json(), "null");
        // The end-to-end path: `1e999` overflows f64 to infinity and must still round-trip to valid
        // JSON (not `inf`). A finite number is unaffected — pins the `is_finite` guard both ways.
        assert_eq!(from_str("1e999").unwrap().to_json(), "null");
        assert_eq!(Value::Number(3.5).to_json(), "3.5");
    }

    #[test]
    fn accessors_return_none_for_the_wrong_type() {
        assert_eq!(Value::Bool(true).as_str(), None);
        assert_eq!(Value::Null.as_f64(), None);
        assert_eq!(Value::Number(1.0).as_bool(), None);
        assert_eq!(Value::String("t".into()).as_bool(), None);
    }
}
