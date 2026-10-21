//! Links, images, pagination and click depth of one page. Links are internal
//! when they lead to the page's own host (`www.` ignored).

use std::collections::HashSet;

use seo_page::{Link, PageFacts};
use url::Url;

use super::Ctx;
use crate::{headers, urls, Issue};

/// More internal links than this on one page dilute what each of them passes.
const TOO_MANY_LINKS: usize = 100;
/// Alt text longer than this (in characters) is a description, not an alt.
const MAX_ALT_CHARS: usize = 100;
/// Image files bigger than this (bytes) are reported.
const LARGE_IMAGE_BYTES: u64 = 500_000;
/// Pages deeper than this many clicks from the start URL are reported.
const MAX_DEPTH: u32 = 4;

pub(super) fn check(ctx: &Ctx, out: &mut Vec<Issue>) {
    if let Some(doc) = ctx.doc() {
        links(ctx, doc, out);
        images(doc, out);
        pagination(ctx, doc, out);
        if ctx.depth > MAX_DEPTH {
            out.push(Issue::with_detail("depth_too_high", format!("{} кликов", ctx.depth)));
        }
    }
    large_image(ctx, out);
}

fn links(ctx: &Ctx, doc: &PageFacts, out: &mut Vec<Issue>) {
    let (internal, external): (Vec<&Link>, Vec<&Link>) =
        doc.links.iter().partition(|link| ctx.is_internal(&link.href));
    let page_nofollow = ctx.robots.nofollow();

    if internal.len() > TOO_MANY_LINKS {
        out.push(Issue::with_detail("too_many_links", format!("{} внутренних", internal.len())));
    }

    // A nofollow page makes every link on it nofollow.
    let nofollow_internal = internal.iter().filter(|link| link.nofollow).count();
    if (page_nofollow && !internal.is_empty()) || nofollow_internal > 0 {
        let detail = if page_nofollow {
            format!("страница nofollow, ссылок: {}", internal.len())
        } else {
            format!("{nofollow_internal} ссылок")
        };
        out.push(Issue::with_detail("internal_nofollow_links", detail));
    }

    // sponsored and ugc are as good as nofollow for an outbound link.
    let followed_external = external
        .iter()
        .filter(|link| !link.nofollow && !link.sponsored && !link.ugc)
        .count();
    if !page_nofollow && followed_external > 0 {
        out.push(Issue::with_detail("external_follow_links", format!("{followed_external} ссылок")));
    }

    if ctx.is_https() {
        let insecure: Vec<&str> = internal
            .iter()
            .filter(|link| link.href.starts_with("http://"))
            .map(|link| link.href.as_str())
            .collect();
        if !insecure.is_empty() {
            out.push(Issue::with_detail("links_to_http", urls::examples(&insecure, 3)));
        }
    }

    if doc.links.is_empty() {
        out.push(Issue::new("dead_end"));
    }

    // A page that itself lives on localhost may link to it.
    if !ctx.url.host_str().is_some_and(is_local_host) {
        let local: Vec<&str> = doc
            .links
            .iter()
            .filter(|link| Url::parse(&link.href).ok().is_some_and(|url| url.host_str().is_some_and(is_local_host)))
            .map(|link| link.href.as_str())
            .collect();
        if !local.is_empty() {
            out.push(Issue::with_detail("localhost_links", urls::examples(&local, 3)));
        }
    }
}

fn is_local_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

fn images(doc: &PageFacts, out: &mut Vec<Issue>) {
    let total = doc.images.len();

    let without_alt = doc
        .images
        .iter()
        .filter(|image| image.alt.as_deref().is_none_or(str::is_empty))
        .count();
    if without_alt > 0 {
        out.push(Issue::with_detail("image_alt_missing", format!("{without_alt} из {total}")));
    }

    let long_alt = doc
        .images
        .iter()
        .filter(|image| image.alt.as_deref().is_some_and(|alt| alt.chars().count() > MAX_ALT_CHARS))
        .count();
    if long_alt > 0 {
        out.push(Issue::with_detail("image_alt_too_long", format!("{long_alt} из {total}")));
    }

    if doc.pictures_without_img > 0 {
        out.push(Issue::with_detail(
            "picture_missing_img",
            format!("{} элементов picture", doc.pictures_without_img),
        ));
    }

    let blank = |value: &Option<String>| value.as_deref().is_none_or(|text| text.trim().is_empty());
    let without_size = doc
        .images
        .iter()
        .filter(|image| blank(&image.width) || blank(&image.height))
        .count();
    if without_size > 0 {
        out.push(Issue::with_detail("image_without_size", format!("{without_size} из {total}")));
    }
}

