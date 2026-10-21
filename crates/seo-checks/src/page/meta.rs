//! Titles, meta descriptions, headings and the content of the document.
//! Lengths are counted in characters: the text of a Russian page is two bytes
//! per letter and must not look twice as long as it is.

use seo_page::PageFacts;

use super::Ctx;
use crate::{urls, Issue};

const TITLE_MIN: usize = 20;
const TITLE_MAX: usize = 60;
const DESCRIPTION_MIN: usize = 80;
const DESCRIPTION_MAX: usize = 160;
/// Fewer words of visible text than this is thin content.
const LITTLE_CONTENT_WORDS: usize = 200;
/// More elements than this makes the DOM heavy.
const DOM_SIZE_MAX: usize = 1500;

pub(super) fn check(ctx: &Ctx, out: &mut Vec<Issue>) {
    if let Some(doc) = ctx.doc() {
        text_tag(
            &doc.titles,
            &Limits {
                missing: "title_missing",
                short: "title_too_short",
                long: "title_too_long",
                multiple: "title_multiple",
                min: TITLE_MIN,
                max: TITLE_MAX,
            },
            out,
        );
        text_tag(
            &doc.descriptions,
            &Limits {
                missing: "description_missing",
                short: "description_too_short",
                long: "description_too_long",
                multiple: "description_multiple",
                min: DESCRIPTION_MIN,
                max: DESCRIPTION_MAX,
            },
            out,
        );
        headings(doc, out);
        content(doc, out);
    }
    if let Some(mismatch) = media_type_mismatch(ctx) {
        out.push(Issue::with_detail("incorrect_media_type", mismatch));
    }
}

/// The codes and length bounds of one kind of tag (title or description).
struct Limits {
    missing: &'static str,
    short: &'static str,
    long: &'static str,
    multiple: &'static str,
    min: usize,
    max: usize,
}

/// `values` are all the tags of one kind in `<head>`, in document order.
fn text_tag(values: &[String], limits: &Limits, out: &mut Vec<Issue>) {
    let first = values.first().map(String::as_str).unwrap_or("");
    if first.is_empty() {
        out.push(Issue::new(limits.missing));
    } else {
        let length = first.chars().count();
        if length < limits.min {
            out.push(Issue::with_detail(limits.short, format!("{length} симв.")));
        }
        if length > limits.max {
            out.push(Issue::with_detail(limits.long, format!("{length} симв.")));
        }
    }
    if values.len() > 1 {
        out.push(Issue::with_detail(limits.multiple, format!("{} тегов", values.len())));
    }
}

fn headings(doc: &PageFacts, out: &mut Vec<Issue>) {
    if !doc.h1().any(|text| !text.is_empty()) {
        out.push(Issue::new("h1_missing"));
    }
    if let Some((from, to)) = first_level_jump(&doc.headings) {
        out.push(Issue::with_detail("headings_order_invalid", format!("H{from} → H{to}")));
    }
}

/// The first place where a heading is more than one level deeper than the one
/// before it. The sequence starts at level 1, so a page opening with H3 jumps
/// too, while going back up (H3 then H2) is fine.
fn first_level_jump(headings: &[(u8, String)]) -> Option<(u8, u8)> {
    let mut current = 1;
    for (level, _) in headings {
        if *level > current + 1 {
            return Some((current, *level));
        }
        current = *level;
    }
    None
}

fn content(doc: &PageFacts, out: &mut Vec<Issue>) {
    if doc.word_count < LITTLE_CONTENT_WORDS {
        out.push(Issue::with_detail("little_content", format!("{} слов", doc.word_count)));
    }
    if !doc.duplicate_ids.is_empty() {
        out.push(Issue::with_detail("duplicate_ids", urls::examples(&doc.duplicate_ids, 5)));
    }
    if doc.dom_elements > DOM_SIZE_MAX {
        out.push(Issue::with_detail("dom_size", format!("{} элементов", doc.dom_elements)));
    }
    if doc.metas_in_body > 0 {
        out.push(Issue::with_detail("metas_in_body", format!("{} тегов", doc.metas_in_body)));
    }
}

/// Describes the mismatch between the Content-Type of a successful response and
/// what the extension of the URL promises. An address without an extension
/// promises HTML; an extension we do not know promises nothing.
fn media_type_mismatch(ctx: &Ctx) -> Option<String> {
    if !ctx.success {
        return None;
    }
    let Some(actual) = ctx.http.media_type().filter(|media| !media.is_empty()) else {
        return Some("нет заголовка Content-Type".to_string());
    };
    let file = ctx.url.path().rsplit('/').next().unwrap_or("");
    let extension = file.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase());
    let expected = expected_media_types(extension.as_deref().unwrap_or("html"))?;
    if expected.contains(&actual.as_str()) {
        None
    } else {
        Some(format!("получен {actual}, ожидался {}", expected[0]))
    }
}

