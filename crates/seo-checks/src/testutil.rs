//! Builders for the tests: a document that passes every check by default and
//! can be spoiled one thing at a time, and the response around it.

use seo_page::{extract, HttpInfo, PageFacts, Source};
use url::Url;

use crate::{check_page, Issue, PageInput, SiteLink, SitePage};

pub(crate) const URL: &str = "https://example.com/page";

/// An HTML document that has everything the checks look for. Each method
/// changes one part of it.
#[derive(Clone)]
pub(crate) struct Doc {
    lang: Option<String>,
    title: Option<String>,
    description: Option<String>,
    viewport: bool,
    h1: Option<String>,
    link: bool,
    words: usize,
    head: String,
    body: String,
}

impl Doc {
    pub fn good() -> Self {
        Doc {
            lang: Some("en".into()),
            title: Some("A reasonably descriptive page title".into()),
            description: Some(
                "A description of the test page, written long enough to sit between the lower and the upper limit of a snippet."
                    .into(),
            ),
            viewport: true,
            h1: Some("The main heading".into()),
            link: true,
            words: 250,
            head: String::new(),
            body: String::new(),
        }
    }

    pub fn lang(mut self, lang: Option<&str>) -> Self {
        self.lang = lang.map(str::to_string);
        self
    }

    pub fn title(mut self, title: &str) -> Self {
        self.title = Some(title.to_string());
        self
    }

    pub fn no_title(mut self) -> Self {
        self.title = None;
        self
    }

    pub fn description(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    pub fn no_description(mut self) -> Self {
        self.description = None;
        self
    }

    pub fn no_viewport(mut self) -> Self {
        self.viewport = false;
        self
    }

    pub fn h1(mut self, h1: Option<&str>) -> Self {
        self.h1 = h1.map(str::to_string);
        self
    }

    pub fn no_link(mut self) -> Self {
        self.link = false;
        self
    }

    pub fn words(mut self, words: usize) -> Self {
        self.words = words;
        self
    }

    /// Extra markup at the end of `<head>`.
    pub fn head(mut self, markup: &str) -> Self {
        self.head.push_str(markup);
        self
    }

    /// Extra markup at the end of `<body>`.
    pub fn body(mut self, markup: &str) -> Self {
        self.body.push_str(markup);
        self
    }

    pub fn html(&self) -> String {
        let lang = self.lang.as_ref().map(|lang| format!(" lang=\"{lang}\"")).unwrap_or_default();
        let title = self.title.as_ref().map(|title| format!("<title>{title}</title>")).unwrap_or_default();
        let description = self
            .description
            .as_ref()
            .map(|text| format!("<meta name=\"description\" content=\"{text}\">"))
            .unwrap_or_default();
        let viewport = if self.viewport {
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
        } else {
            ""
        };
        let h1 = self.h1.as_ref().map(|text| format!("<h1>{text}</h1>")).unwrap_or_default();
        let link = if self.link { "<a href=\"/other\">another page</a>" } else { "" };
        let text = vec!["word"; self.words].join(" ");
        format!(
            "<!doctype html><html{lang}><head>{title}{description}{viewport}{}</head><body>{h1}<p>{text}</p>{link}{}</body></html>",
            self.head, self.body
        )
    }

    pub fn facts(&self, url: &str, source: Source) -> PageFacts {
        extract(&self.html(), &Url::parse(url).unwrap(), source)
    }

    pub fn page(&self, url: &str) -> Fixture {
        Fixture::new(url, Some(self.facts(url, Source::Raw)))
    }
}

/// One URL with its response and facts, ready to be checked.
pub(crate) struct Fixture {
    pub url: Url,
    pub http: HttpInfo,
    pub facts: Option<PageFacts>,
    pub raw: Option<PageFacts>,
    pub rendered: Option<PageFacts>,
    pub render: Option<seo_page::RenderSummary>,
    pub depth: u32,
    pub in_sitemap: bool,
    pub blocked_by_robots: bool,
}

impl Fixture {
    /// A 200 text/html response with the security headers a good site sends.
    fn new(url: &str, facts: Option<PageFacts>) -> Self {
        let http = HttpInfo {
            url: url.to_string(),
            status: 200,
            headers: [
                ("strict-transport-security", "max-age=31536000; includeSubDomains"),
                ("content-security-policy", "default-src 'self'"),
                ("x-content-type-options", "nosniff"),
            ]
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
            content_type: Some("text/html; charset=utf-8".into()),
            ttfb_ms: 120,
            size: 20_000,
            ..HttpInfo::default()
        };
        Fixture {
            url: Url::parse(url).unwrap(),
            http,
            raw: facts.clone(),
            facts,
            rendered: None,
            render: None,
            depth: 1,
            in_sitemap: false,
            blocked_by_robots: false,
        }
    }

