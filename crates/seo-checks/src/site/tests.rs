//! Tests of the cross-page checks: pages are built by hand, or from a
//! `Fixture` where the way `site_page` reads a page is part of the test.

use super::*;
use crate::testutil::{codes_at, link, nofollow, redirect, site, Doc, Fixture};

fn has(found: &[(usize, Issue)], position: usize, code: &str) -> bool {
    codes_at(found, position).contains(&code)
}

fn titled(url: &str, title: &str) -> SitePage {
    SitePage { title: Some(title.into()), ..site(url) }
}

// ---- duplicates ----

#[test]
fn pages_with_the_same_title_are_reported_together() {
    let pages = [
        titled("https://example.com/a", "Same title"),
        titled("https://example.com/b", "Same title"),
        titled("https://example.com/c", "Another title"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["title_duplicate"]);
    assert_eq!(codes_at(&found, 1), ["title_duplicate"]);
    assert!(codes_at(&found, 2).is_empty());
    assert!(found[0].1.detail.as_deref().unwrap().contains("https://example.com/b"));
}

#[test]
fn title_comparison_ignores_case_and_works_on_cyrillic() {
    let pages = [titled("https://example.com/a", "Купить Окна"), titled("https://example.com/b", "купить окна")];
    assert_eq!(codes_at(&check_site(&pages), 0), ["title_duplicate"]);
}

#[test]
fn same_title_in_different_languages_is_not_a_duplicate() {
    let mut french = titled("https://example.com/fr", "Contact");
    french.lang = Some("fr".into());
    let pages = [titled("https://example.com/en", "Contact"), french];
    assert!(check_site(&pages).is_empty());
}

#[test]
fn pages_that_point_elsewhere_or_are_not_documents_do_not_compete() {
    let mut canonicalised = titled("https://example.com/b", "Same title");
    canonicalised.canonical = Some("https://example.com/a".into());
    let mut broken = titled("https://example.com/c", "Same title");
    broken.status = 404;
    let mut image = titled("https://example.com/d", "Same title");
    image.html = false;
    let mut external = titled("https://other.org/", "Same title");
    external.external = true;
    let pages = [titled("https://example.com/a", "Same title"), canonicalised, broken, image, external];
    assert!(check_site(&pages).is_empty());
}

#[test]
fn a_page_may_declare_itself_canonical_and_still_compete() {
    let mut own = titled("https://example.com/b", "Same title");
    own.canonical = Some("https://example.com/b".into());
    let pages = [titled("https://example.com/a", "Same title"), own];
    assert_eq!(codes_at(&check_site(&pages), 1), ["title_duplicate"]);
}

#[test]
fn pages_without_a_title_are_not_duplicates_of_each_other() {
    let pages = [site("https://example.com/a"), site("https://example.com/b")];
    assert!(check_site(&pages).is_empty());
}

#[test]
fn duplicate_descriptions_and_headings() {
    let with = |url: &str| SitePage {
        description: Some("The same description".into()),
        h1: Some("The same heading".into()),
        ..site(url)
    };
    let found = check_site(&[with("https://example.com/a"), with("https://example.com/b")]);
    assert_eq!(codes_at(&found, 0), ["description_duplicate", "h1_duplicate"]);
}

#[test]
fn identical_text_is_duplicate_content() {
    let text = |url: &str, hash: Option<u64>| SitePage { text_hash: hash, ..site(url) };
    let pages = [
        text("https://example.com/a", Some(7)),
        text("https://example.com/b", Some(7)),
        text("https://example.com/c", Some(8)),
        text("https://example.com/d", None),
        text("https://example.com/e", None),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["duplicate_content"]);
    assert_eq!(codes_at(&found, 1), ["duplicate_content"]);
    assert!(codes_at(&found, 2).is_empty() && codes_at(&found, 3).is_empty() && codes_at(&found, 4).is_empty());
}

// ---- redirects ----

#[test]
fn a_single_redirect_is_not_a_chain() {
    let pages = [redirect("https://example.com/a", "https://example.com/b"), site("https://example.com/b")];
    assert!(check_site(&pages).is_empty());
    // Nor is a redirect to a URL the crawl did not visit.
    assert!(check_site(&pages[..1]).is_empty());
}

#[test]
fn redirect_to_a_redirect_is_a_chain() {
    let pages = [
        redirect("https://example.com/a", "https://example.com/b"),
        redirect("https://example.com/b", "https://example.com/c"),
        site("https://example.com/c"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["redirect_chain"]);
    assert_eq!(
        found[0].1.detail.as_deref(),
        Some("https://example.com/a → https://example.com/b → https://example.com/c")
    );
    assert!(codes_at(&found, 1).is_empty());
}

#[test]
fn longer_chain_reports_each_page_that_starts_one() {
    let pages = [
        redirect("https://example.com/a", "https://example.com/b"),
        redirect("https://example.com/b", "https://example.com/c"),
        redirect("https://example.com/c", "https://example.com/d"),
        site("https://example.com/d"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["redirect_chain"]);
    assert_eq!(codes_at(&found, 1), ["redirect_chain"]);
    assert!(codes_at(&found, 2).is_empty());
}

#[test]
fn chain_that_ends_beyond_the_crawl_is_still_a_chain() {
    let pages = [
        redirect("https://example.com/a", "https://example.com/b"),
        redirect("https://example.com/b", "https://elsewhere.org/"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["redirect_chain"]);
    assert!(found[0].1.detail.as_deref().unwrap().ends_with("https://elsewhere.org/"));
}

#[test]
fn two_pages_redirecting_to_each_other_are_a_loop() {
    let pages = [
        redirect("https://example.com/a", "https://example.com/b"),
        redirect("https://example.com/b", "https://example.com/a"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["redirect_loop"]);
    assert_eq!(codes_at(&found, 1), ["redirect_loop"]);
}

#[test]
fn longer_loops_and_self_redirects() {
    let ring = [
        redirect("https://example.com/a", "https://example.com/b"),
        redirect("https://example.com/b", "https://example.com/c"),
        redirect("https://example.com/c", "https://example.com/a"),
    ];
    let found = check_site(&ring);
    assert!((0..3).all(|page| codes_at(&found, page) == ["redirect_loop"]));

    let itself = [redirect("https://example.com/a", "https://example.com/a#top")];
    assert_eq!(codes_at(&check_site(&itself), 0), ["redirect_loop"]);
}

#[test]
fn a_path_into_a_loop_is_a_chain_for_the_page_outside_it() {
    let pages = [
        redirect("https://example.com/a", "https://example.com/b"),
        redirect("https://example.com/b", "https://example.com/c"),
        redirect("https://example.com/c", "https://example.com/b"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["redirect_chain"]);
    assert_eq!(codes_at(&found, 1), ["redirect_loop"]);
    assert_eq!(codes_at(&found, 2), ["redirect_loop"]);
}

#[test]
fn meta_refresh_takes_part_in_chains() {
    let refreshed = SitePage { location: Some("https://example.com/b".into()), ..site("https://example.com/a") };
    let pages = [refreshed, redirect("https://example.com/b", "https://example.com/c"), site("https://example.com/c")];
    assert_eq!(codes_at(&check_site(&pages), 0), ["redirect_chain"]);
}

// ---- canonicals ----

fn canonical_to(url: &str, target: &str) -> SitePage {
    SitePage { canonical: Some(target.into()), ..site(url) }
}

#[test]
fn canonical_to_a_healthy_canonical_page_is_fine() {
    let pages = [canonical_to("https://example.com/a", "https://example.com/b"), site("https://example.com/b")];
    assert!(check_site(&pages).is_empty());
    // A target that declares itself canonical is just as healthy.
    let own = [canonical_to("https://example.com/a", "https://example.com/b"), canonical_to("https://example.com/b", "https://example.com/b")];
    assert!(check_site(&own).is_empty());
    // And a target the crawl never reached cannot be judged.
    assert!(check_site(&pages[..1]).is_empty());
}

#[test]
fn canonical_to_a_non_canonical_page() {
    let pages = [
        canonical_to("https://example.com/a", "https://example.com/b"),
        canonical_to("https://example.com/b", "https://example.com/c"),
        site("https://example.com/c"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["canonical_to_non_canonical"]);
    // The chain's second link points at a healthy page.
    assert!(codes_at(&found, 1).is_empty());
}

#[test]
fn canonical_to_a_page_closed_to_indexing() {
    let noindex = SitePage { noindex: true, ..site("https://example.com/b") };
    let found = check_site(&[canonical_to("https://example.com/a", "https://example.com/b"), noindex]);
    assert_eq!(codes_at(&found, 0), ["canonical_to_non_indexable"]);

    let blocked = SitePage { blocked_by_robots: true, status: 0, ..site("https://example.com/b") };
    let found = check_site(&[canonical_to("https://example.com/a", "https://example.com/b"), blocked]);
    assert_eq!(codes_at(&found, 0), Vec::<&str>::new(), "a blocked page has no status to judge");
}

#[test]
fn canonical_to_a_redirect_or_an_error() {
    let to_redirect = [
        canonical_to("https://example.com/a", "https://example.com/b"),
        redirect("https://example.com/b", "https://example.com/c"),
    ];
    assert_eq!(codes_at(&check_site(&to_redirect), 0), ["canonical_to_redirect"]);

    for status in [404, 410, 500, 0] {
        let broken = SitePage { status, ..site("https://example.com/b") };
        let found = check_site(&[canonical_to("https://example.com/a", "https://example.com/b"), broken]);
        assert_eq!(codes_at(&found, 0), ["canonical_to_error"], "status {status}");
    }
}

// ---- hreflang ----

fn with_hreflangs(url: &str, entries: &[(&str, &str)]) -> SitePage {
    SitePage {
        hreflangs: entries
            .iter()
            .map(|(lang, href)| SiteHreflang { lang: lang.to_string(), href: href.to_string() })
            .collect(),
        ..site(url)
    }
}

const EN: &str = "https://example.com/en";
const FR: &str = "https://example.com/fr";

#[test]
fn hreflang_needs_a_return_link() {
    let both = [with_hreflangs(EN, &[("en", EN), ("fr", FR)]), with_hreflangs(FR, &[("fr", FR), ("en", EN)])];
    assert!(check_site(&both).is_empty());

    let one_way = [with_hreflangs(EN, &[("en", EN), ("fr", FR)]), with_hreflangs(FR, &[("fr", FR)])];
    let found = check_site(&one_way);
    assert_eq!(codes_at(&found, 0), ["hreflang_no_return_link"]);
    assert!(codes_at(&found, 1).is_empty());
    assert_eq!(found[0].1.detail.as_deref(), Some(FR));

    // A target with no annotations at all does not link back either.
    let silent = [with_hreflangs(EN, &[("en", EN), ("fr", FR)]), site(FR)];
    assert_eq!(codes_at(&check_site(&silent), 0), ["hreflang_no_return_link"]);
}

#[test]
fn x_default_and_unknown_targets_need_no_return_link() {
    let home = "https://example.com/";
    let pages = [with_hreflangs(EN, &[("en", EN), ("x-default", home)]), site(home)];
    assert!(check_site(&pages).is_empty());
    let unseen = [with_hreflangs(EN, &[("en", EN), ("de", "https://example.com/de")])];
    assert!(check_site(&unseen).is_empty());
}

#[test]
fn hreflang_to_pages_that_are_not_what_it_should_name() {
    let back_to_en = |url: &str| with_hreflangs(url, &[("en", EN)]);
    let source = with_hreflangs(EN, &[("en", EN), ("fr", FR)]);

    let mut non_canonical = back_to_en(FR);
    non_canonical.canonical = Some("https://example.com/other".into());
    let found = check_site(&[source.clone(), non_canonical]);
    assert!(has(&found, 0, "hreflang_to_non_canonical"));

    let noindex = SitePage { noindex: true, ..back_to_en(FR) };
    assert!(has(&check_site(&[source.clone(), noindex]), 0, "hreflang_to_noindex"));

    let moved = redirect(FR, "https://example.com/fr-new");
    assert!(has(&check_site(&[source.clone(), moved]), 0, "hreflang_to_redirect"));

    for status in [404, 503, 0] {
        let broken = SitePage { status, ..back_to_en(FR) };
        assert!(has(&check_site(&[source.clone(), broken]), 0, "hreflang_to_error"), "status {status}");
    }
}

#[test]
fn a_url_named_with_several_languages() {
    let pages = [
        with_hreflangs("https://example.com/a", &[("en", EN)]),
        with_hreflangs("https://example.com/b", &[("en-gb", EN)]),
        with_hreflangs("https://example.com/c", &[("x-default", EN), ("fr", FR)]),
        site(EN),
        site(FR),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 3), ["hreflang_multiple_langs"]);
    assert_eq!(found.iter().find(|(at, _)| *at == 3).unwrap().1.detail.as_deref(), Some("en, en-gb"));
    // x-default does not count as a language.
    assert!(codes_at(&found, 4).is_empty());
}

// ---- links ----

#[test]
fn internal_links_to_redirects_and_broken_pages() {
    let linking = SitePage {
        links: vec![link("https://example.com/moved"), link("https://example.com/gone"), link("https://example.com/fine")],
        ..site("https://example.com/")
    };
    let pages = [
        linking,
        redirect("https://example.com/moved", "https://example.com/new"),
        SitePage { status: 404, ..site("https://example.com/gone") },
        site("https://example.com/fine"),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["internal_link_to_redirect", "internal_link_to_error"]);
    assert_eq!(found[0].1.detail.as_deref(), Some("1: https://example.com/moved"));
}

#[test]
fn external_links_are_judged_by_the_page_that_was_fetched_for_them() {
    let external = |target: &str| SiteLink { internal: false, ..link(target) };
    let linking = SitePage {
        links: vec![external("https://other.org/old"), external("https://other.org/dead"), external("https://other.org/ok")],
        ..site("https://example.com/")
    };
    let checked = |url: &str, status: u16| SitePage { status, external: true, ..site(url) };
    let pages = [
        linking,
        checked("https://other.org/old", 301),
        checked("https://other.org/dead", 404),
        checked("https://other.org/ok", 200),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 0), ["external_link_redirect", "external_link_broken"]);
    // The fetched external pages are not checked themselves.
    assert!((1..4).all(|page| codes_at(&found, page).is_empty()));
}

#[test]
fn a_failed_request_to_an_external_site_is_a_broken_link() {
    let external = SiteLink { internal: false, ..link("https://other.org/") };
    let pages = [
        SitePage { links: vec![external], ..site("https://example.com/") },
        SitePage { status: 0, external: true, ..site("https://other.org/") },
    ];
    assert_eq!(codes_at(&check_site(&pages), 0), ["external_link_broken"]);
}

#[test]
fn a_link_to_the_page_itself_is_ignored() {
    let me = SitePage { links: vec![link("https://example.com/a")], ..redirect("https://example.com/a", "https://example.com/b") };
    let healthy = SitePage { links: vec![link("https://example.com/a#top")], status: 404, ..site("https://example.com/a") };
    assert!(check_site(&[healthy]).is_empty());
    assert!(!check_site(&[me]).iter().any(|(_, issue)| issue.code == "internal_link_to_redirect"));
}

#[test]
fn nofollow_links_to_pages_that_should_be_indexed() {
    let linking = SitePage { links: vec![nofollow("https://example.com/b")], ..site("https://example.com/a") };
    let found = check_site(&[linking.clone(), site("https://example.com/b")]);
    assert_eq!(codes_at(&found, 0), ["nofollow_to_indexable"]);

    // Linking with nofollow to something that is not indexed anyway is fine.
    let noindex = SitePage { noindex: true, ..site("https://example.com/b") };
    assert!(check_site(&[linking.clone(), noindex]).is_empty());
    let non_canonical = canonical_to("https://example.com/b", "https://example.com/c");
    assert!(check_site(&[linking.clone(), non_canonical]).is_empty());
    // A followed link is fine too.
    let followed = SitePage { links: vec![link("https://example.com/b")], ..site("https://example.com/a") };
    assert!(check_site(&[followed, site("https://example.com/b")]).is_empty());
}

#[test]
fn a_page_linked_both_with_and_without_nofollow() {
    let target = "https://example.com/t";
    let pages = [
        SitePage { links: vec![link(target)], ..site("https://example.com/a") },
        SitePage { links: vec![nofollow(target)], ..site("https://example.com/b") },
        site(target),
    ];
    let found = check_site(&pages);
    assert_eq!(codes_at(&found, 2), ["incoming_follow_and_nofollow"]);
    assert_eq!(found.iter().find(|(at, _)| *at == 2).unwrap().1.detail.as_deref(), Some("1 без nofollow, 1 с nofollow"));

    let only_followed = [pages[0].clone(), SitePage { links: vec![link(target)], ..site("https://example.com/b") }, site(target)];
    assert!(check_site(&only_followed).is_empty());
}

#[test]
fn orphan_pages_are_in_the_sitemap_and_linked_from_nowhere() {
    let orphan = SitePage { in_sitemap: true, ..site("https://example.com/lost") };
    let hub = SitePage { links: vec![link("https://example.com/found")], ..site("https://example.com/") };
    let found_page = SitePage { in_sitemap: true, ..site("https://example.com/found") };
    let found = check_site(&[hub, found_page, orphan]);
    assert!(!has(&found, 1, "orphan_page"));
    assert_eq!(codes_at(&found, 2), ["orphan_page"]);
}

#[test]
fn orphan_check_counts_redirects_as_a_way_in_and_skips_pages_outside_the_sitemap() {
    let target = SitePage { in_sitemap: true, ..site("https://example.com/new") };
    let moved = redirect("https://example.com/old", "https://example.com/new");
    assert!(check_site(&[target, moved]).is_empty());

    assert!(check_site(&[site("https://example.com/unlisted")]).is_empty());
    // Links from the page to itself do not make it findable.
    let selfish = SitePage { in_sitemap: true, links: vec![link("https://example.com/me")], ..site("https://example.com/me") };
    assert_eq!(codes_at(&check_site(&[selfish]), 0), ["orphan_page"]);
    // A page that is an error or not HTML is not an orphan page.
    let gone = SitePage { in_sitemap: true, status: 404, ..site("https://example.com/gone") };
    assert!(check_site(&[gone]).is_empty());
}

// ---- building pages ----

#[test]
fn site_page_reads_what_the_checks_need() {
    let head = r#"<link rel="canonical" href="/main"><link rel="alternate" hreflang="fr" href="https://example.com/fr">"#;
    let body = r#"<a href="/inner">in</a><a href="https://other.org/x" rel="nofollow">out</a>"#;
    let page = Doc::good().head(head).body(body).page("https://www.example.com/page").in_sitemap().depth(3).site_page();

    assert_eq!(page.url, "https://www.example.com/page");
    assert_eq!(page.status, 200);
    assert!(page.html && page.in_sitemap && !page.external && !page.blocked_by_robots);
    assert_eq!(page.depth, 3);
    assert_eq!(page.lang.as_deref(), Some("en"));
    assert_eq!(page.title.as_deref(), Some("A reasonably descriptive page title"));
    assert_eq!(page.h1.as_deref(), Some("The main heading"));
    assert_eq!(page.canonical.as_deref(), Some("https://www.example.com/main"));
    assert_eq!(page.hreflangs, [SiteHreflang { lang: "fr".into(), href: "https://example.com/fr".into() }]);
    assert!(page.text_hash.is_some());

    let internal: Vec<_> = page.links.iter().filter(|l| l.internal).map(|l| &*l.target).collect();
    assert_eq!(internal, ["https://www.example.com/other", "https://www.example.com/inner"]);
    let external = page.links.iter().find(|l| !l.internal).unwrap();
    assert!(external.nofollow);
}

#[test]
fn site_page_takes_redirects_and_header_directives_from_the_response() {
    let mut moved = Fixture::resource("https://example.com/old", 301, None);
    moved.http.location = Some("https://example.com/new#x".into());
    assert_eq!(moved.site_page().location.as_deref(), Some("https://example.com/new"));
    assert!(!moved.site_page().html);

    let refreshed = Doc::good().head(r#"<meta http-equiv="refresh" content="0; url=/next">"#).page("https://example.com/r");
    assert_eq!(refreshed.site_page().location.as_deref(), Some("https://example.com/next"));

    let header = Doc::good().page("https://example.com/h").header("x-robots-tag", "noindex, nofollow");
    let from_header = header.site_page();
    assert!(from_header.noindex);
    // The whole page is nofollow, so its links are.
    assert!(from_header.links.iter().all(|l| l.nofollow));

    let canonical = Fixture::resource("https://example.com/doc.pdf", 200, Some("application/pdf"))
        .header("link", r#"<https://example.com/page>; rel="canonical""#);
    assert_eq!(canonical.site_page().canonical.as_deref(), Some("https://example.com/page"));
}

#[test]
fn site_page_from_input_carries_the_robots_block() {
    let blocked = Fixture::resource("https://example.com/private", 0, None).blocked();
    assert!(blocked.site_page().blocked_by_robots);
    assert!(!blocked.site_page().with_blocked_by_robots(false).blocked_by_robots);
    assert!(Fixture::good().site_page().with_external(true).external);
}

#[test]
fn text_hash_is_absent_for_pages_without_text() {
    let empty = Doc::good().h1(None).no_link().words(0).page("https://example.com/empty");
    assert_eq!(empty.site_page().text_hash, None);
}

#[test]
fn pages_built_by_the_crawler_flow_through_the_checks() {
    // Three pages with the same title; b and c also have the same text, a
    // differs by the text of its link, and links to a redirect.
    let a = Doc::good().body(r#"<a href="/moved">m</a>"#).page("https://example.com/a").site_page();
    let b = Doc::good().page("https://example.com/b").site_page();
    let c = Doc::good().page("https://example.com/c").site_page();
    let mut moved = Fixture::resource("https://example.com/moved", 301, None);
    moved.http.location = Some("https://example.com/final".into());
    let found = check_site(&[a, b, c, moved.site_page()]);

    assert!((0..3).all(|page| has(&found, page, "title_duplicate")));
    assert!(has(&found, 0, "internal_link_to_redirect"));
    assert!(!has(&found, 0, "duplicate_content"));
    assert!(has(&found, 1, "duplicate_content") && has(&found, 2, "duplicate_content"));
    // Sorted by page, so a consumer can walk the result once.
    assert!(found.windows(2).all(|pair| pair[0].0 <= pair[1].0));
}
