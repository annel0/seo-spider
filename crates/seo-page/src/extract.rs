//! HTML → `PageFacts`. One function for raw and rendered documents, so the two
//! differ only where the documents differ.

use std::collections::HashMap;
use std::sync::LazyLock;

use scraper::{ElementRef, Html, Node, Selector};
use url::Url;

use crate::{ExtractOptions, Form, Hreflang, Image, Link, LinkPosition, PageFacts, Pagination, Source};

macro_rules! selector {
    ($name:ident, $css:expr) => {
        static $name: LazyLock<Selector> =
            LazyLock::new(|| Selector::parse($css).expect("valid selector"));
    };
}

selector!(BASE, "base[href]");
selector!(HTML, "html");
selector!(TITLE, "title");
selector!(META, "meta");
selector!(LINK_REL, "link[rel]");
selector!(HEADINGS, "h1, h2, h3, h4, h5, h6");
selector!(ANCHORS, "a[href], area[href]");
selector!(IMG, "img");
selector!(PICTURE, "picture");
selector!(SCRIPT, "script");
selector!(IFRAME, "iframe[src]");
selector!(AUDIO, "audio[src], audio source[src]");
selector!(VIDEO, "video[src], video source[src]");
selector!(FORM, "form");
selector!(WITH_ID, "[id]");
selector!(ALL, "*");
selector!(BODY, "body");

/// Reads the facts of one HTML document fetched from (or rendered at) `url`.
pub fn extract(html: &str, url: &Url, source: Source) -> PageFacts {
    extract_with(html, url, source, &ExtractOptions::default())
}

