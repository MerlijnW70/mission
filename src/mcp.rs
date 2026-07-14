//! A from-scratch, zero-dependency **MCP server** for Mission: it speaks JSON-RPC 2.0 (Model
//! Context Protocol) over stdio, wrapping the library so an agent can render and query HTML.
//!
//! This module is pure logic — [`handle`] turns one request string into one response string (or
//! `None` for a notification). The stdio read/write loop lives in the `mission-mcp` binary. Like
//! the renderer and json layers, it only reads the parsed tree; it never touches the network.

use crate::json::{self, Value};
use crate::{parser, renderer};

/// The MCP protocol revision this server implements.
const PROTOCOL_VERSION: &str = "2024-11-05";

/// Handle one JSON-RPC message. Returns the response to write, or `None` for a notification (which
/// gets no reply). A malformed request yields a JSON-RPC parse error with a null id.
pub fn handle(request: &str) -> Option<String> {
    let req = match json::from_str(request) {
        Some(v) => v,
        None => return Some(error_response(&Value::Null, -32700, "Parse error")),
    };
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let null = Value::Null;
    let id = req.get("id").unwrap_or(&null);

    // Notifications (the `notifications/` namespace, e.g. `initialized`) get no response.
    if method.starts_with("notifications/") {
        return None;
    }

    match method {
        "initialize" => Some(success_response(id, initialize_result())),
        "tools/list" => Some(success_response(id, tools_list_result())),
        "tools/call" => Some(tools_call(id, req.get("params"))),
        _ => Some(error_response(id, -32601, "Method not found")),
    }
}

/// The `initialize` result: protocol version, the tools capability, and server identity.
fn initialize_result() -> Value {
    obj(vec![
        ("protocolVersion", s(PROTOCOL_VERSION)),
        ("capabilities", obj(vec![("tools", obj(vec![]))])),
        (
            "serverInfo",
            obj(vec![
                ("name", s("mission")),
                ("version", s(env!("CARGO_PKG_VERSION"))),
            ]),
        ),
    ])
}

/// The `tools/list` result: Mission's capabilities as MCP tools with JSON-Schema inputs.
fn tools_list_result() -> Value {
    let html = || string_prop("the HTML document");
    let selector = || string_prop("a CSS selector");
    obj(vec![(
        "tools",
        Value::Array(vec![
            tool(
                "render",
                "Render an HTML document to readable plain text (headings, lists, tables, links).",
                vec![
                    ("html", html()),
                    (
                        "width",
                        num_prop("optional column width to wrap the text at"),
                    ),
                ],
                &["html"],
            ),
            tool(
                "select",
                "Query HTML with a CSS selector; returns the matching elements as a JSON array.",
                vec![("html", html()), ("selector", selector())],
                &["html", "selector"],
            ),
            tool(
                "select_text",
                "Query HTML with a CSS selector; returns each matching element rendered as text.",
                vec![("html", html()), ("selector", selector())],
                &["html", "selector"],
            ),
            tool(
                "attributes",
                "For each element matching a selector, extract one attribute's value (JSON array).",
                vec![
                    ("html", html()),
                    ("selector", selector()),
                    ("name", string_prop("the attribute name to extract")),
                ],
                &["html", "selector", "name"],
            ),
        ]),
    )])
}

/// Dispatch a `tools/call`: validate params, run the named tool, wrap its text output.
fn tools_call(id: &Value, params: Option<&Value>) -> String {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params.and_then(|p| p.get("arguments"));
    match run_tool(name, args) {
        Some(text) => success_response(id, tool_text_result(&text)),
        None => error_response(id, -32602, "unknown tool or missing/invalid arguments"),
    }
}

