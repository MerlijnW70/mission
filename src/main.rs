//! Thin CLI entrypoint for the Mission browser.
//!
//! It owns only the I/O the program cannot avoid — read the arguments, read a file, write
//! stdout/stderr, choose an exit code — and delegates every decision to the
//! [`mission::cli`] module. It carries no logic of its own — all behaviour lives in the library.
//!
//! Usage:
//!   mission                            render a built-in demo document
//!   mission <file>... [--select <css>] render/query one or more files (`-` = stdin)
//!   mission --help                     the full option list

use std::process::ExitCode;

use mission::cli::{self, Outcome, StdoutSink};
use mission::parser::parse;

/// A small self-contained page shown when no file argument is given.
const DEMO: &str = "\
<!DOCTYPE html>\
<html>\
<body>\
<!-- this comment is skipped entirely and never reaches the output -->\
<style>h1 { color: rebeccapurple }</style>\
<h1>Mission Browser</h1>\
<p>A tiny <b>text-mode</b> browser, built one careful step at a time.</p>\
<p>It parses and renders HTML — each layer sealed behind a constitutional boundary.</p>\
<p>Fast &amp; safe: zero runtime dependencies, thoroughly tested.</p>\
<p>Line one<br>line two, split by a void &lt;br&gt;.</p>\
<p>Source lives at <a href=\"https://github.com/MerlijnW70/mission\">the repo</a>.</p>\
</body>\
</html>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = cli::parse_args(&args);

    if config.help {
        print!("{}", cli::HELP);
        return ExitCode::SUCCESS;
    }
    if config.version {
        println!("mission {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    // One locked stdout for the whole run: the sink writes each record and flushes it immediately,
    // so a downstream reader (an agent, `head`, another pipe stage) sees each match the moment it
    // is produced rather than at end of run.
    let stdout = std::io::stdout();
    let mut sink = StdoutSink::new(stdout.lock());

    // Filter mode reads its jobs from stdin, one NDJSON request per line, and never touches files.
    if config.filter {
        let stdin = std::io::stdin();
        cli::run_filter(stdin.lock(), &mut sink);
        return ExitCode::SUCCESS;
    }

    // Gather the inputs to process: each path (with `-` = stdin), or the demo when none is given.
    let inputs: Vec<(String, String)> = if config.paths.is_empty() {
        vec![("<demo>".to_string(), DEMO.to_string())]
    } else {
        let mut gathered = Vec::new();
        for &path in &config.paths {
            let read = if path == "-" {
                read_stdin()
            } else {
                std::fs::read_to_string(path)
            };
            match read {
                Ok(html) => gathered.push((path.to_string(), html)),
                Err(e) => {
                    eprintln!("mission: cannot read {path}: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        gathered
    };

    let mut code = ExitCode::SUCCESS;
    for (name, input) in &inputs {
        let dom = parse(input);
        if let Outcome::FailOnEmpty = cli::run(&config, &dom, &mut sink) {
            eprintln!(
                "mission: no elements match `{}` in {name}",
                config.selector.unwrap_or("")
            );
            code = ExitCode::FAILURE;
        }
    }
    code
}

/// Read all of standard input as a UTF-8 string (for the `-` path).
fn read_stdin() -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
