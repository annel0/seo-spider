//! Time `extract` on saved pages: `cargo run --release --example extract_bench -- page.html...`

use std::time::{Duration, Instant};

use seo_page::{extract_with, ExtractOptions, Source};
use url::Url;

fn time(html: &str, url: &Url, options: &ExtractOptions) -> (Duration, usize) {
    let mut runs = 0;
    let started = Instant::now();
    let mut links = 0;
    while started.elapsed() < Duration::from_millis(1500) || runs < 3 {
        links = extract_with(html, url, Source::Raw, options).links.len();
        runs += 1;
    }
    (started.elapsed() / runs, links)
}

fn main() {
    let url = Url::parse("https://example.com/page").unwrap();
    for path in std::env::args().skip(1) {
        let html = std::fs::read_to_string(&path).unwrap_or_default();
        let (full, links) = time(&html, &url, &ExtractOptions::default());
        let (head, _) = time(&html, &url, &ExtractOptions::head_only());
        println!(
            "{:>8} KB {:>5} links  full {:>7.2} ms  head-only {:>7.2} ms  {:>6.1} MB/s  {}",
            html.len() / 1024,
            links,
            full.as_secs_f64() * 1000.0,
            head.as_secs_f64() * 1000.0,
            html.len() as f64 / 1048576.0 / full.as_secs_f64(),
            path.rsplit('/').next().unwrap_or(&path),
        );
    }
}
