//! The auto mode: whether a site needs its JavaScript run to be crawled as
//! a search engine renders it. Before the crawl, the start page and a page
//! of each section it links to are read twice, as the server sends them and
//! rendered, and compared on what an SEO crawl reads: text, links, title,
//! H1, description, canonical, robots. If JavaScript changes any of it on
//! any of them, every page is rendered; if not, the crawl reads the HTML
//! only, several times faster.

use std::collections::HashSet;

use futures_util::stream::{FuturesUnordered, StreamExt};
use reqwest::Client;
use seo_page::{ExtractOptions, PageFacts, Source};
use seo_render::RenderPool;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{fetch, robots::Robots, CrawlConfig, Rules};

/// What the auto mode decided, and on what; kept in the crawl's settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderDecision {
    /// Pages are rendered.
    pub render: bool,
    /// Why, in Russian, as the UI shows it.
    pub reason: String,
    pub samples: Vec<Sample>,
}

/// One page read both ways.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub url: String,
    pub words_raw: usize,
    pub words_rendered: usize,
    pub links_raw: usize,
    pub links_rendered: usize,
    /// What JavaScript changes that a crawl reads, in Russian; empty when
    /// nothing that matters.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<String>,
    /// The page could not be read one of the two ways.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A page as the server sent it and as rendered.
struct Read {
    url: Url,
    raw: Result<PageFacts, String>,
    rendered: Result<PageFacts, String>,
}

/// Paths that are files, not pages: no use reading them both ways.
const FILES: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".webp", ".svg", ".pdf", ".zip", ".rar", ".doc", ".docx", ".xls", ".xlsx", ".mp4",
    ".mp3", ".css", ".js", ".xml", ".txt",
];

/// Decides on the first start URL and up to `pages - 1` pages of other
/// sections, read in parallel as the crawl's settings read pages.
pub(crate) async fn probe(
    client: &Client,
    pool: &RenderPool,
    rules: &Rules,
    robots: &Robots,
    config: &CrawlConfig,
    pages: usize,
) -> RenderDecision {
    let (start, extract, max_body) = (&config.start_urls[0], config.extract, config.http.max_body);
    let first = read(client, pool, start.clone(), extract, max_body).await;
    // Sections from the links the server sends; a page that sends none
    // (all drawn by scripts) gives them after rendering.
    let links = match (&first.raw, &first.rendered) {
        (Ok(raw), _) if !raw.links.is_empty() => raw,
        (_, Ok(rendered)) => rendered,
        (Ok(raw), _) => raw,
        _ => return decide(vec![sample(&first, &HashSet::new(), rules)]),
    };
    let others = pick(start, links, rules, robots, pages.saturating_sub(1));
    let mut reading: FuturesUnordered<_> = others.into_iter().map(|url| read(client, pool, url, extract, max_body)).collect();
    let mut reads = vec![first];
    while let Some(next) = reading.next().await {
        reads.push(next);
    }
    // A link counts as found only after JavaScript when none of the pages
    // links to it in the HTML the server sends.
    let mut linked: HashSet<String> = reads.iter().map(|read| read.url.to_string()).collect();
    for read in &reads {
        if let Ok(raw) = &read.raw {
            linked.extend(raw.links.iter().filter_map(|link| normalized(&link.href, rules)));
        }
    }
    decide(reads.iter().map(|read| sample(read, &linked, rules)).collect())
}

/// A page both ways; a link that redirects is read at its target (one hop),
/// as the renderer follows it.
async fn read(client: &Client, pool: &RenderPool, url: Url, extract: ExtractOptions, max_body: usize) -> Read {
    let mode = fetch::BodyMode::Full(max_body);
    let (mut fetched, rendered) = tokio::join!(fetch::fetch(client, &url, mode, None, false), pool.render(url.to_string()));
    let mut url = url;
    if let Some(target) = fetched.http.is_redirect().then(|| fetched.http.location.clone()).flatten().and_then(|location| Url::parse(&location).ok()) {
        fetched = fetch::fetch(client, &target, mode, None, false).await;
        url = target;
    }
    let http = &fetched.http;
    let raw = match (&fetched.body, &http.error) {
        (_, Some(error)) => Err(error.clone()),
        (Some(body), None) if http.is_html() && (200..300).contains(&http.status) => {
            Ok(seo_page::extract_with(&body.html, &url, Source::Raw, &extract))
        }
        _ => Err(format!("ответ {} без HTML", http.status)),
    };
    let rendered = match rendered.error {
        Some(error) => Err(error),
        None => {
            let base = Url::parse(&rendered.final_url).unwrap_or_else(|_| url.clone());
            Ok(seo_page::extract_with(&rendered.html, &base, Source::Rendered, &extract))
        }
    };
    Read { url, raw, rendered }
}

fn normalized(href: &str, rules: &Rules) -> Option<String> {
    let url = rules.filters.normalize(Url::parse(href).ok()?)?;
    rules.scope.internal(&url).then(|| url.to_string())
}

