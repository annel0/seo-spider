//! Indexability directives, canonical tags, hreflang annotations and the
//! document language. What needs other pages (does the canonical target
//! redirect? does the other language link back?) is in `site`.

use seo_page::PageFacts;

use super::Ctx;
use crate::{headers, lang, urls, Issue};

pub(super) fn check(ctx: &Ctx, out: &mut Vec<Issue>) {
    indexability(ctx, out);
    if let Some(doc) = ctx.doc() {
        canonicals(ctx, doc, out);
        hreflangs(ctx, doc, out);
        language(ctx, doc, out);
    }
}

fn indexability(ctx: &Ctx, out: &mut Vec<Issue>) {
    let noindex = ctx.robots.noindex();
    if noindex {
        out.push(Issue::with_detail("noindex", noindex_sources(ctx)));
    }
    if ctx.blocked_by_robots {
        out.push(Issue::new("blocked_by_robots"));
    }
    if ctx.doc().is_some() {
        if ctx.robots.nosnippet() {
            out.push(Issue::new("nosnippet"));
        }
        if ctx.robots.has("noimageindex") {
            out.push(Issue::new("noimageindex"));
        }
    }

    if ctx.in_sitemap {
        if noindex {
            out.push(Issue::new("sitemap_noindex"));
        }
        if ctx.blocked_by_robots {
            out.push(Issue::new("sitemap_blocked_by_robots"));
        }
        // Only a page with a document declares a canonical.
        if ctx.facts.is_some() {
            if let Some(canonical) = ctx.canonical() {
                if !urls::same_url(&canonical, ctx.url.as_str()) {
                    out.push(Issue::with_detail("sitemap_non_canonical", format!("canonical: {canonical}")));
                }
            }
        }
    }
}

/// Where the noindex comes from, for the detail.
fn noindex_sources(ctx: &Ctx) -> String {
    let mut sources = Vec::new();
    if ctx.robots.meta_noindex() {
        sources.push("meta robots");
    }
    if ctx.robots.header_noindex() {
        sources.push("X-Robots-Tag");
    }
    sources.join(", ")
}

fn canonicals(ctx: &Ctx, doc: &PageFacts, out: &mut Vec<Issue>) {
    if doc.canonicals.len() > 1 {
        out.push(Issue::with_detail("canonical_multiple", urls::examples(&doc.canonicals, 3)));
    }
    if let Some(raw) = doc.canonicals_raw.iter().find(|raw| !raw.is_empty() && urls::is_relative(raw)) {
        out.push(Issue::with_detail("canonical_relative", raw.clone()));
    }
    // The tag and the Link header must agree when a page has both.
    let from_tag = doc.canonicals.iter().find(|canonical| !canonical.is_empty());
    if let (Some(tag), Some(header)) = (from_tag, headers::header_canonical(ctx.http, ctx.url)) {
        if !urls::same_url(tag, &header) {
            out.push(Issue::with_detail("canonical_mismatch", format!("HTML: {tag}, заголовок: {header}")));
        }
    }
}

fn hreflangs(ctx: &Ctx, doc: &PageFacts, out: &mut Vec<Issue>) {
    let list = headers::hreflangs(ctx.http, Some(doc), ctx.url);
    if list.is_empty() {
        return;
    }
    let is_default = |lang: &str| lang.eq_ignore_ascii_case("x-default");

    if !list.iter().any(|item| is_default(&item.lang)) {
        out.push(Issue::new("hreflang_missing_x_default"));
    }

    let own = ctx.own_url();
    let to_self: Vec<_> = list.iter().filter(|item| urls::same_url(&item.href, &own)).collect();
    if to_self.is_empty() {
        out.push(Issue::new("hreflang_missing_self"));
    } else if let Some(page_lang) = headers::language(ctx.http, Some(doc)) {
        // A page speaking one language must not announce itself as another.
        if let Some(item) = to_self
            .iter()
            .find(|item| !is_default(&item.lang) && !item.lang.eq_ignore_ascii_case(&page_lang))
        {
            out.push(Issue::with_detail(
                "hreflang_lang_mismatch",
                format!("hreflang: {}, lang: {page_lang}", item.lang),
            ));
        }
    }

    if let Some(item) = list.iter().find(|item| !item.href_raw.is_empty() && urls::is_relative(&item.href_raw)) {
        out.push(Issue::with_detail("hreflang_relative_url", item.href_raw.clone()));
    }
}

