//! What a crawl knows about one URL: its HTTP response and the facts read
//! from its HTML. The same `PageFacts` comes out of the raw response body and
//! out of the rendered DOM, so checks run on either and the JavaScript tab is a
//! diff of the two.

mod custom;
mod extract;
mod microdata;
mod similarity;
mod stream;

pub use custom::{CustomFacts, CustomRules, ExtractorSpec, Output, SearchScope, SearchSpec};
pub use extract::{extract, extract_tree, extract_with};
pub use similarity::{similarity, SIGNATURE};
pub use stream::extract_stream;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What `extract_with` reads besides the `<head>` facts, which it always
/// reads. Each part switched off saves its pass over the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ExtractOptions {
    /// `<a href>` and `<area href>` with anchor text, rel and position.
    pub links: bool,
    /// Images, scripts, stylesheets, iframes, audio and video.
    pub resources: bool,
    /// h1-h6.
    pub headings: bool,
    /// Visible text: word count and text hash.
    pub text: bool,
    /// JSON-LD blocks and microdata items.
    pub structured_data: bool,
    pub forms: bool,
    /// Element count and duplicate ids.
    pub dom_stats: bool,
    /// A MinHash signature of the main content, for near duplicates
    /// (about 0.5 KB a page stored).
    pub similarity: bool,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        ExtractOptions {
            links: true,
            resources: true,
            headings: true,
            text: true,
            structured_data: true,
            forms: true,
            dom_stats: true,
            similarity: true,
        }
    }
}

impl ExtractOptions {
    /// Only the `<head>` facts.
    pub fn head_only() -> Self {
        ExtractOptions {
            links: false,
            resources: false,
            headings: false,
            text: false,
            structured_data: false,
            forms: false,
            dom_stats: false,
            similarity: false,
        }
    }
}

/// What a render left besides the DOM, for the JavaScript checks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderSummary {
    /// The render failed (navigation error or timeout): no rendered DOM.
    pub error: Option<String>,
    /// The content went quiet before the settle budget ran out.
    pub settled: bool,
    pub console_errors: usize,
    pub first_console_error: Option<String>,
    /// Requests the page made that got an error status or no response.
    pub failed_requests: usize,
    pub first_failed_request: Option<String>,
}

/// Which document the facts were read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// The HTTP response body, before any script ran.
    Raw,
    /// The DOM after scripts ran and the content settled.
    Rendered,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Raw => "raw",
            Source::Rendered => "rendered",
        }
    }
}

/// One HTTP response. A redirect is its own response, so a chain is a series
/// of these linked by `location`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HttpInfo {
    pub url: String,
    /// 0 when the request failed before any response (see `error`).
    pub status: u16,
    /// Lower-cased header names, in response order; repeated headers repeat.
    pub headers: Vec<(String, String)>,
    /// The `Location` of a redirect, resolved against `url`.
    pub location: Option<String>,
    pub content_type: Option<String>,
    /// Time to the response headers.
    pub ttfb_ms: u32,
    /// Body size in bytes as received.
    pub size: u64,
    pub error: Option<String>,
}

