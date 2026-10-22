//! Per-page checks: everything that can be decided from one URL's response and
//! its document. Checks that compare pages live in `site`.

mod indexing;
mod links;
mod meta;
mod response;
mod security;

use seo_page::{HttpInfo, PageFacts};
use url::Url;

use crate::headers::{self, Robots};
use crate::{javascript, urls, Issue};

/// What the crawler knows about one URL when it asks for its issues.
pub struct PageInput<'a> {
    pub url: &'a Url,
    pub http: &'a HttpInfo,
    /// The facts the checks run on: the rendered document if the page was
    /// rendered, otherwise the raw one. `None` for anything that is not HTML.
    pub facts: Option<&'a PageFacts>,
    /// Facts read from the response body before scripts ran.
    pub raw: Option<&'a PageFacts>,
    /// Facts read from the DOM after scripts ran. With `raw` it feeds the
    /// JavaScript tab; the tab is skipped unless both are present.
    pub rendered: Option<&'a PageFacts>,
    /// What the render left besides the DOM, when the page was rendered.
    pub render: Option<&'a seo_page::RenderSummary>,
    /// Clicks from the start URL.
    pub depth: u32,
    pub in_sitemap: bool,
    pub blocked_by_robots: bool,
}

/// Runs every per-page check. The order of the result follows the tabs of the
/// catalog; an issue appears at most once per code.
pub fn check_page(input: &PageInput) -> Vec<Issue> {
    let ctx = Ctx::new(input);
    let mut issues = Vec::new();
    response::check(&ctx, &mut issues);
    meta::check(&ctx, &mut issues);
    indexing::check(&ctx, &mut issues);
    links::check(&ctx, &mut issues);
    security::check(&ctx, &mut issues);
    crate::structured::check(&ctx, &mut issues);
    javascript::check(input, &mut issues);
    issues
}

/// Everything the checks look up more than once, computed once per page.
pub(crate) struct Ctx<'a> {
    pub url: &'a Url,
    pub http: &'a HttpInfo,
    pub facts: Option<&'a PageFacts>,
    pub depth: u32,
    pub in_sitemap: bool,
    pub blocked_by_robots: bool,
    /// The response was 2xx.
    pub success: bool,
    /// Robots directives from the meta tags and `X-Robots-Tag`.
    pub robots: Robots,
    /// The host the page belongs to, for telling internal links from external.
    site: Option<String>,
}

impl<'a> Ctx<'a> {
    fn new(input: &'a PageInput<'a>) -> Self {
        Ctx {
            url: input.url,
            http: input.http,
            facts: input.facts,
            depth: input.depth,
            in_sitemap: input.in_sitemap,
            blocked_by_robots: input.blocked_by_robots,
            success: (200..300).contains(&input.http.status),
            robots: headers::robots(input.http, input.facts),
            site: urls::site_key(input.url),
        }
    }

    /// The document of a page that answered 2xx. Content checks run on this.
    pub fn doc(&self) -> Option<&'a PageFacts> {
        self.facts.filter(|_| self.success)
    }

    pub fn is_https(&self) -> bool {
        self.url.scheme() == "https"
    }

    pub fn is_internal(&self, href: &str) -> bool {
        urls::is_internal(&self.site, href)
    }

    /// The canonical the page declares, from the document or the `Link` header.
    pub fn canonical(&self) -> Option<String> {
        headers::effective_canonical(self.http, self.facts, self.url)
    }

    /// The page URL in the form hreflang and canonical values are compared in.
    pub fn own_url(&self) -> String {
        urls::normalize(self.url.as_str())
    }
}

#[cfg(test)]
mod tests {
    use crate::testutil::Fixture;

    #[test]
    fn healthy_page_has_no_issues() {
        let page = Fixture::good();
        assert_eq!(page.codes(), Vec::<&str>::new());
    }
}
