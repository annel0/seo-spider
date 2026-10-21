//! Check catalog and the checks themselves: per-page issues, cross-page issues
//! and the JavaScript tab (raw HTML against rendered DOM).
//!
//! Page checks and their thresholds are ported from SEOnaut (MIT); the
//! catalog keeps its priorities as `Severity` and groups checks into tabs the
//! way Screaming Frog does.
//!
//! The crate does no I/O. The crawler builds a `PageInput` per URL and calls
//! `check_page`; once the crawl is over it builds a `SitePage` per URL (see
//! `site_page`) and calls `check_site`.

mod catalog;
mod headers;
mod javascript;
mod lang;
mod page;
mod site;
#[cfg(test)]
mod testutil;
mod urls;

pub use catalog::{find, CATALOG};
pub use page::{check_page, PageInput};
pub use site::{check_site, site_page, SiteHreflang, SiteLink, SitePage};

use serde::Serialize;

/// How much an issue matters. SEOnaut priority 1 is `High`, 2 `Medium`, 3 `Low`.
/// The order is the order of importance, so sorting puts `High` first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    High,
    Medium,
    Low,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::High => "high",
            Severity::Medium => "medium",
            Severity::Low => "low",
        }
    }
}

/// One kind of issue the crawler can report.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct CheckDef {
    /// Stable snake_case identifier; what gets stored and exported.
    pub code: &'static str,
    pub severity: Severity,
    /// The tab the issue belongs to: `response_codes`, `page_titles`,
    /// `meta_description`, `headings`, `content`, `indexability`, `canonicals`,
    /// `hreflang`, `links`, `images`, `url`, `security`, `mobile`, `javascript`.
    pub category: &'static str,
    /// Short name, in Russian.
    pub title: &'static str,
    /// What is wrong and why it matters, in Russian.
    pub description: &'static str,
}

/// One issue found on one page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// A `CheckDef::code` from `CATALOG`.
    pub code: &'static str,
    /// What exactly was found (a length, a URL, a count), in Russian.
    pub detail: Option<String>,
}

impl Issue {
    pub fn new(code: &'static str) -> Self {
        debug_assert!(catalog::find(code).is_some(), "unknown check code `{code}`");
        Issue { code, detail: None }
    }

    pub fn with_detail(code: &'static str, detail: impl Into<String>) -> Self {
        let mut issue = Issue::new(code);
        issue.detail = Some(detail.into());
        issue
    }
}