/// Media types a file extension may be served as, preferred one first.
fn expected_media_types(extension: &str) -> Option<&'static [&'static str]> {
    Some(match extension {
        "html" | "htm" => &["text/html", "application/xhtml+xml"],
        "css" => &["text/css"],
        "js" | "mjs" => &["application/javascript", "text/javascript", "application/x-javascript"],
        "json" => &["application/json"],
        "xml" => &["application/xml", "text/xml"],
        "txt" => &["text/plain"],
        "pdf" => &["application/pdf"],
        "png" => &["image/png"],
        "jpg" | "jpeg" => &["image/jpeg"],
        "gif" => &["image/gif"],
        "webp" => &["image/webp"],
        "avif" => &["image/avif"],
        "svg" => &["image/svg+xml"],
        "ico" => &["image/x-icon", "image/vnd.microsoft.icon"],
        "woff" => &["font/woff", "application/font-woff"],
        "woff2" => &["font/woff2", "application/font-woff2"],
        "mp4" => &["video/mp4"],
        "webm" => &["video/webm"],
        "mp3" => &["audio/mpeg"],
        "zip" => &["application/zip", "application/x-zip-compressed"],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use crate::testutil::{Doc, Fixture, URL};

    /// `n` Cyrillic letters: two bytes each, so the byte length is `2 * n`.
    fn cyrillic(n: usize) -> String {
        "я".repeat(n)
    }

    #[test]
    fn missing_title() {
        assert!(Doc::good().no_title().page(URL).has("title_missing"));
        assert!(!Fixture::good().has("title_missing"));
        // An empty tag is as good as none.
        assert!(Doc::good().title("  ").page(URL).has("title_missing"));
    }

    #[test]
    fn title_length_bounds() {
        let short = Doc::good().title("Short title").page(URL);
        assert!(short.has("title_too_short") && !short.has("title_too_long"));
        assert_eq!(short.detail("title_too_short").as_deref(), Some("11 симв."));

        let edge = Doc::good().title(&"a".repeat(20)).page(URL);
        assert!(!edge.has("title_too_short"));
        let edge = Doc::good().title(&"a".repeat(60)).page(URL);
        assert!(!edge.has("title_too_long"));

        let long = Doc::good().title(&"a".repeat(61)).page(URL);
        assert!(long.has("title_too_long") && !long.has("title_too_short"));
    }

    #[test]
    fn title_length_is_counted_in_characters() {
        // 40 letters are 80 bytes: not long. SEOnaut counts bytes and would
        // flag a title that is two thirds of the limit.
        assert!(!Doc::good().title(&cyrillic(40)).page(URL).has("title_too_long"));
        // 15 letters are 30 bytes, yet the title is short.
        assert!(Doc::good().title(&cyrillic(15)).page(URL).has("title_too_short"));
        assert!(Doc::good().title(&cyrillic(61)).page(URL).has("title_too_long"));
        assert!(!Doc::good().title(&cyrillic(60)).page(URL).has("title_too_long"));
    }

    #[test]
    fn several_titles_in_head() {
        let page = Doc::good().head("<title>Another title in the head</title>").page(URL);
        assert!(page.has("title_multiple"));
        assert!(!Fixture::good().has("title_multiple"));
    }

    #[test]
    fn missing_description() {
        assert!(Doc::good().no_description().page(URL).has("description_missing"));
        assert!(!Fixture::good().has("description_missing"));
    }

    #[test]
    fn description_length_bounds() {
        let short = Doc::good().description("A short description.").page(URL);
        assert!(short.has("description_too_short") && !short.has("description_too_long"));
        assert!(!Doc::good().description(&"a".repeat(80)).page(URL).has("description_too_short"));
        assert!(!Doc::good().description(&"a".repeat(160)).page(URL).has("description_too_long"));
        let long = Doc::good().description(&"a".repeat(161)).page(URL);
        assert!(long.has("description_too_long") && !long.has("description_too_short"));
    }

    #[test]
    fn description_length_is_counted_in_characters() {
        // 100 letters are 200 bytes but fit the snippet.
        let fits = Doc::good().description(&cyrillic(100)).page(URL);
        assert!(!fits.has("description_too_long") && !fits.has("description_too_short"));
        // 70 letters are 140 bytes and still short.
        assert!(Doc::good().description(&cyrillic(70)).page(URL).has("description_too_short"));
    }

    #[test]
    fn several_descriptions_in_head() {
        let page = Doc::good().head(r#"<meta name="description" content="A second description tag.">"#).page(URL);
        assert!(page.has("description_multiple"));
        assert!(!Fixture::good().has("description_multiple"));
    }

    #[test]
    fn title_and_description_checks_skip_pages_that_did_not_answer_2xx() {
        let page = Doc::good().no_title().no_description().page(URL).status(404);
        assert!(!page.has("title_missing") && !page.has("description_missing") && !page.has("h1_missing"));
    }

    #[test]
    fn missing_h1() {
        assert!(Doc::good().h1(None).page(URL).has("h1_missing"));
        assert!(Doc::good().h1(Some("")).page(URL).has("h1_missing"));
        assert!(!Fixture::good().has("h1_missing"));
    }

    #[test]
    fn heading_order() {
        let ordered = Doc::good().body("<h2>Two</h2><h3>Three</h3><h2>Back up</h2>").page(URL);
        assert!(!ordered.has("headings_order_invalid"));

        // h1, h3, h2, h4: the jump from h1 to h3 is the first break.
        let skipped = Doc::good().body("<h3>Three</h3><h2>Two</h2><h4>Four</h4>").page(URL);
        assert!(skipped.has("headings_order_invalid"));
        assert_eq!(skipped.detail("headings_order_invalid").as_deref(), Some("H1 → H3"));
    }

    #[test]
    fn heading_order_starts_at_level_one() {
        let opens_deep = Doc::good().h1(None).body("<h3>Three</h3>").page(URL);
        assert!(opens_deep.has("headings_order_invalid"));
        // Opening with h2 is allowed: the same leniency as the original.
        let opens_with_two = Doc::good().h1(None).body("<h2>Two</h2>").page(URL);
        assert!(!opens_with_two.has("headings_order_invalid"));
    }

    #[test]
    fn thin_content() {
        // No heading and no link, so the 30 filler words are all there is.
        let thin = Doc::good().h1(None).no_link().words(30).page(URL);
        assert!(thin.has("little_content"));
        assert_eq!(thin.detail("little_content").as_deref(), Some("30 слов"));
        assert!(!Doc::good().words(195).page(URL).has("little_content"));
        assert!(!Doc::good().words(300).page(URL).has("little_content"));
    }

    #[test]
    fn duplicate_ids() {
        let page = Doc::good().body(r#"<div id="header"></div><span id="header"></span><p id="x"></p>"#).page(URL);
        assert!(page.has("duplicate_ids"));
        assert_eq!(page.detail("duplicate_ids").as_deref(), Some("header"));
        let unique = Doc::good().body(r#"<div id="a"></div><span id="b"></span>"#).page(URL);
        assert!(!unique.has("duplicate_ids"));
    }

    #[test]
    fn dom_size_limit_is_1500_elements() {
        let small = Doc::good().body(&"<i></i>".repeat(100)).page(URL);
        assert!(!small.has("dom_size"));
        let big = Doc::good().body(&"<i></i>".repeat(1500)).page(URL);
        assert!(big.has("dom_size"));
    }

    #[test]
    fn meta_tags_in_body() {
        let page = Doc::good().body(r#"<meta name="robots" content="noindex">"#).page(URL);
        assert!(page.has("metas_in_body"));
        assert!(!Fixture::good().has("metas_in_body"));
    }

    #[test]
    fn media_type_must_match_extension() {
        // No extension means HTML is expected.
        assert!(!Fixture::good().has("incorrect_media_type"));
        let json = Fixture::resource("https://example.com/no-issues", 200, Some("application/json"));
        assert!(json.has("incorrect_media_type"));

        let script = Fixture::resource("https://example.com/script.js", 200, Some("application/javascript"));
        assert!(!script.has("incorrect_media_type"));
        let script = Fixture::resource("https://example.com/script.js", 200, Some("text/javascript; charset=utf-8"));
        assert!(!script.has("incorrect_media_type"));

        let pdf = Fixture::resource("https://example.com/issues.pdf", 200, Some("text/html"));
        assert!(pdf.has("incorrect_media_type"));
        let pdf = Fixture::resource("https://example.com/issues.pdf", 200, Some("application/pdf"));
        assert!(!pdf.has("incorrect_media_type"));
    }

    #[test]
    fn missing_content_type_is_a_wrong_media_type() {
        let page = Fixture::resource("https://example.com/file", 200, None);
        assert!(page.has("incorrect_media_type"));
        // Unknown extensions promise nothing, and errors are not judged.
        let unknown = Fixture::resource("https://example.com/data.xyz", 200, Some("application/octet-stream"));
        assert!(!unknown.has("incorrect_media_type"));
        assert!(!Fixture::resource("https://example.com/x.pdf", 404, Some("text/html")).has("incorrect_media_type"));
    }
}