/// A page of each section the start page links to, in the order it links
/// to them, preferring one a level deeper (a product rather than its
/// catalogue: the template that repeats most).
fn pick(start: &Url, facts: &PageFacts, rules: &Rules, robots: &Robots, count: usize) -> Vec<Url> {
    let mut sections: Vec<(String, Url, bool)> = Vec::new();
    for link in &facts.links {
        let Some(url) = Url::parse(&link.href).ok().and_then(|url| rules.filters.normalize(url)) else { continue };
        let path = url.path().to_ascii_lowercase();
        if url == *start
            || !rules.scope.internal(&url)
            || !rules.filters.allows(&url, true)
            || !robots.allowed(&url)
            || FILES.iter().any(|extension| path.ends_with(extension))
        {
            continue;
        }
        let segments: Vec<&str> = url.path_segments().map(|parts| parts.filter(|part| !part.is_empty()).collect()).unwrap_or_default();
        let section = segments.first().copied().unwrap_or("").to_string();
        let deep = segments.len() >= 2;
        match sections.iter_mut().find(|(have, ..)| *have == section) {
            Some(entry) if deep && !entry.2 => *entry = (section, url, true),
            Some(_) => {}
            None => sections.push((section, url, deep)),
        }
    }
    sections.into_iter().take(count).map(|(_, url, _)| url).collect()
}

fn sample(read: &Read, linked: &HashSet<String>, rules: &Rules) -> Sample {
    let mut sample = Sample { url: read.url.to_string(), ..Sample::default() };
    let (raw, rendered) = match (&read.raw, &read.rendered) {
        (Ok(raw), Ok(rendered)) => (raw, rendered),
        (Err(error), _) => {
            sample.error = Some(format!("HTML: {error}"));
            return sample;
        }
        (_, Err(error)) => {
            sample.error = Some(format!("рендер: {error}"));
            return sample;
        }
    };
    sample.words_raw = raw.word_count;
    sample.words_rendered = rendered.word_count;
    sample.links_raw = raw.links.len();
    sample.links_rendered = rendered.links.len();
    sample.changes = changes(raw, rendered, linked, rules);
    sample
}

/// What JavaScript changes that a crawl reads.
fn changes(raw: &PageFacts, rendered: &PageFacts, linked: &HashSet<String>, rules: &Rules) -> Vec<String> {
    let mut changes = Vec::new();
    if rendered.word_count >= raw.word_count + raw.word_count / 5 + 30 {
        changes.push(format!("текст {} → {} слов", raw.word_count, rendered.word_count));
    }
    let mut only_after: Vec<String> =
        rendered.links.iter().filter_map(|link| normalized(&link.href, rules)).filter(|url| !linked.contains(url)).collect();
    only_after.sort();
    only_after.dedup();
    if let Some(example) = only_after.first() {
        let path = Url::parse(example).map_or_else(|_| example.clone(), |url| url.path().to_string());
        let count = only_after.len();
        changes.push(format!("{count} {} только после JS (например {path})", plural(count, ["ссылка", "ссылки", "ссылок"])));
    }
    let text = |value: Option<&str>| value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    for (name, before, after) in [
        ("title", text(raw.title()), text(rendered.title())),
        ("H1", text(raw.h1().next()), text(rendered.h1().next())),
        ("description", text(raw.description()), text(rendered.description())),
        ("canonical", raw.canonicals.first().cloned(), rendered.canonicals.first().cloned()),
    ] {
        match (before, after) {
            (None, Some(_)) => changes.push(format!("{name} появляется только после JS")),
            (Some(before), Some(after)) if before != after => changes.push(format!("JS меняет {name}")),
            (Some(_), None) if name != "description" => changes.push(format!("JS убирает {name}")),
            _ => {}
        }
    }
    let noindex = |facts: &PageFacts| facts.robots_tokens().any(|token| token == "noindex");
    if noindex(raw) != noindex(rendered) {
        changes.push(if noindex(rendered) { "JS добавляет noindex".into() } else { "JS убирает noindex".into() });
    }
    changes
}

fn plural(count: usize, forms: [&str; 3]) -> &str {
    match (count % 10, count % 100) {
        (1, rest) if rest != 11 => forms[0],
        (2..=4, rest) if !(12..=14).contains(&rest) => forms[1],
        _ => forms[2],
    }
}

