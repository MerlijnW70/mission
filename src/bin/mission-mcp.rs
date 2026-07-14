//! MCP server entrypoint for Mission: a line-delimited JSON-RPC 2.0 loop over stdio. Each line of
//! input is one request; every decision is delegated to the [`mission::mcp`] module.
//! Pure I/O with no logic of its own.

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break }; // input closed or not UTF-8
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = mission::mcp::handle(&line)
            && (writeln!(out, "{response}").is_err() || out.flush().is_err())
        {
            break; // stdout closed
        }
    }
}