/// The page being an image of its own: an image file over the size limit.
fn large_image(ctx: &Ctx, out: &mut Vec<Issue>) {
    let is_image = ctx.http.media_type().is_some_and(|media| media.starts_with("image/"));
    if ctx.success && is_image && ctx.http.size > LARGE_IMAGE_BYTES {
        out.push(Issue::with_detail("image_too_large", format!("{} КБ", ctx.http.size / 1024)));
    }
}

/// `rel=next/prev` must point at an address the page also links to, otherwise
/// the sequence is invisible to a robot that follows only `<a>`.
fn pagination(ctx: &Ctx, doc: &PageFacts, out: &mut Vec<Issue>) {
    let linked: HashSet<&str> = doc.links.iter().map(|link| link.href.as_str()).collect();
    let missing: Vec<String> = headers::pagination(ctx.http, Some(doc), ctx.url)
        .into_iter()
        .filter(|item| !item.href.is_empty() && !linked.contains(item.href.as_str()))
        .map(|item| item.href)
        .collect();
    if !missing.is_empty() {
        out.push(Issue::with_detail("pagination_link_missing", urls::examples(&missing, 2)));
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::{Doc, Fixture, URL};

    fn page_with_body(body: &str) -> Fixture {
        Doc::good().body(body).page(URL)
    }

    fn many_links(n: usize) -> String {
        (0..n).map(|i| format!(r#"<a href="/p{i}">link {i}</a>"#)).collect()
    }

    #[test]
    fn too_many_internal_links() {
        assert!(!page_with_body(&many_links(10)).has("too_many_links"));
        assert!(!page_with_body(&many_links(99)).has("too_many_links"));
        // 100 more plus the one every good page has.
        assert!(page_with_body(&many_links(100)).has("too_many_links"));
        assert!(page_with_body(&many_links(110)).has("too_many_links"));
    }

    #[test]
    fn external_links_do_not_count_towards_the_limit() {
        let external: String = (0..120).map(|i| format!(r#"<a href="https://other.org/{i}" rel="nofollow">x</a>"#)).collect();
        assert!(!page_with_body(&external).has("too_many_links"));
    }

    #[test]
    fn internal_nofollow_links() {
        assert!(!Fixture::good().has("internal_nofollow_links"));
        assert!(page_with_body(r#"<a href="/x" rel="nofollow">x</a>"#).has("internal_nofollow_links"));
        // A nofollow page makes its internal links nofollow.
        let nofollow_page = Doc::good().head(r#"<meta name="robots" content="nofollow">"#).page(URL);
        assert!(nofollow_page.has("internal_nofollow_links"));
        // ...but a nofollow page without links has nothing to report.
        let no_links = Doc::good().head(r#"<meta name="robots" content="nofollow">"#).no_link().page(URL);
        assert!(!no_links.has("internal_nofollow_links"));
    }

    #[test]
    fn external_links_without_nofollow() {
        let followed = page_with_body(r#"<a href="https://other.org/">other</a>"#);
        assert!(followed.has("external_follow_links"));
        let nofollow = page_with_body(r#"<a href="https://other.org/" rel="nofollow">other</a>"#);
        assert!(!nofollow.has("external_follow_links"));
        let sponsored = page_with_body(r#"<a href="https://other.org/" rel="sponsored">other</a>"#);
        assert!(!sponsored.has("external_follow_links"));
        // The whole page is nofollow, so its external links are too.
        let nofollow_page = Doc::good()
            .head(r#"<meta name="robots" content="nofollow">"#)
            .body(r#"<a href="https://other.org/">other</a>"#)
            .page(URL);
        assert!(!nofollow_page.has("external_follow_links"));
        // The www. twin of the site is not external.
        assert!(!page_with_body(r#"<a href="https://www.example.com/x">x</a>"#).has("external_follow_links"));
    }

    #[test]
    fn https_page_linking_to_http() {
        assert!(!Fixture::good().has("links_to_http"));
        let page = page_with_body(r#"<a href="http://example.com/old">old</a>"#);
        assert!(page.has("links_to_http"));
        assert_eq!(page.detail("links_to_http").as_deref(), Some("http://example.com/old"));
        // Only internal links count; other sites are not ours to upgrade.
        assert!(!page_with_body(r#"<a href="http://other.org/">x</a>"#).has("links_to_http"));
    }

    #[test]
    fn dead_end_page_has_no_links_at_all() {
        assert!(Doc::good().no_link().page(URL).has("dead_end"));
        assert!(!Fixture::good().has("dead_end"));
        let external_only = Doc::good().no_link().body(r#"<a href="https://other.org/">x</a>"#).page(URL);
        assert!(!external_only.has("dead_end"));
    }

    #[test]
    fn localhost_links() {
        for target in ["https://localhost/example", "http://127.0.0.1/example", "http://localhost:3000/"] {
            let page = page_with_body(&format!(r#"<a href="{target}">dev</a>"#));
            assert!(page.has("localhost_links"), "{target}");
        }
        assert!(!page_with_body(r#"<a href="https://example.org/">x</a>"#).has("localhost_links"));
        // A page that is itself on localhost may link to it.
        let local = Doc::good().body(r#"<a href="https://127.0.0.1/example">x</a>"#).page("https://127.0.0.1/");
        assert!(!local.has("localhost_links"));
    }

    #[test]
    fn pagination_targets_must_be_linked() {
        let head = r#"<link rel="prev" href="/page/1"><link rel="next" href="/page/3">"#;
        let linked = Doc::good()
            .head(head)
            .body(r#"<a href="/page/1">1</a><a href="/page/3">3</a>"#)
            .page("https://example.com/page/2");
        assert!(!linked.has("pagination_link_missing"));

        let unlinked = Doc::good().head(head).page("https://example.com/page/2");
        assert!(unlinked.has("pagination_link_missing"));
        assert!(unlinked.detail("pagination_link_missing").unwrap().contains("/page/1"));

        // The Link header carries pagination too.
        let header = Fixture::good().header("link", r#"</page/9>; rel="next""#);
        assert!(header.has("pagination_link_missing"));
        // Pages that redirect are not judged.
        assert!(!Doc::good().head(head).page(URL).status(301).has("pagination_link_missing"));
    }

    #[test]
    fn depth_over_four() {
        assert!(!Fixture::good().depth(3).has("depth_too_high"));
        assert!(!Fixture::good().depth(4).has("depth_too_high"));
        assert!(Fixture::good().depth(5).has("depth_too_high"));
        assert!(Fixture::good().depth(8).has("depth_too_high"));
        assert!(!Fixture::good().depth(8).status(404).has("depth_too_high"));
    }

    #[test]
    fn images_without_alt() {
        assert!(!Fixture::good().has("image_alt_missing"));
        let described = page_with_body(r#"<img src="/a.png" alt="Image alt text" width="10" height="10">"#);
        assert!(!described.has("image_alt_missing"));
        let missing = page_with_body(r#"<img src="/a.png" width="10" height="10">"#);
        assert!(missing.has("image_alt_missing"));
        let empty = page_with_body(r#"<img src="/a.png" alt="" width="10" height="10">"#);
        assert!(empty.has("image_alt_missing"));
        assert_eq!(empty.detail("image_alt_missing").as_deref(), Some("1 из 1"));
    }

    #[test]
    fn long_alt_text_is_counted_in_characters() {
        let alt = |n: usize| page_with_body(&format!(r#"<img src="/a.png" alt="{}">"#, "я".repeat(n)));
        // 100 Cyrillic letters are 200 bytes.
        assert!(!alt(100).has("image_alt_too_long"));
        assert!(alt(101).has("image_alt_too_long"));
        let latin = page_with_body(&format!(r#"<img src="/a.png" alt="{}">"#, "a".repeat(120)));
        assert!(latin.has("image_alt_too_long"));
    }

    #[test]
    fn large_image_files() {
        let image = |size| Fixture::resource("https://example.com/a.jpg", 200, Some("image/jpeg")).size(size);
        assert!(!image(300_000).has("image_too_large"));
        assert!(!image(500_000).has("image_too_large"));
        let big = image(700_000);
        assert!(big.has("image_too_large"));
        assert_eq!(big.detail("image_too_large").as_deref(), Some("683 КБ"));
        // A big page is not an image.
        assert!(!Fixture::good().size(700_000).has("image_too_large"));
    }

    #[test]
    fn picture_needs_an_img_fallback() {
        let with_img = page_with_body(
            r#"<picture><source srcset="/a.jpg" media="(orientation: portrait)"><img src="/b.jpg" alt="b" width="1" height="1"></picture>"#,
        );
        assert!(!with_img.has("picture_missing_img"));
        let without = page_with_body(r#"<picture><source srcset="/a.jpg" media="(orientation: portrait)"></picture>"#);
        assert!(without.has("picture_missing_img"));
    }

    #[test]
    fn images_need_both_dimensions() {
        let sized = page_with_body(r#"<img src="/a.png" alt="a" width="80vw" height="100%"><img src="/b.png" alt="b" width="400" height="400">"#);
        assert!(!sized.has("image_without_size"));
        for tag in [r#"<img src="/a.png" alt="a">"#, r#"<img src="/a.png" alt="a" height="200">"#, r#"<img src="/a.png" alt="a" width="200">"#] {
            assert!(page_with_body(tag).has("image_without_size"), "{tag}");
        }
    }
}