/// `extract`, reading only what `options` asks for. The `<head>` facts
/// (title, meta, canonical, hreflang, pagination, refresh) are always read.
pub fn extract_with(html: &str, url: &Url, source: Source, options: &ExtractOptions) -> PageFacts {
    let document = Html::parse_document(html);
    // <base href> changes how every relative URL in the document resolves.
    let base = document
        .select(&BASE)
        .next()
        .and_then(|base| base.value().attr("href"))
        .and_then(|href| url.join(href.trim()).ok())
        .unwrap_or_else(|| url.clone());
    let resolve = |raw: &str| -> Option<String> {
        let mut resolved = base.join(raw.trim()).ok()?;
        if !matches!(resolved.scheme(), "http" | "https") {
            return None;
        }
        resolved.set_fragment(None);
        Some(resolved.to_string())
    };

    let mut facts = PageFacts {
        source,
        extracted: *options,
        lang: document
            .select(&HTML)
            .next()
            .and_then(|html| html.value().attr("lang"))
            .map(|lang| lang.trim().to_string()),
        titles: Vec::new(),
        titles_outside_head: 0,
        descriptions: Vec::new(),
        metas_in_body: 0,
        meta_robots: Vec::new(),
        canonicals: Vec::new(),
        canonicals_raw: Vec::new(),
        canonicals_outside_head: 0,
        hreflangs: Vec::new(),
        pagination: Vec::new(),
        headings: Vec::new(),
        links: Vec::new(),
        images: Vec::new(),
        pictures_without_img: 0,
        scripts: Vec::new(),
        stylesheets: Vec::new(),
        iframes: Vec::new(),
        audios: Vec::new(),
        videos: Vec::new(),
        meta_refresh: None,
        viewport: None,
        csp_meta: false,
        forms: Vec::new(),
        duplicate_ids: Vec::new(),
        dom_elements: if options.dom_stats { document.select(&ALL).count() } else { 0 },
        word_count: 0,
        text_hash: 0,
        json_ld: Vec::new(),
        html_bytes: html.len(),
    };

    for title in document.select(&TITLE) {
        if in_element(title, "head") {
            facts.titles.push(collapse(&title.text().collect::<String>()));
        } else if !in_element(title, "svg") {
            facts.titles_outside_head += 1;
        }
    }

    for meta in document.select(&META) {
        let element = meta.value();
        if in_element(meta, "body") {
            facts.metas_in_body += 1;
        }
        let content = element.attr("content").unwrap_or("").trim();
        if let Some(name) = element.attr("name") {
            if name.eq_ignore_ascii_case("description") && in_element(meta, "head") {
                facts.descriptions.push(collapse(content));
            } else if name.eq_ignore_ascii_case("robots") || name.eq_ignore_ascii_case("googlebot") {
                facts.meta_robots.push(content.to_ascii_lowercase());
            } else if name.eq_ignore_ascii_case("viewport") && facts.viewport.is_none() {
                facts.viewport = Some(content.to_string());
            }
        }
        if let Some(equiv) = element.attr("http-equiv") {
            if equiv.eq_ignore_ascii_case("refresh") && facts.meta_refresh.is_none() {
                facts.meta_refresh = Some(parse_refresh(content, &resolve));
            } else if equiv.eq_ignore_ascii_case("content-security-policy") && in_element(meta, "head") {
                facts.csp_meta = true;
            }
        }
    }

    for link in document.select(&LINK_REL) {
        let element = link.value();
        let rels = rel_tokens(element.attr("rel"));
        let Some(href_raw) = element.attr("href").map(str::trim) else {
            continue;
        };
        let in_head = in_element(link, "head");
        if rels.iter().any(|rel| rel == "canonical") {
            if in_head {
                facts.canonicals_raw.push(href_raw.to_string());
                facts.canonicals.push(resolve(href_raw).unwrap_or_default());
            } else {
                facts.canonicals_outside_head += 1;
            }
        }
        if options.resources && rels.iter().any(|rel| rel == "stylesheet") {
            if let Some(href) = resolve(href_raw) {
                facts.stylesheets.push(href);
            }
        }
        if !in_head {
            continue;
        }
        if rels.iter().any(|rel| rel == "alternate") {
            if let Some(lang) = element.attr("hreflang") {
                facts.hreflangs.push(Hreflang {
                    lang: lang.trim().to_string(),
                    href: resolve(href_raw).unwrap_or_default(),
                    href_raw: href_raw.to_string(),
                });
            }
        }
        for rel in &rels {
            let rel = match rel.as_str() {
                "next" => "next",
                "prev" | "previous" => "prev",
                _ => continue,
            };
            facts.pagination.push(Pagination {
                rel: rel.to_string(),
                href: resolve(href_raw).unwrap_or_default(),
            });
        }
    }

    if options.headings {
        for heading in document.select(&HEADINGS) {
            let level = heading.value().name().as_bytes()[1] - b'0';
            facts.headings.push((level, collapse(&heading.text().collect::<String>())));
        }
    }

    for anchor in document.select(&ANCHORS).filter(|_| options.links) {
        let element = anchor.value();
        let href_raw = element.attr("href").unwrap_or("").trim();
        let Some(href) = resolve(href_raw) else {
            continue;
        };
        let rel = rel_tokens(element.attr("rel"));
        let image_alt = anchor
            .select(&IMG)
            .find_map(|img| img.value().attr("alt"))
            .map(collapse);
        let mut text = collapse(&anchor.text().collect::<String>());
        if text.is_empty() {
            text = element.attr("aria-label").map(collapse).or(image_alt.clone()).unwrap_or_default();
        }
        let target_blank = element
            .attr("target")
            .is_some_and(|target| target.eq_ignore_ascii_case("_blank"));
        facts.links.push(Link {
            href,
            href_raw: href_raw.to_string(),
            anchor: text,
            nofollow: rel.iter().any(|token| token == "nofollow"),
            sponsored: rel.iter().any(|token| token == "sponsored"),
            ugc: rel.iter().any(|token| token == "ugc"),
            unsafe_target_blank: target_blank
                && !rel.iter().any(|token| token == "noopener" || token == "noreferrer"),
            position: link_position(anchor),
            image: anchor.select(&IMG).next().is_some(),
            rel,
        });
    }

    for img in document.select(&IMG).filter(|_| options.resources) {
        let element = img.value();
        let src = element
            .attr("src")
            .filter(|src| !src.trim().is_empty())
            .or_else(|| {
                element
                    .attr("srcset")
                    .and_then(|set| set.split(',').next())
                    .and_then(|candidate| candidate.split_whitespace().next())
            });
        let Some(src) = src.and_then(&resolve) else {
            continue;
        };
        facts.images.push(Image {
            src,
            alt: element.attr("alt").map(|alt| alt.trim().to_string()),
            width: element.attr("width").map(str::to_string),
            height: element.attr("height").map(str::to_string),
        });
    }
    if options.resources {
        facts.pictures_without_img = document
            .select(&PICTURE)
            .filter(|picture| picture.select(&IMG).next().is_none())
            .count();
    }

    if options.resources || options.structured_data {
        for script in document.select(&SCRIPT) {
            let element = script.value();
            if element
                .attr("type")
                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/ld+json"))
            {
                if options.structured_data {
                    facts.json_ld.push(script.text().collect::<String>().trim().to_string());
                }
            } else if let Some(src) = element.attr("src").filter(|_| options.resources).and_then(&resolve) {
                facts.scripts.push(src);
            }
        }
    }
    if options.resources {
        facts.iframes = urls(&document, &IFRAME, "src", &resolve);
        facts.audios = urls(&document, &AUDIO, "src", &resolve);
        facts.videos = urls(&document, &VIDEO, "src", &resolve);
    }

    for form in document.select(&FORM).filter(|_| options.forms) {
        let element = form.value();
        let action = element
            .attr("action")
            .map(str::trim)
            .filter(|action| !action.is_empty())
            .and_then(|action| base.join(action).ok())
            .map(|action| action.to_string())
            .unwrap_or_else(|| url.to_string());
        facts.forms.push(Form {
            action,
            method: element.attr("method").unwrap_or("get").trim().to_ascii_lowercase(),
        });
    }

    if options.dom_stats {
        let mut ids: HashMap<&str, usize> = HashMap::new();
        for element in document.select(&WITH_ID) {
            if let Some(id) = element.value().attr("id").filter(|id| !id.is_empty()) {
                *ids.entry(id).or_default() += 1;
            }
        }
        let mut duplicates: Vec<String> =
            ids.into_iter().filter(|(_, count)| *count > 1).map(|(id, _)| id.to_string()).collect();
        duplicates.sort();
        facts.duplicate_ids = duplicates;
    }

    if options.text {
        let text = visible_text(&document);
        facts.word_count = text.split_whitespace().count();
        facts.text_hash = xxhash_rust::xxh3::xxh3_64(text.as_bytes());
    }
    facts
}

