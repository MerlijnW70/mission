//! Command-line surface for the Mission browser: argument parsing and output dispatch.
//!
//! This is the logic that used to live inline in `main.rs` — a hand-rolled flag parser and a
//! multi-way output-mode selector. Both carry real, branch-heavy behavior, so they belong in a
//! dedicated module rather than in the thin entrypoint. `main.rs` keeps only the I/O it
//! cannot help but own (reading a file, writing stdout/stderr, the process exit code); every
//! decision it used to make is now here, behind pure functions the tests below must pin.
//!
//! Like the renderer and the json layer, this module only reads the parsed tree — it has no
//! business with the network.

use std::io::{BufRead, Write};

use crate::json::{self, Value};
use crate::parser::{Node, parse, select};
use crate::renderer::{render_text, render_text_wrapped};

/// The parsed command line: which file to read (if any) and how to query and format it.
///
/// String fields borrow from the original argument vector, so a `Config` lives only as long as
/// the `args` it was parsed from.
#[derive(Debug, Default, PartialEq)]
pub struct Config<'a> {
    /// Positional input paths (`-` means stdin); empty renders the built-in demo.
    pub paths: Vec<&'a str>,
    /// `--select <css>`: a CSS selector to narrow the document to matching elements.
    pub selector: Option<&'a str>,
    /// `--attr <name>`: print this attribute of each match instead of its text.
    pub attr: Option<&'a str>,
    /// `--json`: emit machine-readable JSON.
    pub as_json: bool,
    /// `--jsonl`: emit one JSON object per match, on its own line.
    pub jsonl: bool,
    /// `--pretty`: emit indented (one-object-per-line) JSON.
    pub pretty: bool,
    /// `--count`: print the number of matches instead of their content.
    pub count: bool,
    /// `--fail-on-empty`: exit non-zero if a selector matches nothing.
    pub fail_on_empty: bool,
    /// `--filter`: read one NDJSON `{html, selector}` job per stdin line and emit one NDJSON
    /// result line each — the engine as a stream-in / stream-out pipeline data-slicer.
    pub filter: bool,
    /// `--width <n>`: wrap rendered text to at most `n` columns (`None` = no wrapping).
    pub width: Option<usize>,
    /// `--help`/`-h`: print usage and exit.
    pub help: bool,
    /// `--version`/`-V`: print the version and exit.
    pub version: bool,
}

/// Usage text for `--help`.
pub const HELP: &str = "\
mission — a zero-dependency text-mode HTML browser and CSS query engine

USAGE:
    mission [FILE]... [OPTIONS]

    With no FILE, renders a built-in demo. Use '-' to read from stdin; multiple
    files are processed in order.

OPTIONS:
    -s, --select <CSS>    print the elements matching a CSS selector
        --attr <NAME>     print that attribute of each match (with --select)
        --json            emit JSON (element objects, or values with --attr)
        --jsonl           emit one JSON object per match, per line
        --pretty          emit indented JSON
        --count           print the number of matches
        --width <N>       wrap rendered text to at most N columns
        --fail-on-empty   exit non-zero if a selector matches nothing
        --filter          NDJSON pipe mode: read one {\"html\",\"selector\"} job per stdin
                          line, emit one {\"matches\":[...]} result line each (flushed)
    -h, --help            print this help
    -V, --version         print the version
";

/// A sink for the records [`run`] streams out, in order. Streaming modes (`--jsonl`, selector
/// text, and `--attr` text) emit one record per match, so the real sink can write-and-flush each
/// the moment it is produced — near-zero time-to-first-line and no whole-output buffer held in
/// memory. Aggregate modes (a JSON array, `--pretty`, `--count`) emit a single record. The
/// flushing, stdout-backed sink is the thin `main` entrypoint's job (pure I/O); the *decision* of
/// what to emit, and in what order, stays here where the recording sink in the tests can pin it.
pub trait Emit {
    /// Emit one already-formatted record, its trailing newline (if any) included.
    fn record(&mut self, text: &str);
}

/// The result of a [`run`]: it either completed (every record emitted) or bailed because a selector
/// matched nothing under `--fail-on-empty` — in which case nothing was emitted and `main` reports
/// it to stderr and exits non-zero.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// All records were emitted to the sink; exit success.
    Done,
    /// A selector matched nothing and `--fail-on-empty` was set: report to stderr, exit failure.
    FailOnEmpty,
}

/// Parse `args` (the process arguments *after* the program name) into a [`Config`].
///
/// Flags that take a value (`--select`, `--attr`) consume the following argument; a value-taking
/// flag at the very end simply captures `None`. Any argument that is not a recognized flag is a
/// positional input path, collected in order (so multiple files can be processed).
pub fn parse_args(args: &[String]) -> Config<'_> {
    let mut config = Config::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--select" | "-s" => config.selector = rest.next().map(String::as_str),
            "--attr" => config.attr = rest.next().map(String::as_str),
            "--json" => config.as_json = true,
            "--jsonl" => config.jsonl = true,
            "--pretty" => config.pretty = true,
            "--count" => config.count = true,
            "--fail-on-empty" => config.fail_on_empty = true,
            "--filter" => config.filter = true,
            "--width" => config.width = rest.next().and_then(|v| v.parse().ok()),
            "--help" | "-h" => config.help = true,
            "--version" | "-V" => config.version = true,
            other => config.paths.push(other),
        }
    }
    config
}

