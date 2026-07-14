//! Throughput benchmark for the Mission pipeline — parse, query, render.
//!
//! Zero dependencies, even here: a `std`-only harness (`harness = false`) so `cargo bench` needs no
//! toolchain beyond Rust. It measures Mission's *own* throughput; it does not compare against other
//! libraries. Run with `cargo bench`.

use std::hint::black_box;
use std::time::Instant;

use mission::parser::{parse, select};
use mission::renderer::render_text;

/// A realistic ~0.7 KB article block — headings, prose with inline markup, links, a list, a table,
/// and a blockquote — repeated to build documents of a given size.
const BLOCK: &str = "\
<article class=\"post\">\
<header><h2 id=\"s\">Section Heading</h2>\
<p class=\"meta\">By <a href=\"/author/jane\">Jane Doe</a> &middot; <time datetime=\"2026-01-02\">Jan 2</time></p></header>\
<p>Lorem ipsum dolor sit amet, <a href=\"https://example.com/a\">consectetur</a> adipiscing elit, sed \
do <em>eiusmod</em> tempor incididunt ut <strong>labore</strong> et dolore magna aliqua.</p>\
<ul><li><a href=\"/x/1\">Item one</a></li><li><a href=\"/x/2\">Item two</a></li><li>Item three</li></ul>\
<table><tr><th>Name</th><th>Value</th></tr><tr><td>alpha</td><td>1</td></tr><tr><td>beta</td><td>2</td></tr></table>\
<blockquote>A quoted line with a <a href=\"/ref\">reference</a> &amp; an entity.</blockquote>\
</article>";

/// Build an HTML document of roughly `target` bytes by repeating [`BLOCK`] inside a page wrapper.
fn document(target: usize) -> String {
    let mut s = String::with_capacity(target + BLOCK.len() + 64);
    s.push_str("<!doctype html><html><body><main>");
    while s.len() < target {
        s.push_str(BLOCK);
    }
    s.push_str("</main></body></html>");
    s
}

/// Time `f` and return `(median_ms_per_op, throughput_mb_per_s)`. Best-of-8 batches (min = least
/// noise), with the per-batch iteration count auto-scaled so each batch does ~20 MB of work.
fn measure(bytes: usize, mut f: impl FnMut()) -> (f64, f64) {
    for _ in 0..3 {
        f(); // warmup
    }
    let iters = (20_000_000 / bytes).max(1);
    let mut best = f64::INFINITY;
    for _ in 0..8 {
        let t = Instant::now();
        for _ in 0..iters {
            f();
        }
        let per = t.elapsed().as_secs_f64() / iters as f64;
        best = best.min(per);
    }
    (best * 1e3, (bytes as f64 / 1_000_000.0) / best)
}

fn main() {
    println!("mission throughput — parse / select / render (best of 8, single core)\n");
    println!(
        "  {:<8} {:>8}  {:<18} {:>10} {:>12}",
        "size", "bytes", "stage", "ms/op", "MB/s"
    );
    for (name, target) in [
        ("small", 4_000usize),
        ("medium", 64_000),
        ("large", 1_000_000),
    ] {
        let html = document(target);
        let bytes = html.len();

        let (ms, mbps) = measure(bytes, || {
            black_box(parse(black_box(&html)));
        });
        println!(
            "  {name:<8} {bytes:>8}  {:<18} {ms:>10.3} {mbps:>12.0}",
            "parse"
        );

        let dom = parse(&html);
        let (ms, mbps) = measure(bytes, || {
            black_box(select(black_box(&dom), "a[href]"));
        });
        println!(
            "  {name:<8} {bytes:>8}  {:<18} {ms:>10.3} {mbps:>12.0}",
            "select a[href]"
        );

        let (ms, mbps) = measure(bytes, || {
            black_box(render_text(black_box(&dom)));
        });
        println!(
            "  {name:<8} {bytes:>8}  {:<18} {ms:>10.3} {mbps:>12.0}",
            "render_text"
        );
    }
}