/// Visible text of `<body>`, whitespace-collapsed.
fn visible_text(document: &Html) -> String {
    let Some(body) = document.select(&BODY).next() else {
        return String::new();
    };
    let mut words = String::new();
    for node in body.descendants() {
        let Node::Text(text) = node.value() else { continue };
        let hidden = node.ancestors().any(|ancestor| {
            ancestor.value().as_element().is_some_and(|element| {
                matches!(element.name(), "script" | "style" | "noscript" | "template")
            })
        });
        if hidden {
            continue;
        }
        for word in text.split_whitespace() {
            if !words.is_empty() {
                words.push(' ');
            }
            words.push_str(word);
        }
    }
    words
}

fn urls(
    document: &Html,
    selector: &Selector,
    attribute: &str,
    resolve: &impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    document
        .select(selector)
        .filter_map(|element| element.value().attr(attribute).and_then(resolve))
        .collect()
}

fn in_element(element: ElementRef<'_>, name: &str) -> bool {
    element
        .ancestors()
        .any(|ancestor| ancestor.value().as_element().is_some_and(|parent| parent.name() == name))
}

/// Header, nav, footer or aside the link sits in, by element or ARIA role.
fn link_position(anchor: ElementRef<'_>) -> LinkPosition {
    for ancestor in anchor.ancestors() {
        let Some(element) = ancestor.value().as_element() else { continue };
        let role = element.attr("role").unwrap_or("");
        match (element.name(), role) {
            ("nav", _) | (_, "navigation") => return LinkPosition::Nav,
            ("header", _) | (_, "banner") => return LinkPosition::Header,
            ("footer", _) | (_, "contentinfo") => return LinkPosition::Footer,
            ("aside", _) | (_, "complementary") => return LinkPosition::Aside,
            _ => {}
        }
    }
    LinkPosition::Content
}