/// Apply a [`Config`] to a parsed document, streaming each output record to `out` (or returning a
/// fail signal). Records are emitted as they are produced — one per match in the streaming modes —
/// rather than concatenated into one string, so the sink can flush each immediately.
///
/// The node set is the selector's matches, or every root node when there is no selector. The
/// four output shapes — attribute JSON, attribute text, element JSON, and rendered text — mirror
/// the flag combinations documented on the CLI.
pub fn run(config: &Config, dom: &[Node], out: &mut dyn Emit) -> Outcome {
    let nodes: Vec<&Node> = match config.selector {
        Some(css) => select(dom, css),
        None => dom.iter().collect(),
    };

    if config.selector.is_some() && nodes.is_empty() && config.fail_on_empty {
        return Outcome::FailOnEmpty;
    }

    if config.count {
        // Grep-style: just how many matched.
        out.record(&format!("{}\n", nodes.len()));
    } else if let Some(name) = config.attr {
        // Attribute extraction: the named attribute of each node that carries it.
        let values: Vec<&str> = nodes.iter().filter_map(|n| n.attr(name)).collect();
        if config.as_json {
            out.record(&format!("{}\n", json::strings(&values)));
        } else {
            // One value per record, so each streams out on its own line.
            for value in values {
                out.record(&format!("{value}\n"));
            }
        }
    } else if config.pretty {
        out.record(&pretty_selection(&nodes));
    } else if config.jsonl {
        // One JSON object per match, each its own record (and thus its own flushed line).
        for node in &nodes {
            out.record(&format!("{}\n", json::object(node)));
        }
    } else if config.as_json {
        out.record(&format!("{}\n", json::selection(&nodes)));
    } else if config.selector.is_some() {
        // Selector text mode: each match its own record on its own line (links keep their URL).
        for node in &nodes {
            out.record(&format!("{}\n", render(config, std::slice::from_ref(node))));
        }
    } else {
        out.record(&format!("{}\n", render(config, dom)));
    }
    Outcome::Done
}

/// Process one NDJSON request line for `--filter` mode into its NDJSON response line.
///
/// A request is a JSON object `{"html": "…", "selector": "…"}`. The response is
/// `{"matches":[ <element object>, … ]}` — the elements matching the selector, in document order —
/// or `{"error":"…"}` when the line is not a JSON object carrying both string fields. Exactly one
/// response is produced per request, so the engine drops into a Unix pipeline as a stream-in /
/// stream-out data-slicer. The returned string carries no trailing newline: the caller frames each
/// record (and flushes it) through the streaming [`Emit`] sink.
pub fn filter_line(line: &str) -> String {
    let Some(value) = json::from_str(line) else {
        return filter_error("invalid JSON");
    };
    let Some(html) = value.get("html").and_then(Value::as_str) else {
        return filter_error("missing \"html\" string");
    };
    let Some(selector) = value.get("selector").and_then(Value::as_str) else {
        return filter_error("missing \"selector\" string");
    };
    let dom = parse(html);
    let nodes = select(&dom, selector);
    format!("{{\"matches\":{}}}", json::selection(&nodes))
}