    /// A response that has no document (an image, a PDF, an error).
    pub fn resource(url: &str, status: u16, content_type: Option<&str>) -> Self {
        let mut fixture = Fixture::new(url, None);
        fixture.http.status = status;
        fixture.http.content_type = content_type.map(str::to_string);
        fixture.raw = None;
        fixture
    }

    pub fn good() -> Self {
        Doc::good().page(URL)
    }

    pub fn status(mut self, status: u16) -> Self {
        self.http.status = status;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.http.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn no_header(mut self, name: &str) -> Self {
        self.http.headers.retain(|(have, _)| have != name);
        self
    }

    pub fn error(mut self, text: &str) -> Self {
        self.http.error = Some(text.to_string());
        self
    }

    pub fn ttfb(mut self, ms: u32) -> Self {
        self.http.ttfb_ms = ms;
        self
    }

    pub fn size(mut self, bytes: u64) -> Self {
        self.http.size = bytes;
        self
    }

    pub fn depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }

    pub fn in_sitemap(mut self) -> Self {
        self.in_sitemap = true;
        self
    }

    pub fn blocked(mut self) -> Self {
        self.blocked_by_robots = true;
        self
    }

    /// The page after scripts ran: the current facts become the raw ones and
    /// the DOM in `rendered` becomes both the rendered and the effective ones.
    pub fn rendered(mut self, rendered: &Doc) -> Self {
        let facts = rendered.facts(self.url.as_str(), Source::Rendered);
        self.raw = self.facts.take();
        self.rendered = Some(facts.clone());
        self.facts = Some(facts);
        self
    }

    pub fn input(&self) -> PageInput<'_> {
        PageInput {
            url: &self.url,
            http: &self.http,
            facts: self.facts.as_ref(),
            raw: self.raw.as_ref(),
            rendered: self.rendered.as_ref(),
            render: self.render.as_ref(),
            depth: self.depth,
            in_sitemap: self.in_sitemap,
            blocked_by_robots: self.blocked_by_robots,
        }
    }

    pub fn issues(&self) -> Vec<Issue> {
        check_page(&self.input())
    }

    pub fn codes(&self) -> Vec<&'static str> {
        self.issues().iter().map(|issue| issue.code).collect()
    }

    pub fn has(&self, code: &str) -> bool {
        self.codes().contains(&code)
    }

    pub fn detail(&self, code: &str) -> Option<String> {
        self.issues().into_iter().find(|issue| issue.code == code).and_then(|issue| issue.detail)
    }

    pub fn site_page(&self) -> SitePage {
        SitePage::from_input(&self.input())
    }
}

/// A healthy 200 HTML page for the cross-page tests; change fields as needed.
pub(crate) fn site(url: &str) -> SitePage {
    SitePage {
        url: url.to_string(),
        status: 200,
        location: None,
        html: true,
        lang: Some("en".into()),
        title: None,
        description: None,
        h1: None,
        canonical: None,
        hreflangs: Vec::new(),
        text_hash: None,
        noindex: false,
        blocked_by_robots: false,
        depth: 1,
        in_sitemap: false,
        external: false,
        links: Vec::new(),
    }
}

pub(crate) fn redirect(url: &str, to: &str) -> SitePage {
    SitePage { status: 301, location: Some(to.to_string()), html: false, ..site(url) }
}

pub(crate) fn link(target: &str) -> SiteLink {
    SiteLink { target: target.to_string(), nofollow: false, internal: true }
}

pub(crate) fn nofollow(target: &str) -> SiteLink {
    SiteLink { nofollow: true, ..link(target) }
}

/// The codes reported for the page at `position`.
pub(crate) fn codes_at(found: &[(usize, Issue)], position: usize) -> Vec<&'static str> {
    found.iter().filter(|(at, _)| *at == position).map(|(_, issue)| issue.code).collect()
}