/// Run a tool against its arguments, returning the text to report, or `None` if the tool is
/// unknown or a required argument is missing.
fn run_tool(name: &str, args: Option<&Value>) -> Option<String> {
    let args = args?;
    let html = args.get("html").and_then(Value::as_str)?;
    let dom = parser::parse(html);
    match name {
        "render" => Some(match args.get("width").and_then(Value::as_f64) {
            Some(w) if w >= 1.0 => renderer::render_text_wrapped(&dom, w as usize),
            _ => renderer::render_text(&dom),
        }),
        "select" => {
            let selector = args.get("selector").and_then(Value::as_str)?;
            Some(json::selection(&parser::select(&dom, selector)))
        }
        "select_text" => {
            let selector = args.get("selector").and_then(Value::as_str)?;
            Some(renderer::render_nodes(&parser::select(&dom, selector)))
        }
        "attributes" => {
            let selector = args.get("selector").and_then(Value::as_str)?;
            let name = args.get("name").and_then(Value::as_str)?;
            let nodes = parser::select(&dom, selector);
            let values: Vec<&str> = nodes.iter().filter_map(|n| n.attr(name)).collect();
            Some(json::strings(&values))
        }
        _ => None,
    }
}

// --- response / value builders ---------------------------------------------------------------

/// A `{"jsonrpc":"2.0","id":…,"result":…}` response, serialized.
fn success_response(id: &Value, result: Value) -> String {
    obj(vec![
        ("jsonrpc", s("2.0")),
        ("id", id.clone()),
        ("result", result),
    ])
    .to_json()
}

/// A `{"jsonrpc":"2.0","id":…,"error":{code,message}}` response, serialized.
fn error_response(id: &Value, code: i64, message: &str) -> String {
    let error = obj(vec![
        ("code", Value::Number(code as f64)),
        ("message", s(message)),
    ]);
    obj(vec![
        ("jsonrpc", s("2.0")),
        ("id", id.clone()),
        ("error", error),
    ])
    .to_json()
}

/// Wrap plain text as an MCP tool result: `{"content":[{"type":"text","text":…}]}`.
fn tool_text_result(text: &str) -> Value {
    obj(vec![(
        "content",
        Value::Array(vec![obj(vec![("type", s("text")), ("text", s(text))])]),
    )])
}

