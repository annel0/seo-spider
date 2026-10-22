//! Cross-page checks: duplicates, redirect chains, canonical and hreflang
//! targets, links to broken pages, orphans. They run once, after the crawl, over
//! a `SitePage` per URL.

mod canonicals;
mod duplicates;
mod linking;
mod near_duplicates;
mod redirects;

use std::collections::HashMap;
use std::sync::Arc;

use seo_page::{HttpInfo, PageFacts};
use url::Url;

use crate::headers;
use crate::page::PageInput;
use crate::{urls, Issue};

/// One hreflang annotation: the language it names and the address it points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteHreflang {
    pub lang: String,
    /// Resolved, no fragment.
    pub href: String,
}

/// One outgoing link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteLink {
    /// Resolved, no fragment. Shared: a site's pages link to the same URLs
    /// over and over, and a copy per link was most of what the site checks
    /// held in memory.
    pub target: Arc<str>,
    /// `rel=nofollow` on the link, or the whole page is nofollow.
    pub nofollow: bool,
    /// Leads to the same site (`www.` ignored). The rest are external links,
    /// which are only judged by the status of their target.
    pub internal: bool,
}

/// What the cross-page checks need to know about one URL.
#[derive(Debug, Clone)]
pub struct SitePage {
    /// Resolved, no fragment; how other pages refer to this one.
    pub url: String,
    /// 0 when no response came back.
    pub status: u16,
    /// Where the page sends the visitor: the `Location` of a redirect, or the
    /// target of a meta refresh on a 2xx page.
    pub location: Option<String>,
    /// The URL serves an HTML document.
    pub html: bool,
    pub lang: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    /// The first non-empty `<h1>`.
    pub h1: Option<String>,
    /// The canonical the page declares (tag, else `Link` header), resolved.
    pub canonical: Option<String>,
    pub hreflangs: Vec<SiteHreflang>,
    /// Hash of the visible text; `None` when the page has no text.
    pub text_hash: Option<u64>,
    /// MinHash signature of the main content, for near duplicates.
    pub minhash: Option<Vec<u16>>,
    /// `noindex` in a meta tag or `X-Robots-Tag`.
    pub noindex: bool,
    pub blocked_by_robots: bool,
    /// Clicks from the start URL. The checks do not use it; it travels with the
    /// page so one record serves both kinds of checks.
    pub depth: u32,
    pub in_sitemap: bool,
    /// The page was fetched only to learn the status of a link to another
    /// site. It is a target for link checks but is not checked itself.
    pub external: bool,
    pub links: Vec<SiteLink>,
}