/// A `{"error":"…"}` response line, with `message` JSON-escaped (routed through [`Value::String`]
/// so the escaping is the same battle-tested path the serializers use).
fn filter_error(message: &str) -> String {
    format!("{{\"error\":{}}}", Value::String(message.to_string()).to_json())
}

/// Pretty-print a selection: an indented JSON array, one object per line.
fn pretty_selection(nodes: &[&Node]) -> String {
    if nodes.is_empty() {
        return "[]\n".to_string();
    }
    let mut out = String::from("[\n");
    for (i, node) in nodes.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  ");
        out.push_str(&json::object(node));
    }
    out.push_str("\n]\n");
    out
}

/// Render `tree` to text, wrapping to `config.width` columns when that flag was given.
fn render(config: &Config, tree: &[Node]) -> String {
    match config.width {
        Some(width) => render_text_wrapped(tree, width),
        None => render_text(tree),
    }
}

/// The real [`Emit`] sink: it writes each record to a writer and flushes it at once, so the
/// streaming modes reach the reader with near-zero latency. A broken pipe (a reader that hung up,
/// e.g. `mission … | head`) is not an error — it just stops further writes. Generic over the writer
/// so the tests can drive it with an in-memory buffer; `main` wraps a locked stdout.
///
/// This carries the only real branch `main` used to own (the broken-pipe latch), so it lives here
/// in the library module rather than in the thin entrypoint.
pub struct StdoutSink<W: Write> {
    out: W,
    broken: bool,
}

impl<W: Write> StdoutSink<W> {
    /// Wrap a writer as a flushing, broken-pipe-tolerant sink.
    pub fn new(out: W) -> Self {
        StdoutSink { out, broken: false }
    }

    /// Whether the downstream reader has hung up — once true, every further record is dropped.
    pub fn is_broken(&self) -> bool {
        self.broken
    }
}

impl<W: Write> Emit for StdoutSink<W> {
    fn record(&mut self, text: &str) {
        if self.broken {
            return;
        }
        if self.out.write_all(text.as_bytes()).is_err() || self.out.flush().is_err() {
            self.broken = true; // downstream closed: stop writing, exit cleanly
        }
    }
}

