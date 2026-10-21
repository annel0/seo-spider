//! Time `extract` and `extract_stream` on saved pages and show where their
//! facts differ: `cargo run --release --example extract_bench -- page.html...`

use std::time::{Duration, Instant};

use seo_page::{extract_stream, extract_with, ExtractOptions, PageFacts, Source};
use url::Url;

fn time(f: impl Fn() -> PageFacts) -> Duration {
    let mut runs = 0;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1000) || runs < 3 {
        std::hint::black_box(f());
        runs += 1;
    }
    started.elapsed() / runs
}

fn main() {
    let url = Url::parse("https://example.com/page").unwrap();
    let all = ExtractOptions::default();
    for path in std::env::args().skip(1) {
        let html = std::fs::read_to_string(&path).unwrap_or_default();
        let tree = time(|| extract_with(&html, &url, Source::Raw, &all));
        let stream = time(|| extract_stream(&html, &url, Source::Raw, &all));
        println!(
            "{:>6} KB  tree {:>7.2} ms  stream {:>7.2} ms  x{:.1}  {}",
            html.len() / 1024,
            tree.as_secs_f64() * 1000.0,
            stream.as_secs_f64() * 1000.0,
            tree.as_secs_f64() / stream.as_secs_f64(),
            path.rsplit('/').next().unwrap_or(&path),
        );
        let a = serde_json::to_value(extract_with(&html, &url, Source::Raw, &all)).unwrap();
        let b = serde_json::to_value(extract_stream(&html, &url, Source::Raw, &all)).unwrap();
        for (field, left) in a.as_object().unwrap() {
            let right = &b[field];
            if left == right {
                continue;
            }
            match (left, right) {
                (serde_json::Value::Array(l), serde_json::Value::Array(r)) => {
                    let first = l.iter().zip(r).position(|(x, y)| x != y).unwrap_or(l.len().min(r.len()));
                    println!(
                        "    {field}: {} vs {} items; first difference at {first}: {} | {}",
                        l.len(),
                        r.len(),
                        l.get(first).map_or("-".into(), |v| v.to_string().chars().take(150).collect::<String>()),
                        r.get(first).map_or("-".into(), |v| v.to_string().chars().take(150).collect::<String>()),
                    );
                }
                _ => println!("    {field}: {left} | {right}"),
            }
        }
    }
}
