//! Synthetic site for crawler throughput benchmarks:
//!
//!     bench-site <pages> [port]
//!
//! Pages are generated from their number on every request, so the server does
//! no I/O and a crawl of it measures the crawler. It listens on 127.0.0.1 (the
//! site) and 127.0.0.2 (an "external" host the pages link to).
//!
//! Page `/p/{i}` is about 40 KB: a full `<head>`, ten nav links, forty links
//! spread over the site, links to its neighbours (so every page is reachable),
//! a redirecting link, a broken link every tenth page, five images, a script,
//! a stylesheet and five external links.

use std::convert::Infallible;
use std::fmt::Write as _;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

const WORDS: &[&str] = &[
    "crawler", "render", "sitemap", "canonical", "index", "robots", "anchor", "heading", "content",
    "speed", "page", "link", "search", "engine", "audit", "redirect", "status", "header", "meta",
    "title", "description", "image", "script", "style", "mobile", "schema", "language", "server",
];

fn words(seed: usize, count: usize, out: &mut String) {
    let mut state = seed.wrapping_mul(2654435761).wrapping_add(1);
    for n in 0..count {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        if n > 0 {
            out.push(' ');
        }
        out.push_str(WORDS[state % WORDS.len()]);
    }
}

fn page(i: usize, pages: usize, port: u16) -> String {
    let mut html = String::with_capacity(48 * 1024);
    let mut description = String::new();
    words(i * 3 + 1, 22, &mut description);
    let _ = write!(
        html,
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>Page {i} of the bench site</title>
<meta name="description" content="{description}">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="canonical" href="/p/{i}">
<link rel="alternate" hreflang="en" href="/p/{i}"><link rel="alternate" hreflang="x-default" href="/p/{i}">
<meta property="og:title" content="Page {i}"><link rel="stylesheet" href="/static/site.css">
<script type="application/ld+json">{{"@context":"https://schema.org","@type":"WebPage","name":"Page {i}"}}</script>
</head><body><header><nav>"#
    );
    for n in 0..10 {
        let _ = write!(html, r#"<a href="/p/{n}">Section {n}</a> "#);
    }
    let _ = write!(html, "</nav></header><main><h1>Page {i}</h1>");
    for section in 0..3 {
        let _ = write!(html, "<h2>Part {section} of page {i}</h2>");
        for paragraph in 0..4 {
            html.push_str("<p>");
            words(i * 31 + section * 7 + paragraph, 60, &mut html);
            html.push_str("</p>");
        }
        for k in 0..13 {
            let target = (i * 31 + (section * 13 + k) * 97) % pages;
            let _ = write!(html, r#"<p><a href="/p/{target}">Read page {target}</a></p>"#);
        }
    }
    let next = (i + 1) % pages;
    let previous = (i + pages - 1) % pages;
    let _ = write!(
        html,
        r#"<a href="/p/{previous}">Previous</a> <a href="/p/{next}">Next</a> <a href="/r/{next}">Moved</a>"#
    );
    if i.is_multiple_of(10) {
        let _ = write!(html, r#" <a href="/missing/{i}">Broken</a>"#);
    }
    for k in 0..5 {
        let _ = write!(html, r#"<img src="/img/{}.png" alt="Picture {k}" width="64" height="64">"#, (i + k) % 50);
    }
    html.push_str("</main><footer>");
    for k in 0..5 {
        let _ = write!(html, r#"<a href="http://127.0.0.2:{port}/ext/{}">Partner {k}</a> "#, (i + k) % 20);
    }
    html.push_str(r#"</footer><script src="/static/app.js"></script></body></html>"#);
    html
}

fn respond(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", content_type)
        .body(Full::new(body.into()))
        .expect("static response parts are valid")
}

fn route(path: &str, pages: usize, port: u16) -> Response<Full<Bytes>> {
    let number = |prefix: &str| path.strip_prefix(prefix).and_then(|rest| rest.parse::<usize>().ok());
    if let Some(i) = number("/p/").filter(|&i| i < pages) {
        return respond(StatusCode::OK, "text/html; charset=utf-8", page(i, pages, port));
    }
    if let Some(i) = number("/r/") {
        return Response::builder()
            .status(StatusCode::MOVED_PERMANENTLY)
            .header("location", format!("/p/{i}"))
            .body(Full::new(Bytes::new()))
            .expect("static response parts are valid");
    }
    match path {
        "/" => respond(StatusCode::OK, "text/html; charset=utf-8", page(0, pages, port)),
        "/robots.txt" => respond(
            StatusCode::OK,
            "text/plain",
            format!("User-agent: *\nDisallow: /private/\nSitemap: http://127.0.0.1:{port}/sitemap.xml\n"),
        ),
        "/sitemap.xml" => {
            let mut xml = String::from(r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">"#);
            for i in 0..pages.min(50_000) {
                let _ = write!(xml, "<url><loc>http://127.0.0.1:{port}/p/{i}</loc></url>");
            }
            xml.push_str("</urlset>");
            respond(StatusCode::OK, "application/xml", xml)
        }
        "/static/app.js" => respond(StatusCode::OK, "application/javascript", "document.documentElement.dataset.js = 1;"),
        "/static/site.css" => respond(StatusCode::OK, "text/css", "body{font:16px sans-serif}"),
        _ if path.starts_with("/img/") => respond(StatusCode::OK, "image/png", vec![0u8; 256]),
        _ if path.starts_with("/ext/") => respond(StatusCode::OK, "text/html", "<title>Partner</title>"),
        _ => respond(StatusCode::NOT_FOUND, "text/html", "<title>Not found</title>"),
    }
}

async fn serve(listener: TcpListener, pages: usize, port: u16) {
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let _ = stream.set_nodelay(true);
        tokio::spawn(async move {
            let service = service_fn(move |request: Request<Incoming>| async move {
                Ok::<_, Infallible>(route(request.uri().path(), pages, port))
            });
            let _ = http1::Builder::new().serve_connection(TokioIo::new(stream), service).await;
        });
    }
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let pages: usize = args.next().and_then(|value| value.parse().ok()).unwrap_or(10_000).max(1);
    let port: u16 = args.next().and_then(|value| value.parse().ok()).unwrap_or(8090);
    let site = TcpListener::bind(("127.0.0.1", port)).await.expect("bind 127.0.0.1");
    let external = TcpListener::bind(("127.0.0.2", port)).await.expect("bind 127.0.0.2");
    eprintln!("bench site: {pages} pages on http://127.0.0.1:{port}/ (external host 127.0.0.2)");
    tokio::join!(serve(site, pages, port), serve(external, pages, port));
}
