//! URL helpers shared by page and site checks.

use url::{ParseError, Url};

/// The canonical spelling used to compare URLs: parsed, no fragment. Anything
/// that does not parse is compared as written.
pub(crate) fn normalize(url: &str) -> String {
    let url = url.trim();
    match Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

pub(crate) fn same_url(a: &str, b: &str) -> bool {
    a == b || normalize(a) == normalize(b)
}

/// What decides whether a link stays on the site: the lower-cased host without
/// a leading `www.`. Scheme and port are ignored, so `http://example.com/` is
/// internal for an `https://www.example.com/` page.
pub(crate) fn site_key(url: &Url) -> Option<String> {
    let host = url.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").map(str::to_string).unwrap_or(host))
}

/// Whether `href` is on the same site as the page with `page_key`.
pub(crate) fn is_internal(page_key: &Option<String>, href: &str) -> bool {
    match (page_key, Url::parse(href).ok().and_then(|url| site_key(&url))) {
        (Some(page), Some(link)) => *page == link,
        _ => false,
    }
}

/// An address without a scheme (`/path`, `//host/path`, `page.html`).
pub(crate) fn is_relative(raw: &str) -> bool {
    matches!(Url::parse(raw.trim()), Err(ParseError::RelativeUrlWithoutBase))
}

/// `text` cut to `max` characters with an ellipsis, for details.
pub(crate) fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut clipped: String = text.chars().take(max).collect();
    clipped.push('…');
    clipped
}

/// `a, b, c и ещё 4`: a few examples and how many more there are.
pub(crate) fn examples<S: AsRef<str>>(items: &[S], shown: usize) -> String {
    let head: Vec<&str> = items.iter().take(shown).map(AsRef::as_ref).collect();
    let mut text = head.join(", ");
    if items.len() > shown {
        text.push_str(&format!(" и ещё {}", items.len() - shown));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_drops_fragment_and_fills_path() {
        assert_eq!(normalize("https://example.com#top"), "https://example.com/");
        assert!(same_url("https://example.com/a#x", "https://example.com/a"));
        assert!(!same_url("https://example.com/a", "https://example.com/b"));
    }

    #[test]
    fn site_key_ignores_www_scheme_and_port() {
        let page = Url::parse("https://www.Example.com/").unwrap();
        let key = site_key(&page);
        assert!(is_internal(&key, "http://example.com:8080/x"));
        assert!(!is_internal(&key, "https://blog.example.com/"));
        assert!(!is_internal(&key, "not a url"));
    }

    #[test]
    fn relative_means_no_scheme() {
        assert!(is_relative("/home"));
        assert!(is_relative("//cdn.example.com/x"));
        assert!(is_relative("page.html"));
        assert!(!is_relative("https://example.com/home"));
    }

    #[test]
    fn clip_counts_characters_not_bytes() {
        assert_eq!(clip("Привет, мир", 6), "Привет…");
        assert_eq!(clip("коротко", 20), "коротко");
        assert_eq!(examples(&["a", "b", "c", "d"], 2), "a, b и ещё 2");
    }
}