/// Build a JSON object from `(key, value)` pairs.
fn obj(pairs: Vec<(&str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// A JSON string value.
fn s(text: &str) -> Value {
    Value::String(text.to_string())
}

/// A `{"type":"string","description":…}` schema property.
fn string_prop(description: &str) -> Value {
    obj(vec![("type", s("string")), ("description", s(description))])
}

/// A `{"type":"number","description":…}` schema property.
fn num_prop(description: &str) -> Value {
    obj(vec![("type", s("number")), ("description", s(description))])
}

/// A tool definition: name, description, and an object input schema with required keys.
fn tool(name: &str, description: &str, properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    obj(vec![
        ("name", s(name)),
        ("description", s(description)),
        (
            "inputSchema",
            obj(vec![
                ("type", s("object")),
                ("properties", obj(properties)),
                (
                    "required",
                    Value::Array(required.iter().map(|r| s(r)).collect()),
                ),
            ]),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse the response to `request` back into a `Value` for structural assertions.
    fn resp(request: &str) -> Value {
        json::from_str(&handle(request).unwrap()).unwrap()
    }

    /// Call `tool` with a JSON `arguments` object and return the text of its first content item.
    fn call(tool: &str, arguments: &str) -> String {
        let request = format!(
            r#"{{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{{"name":"{tool}","arguments":{arguments}}}}}"#
        );
        match resp(&request)
            .get("result")
            .unwrap()
            .get("content")
            .unwrap()
        {
            Value::Array(items) => items[0]
                .get("text")
                .and_then(Value::as_str)
                .unwrap()
                .to_string(),
            _ => panic!("content was not an array"),
        }
    }

    fn error_code(response: &Value) -> Option<f64> {
        response.get("error")?.get("code")?.as_f64()
    }

    #[test]
    fn initialize_reports_protocol_and_server_info() {
        let r = resp(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        let result = r.get("result").unwrap();
        assert_eq!(
            result.get("protocolVersion").and_then(Value::as_str),
            Some(PROTOCOL_VERSION)
        );
        let info = result.get("serverInfo").unwrap();
        assert_eq!(info.get("name").and_then(Value::as_str), Some("mission"));
        assert!(info.get("version").and_then(Value::as_str).is_some());
    }

    #[test]
    fn a_notification_gets_no_response() {
        assert_eq!(
            handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
    }

    #[test]
    fn tools_list_returns_the_four_tools_in_order() {
        let tools = match resp(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#)
            .get("result")
            .unwrap()
            .get("tools")
            .unwrap()
        {
            Value::Array(a) => a.clone(),
            _ => panic!("tools was not an array"),
        };
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| t.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(names, ["render", "select", "select_text", "attributes"]);
        // Each tool carries an object input schema.
        for t in &tools {
            assert_eq!(
                t.get("inputSchema")
                    .unwrap()
                    .get("type")
                    .and_then(Value::as_str),
                Some("object")
            );
        }
    }

    #[test]
    fn render_tool_returns_rendered_text() {
        assert_eq!(
            call("render", r#"{"html":"<h1>Hi</h1><p>a <b>b</b></p>"}"#),
            "# Hi\na *b*"
        );
    }

    #[test]
    fn render_tool_honors_a_width_at_least_one_and_ignores_smaller() {
        assert_eq!(
            call("render", r#"{"html":"<p>a b c</p>","width":1}"#),
            "a\nb\nc"
        );
        // width below 1 falls back to unwrapped rendering (pins the `w >= 1.0` guard).
        assert_eq!(
            call("render", r#"{"html":"<p>a b c</p>","width":0}"#),
            "a b c"
        );
    }

    #[test]
    fn select_tool_returns_element_json() {
        assert_eq!(
            call(
                "select",
                r#"{"html":"<a href=\"/x\">go</a>","selector":"a"}"#
            ),
            r#"[{"tag":"a","attrs":{"href":"/x"},"text":"go"}]"#
        );
    }

    #[test]
    fn select_text_tool_renders_each_match() {
        assert_eq!(
            call(
                "select_text",
                r#"{"html":"<p>x</p><p>y</p>","selector":"p"}"#
            ),
            "x\ny"
        );
    }

    #[test]
    fn attributes_tool_extracts_values() {
        assert_eq!(
            call(
                "attributes",
                r#"{"html":"<a href=\"/x\"></a><a href=\"/y\"></a>","selector":"a","name":"href"}"#
            ),
            r#"["/x","/y"]"#
        );
    }

    #[test]
    fn an_unknown_method_is_method_not_found() {
        assert_eq!(
            error_code(&resp(r#"{"jsonrpc":"2.0","id":1,"method":"nope"}"#)),
            Some(-32601.0)
        );
    }

    #[test]
    fn a_malformed_request_is_a_parse_error_with_null_id() {
        let r = resp("not json");
        assert_eq!(r.get("id"), Some(&Value::Null));
        assert_eq!(error_code(&r), Some(-32700.0));
    }

    #[test]
    fn an_unknown_tool_is_invalid_params() {
        let r = resp(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"bogus","arguments":{"html":"<p>x</p>"}}}"#,
        );
        assert_eq!(error_code(&r), Some(-32602.0));
    }

    #[test]
    fn a_tool_call_missing_a_required_argument_errors() {
        // select without a selector, and render without html — both invalid.
        let no_selector = resp(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"select","arguments":{"html":"<p>x</p>"}}}"#,
        );
        assert_eq!(error_code(&no_selector), Some(-32602.0));
        let no_html = resp(
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"render","arguments":{}}}"#,
        );
        assert_eq!(error_code(&no_html), Some(-32602.0));
    }

    #[test]
    fn the_request_id_is_echoed_for_int_and_string_ids() {
        let int_id = resp(r#"{"jsonrpc":"2.0","id":42,"method":"initialize","params":{}}"#);
        assert_eq!(int_id.get("id"), Some(&Value::Number(42.0)));
        let str_id = resp(r#"{"jsonrpc":"2.0","id":"abc","method":"initialize","params":{}}"#);
        assert_eq!(str_id.get("id").and_then(Value::as_str), Some("abc"));
    }
}
