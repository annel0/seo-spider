//! What a page says about itself outside its HTML: `Link` and `X-Robots-Tag`
//! response headers, `Content-Language`, merged with the facts read from the
//! document so that checks see one canonical, one list of hreflangs and one set
//! of robots directives.

use seo_page::{Hreflang, HttpInfo, PageFacts, Pagination};
use url::Url;

/// One target of a `Link` header: `<https://example.com/>; rel="canonical"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkEntry {
    /// Resolved against the page URL, no fragment; empty if unresolvable.
    pub href: String,
    /// As written in the header.
    pub href_raw: String,
    /// Lower-cased `rel` tokens.
    pub rels: Vec<String>,
    pub hreflang: Option<String>,
}

/// Every target of every `Link` header.
pub(crate) fn link_entries(http: &HttpInfo, base: &Url) -> Vec<LinkEntry> {
    http.header_values("link")
        .flat_map(|value| split_top_level(value, ','))
        .filter_map(|item| parse_link_entry(item, base))
        .collect()
}

fn parse_link_entry(item: &str, base: &Url) -> Option<LinkEntry> {
    let (href_raw, params) = item.trim().strip_prefix('<')?.split_once('>')?;
    let href_raw = href_raw.trim();
    let mut rels = Vec::new();
    let mut hreflang = None;
    for param in split_top_level(params, ';') {
        let Some((name, value)) = param.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').trim();
        match name.trim().to_ascii_lowercase().as_str() {
            "rel" => rels.extend(value.split_ascii_whitespace().map(str::to_ascii_lowercase)),
            "hreflang" => hreflang = Some(value.to_string()),
            _ => {}
        }
    }
    let href = base
        .join(href_raw)
        .ok()
        .filter(|resolved| matches!(resolved.scheme(), "http" | "https"))
        .map(|mut resolved| {
            resolved.set_fragment(None);
            resolved.to_string()
        })
        .unwrap_or_default();
    Some(LinkEntry { href, href_raw: href_raw.to_string(), rels, hreflang })
}