/// Rendering when JavaScript changes something on any page read both ways.
fn decide(samples: Vec<Sample>) -> RenderDecision {
    let read: Vec<&Sample> = samples.iter().filter(|sample| sample.error.is_none()).collect();
    let changed: Vec<&Sample> = read.iter().copied().filter(|sample| !sample.changes.is_empty()).collect();
    let path = |url: &str| Url::parse(url).map_or_else(|_| url.to_string(), |url| url.path().to_string());
    let (render, reason) = match (read.is_empty(), changed.first()) {
        (true, _) => {
            let error = samples.iter().find_map(|sample| sample.error.clone()).unwrap_or_default();
            (false, format!("не удалось прочитать пробные страницы ({error}), обход без JS"))
        }
        (false, Some(first)) => {
            let more = changed.len() - 1;
            let tail = if more > 0 { format!(", и ещё на {more} из {} страниц", read.len()) } else { String::new() };
            (true, format!("{} на {}{tail}", first.changes.join(", "), path(&first.url)))
        }
        (false, None) => {
            let words: (usize, usize) = read.iter().fold((0, 0), |sum, sample| (sum.0 + sample.words_raw, sum.1 + sample.words_rendered));
            let links: (usize, usize) = read.iter().fold((0, 0), |sum, sample| (sum.0 + sample.links_raw, sum.1 + sample.links_rendered));
            let pages = read.len();
            (
                false,
                format!(
                    "на {pages} {} JS почти ничего не меняет: текст {} → {} слов, ссылок {} → {}",
                    plural(pages, ["странице", "страницах", "страницах"]),
                    words.0,
                    words.1,
                    links.0,
                    links.1
                ),
            )
        }
    };
    RenderDecision { render, reason, samples }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Filters, Scope};

    fn rules() -> Rules {
        let start = Url::parse("https://shop.example/").unwrap();
        let config = crate::CrawlConfig::new(start.clone());
        Rules {
            scope: Scope::new(&[start], false),
            filters: Filters::new(&config).unwrap(),
            follow_links: true,
            follow_nofollow: false,
            check_external: true,
            check_resources: true,
        }
    }

    fn facts(html: &str, source: Source) -> PageFacts {
        seo_page::extract(html, &Url::parse("https://shop.example/a").unwrap(), source)
    }

    fn read(url: &str, raw: &str, rendered: &str) -> Read {
        Read { url: Url::parse(url).unwrap(), raw: Ok(facts(raw, Source::Raw)), rendered: Ok(facts(rendered, Source::Rendered)) }
    }

    const PAGE: &str = "<html><head><title>Kettles</title></head><body><h1>Kettles</h1><p>Steel kettles for every kitchen</p><a href=/b>b</a></body></html>";

    #[test]
    fn the_same_page_needs_no_rendering() {
        let rules = rules();
        let same = read("https://shop.example/a", PAGE, &PAGE.replace("every kitchen", "every kitchen today"));
        let linked: HashSet<String> = ["https://shop.example/b".to_string()].into();
        let decision = decide(vec![sample(&same, &linked, &rules)]);
        assert!(!decision.render, "{decision:?}");
        assert!(decision.reason.starts_with("на 1 странице JS почти ничего не меняет"), "{}", decision.reason);
    }

    #[test]
    fn what_javascript_adds_turns_rendering_on() {
        let rules = rules();
        let linked: HashSet<String> = ["https://shop.example/b".to_string()].into();
        let words = " word".repeat(200);
        let empty = read(
            "https://shop.example/app",
            "<html><head><title></title></head><body><div id=root></div></body></html>",
            &format!("<html><head><title>Shop</title></head><body><h1>Shop</h1><p>{words}</p><a href=/c>c</a><a href=/d>d</a></body></html>"),
        );
        let found = sample(&empty, &linked, &rules);
        assert_eq!(
            found.changes,
            [
                "текст 0 → 203 слов",
                "2 ссылки только после JS (например /c)",
                "title появляется только после JS",
                "H1 появляется только после JS",
            ]
        );
        let noindex = read("https://shop.example/a", PAGE, &PAGE.replace("<head>", "<head><meta name=robots content=noindex>"));
        assert_eq!(sample(&noindex, &linked, &rules).changes, ["JS добавляет noindex"]);
        let same = sample(&read("https://shop.example/b", PAGE, PAGE), &linked, &rules);
        let decision = decide(vec![same, found]);
        assert!(decision.render);
        assert!(decision.reason.ends_with("на /app"), "{}", decision.reason);
    }

    #[test]
    fn unreadable_pages_say_nothing() {
        let rules = rules();
        let broken = Read { url: Url::parse("https://shop.example/").unwrap(), raw: Ok(facts(PAGE, Source::Raw)), rendered: Err("timeout".into()) };
        let decision = decide(vec![sample(&broken, &HashSet::new(), &rules)]);
        assert!(!decision.render && decision.reason.contains("рендер: timeout"), "{}", decision.reason);
    }

    #[test]
    fn picks_a_page_a_section() {
        let rules = rules();
        let robots = Robots::new("seo-spider");
        let home = facts(
            "<a href=/catalog/>catalog</a><a href=/catalog/kettles/steel>steel</a><a href=/blog/>blog</a><a href=/blog/post>post</a>\
             <a href=/about>about</a><a href=/price.pdf>price</a><a href=https://other.example/>other</a><a href=/>home</a>",
            Source::Raw,
        );
        let start = Url::parse("https://shop.example/").unwrap();
        let picked: Vec<String> = pick(&start, &home, &rules, &robots, 7).iter().map(|url| url.path().to_string()).collect();
        assert_eq!(picked, ["/catalog/kettles/steel", "/blog/post", "/about"]);
        assert_eq!(pick(&start, &home, &rules, &robots, 1).len(), 1);
        assert_eq!(plural(21, ["ссылка", "ссылки", "ссылок"]), "ссылка");
        assert_eq!(plural(12, ["ссылка", "ссылки", "ссылок"]), "ссылок");
    }
}
