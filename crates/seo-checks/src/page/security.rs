//! Transport security and security headers, forms, URL shape and the mobile
//! viewport.

use super::Ctx;
use crate::{urls, Issue};

pub(super) fn check(ctx: &Ctx, out: &mut Vec<Issue>) {
    url_shape(ctx, out);

    if ctx.success && !ctx.is_https() {
        out.push(Issue::new("http_scheme"));
    }

    let Some(doc) = ctx.doc() else { return };

    // Response headers only matter for pages served over HTTPS; HSTS is not
    // even honoured over plain HTTP, and `http_scheme` already covers those.
    if ctx.is_https() {
        if let Some(problem) = hsts_problem(ctx.http.header("strict-transport-security")) {
            out.push(Issue::with_detail("hsts_missing", problem));
        }
        let csp_header = ctx.http.header("content-security-policy").is_some_and(|value| !value.trim().is_empty());
        if !csp_header && !doc.csp_meta {
            out.push(Issue::new("csp_missing"));
        }
        if !declares_nosniff(ctx) {
            out.push(Issue::new("x_content_type_options_missing"));
        }
    }

    if !doc.forms.is_empty() && !ctx.is_https() {
        out.push(Issue::with_detail("form_on_http", format!("{} форм", doc.forms.len())));
    }
    // On an HTTP page the form is already reported above; here the page is
    // secure and the form sends its data somewhere that is not.
    if ctx.is_https() {
        let insecure: Vec<&str> = doc
            .forms
            .iter()
            .filter(|form| form.action.starts_with("http://"))
            .map(|form| form.action.as_str())
            .collect();
        if !insecure.is_empty() {
            out.push(Issue::with_detail("insecure_form", urls::examples(&insecure, 3)));
        }
    }

    if doc.viewport.as_deref().map_or(true, |content| content.trim().is_empty()) {
        out.push(Issue::new("viewport_missing"));
    }
}

/// Underscores, spaces and doubled slashes in the path. The query is left
/// alone: parameters like `utm_source` are written with underscores everywhere.
fn url_shape(ctx: &Ctx, out: &mut Vec<Issue>) {
    let path = ctx.url.path();
    if path.contains('_') {
        out.push(Issue::new("url_underscore"));
    }
    // The URL parser writes a space as %20.
    if path.contains("%20") {
        out.push(Issue::new("url_space"));
    }
    if path.contains("//") {
        out.push(Issue::new("url_multiple_slashes"));
    }
}

/// What is wrong with the `Strict-Transport-Security` header, if anything:
/// absent, without a numeric `max-age`, or with `max-age=0`, which switches
/// HSTS off.
fn hsts_problem(value: Option<&str>) -> Option<&'static str> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Some("заголовок отсутствует");
    };
    let max_age = value.split(';').find_map(|directive| {
        let (name, number) = directive.split_once('=')?;
        name.trim().eq_ignore_ascii_case("max-age").then(|| number.trim().trim_matches('"'))
    });
    match max_age.map(str::parse::<u64>) {
        None => Some("нет директивы max-age"),
        Some(Err(_)) => Some("max-age не число"),
        Some(Ok(0)) => Some("max-age=0 отключает HSTS"),
        Some(Ok(_)) => None,
    }
}

fn declares_nosniff(ctx: &Ctx) -> bool {
    ctx.http
        .header_values("x-content-type-options")
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("nosniff"))
}

#[cfg(test)]
mod tests {
    use crate::testutil::{Doc, Fixture, URL};

    const HTTP_URL: &str = "http://example.com/page";

    #[test]
    fn page_served_over_http() {
        assert!(!Fixture::good().has("http_scheme"));
        assert!(Doc::good().page(HTTP_URL).has("http_scheme"));
        // Only a page that answered is a page served over HTTP.
        assert!(!Doc::good().page(HTTP_URL).status(404).has("http_scheme"));
        assert!(!Doc::good().page(HTTP_URL).status(301).has("http_scheme"));
    }

    #[test]
    fn hsts_header() {
        assert!(!Fixture::good().has("hsts_missing"));
        let missing = Fixture::good().no_header("strict-transport-security");
        assert!(missing.has("hsts_missing"));
        let full = Fixture::good()
            .no_header("strict-transport-security")
            .header("strict-transport-security", "max-age=31536000; includeSubDomains; preload");
        assert!(!full.has("hsts_missing"));

        for (value, detail) in [
            ("includeSubDomains", "нет директивы max-age"),
            ("max-age=soon", "max-age не число"),
            ("max-age=0", "max-age=0 отключает HSTS"),
        ] {
            let page = Fixture::good().no_header("strict-transport-security").header("strict-transport-security", value);
            assert_eq!(page.detail("hsts_missing").as_deref(), Some(detail), "{value}");
        }
        // The header means nothing over HTTP, where `http_scheme` already speaks.
        assert!(!Doc::good().page(HTTP_URL).no_header("strict-transport-security").has("hsts_missing"));
    }