impl HttpInfo {
    /// All values of a header, case-insensitively.
    pub fn header_values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.headers
            .iter()
            .filter(move |(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The media type without parameters, lower-cased (`text/html`).
    pub fn media_type(&self) -> Option<String> {
        self.content_type
            .as_deref()
            .map(|value| value.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
    }

    pub fn is_html(&self) -> bool {
        matches!(self.media_type().as_deref(), Some("text/html" | "application/xhtml+xml"))
    }

    pub fn is_redirect(&self) -> bool {
        (300..400).contains(&self.status) && self.location.is_some()
    }
}

/// Facts read from one HTML document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageFacts {
    pub source: Source,
    /// What was read besides the `<head>`; a part switched off leaves its
    /// fields empty, which checks must not read as "absent".
    #[serde(default)]
    pub extracted: ExtractOptions,
    /// `<html lang>`.
    pub lang: Option<String>,
    /// Every `<title>` in `<head>`, trimmed.
    pub titles: Vec<String>,
    /// `<title>` elements outside `<head>`.
    pub titles_outside_head: usize,
    /// Every `<meta name=description>` in `<head>`.
    pub descriptions: Vec<String>,
    /// `<meta>` elements inside `<body>` (browsers ignore most of them).
    pub metas_in_body: usize,
    /// `content` of `<meta name=robots>` and `<meta name=googlebot>`, lower-cased.
    pub meta_robots: Vec<String>,
    /// `href` of `<link rel=canonical>` in `<head>`, resolved.
    pub canonicals: Vec<String>,
    /// Canonical hrefs as written, to tell relative ones apart.
    pub canonicals_raw: Vec<String>,
    /// `<link rel=canonical>` outside `<head>`.
    pub canonicals_outside_head: usize,
    pub hreflangs: Vec<Hreflang>,
    /// `<link rel=next|prev|previous>` in `<head>`, resolved.
    pub pagination: Vec<Pagination>,
    /// Headings in document order: (level 1-6, text).
    pub headings: Vec<(u8, String)>,
    pub links: Vec<Link>,
    pub images: Vec<Image>,
    /// `<picture>` elements without an `<img>` fallback.
    pub pictures_without_img: usize,
    pub scripts: Vec<String>,
    pub stylesheets: Vec<String>,
    pub iframes: Vec<String>,
    pub audios: Vec<String>,
    pub videos: Vec<String>,
    /// `<meta http-equiv=refresh>`: (delay seconds, target URL resolved).
    pub meta_refresh: Option<(u32, Option<String>)>,
    /// `<meta name=viewport content>`.
    pub viewport: Option<String>,
    /// `<meta http-equiv=Content-Security-Policy>` present in `<head>`.
    pub csp_meta: bool,
    pub forms: Vec<Form>,
    /// Ids used by more than one element.
    pub duplicate_ids: Vec<String>,
    /// Number of elements in the document.
    pub dom_elements: usize,
    /// Words of visible text (script, style, noscript and template excluded).
    pub word_count: usize,
    /// Hash of the normalised visible text, for exact-duplicate detection.
    pub text_hash: u64,
    /// MinHash signature of the main content (visible text outside header,
    /// nav, footer and aside), `SIGNATURE` values; empty without text.
    #[serde(default, skip_serializing_if = "Vec::is_empty", with = "similarity::hex")]
    pub minhash: Vec<u16>,
    /// `<script type=application/ld+json>` bodies.
    pub json_ld: Vec<String>,
    /// Microdata items (`itemscope` without `itemprop`), each as a JSON
    /// object shaped like JSON-LD.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub microdata: Vec<String>,
    /// Length of the serialised document in bytes.
    pub html_bytes: usize,
    /// What the crawl's custom extractors and searches found; filled in by
    /// the crawler, which holds the rules.
    #[serde(default, skip_serializing_if = "CustomFacts::is_empty")]
    pub custom: CustomFacts,
}

impl PageFacts {
    pub fn title(&self) -> Option<&str> {
        self.titles.first().map(String::as_str)
    }

    pub fn description(&self) -> Option<&str> {
        self.descriptions.first().map(String::as_str)
    }

    pub fn h1(&self) -> impl Iterator<Item = &str> {
        self.headings.iter().filter(|(level, _)| *level == 1).map(|(_, text)| text.as_str())
    }

    pub fn h2(&self) -> impl Iterator<Item = &str> {
        self.headings.iter().filter(|(level, _)| *level == 2).map(|(_, text)| text.as_str())
    }

    /// Robots directives from meta tags, split into tokens (`noindex`, ...).
    pub fn robots_tokens(&self) -> impl Iterator<Item = &str> {
        self.meta_robots
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hreflang {
    /// The `hreflang` value as written (`en-gb`, `x-default`).
    pub lang: String,
    /// Resolved `href`.
    pub href: String,
    /// `href` as written.
    pub href_raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pagination {
    /// `next` or `prev` (`previous` is folded into `prev`).
    pub rel: String,
    pub href: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    /// Resolved absolute URL without the fragment; empty if unresolvable.
    pub href: String,
    /// `href` as written.
    pub href_raw: String,
    /// Visible text, whitespace-collapsed; the `alt` of an image link if no text.
    pub anchor: String,
    /// `rel` tokens, lower-cased.
    pub rel: Vec<String>,
    pub nofollow: bool,
    pub sponsored: bool,
    pub ugc: bool,
    /// `target=_blank` without `noopener`/`noreferrer`.
    pub unsafe_target_blank: bool,
    /// Where the link sits: header, nav, footer, aside or content.
    pub position: LinkPosition,
    /// The link wraps an `<img>`.
    pub image: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkPosition {
    Header,
    Nav,
    Footer,
    Aside,
    Content,
}

impl LinkPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkPosition::Header => "header",
            LinkPosition::Nav => "nav",
            LinkPosition::Footer => "footer",
            LinkPosition::Aside => "aside",
            LinkPosition::Content => "content",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// Resolved `src` (or the first `srcset` candidate).
    pub src: String,
    /// `None` when the attribute is missing, `Some("")` when it is empty.
    pub alt: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Form {
    /// Resolved `action`; the page URL when absent.
    pub action: String,
    pub method: String,
}
