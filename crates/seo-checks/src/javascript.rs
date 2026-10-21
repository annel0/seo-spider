//! The JavaScript tab: what a page looks like to a robot that does not run
//! scripts (the raw response) against what a browser shows (the rendered DOM).
//! The tab is built the way Screaming Frog's is, one pair of issues per thing a
//! script can add or change.

use std::collections::HashSet;

use seo_page::PageFacts;

use crate::headers;
use crate::page::PageInput;
use crate::{urls, Issue};

/// Rendered text counts as a big addition when it has more than this many
/// times the words of the raw text (3 / 2 = 1.5)...
const RATIO_NUMERATOR: usize = 3;
const RATIO_DENOMINATOR: usize = 2;
/// ...and at least this many more words, so a tiny page that grew from ten
/// words to twenty does not count.
const MIN_EXTRA_WORDS: usize = 50;

pub(crate) fn check(input: &PageInput, out: &mut Vec<Issue>) {
    let (Some(raw), Some(rendered)) = (input.raw, input.rendered) else {
        return;
    };
    // Redirects and errors have nothing worth comparing.
    if !(200..300).contains(&input.http.status) {
        return;
    }

    compare(
        raw.title().filter(|text| !text.is_empty()),
        rendered.title().filter(|text| !text.is_empty()),
        ("js_title_only_in_rendered", "js_title_changed"),
        |before, after| before == after,
        out,
    );
    compare(
        raw.description().filter(|text| !text.is_empty()),
        rendered.description().filter(|text| !text.is_empty()),
        ("js_description_only_in_rendered", "js_description_changed"),
        |before, after| before == after,
        out,
    );
    compare(
        raw.h1().find(|text| !text.is_empty()),
        rendered.h1().find(|text| !text.is_empty()),
        ("js_h1_only_in_rendered", "js_h1_changed"),
        |before, after| before == after,
        out,
    );
    compare(
        first_canonical(raw),
        first_canonical(rendered),
        ("js_canonical_only_in_rendered", "js_canonical_mismatch"),
        urls::same_url,
        out,
    );

    directives(input, raw, rendered, out);
    links(raw, rendered, out);
    content(raw, rendered, out);
}

fn first_canonical(facts: &PageFacts) -> Option<&str> {
    facts.canonicals.iter().map(String::as_str).find(|canonical| !canonical.is_empty())
}

/// `only_in_rendered` when the raw document lacks the value and the rendered
/// one has it; `changed` when both have it and `same` says they differ, or when
/// a script removed it.
fn compare(
    raw: Option<&str>,
    rendered: Option<&str>,
    (only_in_rendered, changed): (&'static str, &'static str),
    same: impl Fn(&str, &str) -> bool,
    out: &mut Vec<Issue>,
) {
    match (raw, rendered) {
        (None, Some(after)) => out.push(Issue::with_detail(only_in_rendered, urls::clip(after, 80))),
        (Some(before), Some(after)) if !same(before, after) => out.push(Issue::with_detail(
            changed,
            format!("было: {} → стало: {}", urls::clip(before, 60), urls::clip(after, 60)),
        )),
        (Some(before), None) => out.push(Issue::with_detail(
            changed,
            format!("удалено скриптом: {}", urls::clip(before, 60)),
        )),
        _ => {}
    }
}

/// A `noindex` or `nofollow` written in a meta tag of the raw HTML that the
/// rendered DOM no longer has. Headers are the same before and after scripts,
/// so a directive that also comes from `X-Robots-Tag` never goes away.
fn directives(input: &PageInput, raw: &PageFacts, rendered: &PageFacts, out: &mut Vec<Issue>) {
    let before = headers::robots(input.http, Some(raw));
    let after = headers::robots(input.http, Some(rendered));
    if before.meta_noindex() && !after.noindex() {
        out.push(Issue::new("js_noindex_only_in_raw"));
    }
    if before.meta_nofollow() && !after.nofollow() {
        out.push(Issue::new("js_nofollow_only_in_raw"));
    }
}