/// NDJSON pipe mode: read one request per line from `reader`, hand each to [`filter_line`], and
/// stream its response line back through the flushing `sink` — so the engine slots into a Unix
/// pipeline as a continuous data-slicer. Blank lines are skipped; the loop ends at EOF (or the first
/// non-UTF-8/broken read).
pub fn run_filter<R: BufRead, W: Write>(reader: R, sink: &mut StdoutSink<W>) {
    for line in reader.lines() {
        let Ok(line) = line else { break }; // input closed or not UTF-8
        if line.trim().is_empty() {
            continue;
        }
        sink.record(&format!("{}\n", filter_line(&line)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    // --- parse_args --------------------------------------------------------------------------

    #[test]
    fn no_arguments_is_all_defaults() {
        // Distinct from any mutant that flips a default: every field must be its zero value.
        assert_eq!(parse_args(&args(&[])), Config::default());
    }

    #[test]
    fn a_bare_argument_is_a_path() {
        assert_eq!(parse_args(&args(&["page.html"])).paths, vec!["page.html"]);
    }

    #[test]
    fn select_captures_the_next_argument() {
        // Kills a mutant that drops the `rest.next()` consumption: the value would fall through
        // to the `other` arm and become the path, leaving selector None.
        let argv = args(&["--select", "h1"]);
        let parsed = parse_args(&argv);
        assert_eq!(parsed.selector, Some("h1"));
        assert!(parsed.paths.is_empty());
    }

    #[test]
    fn short_select_alias_is_equivalent() {
        // Pins the `| "-s"` alias arm specifically.
        assert_eq!(parse_args(&args(&["-s", "h1"])).selector, Some("h1"));
    }

    #[test]
    fn attr_captures_the_next_argument() {
        let argv = args(&["--attr", "href"]);
        let parsed = parse_args(&argv);
        assert_eq!(parsed.attr, Some("href"));
        assert!(parsed.paths.is_empty());
    }

    #[test]
    fn json_flag_sets_the_boolean() {
        // Kills `as_json = true` → `false`: default is false, so only a real set shows here.
        assert!(parse_args(&args(&["--json"])).as_json);
    }

    #[test]
    fn fail_on_empty_flag_sets_the_boolean() {
        assert!(parse_args(&args(&["--fail-on-empty"])).fail_on_empty);
    }

    #[test]
    fn filter_flag_sets_the_boolean() {
        assert!(parse_args(&args(&["--filter"])).filter);
    }

    #[test]
    fn width_flag_parses_a_column_count() {
        assert_eq!(parse_args(&args(&["--width", "72"])).width, Some(72));
    }

    #[test]
    fn a_non_numeric_width_is_ignored() {
        // Pins `.parse().ok()`: a bad value leaves width None (no wrapping) rather than crashing.
        assert_eq!(parse_args(&args(&["--width", "wide"])).width, None);
    }

    #[test]
    fn a_value_flag_at_the_end_captures_none_without_panicking() {
        // The iterator form must yield None (not index-panic) when a flag has no following value.
        assert_eq!(parse_args(&args(&["--select"])).selector, None);
    }

    #[test]
    fn positionals_are_collected_in_order() {
        assert_eq!(parse_args(&args(&["a.html", "b.html"])).paths, vec!["a.html", "b.html"]);
    }

    #[test]
    fn all_flags_together_are_parsed() {
        let argv = args(&[
            "page.html",
            "--select",
            "a",
            "--attr",
            "href",
            "--json",
            "--jsonl",
            "--pretty",
            "--count",
            "--fail-on-empty",
            "--filter",
            "--width",
            "72",
            "--help",
            "--version",
        ]);
        let parsed = parse_args(&argv);
        assert_eq!(
            parsed,
            Config {
                paths: vec!["page.html"],
                selector: Some("a"),
                attr: Some("href"),
                as_json: true,
                jsonl: true,
                pretty: true,
                count: true,
                fail_on_empty: true,
                filter: true,
                width: Some(72),
                help: true,
                version: true,
            }
        );
    }

    // --- run ---------------------------------------------------------------------------------

    const LINKS: &str = "<a href=\"/x\">go</a><a href=\"/y\">back</a>";

    /// A recording [`Emit`] sink: it keeps every record separately (so a test can assert the
    /// streaming *granularity*, not just the concatenated bytes) and can join them for the
    /// whole-output assertions that pin each output shape.
    #[derive(Default)]
    struct Recorder {
        records: Vec<String>,
    }

    impl Emit for Recorder {
        fn record(&mut self, text: &str) {
            self.records.push(text.to_string());
        }
    }

    impl Recorder {
        /// The concatenation of every record — what the old `Output::Write(String)` used to hold.
        fn joined(&self) -> String {
            self.records.concat()
        }
    }

    /// Run into a fresh recorder, returning it alongside the outcome.
    fn run_rec(config: &Config, dom: &[Node]) -> (Recorder, Outcome) {
        let mut rec = Recorder::default();
        let outcome = run(config, dom, &mut rec);
        (rec, outcome)
    }

    /// The joined output of a successful run (asserting it completed).
    fn output(config: &Config, dom: &[Node]) -> String {
        let (rec, outcome) = run_rec(config, dom);
        assert_eq!(outcome, Outcome::Done);
        rec.joined()
    }

    #[test]
    fn no_selector_renders_the_whole_document() {
        let dom = parse(LINKS);
        let config = Config::default();
        // Independently computed from the renderer, so a wrong branch or a dropped newline fails.
        assert_eq!(output(&config, &dom), format!("{}\n", render_text(&dom)));
    }

    #[test]
    fn width_wraps_the_rendered_document() {
        // With --width set, the whole-document render goes through render_text_wrapped.
        let dom = parse("<p>the quick brown fox jumps</p>");
        let config = Config { width: Some(9), ..Config::default() };
        assert_eq!(output(&config, &dom), format!("{}\n", render_text_wrapped(&dom, 9)));
    }

    #[test]
    fn selector_text_mode_renders_each_match_on_its_own_line() {
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), ..Config::default() };
        let a = render_text(std::slice::from_ref(&dom[0]));
        let b = render_text(std::slice::from_ref(&dom[1]));
        // Per-node rendering with a trailing newline each; a `&nodes`-at-once mutant would differ.
        assert_eq!(output(&config, &dom), format!("{a}\n{b}\n"));
    }

    #[test]
    fn selector_text_mode_streams_one_record_per_match() {
        // The streaming contract: two matches produce two separate records (each its own flushed
        // line), not one concatenated blob. Kills a mutant that renders all nodes into one record.
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), ..Config::default() };
        let (rec, outcome) = run_rec(&config, &dom);
        assert_eq!(outcome, Outcome::Done);
        assert_eq!(rec.records, vec!["go [/x]\n", "back [/y]\n"]);
    }

    #[test]
    fn attr_text_mode_prints_one_value_per_line() {
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), attr: Some("href"), ..Config::default() };
        assert_eq!(output(&config, &dom), "/x\n/y\n".to_string());
    }

    #[test]
    fn attr_text_mode_streams_one_record_per_value() {
        // One value per record, so each attribute streams out on its own flushed line.
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), attr: Some("href"), ..Config::default() };
        let (rec, _) = run_rec(&config, &dom);
        assert_eq!(rec.records, vec!["/x\n", "/y\n"]);
    }

    #[test]
    fn attr_json_mode_emits_a_json_array_of_values() {
        let dom = parse(LINKS);
        let config =
            Config { selector: Some("a"), attr: Some("href"), as_json: true, ..Config::default() };
        assert_eq!(output(&config, &dom), "[\"/x\",\"/y\"]\n".to_string());
    }

    #[test]
    fn selection_json_mode_emits_element_objects() {
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), as_json: true, ..Config::default() };
        let nodes = select(&dom, "a");
        // Kills a mutant that routes JSON output through the attr branch or the text branch.
        assert_eq!(output(&config, &dom), format!("{}\n", json::selection(&nodes)));
    }

    #[test]
    fn an_aggregate_json_array_is_a_single_record() {
        // Contrast with the streaming modes: a JSON array is one value, so it must be emitted as a
        // single record (splitting it per element would produce invalid JSON on the wire).
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), as_json: true, ..Config::default() };
        let (rec, _) = run_rec(&config, &dom);
        assert_eq!(rec.records.len(), 1);
    }

    #[test]
    fn count_reports_the_number_of_matches() {
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), count: true, ..Config::default() };
        assert_eq!(output(&config, &dom), "2\n".to_string());
    }

    #[test]
    fn jsonl_emits_one_object_per_line() {
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), jsonl: true, ..Config::default() };
        let nodes = select(&dom, "a");
        let expected = format!("{}\n{}\n", json::object(nodes[0]), json::object(nodes[1]));
        assert_eq!(output(&config, &dom), expected);
    }

    #[test]
    fn jsonl_streams_one_record_per_object() {
        // NDJSON's whole point: each object is its own record, flushed on its own line — so an agent
        // reading the pipe sees the first match without waiting for the rest.
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), jsonl: true, ..Config::default() };
        let nodes = select(&dom, "a");
        let (rec, _) = run_rec(&config, &dom);
        assert_eq!(
            rec.records,
            vec![format!("{}\n", json::object(nodes[0])), format!("{}\n", json::object(nodes[1]))]
        );
    }

    #[test]
    fn pretty_emits_an_indented_array() {
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), pretty: true, ..Config::default() };
        let nodes = select(&dom, "a");
        let expected = format!("[\n  {},\n  {}\n]\n", json::object(nodes[0]), json::object(nodes[1]));
        assert_eq!(output(&config, &dom), expected);
    }

    #[test]
    fn pretty_of_an_empty_selection_is_an_empty_array() {
        // Pins the empty-nodes branch of pretty_selection.
        let dom = parse(LINKS);
        let config = Config { selector: Some("h1"), pretty: true, ..Config::default() };
        assert_eq!(output(&config, &dom), "[]\n".to_string());
    }

    #[test]
    fn attr_extraction_skips_nodes_without_the_attribute() {
        // Only the anchor carries `href`; the `filter_map` must drop the <p>.
        let dom = parse("<a href=\"/x\">go</a><p>plain</p>");
        let config = Config { selector: Some("*"), attr: Some("href"), ..Config::default() };
        assert_eq!(output(&config, &dom), "/x\n".to_string());
    }

    #[test]
    fn fail_on_empty_fails_when_a_selector_matches_nothing() {
        let dom = parse(LINKS);
        let config = Config { selector: Some("h1"), fail_on_empty: true, ..Config::default() };
        let (rec, outcome) = run_rec(&config, &dom);
        assert_eq!(outcome, Outcome::FailOnEmpty);
        // A fail signal emits nothing: no half-written record precedes the stderr report.
        assert!(rec.records.is_empty());
    }

    #[test]
    fn fail_on_empty_is_inert_without_the_flag() {
        // Same empty match, but the flag is off: pins the `&& config.fail_on_empty` conjunct.
        let dom = parse(LINKS);
        let config = Config { selector: Some("h1"), ..Config::default() };
        let (rec, outcome) = run_rec(&config, &dom);
        assert_eq!(outcome, Outcome::Done);
        // An empty selector match under text mode emits no records at all.
        assert!(rec.records.is_empty());
    }

    #[test]
    fn a_matching_selector_never_fails_on_empty() {
        // Non-empty match with the flag on: pins the `nodes.is_empty()` conjunct (and `&&`→`||`).
        let dom = parse(LINKS);
        let config = Config { selector: Some("a"), fail_on_empty: true, ..Config::default() };
        let a = render_text(std::slice::from_ref(&dom[0]));
        let b = render_text(std::slice::from_ref(&dom[1]));
        assert_eq!(output(&config, &dom), format!("{a}\n{b}\n"));
    }

    #[test]
    fn fail_on_empty_needs_a_selector() {
        // No selector + empty document + flag on: the `selector.is_some()` conjunct must keep this
        // from failing (it renders the empty document instead). Kills `is_some()`→true and the
        // first `&&`→`||`.
        let dom = parse("");
        let config = Config { fail_on_empty: true, ..Config::default() };
        assert_eq!(output(&config, &dom), format!("{}\n", render_text(&dom)));
    }

    // --- filter (NDJSON pipe mode) -----------------------------------------------------------

    #[test]
    fn filter_line_selects_and_serializes_matches() {
        // A well-formed job parses its HTML, applies the selector, and returns the element objects
        // as `{"matches":[…]}` on one line — the same object shape `--json` emits.
        assert_eq!(
            filter_line(r#"{"html":"<a href=\"/x\">go</a>","selector":"a"}"#),
            r#"{"matches":[{"tag":"a","attrs":{"href":"/x"},"text":"go"}]}"#
        );
    }

    #[test]
    fn filter_line_reports_an_empty_match_as_an_empty_array() {
        // A selector that matches nothing is not an error — it is an empty result set. Pins the
        // success branch as distinct from the error branches.
        assert_eq!(filter_line(r#"{"html":"<p>x</p>","selector":"a"}"#), r#"{"matches":[]}"#);
    }

    #[test]
    fn filter_line_rejects_non_json_with_an_error() {
        // Pins the `from_str` None branch (invalid JSON) and its distinct message.
        assert_eq!(filter_line("not json"), r#"{"error":"invalid JSON"}"#);
    }

    #[test]
    fn filter_line_requires_an_html_string() {
        // Missing entirely, and present-but-not-a-string, both take the missing-html branch (a
        // number's `as_str()` is None) — pins that branch and its message.
        assert_eq!(filter_line(r#"{"selector":"a"}"#), r#"{"error":"missing \"html\" string"}"#);
        assert_eq!(filter_line(r#"{"html":5,"selector":"a"}"#), r#"{"error":"missing \"html\" string"}"#);
    }

    #[test]
    fn filter_line_requires_a_selector_string() {
        // html present but no selector: pins the missing-selector branch distinctly from missing
        // html (a `html`↔`selector` swap in either lookup would cross these two messages).
        assert_eq!(
            filter_line(r#"{"html":"<a></a>"}"#),
            r#"{"error":"missing \"selector\" string"}"#
        );
    }

    #[test]
    fn filter_line_escapes_control_characters_in_its_error() {
        // The error message is routed through the JSON string escaper, so a job that is valid JSON
        // but the wrong shape still yields a valid JSON error line. Here a bare array has no "html".
        assert_eq!(filter_line("[1,2]"), r#"{"error":"missing \"html\" string"}"#);
    }

    // --- StdoutSink / run_filter (the streaming I/O logic moved out of the thin main) ----------

    /// A writer that always fails, standing in for a downstream reader that has hung up.
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stdout_sink_writes_and_flushes_each_record_in_order() {
        let mut sink = StdoutSink::new(Vec::new());
        sink.record("a\n");
        sink.record("b\n");
        assert!(!sink.is_broken());
        assert_eq!(sink.out, b"a\nb\n");
    }

    #[test]
    fn stdout_sink_latches_broken_and_drops_every_later_record() {
        // The first failed write marks the sink broken (a `head`-style reader closing the pipe);
        // afterwards it is a silent no-op, never a panic. Pins the broken latch and its early return.
        let mut sink = StdoutSink::new(BrokenWriter);
        sink.record("x");
        assert!(sink.is_broken());
        sink.record("y"); // must not panic and must stay broken
        assert!(sink.is_broken());
    }

    /// A writer that fails its first write (simulating a pipe that breaks, then recovers) and
    /// records everything after. Lets a test prove the broken latch *stops* later writes rather
    /// than merely tolerating them.
    struct FailOnceWriter {
        failed: bool,
        buf: Vec<u8>,
    }
    impl Write for FailOnceWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            if !self.failed {
                self.failed = true;
                return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
            }
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn once_broken_the_sink_never_writes_again_even_if_the_writer_recovers() {
        // The latch must SHORT-CIRCUIT later records, not just survive them: after the first write
        // fails, the second record is dropped even though the underlying writer would now accept it.
        // Pins the `if self.broken { return }` guard — without it, "second" would reach the buffer.
        let mut sink = StdoutSink::new(FailOnceWriter { failed: false, buf: Vec::new() });
        sink.record("first"); // fails, latches broken
        sink.record("second"); // must be dropped by the latch, not retried
        assert!(sink.is_broken());
        assert!(sink.out.buf.is_empty(), "a latched-broken sink must not write later records");
    }

    #[test]
    fn run_filter_streams_one_response_per_nonblank_line() {
        // Two jobs separated by a blank line: the blank is skipped and each job yields exactly one
        // response line, in order. Pins the blank-line skip and the per-line record framing.
        let input = concat!(
            r#"{"html":"<a href=\"/x\">go</a>","selector":"a"}"#,
            "\n\n",
            r#"{"html":"<p>x</p>","selector":"a"}"#,
            "\n",
        );
        let mut sink = StdoutSink::new(Vec::new());
        run_filter(input.as_bytes(), &mut sink);
        assert_eq!(
            String::from_utf8(sink.out).unwrap(),
            "{\"matches\":[{\"tag\":\"a\",\"attrs\":{\"href\":\"/x\"},\"text\":\"go\"}]}\n{\"matches\":[]}\n"
        );
    }

    #[test]
    fn run_filter_stops_at_a_broken_pipe_without_panicking() {
        // A downstream reader that hangs up mid-stream: run_filter records into a broken sink and
        // simply produces nothing, cleanly.
        let input = concat!(r#"{"html":"<a></a>","selector":"a"}"#, "\n");
        let mut sink = StdoutSink::new(BrokenWriter);
        run_filter(input.as_bytes(), &mut sink);
        assert!(sink.is_broken());
    }
}
