//! Response codes, failed requests and response time.

use super::Ctx;
use crate::{urls, Issue};

/// A server slower than this to the first byte is reported (milliseconds).
const SLOW_TTFB_MS: u32 = 800;

pub(super) fn check(ctx: &Ctx, out: &mut Vec<Issue>) {
    let http = ctx.http;
    match http.status {
        300..=399 => out.push(match &http.location {
            Some(target) => Issue::with_detail("redirect_3xx", format!("{} → {}", http.status, target)),
            None => Issue::with_detail("redirect_3xx", http.status.to_string()),
        }),
        400..=499 => out.push(Issue::with_detail("client_error_4xx", http.status.to_string())),
        500..=599 => out.push(Issue::with_detail("server_error_5xx", http.status.to_string())),
        // No response at all. A URL that robots.txt kept us from requesting is
        // reported by `blocked_by_robots`, not as a failure.
        0 if !ctx.blocked_by_robots => out.push(failed_request(http.error.as_deref())),
        _ => {}
    }

    // A refresh with a target is a redirect done in the document.
    if let Some(doc) = ctx.doc() {
        if let Some((delay, Some(target))) = &doc.meta_refresh {
            out.push(Issue::with_detail(
                "meta_refresh_redirect",
                format!("через {delay} с → {target}"),
            ));
        }
    }

    if http.status > 0 && http.ttfb_ms > SLOW_TTFB_MS {
        out.push(Issue::with_detail("slow_ttfb", format!("{} мс", http.ttfb_ms)));
    }
}

/// `timeout` when the error text says the request ran out of time,
/// `no_response` for every other failure.
fn failed_request(error: Option<&str>) -> Issue {
    let text = error.unwrap_or("");
    let lower = text.to_lowercase();
    let timed_out = ["timed out", "timeout", "deadline"].iter().any(|word| lower.contains(word));
    let code = if timed_out { "timeout" } else { "no_response" };
    if text.is_empty() {
        Issue::new(code)
    } else {
        Issue::with_detail(code, urls::clip(text, 160))
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::{Doc, Fixture, URL};

    #[test]
    fn ok_response_reports_no_status_issue() {
        let page = Fixture::good();
        for code in ["redirect_3xx", "client_error_4xx", "server_error_5xx", "timeout", "no_response"] {
            assert!(!page.has(code), "{code}");
        }
    }

    #[test]
    fn redirect_status_is_reported_with_its_target() {
        let mut page = Fixture::good().status(301);
        page.http.location = Some("https://example.com/new".into());
        assert!(page.has("redirect_3xx"));
        assert_eq!(page.detail("redirect_3xx").as_deref(), Some("301 → https://example.com/new"));
        assert!(!page.has("client_error_4xx"));
    }

    #[test]
    fn client_and_server_errors() {
        assert!(Fixture::good().status(404).has("client_error_4xx"));
        assert!(!Fixture::good().status(404).has("server_error_5xx"));
        assert!(Fixture::good().status(500).has("server_error_5xx"));
        assert!(Fixture::good().status(503).has("server_error_5xx"));
        assert!(!Fixture::good().status(503).has("client_error_4xx"));
    }

    #[test]
    fn failed_request_is_a_timeout_or_no_response() {
        let timed_out = Fixture::resource(URL, 0, None).error("operation timed out");
        assert_eq!(timed_out.codes(), ["timeout"]);
        let refused = Fixture::resource(URL, 0, None).error("connection refused");
        assert_eq!(refused.codes(), ["no_response"]);
        assert_eq!(refused.detail("no_response").as_deref(), Some("connection refused"));
    }

    #[test]
    fn page_blocked_by_robots_is_not_a_failed_request() {
        let blocked = Fixture::resource(URL, 0, None).blocked();
        assert_eq!(blocked.codes(), ["blocked_by_robots"]);
    }

    #[test]
    fn meta_refresh_with_a_target_is_a_redirect() {
        let page = Doc::good().head(r#"<meta http-equiv="refresh" content="5; url=/next">"#).page(URL);
        assert!(page.has("meta_refresh_redirect"));
        assert!(page.detail("meta_refresh_redirect").unwrap().contains("https://example.com/next"));
        // A refresh that only reloads the same page is not a redirect.
        let reload = Doc::good().head(r#"<meta http-equiv="refresh" content="30">"#).page(URL);
        assert!(!reload.has("meta_refresh_redirect"));
    }

    #[test]
    fn slow_first_byte_over_800_ms() {
        assert!(!Fixture::good().ttfb(800).has("slow_ttfb"));
        assert!(Fixture::good().ttfb(801).has("slow_ttfb"));
        assert_eq!(Fixture::good().ttfb(1500).detail("slow_ttfb").as_deref(), Some("1500 мс"));
    }
}