    #[test]
    fn content_security_policy_from_header_or_meta() {
        let missing = Fixture::good().no_header("content-security-policy");
        assert!(missing.has("csp_missing"));
        assert!(!Fixture::good().has("csp_missing"));
        let meta = Doc::good()
            .head(r#"<meta http-equiv="Content-Security-Policy" content="default-src 'self'">"#)
            .page(URL)
            .no_header("content-security-policy");
        assert!(!meta.has("csp_missing"));
        // Not an HTML document: nothing to protect.
        let image = Fixture::resource("https://example.com/a.png", 200, Some("image/png"));
        assert!(!image.has("csp_missing"));
    }

    #[test]
    fn x_content_type_options_must_be_nosniff() {
        assert!(!Fixture::good().has("x_content_type_options_missing"));
        assert!(Fixture::good().no_header("x-content-type-options").has("x_content_type_options_missing"));
        let wrong = Fixture::good().no_header("x-content-type-options").header("x-content-type-options", "sniff");
        assert!(wrong.has("x_content_type_options_missing"));
        let loud = Fixture::good().no_header("x-content-type-options").header("x-content-type-options", "NoSniff");
        assert!(!loud.has("x_content_type_options_missing"));
    }

    #[test]
    fn form_on_an_http_page() {
        let form = r#"<form action="/subscribe"><input name="email"></form>"#;
        let insecure = Doc::good().body(form).page(HTTP_URL);
        assert!(insecure.has("form_on_http"));
        assert!(!Doc::good().body(form).page(URL).has("form_on_http"));
        assert!(!Doc::good().page(HTTP_URL).has("form_on_http"));
        // Errors are not judged.
        assert!(!Doc::good().body(form).page(HTTP_URL).status(404).has("form_on_http"));
    }

    #[test]
    fn form_posting_from_https_to_http() {
        let to_http = Doc::good().body(r#"<form action="http://example.com/send"></form>"#).page(URL);
        assert!(to_http.has("insecure_form"));
        let to_https = Doc::good().body(r#"<form action="https://example.com/send"></form>"#).page(URL);
        assert!(!to_https.has("insecure_form"));
        // A form without an action goes to the page itself.
        assert!(!Doc::good().body("<form></form>").page(URL).has("insecure_form"));
        // On an HTTP page the form is reported once, as `form_on_http`.
        let on_http = Doc::good().body(r#"<form action="/send"></form>"#).page(HTTP_URL);
        assert!(on_http.has("form_on_http") && !on_http.has("insecure_form"));
    }

    #[test]
    fn viewport_meta() {
        assert!(!Fixture::good().has("viewport_missing"));
        assert!(Doc::good().no_viewport().page(URL).has("viewport_missing"));
        let empty = Doc::good().no_viewport().head(r#"<meta name="viewport" content="">"#).page(URL);
        assert!(empty.has("viewport_missing"));
    }

    #[test]
    fn underscore_in_the_path() {
        assert!(!Doc::good().page("https://example.com/some-url").has("url_underscore"));
        assert!(Doc::good().page("https://example.com/some_url").has("url_underscore"));
        // Query parameters are conventionally written with underscores.
        assert!(!Doc::good().page("https://example.com/page?utm_source=mail").has("url_underscore"));
    }

    #[test]
    fn space_in_the_path() {
        assert!(!Doc::good().page("https://example.com/someurl").has("url_space"));
        assert!(Doc::good().page("https://example.com/some url").has("url_space"));
        assert!(Doc::good().page("https://example.com/some%20url").has("url_space"));
        assert!(!Doc::good().page("https://example.com/search?q=some%20words").has("url_space"));
    }

    #[test]
    fn doubled_slashes_in_the_path() {
        assert!(!Doc::good().page("https://example.com/someurl").has("url_multiple_slashes"));
        assert!(Doc::good().page("https://example.com/someurl//").has("url_multiple_slashes"));
        assert!(Doc::good().page("https://example.com/a//b").has("url_multiple_slashes"));
    }
}