fn language(ctx: &Ctx, doc: &PageFacts, out: &mut Vec<Issue>) {
    match headers::language(ctx.http, Some(doc)) {
        None => out.push(Issue::new("lang_missing")),
        Some(value) if !lang::is_valid_language_list(&value) => {
            out.push(Issue::with_detail("lang_invalid", urls::clip(&value, 40)));
        }
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::{Doc, Fixture, URL};

    fn robots_page(content: &str) -> Fixture {
        Doc::good().head(&format!(r#"<meta name="robots" content="{content}">"#)).page(URL)
    }

    #[test]
    fn noindex_from_meta_and_header() {
        assert!(!Fixture::good().has("noindex"));
        let meta = robots_page("noindex, follow");
        assert!(meta.has("noindex"));
        assert_eq!(meta.detail("noindex").as_deref(), Some("meta robots"));
        assert!(robots_page("none").has("noindex"));
        assert!(!robots_page("index, nofollow").has("noindex"));

        let header = Fixture::good().header("x-robots-tag", "noindex");
        assert_eq!(header.detail("noindex").as_deref(), Some("X-Robots-Tag"));
        // Named for Google, or for nobody in particular: the page is noindex.
        assert!(Fixture::good().header("x-robots-tag", "googlebot: noindex").has("noindex"));
        // Named for another crawler: it is not.
        assert!(!Fixture::good().header("x-robots-tag", "bingbot: noindex").has("noindex"));
    }

    #[test]
    fn noindex_applies_to_any_response() {
        let pdf = Fixture::resource("https://example.com/a.pdf", 200, Some("application/pdf"))
            .header("x-robots-tag", "noindex");
        assert!(pdf.has("noindex"));
    }

    #[test]
    fn blocked_by_robots_txt() {
        assert!(!Fixture::good().has("blocked_by_robots"));
        assert!(Fixture::good().blocked().has("blocked_by_robots"));
    }

    #[test]
    fn sitemap_pages_that_should_not_be_there() {
        // In the sitemap and fine: nothing to report.
        assert!(!Fixture::good().in_sitemap().has("sitemap_noindex"));
        // noindex but not in the sitemap: not a sitemap problem.
        assert!(!robots_page("noindex").has("sitemap_noindex"));
        assert!(robots_page("noindex").in_sitemap().has("sitemap_noindex"));

        assert!(!Fixture::good().in_sitemap().has("sitemap_blocked_by_robots"));
        assert!(Fixture::good().in_sitemap().blocked().has("sitemap_blocked_by_robots"));
        assert!(!Fixture::good().blocked().has("sitemap_blocked_by_robots"));
    }

    #[test]
    fn sitemap_page_that_points_its_canonical_elsewhere() {
        let canonical = Doc::good().head(r#"<link rel="canonical" href="https://example.com/page">"#).page(URL);
        assert!(!canonical.in_sitemap().has("sitemap_non_canonical"));

        let elsewhere = Doc::good().head(r#"<link rel="canonical" href="https://example.com/canonical">"#).page(URL);
        assert!(!elsewhere.has("sitemap_non_canonical"));
        assert!(elsewhere.in_sitemap().has("sitemap_non_canonical"));
        // No canonical at all is fine.
        assert!(!Fixture::good().in_sitemap().has("sitemap_non_canonical"));
    }

    #[test]
    fn nosnippet_and_noimageindex() {
        assert!(!Fixture::good().has("nosnippet"));
        assert!(robots_page("nosnippet").has("nosnippet"));
        assert!(robots_page("max-snippet:0").has("nosnippet"));
        assert!(robots_page("max-snippet: 0").has("nosnippet"));
        assert!(!robots_page("max-snippet:50").has("nosnippet"));
        assert!(Fixture::good().header("x-robots-tag", "nosnippet").has("nosnippet"));
        assert!(robots_page("noimageindex").has("noimageindex"));
        assert!(!Fixture::good().has("noimageindex"));
    }

    #[test]
    fn several_canonical_tags() {
        let one = Doc::good().head(r#"<link rel="canonical" href="https://example.com/home">"#).page(URL);
        assert!(!one.has("canonical_multiple"));
        let two = Doc::good()
            .head(r#"<link rel="canonical" href="https://example.com/home"><link rel="canonical" href="https://example.com/home-2">"#)
            .page(URL);
        assert!(two.has("canonical_multiple"));
    }

    #[test]
    fn relative_canonical() {
        let absolute = Doc::good().head(r#"<link rel="canonical" href="https://example.com/home">"#).page(URL);
        assert!(!absolute.has("canonical_relative"));
        let relative = Doc::good().head(r#"<link rel="canonical" href="/home">"#).page(URL);
        assert!(relative.has("canonical_relative"));
        assert_eq!(relative.detail("canonical_relative").as_deref(), Some("/home"));
    }

    #[test]
    fn canonical_tag_against_link_header() {
        let tag = r#"<link rel="canonical" href="https://example.com/home">"#;
        let agrees = Doc::good()
            .head(tag)
            .page(URL)
            .header("link", r#"<https://example.com/home>; rel="canonical""#);
        assert!(!agrees.has("canonical_mismatch"));
        let disagrees = Doc::good()
            .head(tag)
            .page(URL)
            .header("link", r#"<https://example.com/home-2>; rel="canonical""#);
        assert!(disagrees.has("canonical_mismatch"));
        // A header alone has nothing to disagree with.
        let header_only = Fixture::good().header("link", r#"<https://example.com/home-2>; rel="canonical""#);
        assert!(!header_only.has("canonical_mismatch"));
    }

    fn hreflang_page(links: &str, lang: Option<&str>) -> Fixture {
        Doc::good().lang(lang).head(links).page(URL)
    }

    const SELF_EN: &str = r#"<link rel="alternate" hreflang="en" href="https://example.com/page">"#;
    const FR: &str = r#"<link rel="alternate" hreflang="fr" href="https://example.com/fr">"#;
    const DEFAULT: &str = r#"<link rel="alternate" hreflang="x-default" href="https://example.com/">"#;

    #[test]
    fn hreflang_x_default() {
        let with = hreflang_page(&format!("{SELF_EN}{FR}{DEFAULT}"), Some("en"));
        assert!(!with.has("hreflang_missing_x_default"));
        let without = hreflang_page(&format!("{SELF_EN}{FR}"), Some("en"));
        assert!(without.has("hreflang_missing_x_default"));
        // No hreflang at all is not a hreflang problem.
        assert!(!Fixture::good().has("hreflang_missing_x_default"));
    }

    #[test]
    fn hreflang_self_reference() {
        assert!(!hreflang_page(&format!("{SELF_EN}{FR}{DEFAULT}"), Some("en")).has("hreflang_missing_self"));
        let missing = hreflang_page(&format!("{FR}{DEFAULT}"), Some("en"));
        assert!(missing.has("hreflang_missing_self"));
        assert!(!Fixture::good().has("hreflang_missing_self"));
    }

    #[test]
    fn hreflang_language_must_match_page_language() {
        let both = format!("{SELF_EN}{FR}{DEFAULT}");
        assert!(!hreflang_page(&both, Some("en")).has("hreflang_lang_mismatch"));
        let mismatch = hreflang_page(&both, Some("it"));
        assert!(mismatch.has("hreflang_lang_mismatch"));
        assert_eq!(mismatch.detail("hreflang_lang_mismatch").as_deref(), Some("hreflang: en, lang: it"));
        // The x-default entry that points back at the page is not a language claim.
        let default_self = r#"<link rel="alternate" hreflang="x-default" href="https://example.com/page">"#;
        assert!(!hreflang_page(&format!("{default_self}{FR}"), Some("it")).has("hreflang_lang_mismatch"));
        // Without a page language there is nothing to compare.
        assert!(!hreflang_page(&both, None).has("hreflang_lang_mismatch"));
    }

    #[test]
    fn hreflang_urls_must_be_absolute() {
        let relative = hreflang_page(
            r#"<link rel="alternate" hreflang="x-default" href="/"><link rel="alternate" hreflang="am" href="/am">"#,
            Some("en"),
        );
        assert!(relative.has("hreflang_relative_url"));
        assert!(!hreflang_page(&format!("{SELF_EN}{FR}{DEFAULT}"), Some("en")).has("hreflang_relative_url"));
        // An alternate link that is not hreflang (a feed) is not judged.
        let feed = hreflang_page(r#"<link rel="alternate" type="application/rss+xml" href="/feed">"#, Some("en"));
        assert!(!feed.has("hreflang_relative_url"));
    }

    #[test]
    fn hreflang_from_the_link_header_counts() {
        let page = Fixture::good().header(
            "link",
            r#"<https://example.com/page>; rel="alternate"; hreflang="en", <https://example.com/fr>; rel="alternate"; hreflang="fr""#,
        );
        assert!(page.has("hreflang_missing_x_default"));
        assert!(!page.has("hreflang_missing_self"));
    }

    #[test]
    fn missing_lang() {
        assert!(Doc::good().lang(None).page(URL).has("lang_missing"));
        assert!(Doc::good().lang(Some("")).page(URL).has("lang_missing"));
        assert!(!Fixture::good().has("lang_missing"));
        // A redirect is not judged.
        assert!(!Doc::good().lang(None).page(URL).status(301).has("lang_missing"));
        // The Content-Language header stands in for the attribute.
        let from_header = Doc::good().lang(None).page(URL).header("content-language", "ru");
        assert!(!from_header.has("lang_missing"));
    }

    #[test]
    fn invalid_lang() {
        assert!(Doc::good().lang(Some("InvalidLangCode")).page(URL).has("lang_invalid"));
        assert!(Doc::good().lang(Some("en_US")).page(URL).has("lang_invalid"));
        assert!(Doc::good().lang(Some("xx")).page(URL).has("lang_invalid"));
        for valid in ["en", "ru", "ru-RU", "zh-Hans", "pt-BR"] {
            assert!(!Doc::good().lang(Some(valid)).page(URL).has("lang_invalid"), "{valid}");
        }
        assert!(!Doc::good().lang(None).page(URL).has("lang_invalid"));
        assert!(!Doc::good().lang(Some("InvalidLangCode")).page(URL).status(301).has("lang_invalid"));
    }
}