fn rel_tokens(rel: Option<&str>) -> Vec<String> {
    rel.unwrap_or("").split_ascii_whitespace().map(str::to_ascii_lowercase).collect()
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `content` of a meta refresh: `5`, `0; url=/next`, `0;URL='/next'`.
fn parse_refresh(content: &str, resolve: &impl Fn(&str) -> Option<String>) -> (u32, Option<String>) {
    let mut parts = content.splitn(2, [';', ',']);
    let delay = parts
        .next()
        .and_then(|delay| delay.trim().split('.').next())
        .and_then(|delay| delay.parse().ok())
        .unwrap_or(0);
    let target = parts.next().and_then(|rest| {
        let rest = rest.trim();
        let value = if rest.len() >= 3 && rest[..3].eq_ignore_ascii_case("url") {
            rest[3..].trim_start().strip_prefix('=').unwrap_or(&rest[3..]).trim()
        } else {
            rest
        };
        let value = value.trim_matches(|c| c == '\'' || c == '"');
        (!value.is_empty()).then(|| resolve(value)).flatten()
    });
    (delay, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(html: &str) -> PageFacts {
        extract(html, &Url::parse("https://example.com/a/page.html").unwrap(), Source::Raw)
    }

    #[test]
    fn reads_head_tags() {
        let f = facts(
            r#"<html lang="ru"><head><title> Привет  мир </title>
            <meta name="description" content="Описание">
            <meta name="robots" content="NOINDEX, follow">
            <link rel="canonical" href="/canon">
            <link rel="alternate" hreflang="en" href="https://example.com/en/">
            <link rel="next" href="?page=2">
            <meta http-equiv="refresh" content="5; url=/later">
            </head><body><meta name="x"><h1>Один</h1><h3>Три</h3><p>два слова</p></body></html>"#,
        );
        assert_eq!(f.lang.as_deref(), Some("ru"));
        assert_eq!(f.title(), Some("Привет мир"));
        assert_eq!(f.description(), Some("Описание"));
        assert_eq!(f.robots_tokens().collect::<Vec<_>>(), ["noindex", "follow"]);
        assert_eq!(f.canonicals, ["https://example.com/canon"]);
        assert_eq!(f.canonicals_raw, ["/canon"]);
        assert_eq!(f.hreflangs[0].lang, "en");
        assert_eq!(f.pagination[0].href, "https://example.com/a/page.html?page=2");
        assert_eq!(f.meta_refresh, Some((5, Some("https://example.com/later".into()))));
        assert_eq!(f.metas_in_body, 1);
        assert_eq!(f.headings, [(1, "Один".into()), (3, "Три".into())]);
        assert_eq!(f.word_count, 4);
    }

    #[test]
    fn reads_links_and_images() {
        let f = facts(
            r#"<body><nav><a href="/x" rel="nofollow">Икс</a></nav>
            <a href="https://other.org/#top" target="_blank"><img src="i.png" alt="Картинка"></a>
            <a href="mailto:a@b.c">почта</a>
            <img src="/no-alt.png"><picture><source srcset="p.webp"></picture>
            <form action="/s" method="POST"></form><div id="d"></div><span id="d"></span></body>"#,
        );
        assert_eq!(f.links.len(), 2);
        assert_eq!(f.links[0].href, "https://example.com/x");
        assert!(f.links[0].nofollow);
        assert_eq!(f.links[0].position, LinkPosition::Nav);
        assert_eq!(f.links[1].href, "https://other.org/");
        assert_eq!(f.links[1].anchor, "Картинка");
        assert!(f.links[1].unsafe_target_blank && f.links[1].image);
        assert_eq!(f.images.len(), 2);
        assert_eq!(f.images[1].alt, None);
        assert_eq!(f.pictures_without_img, 1);
        assert_eq!(f.forms[0].action, "https://example.com/s");
        assert_eq!(f.forms[0].method, "post");
        assert_eq!(f.duplicate_ids, ["d"]);
    }

    #[test]
    fn base_href_changes_resolution() {
        let f = facts(r#"<head><base href="https://cdn.example.com/b/"></head><body><a href="c">c</a></body>"#);
        assert_eq!(f.links[0].href, "https://cdn.example.com/b/c");
    }
}