/// Splits on `separator` outside `<...>` and double quotes, so the commas in
/// `<https://a/?x=1,2>; title="a, b"` do not start a new link.
fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut in_angle, mut in_quote) = (0, false, false);
    for (index, c) in text.char_indices() {
        match c {
            '"' if !in_angle => in_quote = !in_quote,
            '<' if !in_quote => in_angle = true,
            '>' if !in_quote => in_angle = false,
            c if c == separator && !in_angle && !in_quote => {
                parts.push(&text[start..index]);
                start = index + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

/// The canonical of a `Link: <...>; rel="canonical"` header.
pub(crate) fn header_canonical(http: &HttpInfo, base: &Url) -> Option<String> {
    link_entries(http, base)
        .into_iter()
        .find(|entry| !entry.href.is_empty() && entry.rels.iter().any(|rel| rel == "canonical"))
        .map(|entry| entry.href)
}

/// The canonical the page effectively declares: the first usable `<link>` in
/// the document, else the `Link` header.
pub(crate) fn effective_canonical(http: &HttpInfo, facts: Option<&PageFacts>, base: &Url) -> Option<String> {
    facts
        .and_then(|facts| facts.canonicals.iter().find(|canonical| !canonical.is_empty()).cloned())
        .or_else(|| header_canonical(http, base))
}

/// Hreflang annotations from the document and from `Link` headers, each pair once.
pub(crate) fn hreflangs(http: &HttpInfo, facts: Option<&PageFacts>, base: &Url) -> Vec<Hreflang> {
    let mut list: Vec<Hreflang> = facts.map(|facts| facts.hreflangs.clone()).unwrap_or_default();
    for entry in link_entries(http, base) {
        let Some(lang) = entry.hreflang else { continue };
        if !entry.rels.iter().any(|rel| rel == "alternate") {
            continue;
        }
        let item = Hreflang { lang, href: entry.href, href_raw: entry.href_raw };
        if !list.contains(&item) {
            list.push(item);
        }
    }
    list
}

/// `rel=next|prev` from the document and from `Link` headers.
pub(crate) fn pagination(http: &HttpInfo, facts: Option<&PageFacts>, base: &Url) -> Vec<Pagination> {
    let mut list: Vec<Pagination> = facts.map(|facts| facts.pagination.clone()).unwrap_or_default();
    for entry in link_entries(http, base) {
        for rel in &entry.rels {
            let rel = match rel.as_str() {
                "next" => "next",
                "prev" | "previous" => "prev",
                _ => continue,
            };
            let item = Pagination { rel: rel.to_string(), href: entry.href.clone() };
            if !list.contains(&item) {
                list.push(item);
            }
        }
    }
    list
}

/// `<html lang>`, else the first `Content-Language` value.
pub(crate) fn language(http: &HttpInfo, facts: Option<&PageFacts>) -> Option<String> {
    let from_html = facts.and_then(|facts| facts.lang.as_deref());
    let from_header = http.header("content-language").and_then(|value| value.split(',').next());
    from_html
        .into_iter()
        .chain(from_header)
        .map(str::trim)
        .find(|lang| !lang.is_empty())
        .map(str::to_string)
}

/// Robots directives of a page, lower-cased, with the place they came from kept
/// apart: the JavaScript tab needs the meta ones alone because the headers do
/// not change when scripts run.
#[derive(Debug, Default, Clone)]
pub(crate) struct Robots {
    pub meta: Vec<String>,
    pub header: Vec<String>,
}

pub(crate) fn robots(http: &HttpInfo, facts: Option<&PageFacts>) -> Robots {
    Robots {
        meta: facts
            .map(|facts| facts.robots_tokens().map(normalize_token).collect())
            .unwrap_or_default(),
        header: http.header_values("x-robots-tag").flat_map(x_robots_tokens).collect(),
    }
}

/// Directives that carry a value and so contain a colon of their own.
const VALUE_DIRECTIVES: [&str; 4] = ["max-snippet", "max-image-preview", "max-video-preview", "unavailable_after"];

/// `noindex, nofollow` or `googlebot: noindex, nofollow`. Directives addressed
/// to another crawler (`bingbot: noindex`) do not make the page non-indexable
/// for Google, so they are dropped.
fn x_robots_tokens(value: &str) -> Vec<String> {
    let mut in_scope = true;
    let mut tokens = Vec::new();
    for part in value.to_ascii_lowercase().split(',') {
        let mut part = part.trim();
        if let Some((key, rest)) = part.split_once(':') {
            let key = key.trim();
            if !VALUE_DIRECTIVES.contains(&key) {
                in_scope = matches!(key, "googlebot" | "google" | "robots" | "*");
                part = rest.trim();
            }
        }
        if in_scope && !part.is_empty() {
            tokens.push(normalize_token(part));
        }
    }
    tokens
}

/// `max-snippet: 0` and `max-snippet:0` are the same directive.
fn normalize_token(token: &str) -> String {
    match token.split_once(':') {
        Some((key, value)) => format!("{}:{}", key.trim(), value.trim()),
        None => token.trim().to_string(),
    }
}

impl Robots {
    pub fn has(&self, token: &str) -> bool {
        self.meta.iter().chain(&self.header).any(|have| have == token)
    }

    /// `none` is shorthand for `noindex, nofollow`.
    pub fn noindex(&self) -> bool {
        self.has("noindex") || self.has("none")
    }

    pub fn nofollow(&self) -> bool {
        self.has("nofollow") || self.has("none")
    }

    pub fn nosnippet(&self) -> bool {
        self.has("nosnippet") || self.has("max-snippet:0")
    }

    pub fn meta_noindex(&self) -> bool {
        self.meta.iter().any(|token| token == "noindex" || token == "none")
    }

    pub fn meta_nofollow(&self) -> bool {
        self.meta.iter().any(|token| token == "nofollow" || token == "none")
    }

    pub fn header_noindex(&self) -> bool {
        self.header.iter().any(|token| token == "noindex" || token == "none")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(headers: &[(&str, &str)]) -> HttpInfo {
        HttpInfo {
            url: "https://example.com/page".into(),
            status: 200,
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..HttpInfo::default()
        }
    }

    fn base() -> Url {
        Url::parse("https://example.com/page").unwrap()
    }

    #[test]
    fn link_header_canonical_matches_seonaut_case() {
        let info = http(&[("link", "<https://example.com/home>; rel=\"canonical\"")]);
        assert_eq!(header_canonical(&info, &base()).as_deref(), Some("https://example.com/home"));
    }

    #[test]
    fn link_header_with_several_targets_and_commas_in_quotes() {
        let info = http(&[(
            "link",
            r#"</fr?a=1,2>; rel="alternate"; hreflang="fr"; title="un, deux", </p/2>; rel=next, <https://example.com/c#frag>; rel="canonical""#,
        )]);
        let entries = link_entries(&info, &base());
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].href, "https://example.com/fr?a=1,2");
        assert_eq!(entries[0].hreflang.as_deref(), Some("fr"));
        assert_eq!(entries[1].rels, ["next"]);
        assert_eq!(header_canonical(&info, &base()).as_deref(), Some("https://example.com/c"));
        let paging = pagination(&info, None, &base());
        assert_eq!(paging, [Pagination { rel: "next".into(), href: "https://example.com/p/2".into() }]);
    }

    #[test]
    fn hreflangs_merge_document_and_header_without_repeats() {
        let info = http(&[("link", r#"<https://example.com/en>; rel="alternate"; hreflang="en""#)]);
        let list = hreflangs(&info, None, &base());
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].lang, "en");
    }

    #[test]
    fn x_robots_tag_scopes_and_values() {
        let info = http(&[
            ("x-robots-tag", "noindex, nofollow"),
            ("x-robots-tag", "bingbot: noimageindex"),
            ("x-robots-tag", "googlebot: nosnippet, max-snippet: 0"),
            ("x-robots-tag", "unavailable_after: 25 Jun 2030 15:00:00 PST"),
        ]);
        let robots = robots(&info, None);
        assert!(robots.noindex() && robots.nofollow() && robots.nosnippet());
        assert!(!robots.has("noimageindex"));
        assert!(robots.has("max-snippet:0"));
        assert!(robots.header_noindex() && !robots.meta_noindex());
    }

    #[test]
    fn none_means_noindex_and_nofollow() {
        let info = http(&[("x-robots-tag", "none")]);
        let robots = robots(&info, None);
        assert!(robots.noindex() && robots.nofollow());
    }

    #[test]
    fn language_falls_back_to_content_language() {
        let info = http(&[("content-language", "de, en")]);
        assert_eq!(language(&info, None).as_deref(), Some("de"));
        assert_eq!(language(&http(&[]), None), None);
    }
}