/// Builds the record of a page from what the crawler has just fetched. `facts`
/// are the effective ones (rendered if the page was rendered).
/// `blocked_by_robots` and `external` start false; set them with the
/// `with_*` methods.
pub fn site_page(
    url: &Url,
    http: &HttpInfo,
    facts: Option<&PageFacts>,
    depth: u32,
    in_sitemap: bool,
) -> SitePage {
    let robots = headers::robots(http, facts);
    let site = urls::site_key(url);
    let page_nofollow = robots.nofollow();
    let success = (200..300).contains(&http.status);

    let location = if http.is_redirect() {
        http.location.as_deref().map(urls::normalize)
    } else if success {
        facts
            .and_then(|facts| facts.meta_refresh.as_ref())
            .and_then(|(_, target)| target.as_deref())
            .map(urls::normalize)
    } else {
        None
    };

    let non_empty = |text: &str| (!text.is_empty()).then(|| text.to_string());

    SitePage {
        url: urls::normalize(url.as_str()),
        status: http.status,
        location,
        html: facts.is_some() || http.is_html(),
        lang: headers::language(http, facts),
        title: facts.and_then(|facts| facts.title()).and_then(non_empty),
        description: facts.and_then(|facts| facts.description()).and_then(non_empty),
        h1: facts.and_then(|facts| facts.h1().find(|text| !text.is_empty())).and_then(non_empty),
        canonical: headers::effective_canonical(http, facts, url),
        hreflangs: headers::hreflangs(http, facts, url)
            .into_iter()
            .filter(|item| !item.href.is_empty())
            .map(|item| SiteHreflang { lang: item.lang, href: item.href })
            .collect(),
        text_hash: facts.filter(|facts| facts.word_count > 0).map(|facts| facts.text_hash),
        minhash: facts.filter(|facts| !facts.minhash.is_empty()).map(|facts| facts.minhash.clone()),
        noindex: robots.noindex(),
        blocked_by_robots: false,
        depth,
        in_sitemap,
        external: false,
        links: facts
            .map(|facts| {
                facts
                    .links
                    .iter()
                    .filter(|link| !link.href.is_empty())
                    .map(|link| SiteLink {
                        target: Arc::from(link.href.as_str()),
                        nofollow: link.nofollow || page_nofollow,
                        internal: urls::is_internal(&site, &link.href),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

impl SitePage {
    /// The same as `site_page`, from the input already built for `check_page`
    /// (so `blocked_by_robots` comes along).
    pub fn from_input(input: &PageInput) -> SitePage {
        site_page(input.url, input.http, input.facts, input.depth, input.in_sitemap)
            .with_blocked_by_robots(input.blocked_by_robots)
    }

    pub fn with_blocked_by_robots(mut self, blocked: bool) -> Self {
        self.blocked_by_robots = blocked;
        self
    }

    pub fn with_external(mut self, external: bool) -> Self {
        self.external = external;
        self
    }

    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub(crate) fn is_redirect(&self) -> bool {
        (300..400).contains(&self.status)
    }

    /// 4xx, 5xx, or no response for a URL that was actually requested.
    pub(crate) fn is_error(&self) -> bool {
        self.status >= 400 || (self.status == 0 && !self.blocked_by_robots)
    }

    /// Declares no canonical, or declares itself.
    pub(crate) fn is_self_canonical(&self) -> bool {
        self.canonical.as_deref().is_none_or(|canonical| urls::same_url(canonical, &self.url))
    }

    /// Declares a different canonical.
    pub(crate) fn is_non_canonical(&self) -> bool {
        !self.is_self_canonical()
    }

    /// `noindex` or kept out of reach by robots.txt.
    pub(crate) fn is_closed_to_index(&self) -> bool {
        self.noindex || self.blocked_by_robots
    }

    /// A page search engines can put in the index as itself.
    pub(crate) fn is_indexable(&self) -> bool {
        self.is_success() && self.html && !self.is_closed_to_index() && self.is_self_canonical()
    }
}

/// Settings of the cross-page checks.
#[derive(Debug, Clone, Copy)]
pub struct SiteOptions {
    /// Near duplicates: the estimated share of main-content shingles two
    /// pages have in common, at or above which both are reported.
    pub near_duplicate_threshold: f64,
}

impl Default for SiteOptions {
    fn default() -> Self {
        SiteOptions { near_duplicate_threshold: 0.9 }
    }
}

/// Runs every cross-page check with the default settings.
pub fn check_site(pages: &[SitePage]) -> Vec<(usize, Issue)> {
    check_site_with(pages, &SiteOptions::default())
}

/// Runs every cross-page check. Returns the index into `pages` and the issue,
/// ordered by page and, within a page, by check. Pages marked `external` are
/// only looked at as link targets.
pub fn check_site_with(pages: &[SitePage], options: &SiteOptions) -> Vec<(usize, Issue)> {
    let index = Index::new(pages);
    let mut found = Vec::new();
    redirects::check(&index, &mut found);
    duplicates::check(&index, &mut found);
    near_duplicates::check(&index, options.near_duplicate_threshold, &mut found);
    canonicals::check(&index, &mut found);
    linking::check(&index, &mut found);
    // Stable: the order the checks ran in survives within a page.
    found.sort_by_key(|(page, _)| *page);
    found
}

/// The pages by URL, so a link target is found without scanning.
pub(crate) struct Index<'a> {
    pub pages: &'a [SitePage],
    by_url: HashMap<String, usize>,
}

impl<'a> Index<'a> {
    fn new(pages: &'a [SitePage]) -> Self {
        let mut by_url = HashMap::with_capacity(pages.len());
        for (position, page) in pages.iter().enumerate() {
            by_url.entry(urls::normalize(&page.url)).or_insert(position);
        }
        Index { pages, by_url }
    }

    /// The position of the page at `url`. A fragment or a missing path slash
    /// does not matter.
    pub fn position(&self, url: &str) -> Option<usize> {
        self.by_url.get(url).or_else(|| self.by_url.get(&urls::normalize(url))).copied()
    }

    pub fn get(&self, url: &str) -> Option<&'a SitePage> {
        self.position(url).map(|position| &self.pages[position])
    }

    /// Pages that take part in checks as themselves.
    pub fn own_pages(&self) -> impl Iterator<Item = (usize, &'a SitePage)> {
        self.pages.iter().enumerate().filter(|(_, page)| !page.external)
    }
}

pub(crate) type Found = Vec<(usize, Issue)>;

/// Adds `url` to a list of examples unless it is already there.
fn push_unique(list: &mut Vec<String>, url: &str) {
    if !list.iter().any(|have| have == url) {
        list.push(url.to_string());
    }
}

#[cfg(test)]
mod tests;