/// Links that appear only after scripts ran.
fn links(raw: &PageFacts, rendered: &PageFacts, out: &mut Vec<Issue>) {
    let known: HashSet<&str> = raw.links.iter().map(|link| link.href.as_str()).collect();
    let mut seen = HashSet::new();
    let added: Vec<&str> = rendered
        .links
        .iter()
        .map(|link| link.href.as_str())
        .filter(|href| !known.contains(href) && seen.insert(*href))
        .collect();
    if !added.is_empty() {
        out.push(Issue::with_detail(
            "js_links_only_in_rendered",
            format!("{} ссылок: {}", added.len(), urls::examples(&added, 3)),
        ));
    }
}

/// A large share of the text that exists only after scripts ran.
fn content(raw: &PageFacts, rendered: &PageFacts, out: &mut Vec<Issue>) {
    let (before, after) = (raw.word_count, rendered.word_count);
    if after * RATIO_DENOMINATOR > before * RATIO_NUMERATOR && after > before + MIN_EXTRA_WORDS {
        out.push(Issue::with_detail(
            "js_content_only_in_rendered",
            format!("слов: {before} в исходном HTML, {after} после рендера"),
        ));
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::{Doc, Fixture, URL};

    /// The page as served, and what the browser shows after scripts ran.
    fn page(raw: Doc, rendered: Doc) -> Fixture {
        raw.page(URL).rendered(&rendered)
    }

    fn js_codes(fixture: &Fixture) -> Vec<&'static str> {
        fixture.codes().into_iter().filter(|code| code.starts_with("js_")).collect()
    }

    const CANONICAL: &str = r#"<link rel="canonical" href="https://example.com/page">"#;
    const OTHER_CANONICAL: &str = r#"<link rel="canonical" href="https://example.com/other">"#;

    #[test]
    fn identical_documents_have_no_javascript_issues() {
        let same = page(Doc::good(), Doc::good());
        assert_eq!(js_codes(&same), Vec::<&str>::new());
    }

    #[test]
    fn tab_is_skipped_without_both_documents() {
        let rendered_only = Fixture::good();
        assert!(js_codes(&rendered_only).is_empty());
        let mut raw_missing = page(Doc::good().no_title(), Doc::good());
        raw_missing.raw = None;
        assert!(js_codes(&raw_missing).is_empty());
        let mut rendered_missing = page(Doc::good().no_title(), Doc::good());
        rendered_missing.rendered = None;
        assert!(js_codes(&rendered_missing).is_empty());
    }

    #[test]
    fn tab_is_skipped_for_error_pages() {
        let broken = page(Doc::good().no_title(), Doc::good()).status(404);
        assert!(js_codes(&broken).is_empty());
    }

    #[test]
    fn title_added_or_changed_by_script() {
        let added = page(Doc::good().no_title(), Doc::good());
        assert_eq!(js_codes(&added), ["js_title_only_in_rendered"]);
        assert_eq!(added.detail("js_title_only_in_rendered").as_deref(), Some("A reasonably descriptive page title"));

        let changed = page(Doc::good().title("Old title of the page here"), Doc::good().title("New title of the page here"));
        assert_eq!(js_codes(&changed), ["js_title_changed"]);
        assert!(changed.detail("js_title_changed").unwrap().contains("Old title"));

        let removed = page(Doc::good(), Doc::good().no_title());
        assert_eq!(js_codes(&removed), ["js_title_changed"]);
    }

    #[test]
    fn description_added_or_changed_by_script() {
        let added = page(Doc::good().no_description(), Doc::good());
        assert_eq!(js_codes(&added), ["js_description_only_in_rendered"]);
        let changed = page(Doc::good().description("The first description of the page."), Doc::good());
        assert_eq!(js_codes(&changed), ["js_description_changed"]);
    }

    #[test]
    fn h1_added_or_changed_by_script() {
        let added = page(Doc::good().h1(None), Doc::good());
        assert_eq!(js_codes(&added), ["js_h1_only_in_rendered"]);
        let changed = page(Doc::good().h1(Some("Before")), Doc::good().h1(Some("After")));
        assert_eq!(js_codes(&changed), ["js_h1_changed"]);
    }

    #[test]
    fn canonical_added_or_changed_by_script() {
        let added = page(Doc::good(), Doc::good().head(CANONICAL));
        assert_eq!(js_codes(&added), ["js_canonical_only_in_rendered"]);
        let changed = page(Doc::good().head(CANONICAL), Doc::good().head(OTHER_CANONICAL));
        assert_eq!(js_codes(&changed), ["js_canonical_mismatch"]);
        let same = page(Doc::good().head(CANONICAL), Doc::good().head(CANONICAL));
        assert!(js_codes(&same).is_empty());
    }

    #[test]
    fn noindex_that_exists_only_in_the_raw_html() {
        let robots = r#"<meta name="robots" content="noindex, follow">"#;
        let removed = page(Doc::good().head(robots), Doc::good());
        assert_eq!(js_codes(&removed), ["js_noindex_only_in_raw"]);
        // Added by a script is a different story and not part of this filter.
        let added = page(Doc::good(), Doc::good().head(robots));
        assert!(js_codes(&added).is_empty());
        // A header says noindex before and after scripts: nothing was removed.
        let in_header = page(Doc::good().head(robots), Doc::good()).header("x-robots-tag", "noindex");
        assert!(js_codes(&in_header).is_empty());
        let kept = page(Doc::good().head(robots), Doc::good().head(robots));
        assert!(js_codes(&kept).is_empty());
    }

    #[test]
    fn nofollow_that_exists_only_in_the_raw_html() {
        let robots = r#"<meta name="robots" content="index, nofollow">"#;
        let removed = page(Doc::good().head(robots), Doc::good());
        assert_eq!(js_codes(&removed), ["js_nofollow_only_in_raw"]);
        let none = page(Doc::good().head(r#"<meta name="robots" content="none">"#), Doc::good());
        assert_eq!(js_codes(&none), ["js_noindex_only_in_raw", "js_nofollow_only_in_raw"]);
    }

    #[test]
    fn links_that_appear_after_rendering() {
        let rendered = Doc::good().body(r#"<a href="/js-one">1</a><a href="/js-two">2</a><a href="/js-one">again</a>"#);
        let added = page(Doc::good(), rendered);
        assert_eq!(js_codes(&added), ["js_links_only_in_rendered"]);
        let detail = added.detail("js_links_only_in_rendered").unwrap();
        assert!(detail.starts_with("2 ссылок"), "{detail}");
        assert!(detail.contains("https://example.com/js-two"));

        // Removing a link is not the same thing.
        let removed = page(Doc::good().body(r#"<a href="/gone">x</a>"#), Doc::good());
        assert!(js_codes(&removed).is_empty());
    }

    #[test]
    fn content_that_appears_after_rendering() {
        // The heading and the link add five words to the filler text.
        // 255 words became 605: more than 1.5 times and more than 50 words.
        let grown = page(Doc::good().words(250), Doc::good().words(600));
        assert_eq!(js_codes(&grown), ["js_content_only_in_rendered"]);
        assert_eq!(
            grown.detail("js_content_only_in_rendered").as_deref(),
            Some("слов: 255 в исходном HTML, 605 после рендера")
        );

        // 250 to 300 is only 1.2 times.
        assert!(js_codes(&page(Doc::good().words(250), Doc::good().words(300))).is_empty());
        // 20 to 60 is three times, but only 40 words more.
        assert!(js_codes(&page(Doc::good().words(20), Doc::good().words(60))).is_empty());
        // 20 to 100 is both.
        assert_eq!(js_codes(&page(Doc::good().words(20), Doc::good().words(100))), ["js_content_only_in_rendered"]);
        // An empty shell filled by scripts, the typical single-page app.
        let shell = page(Doc::good().h1(None).no_link().words(0), Doc::good().words(300));
        assert!(js_codes(&shell).contains(&"js_content_only_in_rendered"));
    }

    #[test]
    fn cyrillic_titles_are_compared_and_clipped_by_characters() {
        let long = "Очень длинный заголовок страницы, который не помещается в сто символов и обязательно будет обрезан в подробностях".to_string();
        let changed = page(Doc::good().title(&long), Doc::good().title("Короткий заголовок страницы"));
        let detail = changed.detail("js_title_changed").unwrap();
        assert!(detail.contains('…'), "{detail}");
        assert!(detail.contains("Короткий заголовок страницы"));
    }
}
