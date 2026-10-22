//! HTML → `PageFacts` in one streaming pass (lol_html), without building a
//! DOM. The facts are the same as `extract`'s (the tests compare both on real
//! pages); ancestry that `extract` reads from the tree — in `<head>`, inside
//! a link, under a `<nav>` — is tracked here from start and end tags.

use std::cell::RefCell;

use lol_html::html_content::{Element, EndTag, TextChunk};
use lol_html::{doc_text, element, HtmlRewriter, Settings};
use rustc_hash::FxHashMap;
use url::Url;
use xxhash_rust::xxh3::Xxh3;

use crate::{ExtractOptions, Form, Hreflang, Image, Link, LinkPosition, PageFacts, Pagination, Source};

/// A link being read: text arrives between its start and end tags.
struct OpenLink {
    href_raw: String,
    rel: Vec<String>,
    aria_label: Option<String>,
    target_blank: bool,
    position: LinkPosition,
    text: String,
    image_alt: Option<String>,
    image: bool,
}

/// An `<img>` as written: src, alt, width, height.
type RawImage = (String, Option<String>, Option<String>, Option<String>);

#[derive(Default)]
struct State {
    base: Option<String>,
    lang: Option<String>,
    head_depth: u32,
    /// An explicit `</head>` or `<body>` was read.
    head_closed: bool,
    body_seen: bool,
    /// Open `<audio>`/`<video>` elements, for their `<source>` children.
    media: Vec<bool>,
    svg_depth: u32,
    /// Inside script, style, noscript or template: not visible text.
    hidden_depth: u32,
    landmarks: Vec<LinkPosition>,

    titles: Vec<String>,
    titles_outside_head: usize,
    title: Option<String>,
    descriptions: Vec<String>,
    metas_in_body: usize,
    meta_robots: Vec<String>,
    viewport: Option<String>,
    refresh: Option<String>,
    csp_meta: bool,
    canonicals_raw: Vec<String>,
    canonicals_outside_head: usize,
    stylesheets_raw: Vec<String>,
    hreflangs_raw: Vec<(String, String)>,
    pagination_raw: Vec<(String, String)>,

    heading: Option<(u8, String)>,
    headings: Vec<(u8, String)>,
    link: Option<OpenLink>,
    links: Vec<OpenLink>,
    images_raw: Vec<RawImage>,
    pictures: Vec<bool>,
    pictures_without_img: usize,
    scripts_raw: Vec<String>,
    json_ld: Option<String>,
    json_lds: Vec<String>,
    iframes_raw: Vec<String>,
    audios_raw: Vec<String>,
    videos_raw: Vec<String>,
    forms_raw: Vec<(Option<String>, String)>,
    ids: FxHashMap<String, usize>,
    dom_elements: usize,
    /// Explicit <html>, <head>, <body>: a parser creates the missing ones.
    structure: [bool; 3],

    words: usize,
    /// The text node being read: references are resolved, and words split,
    /// once it is whole.
    node_text: String,
    text_hash: Option<Xxh3>,
}

impl State {
    /// In `<head>`, explicit or (before any body content) implied, as an
    /// HTML parser places it.
    fn in_head(&self) -> bool {
        self.head_depth > 0 || (!self.body_seen && !self.head_closed)
    }

    fn visible(&self) -> bool {
        self.hidden_depth == 0 && self.body_seen && self.head_depth == 0 && self.title.is_none()
    }

    fn word(&mut self, word: &str) {
        if word.is_empty() {
            return;
        }
        let hash = self.text_hash.get_or_insert_with(Xxh3::new);
        if self.words > 0 {
            hash.update(b" ");
        }
        hash.update(word.as_bytes());
        self.words += 1;
    }

    fn close_link(&mut self) {
        if let Some(link) = self.link.take() {
            self.links.push(link);
        }
    }
}

/// An attribute value with character references resolved: lol_html hands
/// over the source text, an HTML parser the value.
fn attr(element: &Element, name: &str) -> Option<String> {
    let value = element.get_attribute(name)?;
    Some(if value.contains('&') { htmlize::unescape_attribute(&value).into_owned() } else { value })
}

/// Text with character references resolved.
fn decode(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('&') {
        std::borrow::Cow::Owned(htmlize::unescape(text).into_owned())
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn rel_tokens(rel: Option<String>) -> Vec<String> {
    rel.unwrap_or_default().split_ascii_whitespace().map(str::to_ascii_lowercase).collect()
}

fn landmark(element: &Element) -> Option<LinkPosition> {
    let role = attr(element, "role").unwrap_or_default();
    match (element.tag_name().as_str(), role.as_str()) {
        ("nav", _) | (_, "navigation") => Some(LinkPosition::Nav),
        ("header", _) | (_, "banner") => Some(LinkPosition::Header),
        ("footer", _) | (_, "contentinfo") => Some(LinkPosition::Footer),
        ("aside", _) | (_, "complementary") => Some(LinkPosition::Aside),
        _ => None,
    }
}

/// Runs `on_end` when the element's end tag is read; elements without one
/// (void, or closed implicitly) never call it, which callers allow for.
fn on_end(element: &mut Element, on_end: impl FnOnce() + 'static) {
    if let Some(handlers) = element.end_tag_handlers() {
        handlers.push(Box::new(move |_: &mut EndTag| {
            on_end();
            Ok(())
        }));
    }
}

/// `extract_with` in one streaming pass.
pub fn extract_stream(html: &str, url: &Url, source: Source, options: &ExtractOptions) -> PageFacts {
    let state = std::rc::Rc::new(RefCell::new(State::default()));
    let opts = *options;
    {
        let s = &state;
        let mut handlers = vec![
            element!("*", |el| {
                let mut st = s.borrow_mut();
                st.dom_elements += 1;
                if opts.dom_stats {
                    if let Some(id) = attr(el, "id").filter(|id| !id.is_empty()) {
                        *st.ids.entry(id).or_default() += 1;
                    }
                }
                let name = el.tag_name().to_ascii_lowercase();
                // Content that cannot be in <head> starts the body, as an
                // HTML parser implies it when the tag is missing.
                if !st.body_seen
                    && st.head_depth == 0
                    && !matches!(
                        name.as_str(),
                        "html" | "head" | "title" | "meta" | "link" | "base" | "script" | "style" | "noscript" | "template"
                    )
                {
                    st.body_seen = true;
                    st.head_closed = true;
                }
                match name.as_str() {
                    "html" | "head" | "body" => {
                        let at = ["html", "head", "body"].iter().position(|tag| *tag == name).unwrap_or(0);
                        st.structure[at] = true;
                    }
                    _ => {}
                }
                match name.as_str() {
                    // A <head> after body content is ignored by a parser.
                    "head" if !st.body_seen => {
                        st.head_depth += 1;
                        drop(st);
                        let s = s.clone();
                        on_end(el, move || {
                            let mut st = s.borrow_mut();
                            st.head_depth = st.head_depth.saturating_sub(1);
                            st.head_closed = true;
                        });
                    }
                    "body" => {
                        st.body_seen = true;
                        // A body start closes an unclosed head.
                        st.head_depth = 0;
                        st.head_closed = true;
                    }
                    "html" => st.lang = attr(el, "lang").map(|lang| lang.trim().to_string()),
                    "base" => {
                        if st.base.is_none() {
                            st.base = attr(el, "href");
                        }
                    }
                    "svg" => {
                        st.svg_depth += 1;
                        drop(st);
                        let s = s.clone();
                        on_end(el, move || s.borrow_mut().svg_depth -= 1);
                    }
                    "script" | "style" | "noscript" | "template" => {
                        st.hidden_depth += 1;
                        let json_ld = name == "script"
                            && el
                                .get_attribute("type")
                                .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/ld+json"));
                        if name == "script" {
                            if json_ld && opts.structured_data {
                                st.json_ld = Some(String::new());
                            } else if !json_ld && opts.resources {
                                if let Some(src) = attr(el, "src") {
                                    st.scripts_raw.push(src);
                                }
                            }
                        }
                        drop(st);
                        let s = s.clone();
                        on_end(el, move || {
                            let mut st = s.borrow_mut();
                            st.hidden_depth = st.hidden_depth.saturating_sub(1);
                            if let Some(json) = st.json_ld.take() {
                                st.json_lds.push(json.trim().to_string());
                            }
                        });
                    }
                    "title" => {
                        if st.in_head() {
                            st.title = Some(String::new());
                            drop(st);
                            let s = s.clone();
                            on_end(el, move || {
                                let mut st = s.borrow_mut();
                                if let Some(title) = st.title.take() {
                                    st.titles.push(collapse(&title));
                                }
                            });
                        } else if st.svg_depth == 0 {
                            st.titles_outside_head += 1;
                        }
                    }
                    "meta" => {
                        if st.body_seen && !st.in_head() {
                            st.metas_in_body += 1;
                        }
                        let content = attr(el, "content").unwrap_or_default();
                        let content = content.trim();
                        if let Some(meta_name) = attr(el, "name") {
                            if meta_name.eq_ignore_ascii_case("description") && st.in_head() {
                                st.descriptions.push(collapse(content));
                            } else if meta_name.eq_ignore_ascii_case("robots")
                                || meta_name.eq_ignore_ascii_case("googlebot")
                            {
                                st.meta_robots.push(content.to_ascii_lowercase());
                            } else if meta_name.eq_ignore_ascii_case("viewport") && st.viewport.is_none() {
                                st.viewport = Some(content.to_string());
                            }
                        }
                        if let Some(equiv) = attr(el, "http-equiv") {
                            if equiv.eq_ignore_ascii_case("refresh") && st.refresh.is_none() {
                                st.refresh = Some(content.to_string());
                            } else if equiv.eq_ignore_ascii_case("content-security-policy") && st.in_head() {
                                st.csp_meta = true;
                            }
                        }
                    }
                    "link" => {
                        let rels = rel_tokens(attr(el, "rel"));
                        let Some(href) = attr(el, "href").map(|href| href.trim().to_string()) else {
                            return Ok(());
                        };
                        let in_head = st.in_head();
                        if rels.iter().any(|rel| rel == "canonical") {
                            if in_head {
                                st.canonicals_raw.push(href.clone());
                            } else {
                                st.canonicals_outside_head += 1;
                            }
                        }
                        if opts.resources && rels.iter().any(|rel| rel == "stylesheet") {
                            st.stylesheets_raw.push(href.clone());
                        }
                        if in_head {
                            if rels.iter().any(|rel| rel == "alternate") {
                                if let Some(lang) = attr(el, "hreflang") {
                                    st.hreflangs_raw.push((lang.trim().to_string(), href.clone()));
                                }
                            }
                            for rel in &rels {
                                let rel = match rel.as_str() {
                                    "next" => "next",
                                    "prev" | "previous" => "prev",
                                    _ => continue,
                                };
                                st.pagination_raw.push((rel.to_string(), href.clone()));
                            }
                        }
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if opts.headings => {
                        let level = name.as_bytes()[1] - b'0';
                        // An unclosed heading ends where the next begins.
                        if let Some(open) = st.heading.take() {
                            st.headings.push((open.0, collapse(&open.1)));
                        }
                        st.heading = Some((level, String::new()));
                        drop(st);
                        let s = s.clone();
                        on_end(el, move || {
                            let mut st = s.borrow_mut();
                            if let Some((level, text)) = st.heading.take() {
                                st.headings.push((level, collapse(&text)));
                            }
                        });
                    }
                    "a" | "area" if opts.links => {
                        let Some(href) = attr(el, "href") else { return Ok(()) };
                        // Links do not nest: a new one closes the open one.
                        st.close_link();
                        let rel = rel_tokens(attr(el, "rel"));
                        let target_blank = el
                            .get_attribute("target")
                            .is_some_and(|target| target.eq_ignore_ascii_case("_blank"));
                        st.link = Some(OpenLink {
                            href_raw: href.trim().to_string(),
                            rel,
                            aria_label: attr(el, "aria-label"),
                            target_blank,
                            position: st.landmarks.last().copied().unwrap_or(LinkPosition::Content),
                            text: String::new(),
                            image_alt: None,
                            image: false,
                        });
                        if name == "a" {
                            drop(st);
                            let s = s.clone();
                            on_end(el, move || s.borrow_mut().close_link());
                        } else {
                            // <area> is void: it has no text and ends at once.
                            st.close_link();
                        }
                    }
                    "img" => {
                        if let Some(link) = st.link.as_mut() {
                            link.image = true;
                            if link.image_alt.is_none() {
                                link.image_alt = attr(el, "alt").map(|alt| collapse(&alt));
                            }
                        }
                        if let Some(has_img) = st.pictures.last_mut() {
                            *has_img = true;
                        }
                        if opts.resources {
                            let src = attr(el, "src").filter(|src| !src.trim().is_empty()).or_else(|| {
                                attr(el, "srcset").and_then(|set| {
                                    set.split(',').next()?.split_whitespace().next().map(str::to_string)
                                })
                            });
                            if let Some(src) = src {
                                st.images_raw.push((
                                    src,
                                    attr(el, "alt").map(|alt| alt.trim().to_string()),
                                    attr(el, "width"),
                                    attr(el, "height"),
                                ));
                            }
                        }
                    }
                    "picture" if opts.resources => {
                        st.pictures.push(false);
                        drop(st);
                        let s = s.clone();
                        on_end(el, move || {
                            let mut st = s.borrow_mut();
                            if st.pictures.pop() == Some(false) {
                                st.pictures_without_img += 1;
                            }
                        });
                    }
                    "iframe" if opts.resources => {
                        if let Some(src) = attr(el, "src") {
                            st.iframes_raw.push(src);
                        }
                    }
                    "audio" | "video" if opts.resources => {
                        let video = name == "video";
                        if let Some(src) = attr(el, "src") {
                            if video {
                                st.videos_raw.push(src);
                            } else {
                                st.audios_raw.push(src);
                            }
                        }
                        st.media.push(video);
                        drop(st);
                        let s = s.clone();
                        on_end(el, move || {
                            s.borrow_mut().media.pop();
                        });
                    }
                    "source" if opts.resources => {
                        // A <source> counts for the innermost open media element.
                        if let (Some(&video), Some(src)) = (st.media.last(), attr(el, "src")) {
                            if video {
                                st.videos_raw.push(src);
                            } else {
                                st.audios_raw.push(src);
                            }
                        }
                    }
                    "form" if opts.forms => {
                        let action = attr(el, "action").map(|action| action.trim().to_string());
                        let method = attr(el, "method").unwrap_or_else(|| "get".into()).trim().to_ascii_lowercase();
                        st.forms_raw.push((action, method));
                    }
                    _ => {}
                }
                Ok(())
            }),
        ];
        // Landmarks for link positions: pushed here, popped at their end.
        handlers.push(element!("nav, header, footer, aside, [role]", |el| {
            if let Some(kind) = landmark(el) {
                s.borrow_mut().landmarks.push(kind);
                let s = s.clone();
                on_end(el, move || {
                    s.borrow_mut().landmarks.pop();
                });
            }
            Ok(())
        }));
        let document_handlers = vec![
            doc_text!(|chunk: &mut TextChunk| {
                let mut st = s.borrow_mut();
                st.node_text.push_str(chunk.as_str());
                if !chunk.last_in_text_node() {
                    return Ok(());
                }
                let raw = std::mem::take(&mut st.node_text);
                // Script content is raw text: no references to resolve.
                if let Some(json) = st.json_ld.as_mut() {
                    json.push_str(&raw);
                    return Ok(());
                }
                let text = decode(&raw);
                let text = text.as_ref();
                if let Some(title) = st.title.as_mut() {
                    title.push_str(text);
                    return Ok(());
                }
                // Text nodes of a link or heading join without a separator,
                // hidden ones included, as `extract` reads `element.text()`.
                if let Some(link) = st.link.as_mut() {
                    link.text.push_str(text);
                }
                if let Some((_, heading)) = st.heading.as_mut() {
                    heading.push_str(text);
                }
                if st.hidden_depth > 0 {
                    return Ok(());
                }
                if !st.body_seen && st.head_depth == 0 && !text.trim().is_empty() {
                    st.body_seen = true;
                    st.head_closed = true;
                }
                if opts.text && st.visible() {
                    for word in text.split_whitespace() {
                        st.word(word);
                    }
                }
                Ok(())
            }),
        ];
        let mut rewriter = HtmlRewriter::new(
            Settings {
                element_content_handlers: handlers,
                document_content_handlers: document_handlers,
                ..Settings::new()
            },
            |_: &[u8]| {},
        );
        let _ = rewriter.write(html.as_bytes());
        let _ = rewriter.end();
    }
    let mut st = std::rc::Rc::try_unwrap(state).ok().expect("handlers dropped").into_inner();
    st.close_link();
    if let Some((level, text)) = st.heading.take() {
        st.headings.push((level, collapse(&text)));
    }

    let base = st.base.as_deref().and_then(|href| url.join(href.trim()).ok()).unwrap_or_else(|| url.clone());
    let resolve = |raw: &str| -> Option<String> {
        let mut resolved = base.join(raw.trim()).ok()?;
        if !matches!(resolved.scheme(), "http" | "https") {
            return None;
        }
        resolved.set_fragment(None);
        Some(resolved.to_string())
    };
    let mut duplicate_ids: Vec<String> =
        st.ids.into_iter().filter(|(_, count)| *count > 1).map(|(id, _)| id).collect();
    duplicate_ids.sort();
    let text_hash = st.text_hash.map_or_else(|| xxhash_rust::xxh3::xxh3_64(b""), |hash| hash.digest());
    PageFacts {
        source,
        extracted: opts,
        lang: st.lang,
        titles: st.titles,
        titles_outside_head: st.titles_outside_head,
        descriptions: st.descriptions,
        metas_in_body: st.metas_in_body,
        meta_robots: st.meta_robots,
        canonicals: st.canonicals_raw.iter().map(|href| resolve(href).unwrap_or_default()).collect(),
        canonicals_raw: st.canonicals_raw,
        canonicals_outside_head: st.canonicals_outside_head,
        hreflangs: st
            .hreflangs_raw
            .into_iter()
            .map(|(lang, href_raw)| Hreflang { lang, href: resolve(&href_raw).unwrap_or_default(), href_raw })
            .collect(),
        pagination: st
            .pagination_raw
            .into_iter()
            .map(|(rel, href)| Pagination { rel, href: resolve(&href).unwrap_or_default() })
            .collect(),
        headings: st.headings,
        links: st
            .links
            .into_iter()
            .filter_map(|link| {
                let href = resolve(&link.href_raw)?;
                let mut anchor = collapse(&link.text);
                if anchor.is_empty() {
                    anchor = link.aria_label.as_deref().map(collapse).or(link.image_alt.clone()).unwrap_or_default();
                }
                Some(Link {
                    href,
                    nofollow: link.rel.iter().any(|token| token == "nofollow"),
                    sponsored: link.rel.iter().any(|token| token == "sponsored"),
                    ugc: link.rel.iter().any(|token| token == "ugc"),
                    unsafe_target_blank: link.target_blank
                        && !link.rel.iter().any(|token| token == "noopener" || token == "noreferrer"),
                    position: link.position,
                    image: link.image,
                    href_raw: link.href_raw,
                    anchor,
                    rel: link.rel,
                })
            })
            .collect(),
        images: st
            .images_raw
            .into_iter()
            .filter_map(|(src, alt, width, height)| Some(Image { src: resolve(&src)?, alt, width, height }))
            .collect(),
        pictures_without_img: st.pictures_without_img,
        scripts: st.scripts_raw.iter().filter_map(|src| resolve(src)).collect(),
        stylesheets: st.stylesheets_raw.iter().filter_map(|href| resolve(href)).collect(),
        iframes: st.iframes_raw.iter().filter_map(|src| resolve(src)).collect(),
        audios: st.audios_raw.iter().filter_map(|src| resolve(src)).collect(),
        videos: st.videos_raw.iter().filter_map(|src| resolve(src)).collect(),
        meta_refresh: st.refresh.as_deref().map(|content| crate::extract::parse_refresh(content, &resolve)),
        viewport: st.viewport,
        csp_meta: st.csp_meta,
        forms: st
            .forms_raw
            .into_iter()
            .map(|(action, method)| Form {
                action: action
                    .filter(|action| !action.is_empty())
                    .and_then(|action| base.join(&action).ok())
                    .map(|action| action.to_string())
                    .unwrap_or_else(|| url.to_string()),
                method,
            })
            .collect(),
        duplicate_ids,
        dom_elements: if opts.dom_stats {
            st.dom_elements + st.structure.iter().filter(|seen| !**seen).count()
        } else {
            0
        },
        word_count: if opts.text { st.words } else { 0 },
        text_hash: if opts.text { text_hash } else { 0 },
        json_ld: st.json_lds,
        html_bytes: html.len(),
        custom: Default::default(),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_tree;

    /// The streaming facts equal the DOM's, except the element count: a
    /// parser inserts elements (tbody, implied html/head/body) a stream sees
    /// only some of.
    fn agrees(html: &str) {
        let url = Url::parse("https://example.com/dir/page.html").unwrap();
        for options in [ExtractOptions::default(), ExtractOptions::head_only()] {
            let mut tree = serde_json::to_value(extract_tree(html, &url, Source::Raw, &options)).unwrap();
            let mut stream = serde_json::to_value(extract_stream(html, &url, Source::Raw, &options)).unwrap();
            tree["dom_elements"] = 0.into();
            stream["dom_elements"] = 0.into();
            assert_eq!(tree, stream, "{html}");
        }
    }

    #[test]
    fn agrees_with_the_dom_on_tricky_markup() {
        for html in [
            "<!doctype html><title>A &amp; B</title><meta name=description content='x &gt; y'><p>Hello&nbsp;world &copy; 2026</p>",
            "<html><head><base href='/base/'><link rel=canonical href='c?a=1&amp;b=2'></head><body><a href='x'>X</a></body></html>",
            "<body><nav><a href=/1>one</a><header><a href=/2>two</a></header></nav><footer><a href=/3 rel='nofollow noopener' target=_blank>three</a></footer><a href=/4><img src=i.png alt=' Pic '></a></body>",
            "<body><div role=navigation><a href=/r>role</a></div><aside><a href=/a aria-label=' Label  here '></a></aside></body>",
            "<body><h1>Title <b>bold</b></h1><h2>Two</h2><h3>Three</h3><p>text</p></body>",
            "<body><picture><source srcset=a.webp></picture><picture><img src=b.png></picture><video src=v.mp4><source src=v2.webm></video><audio><source src=a.mp3></audio></body>",
            "<body><template><a href=/t>in template</a></template><script>var a = '<a href=/s>no</a>';</script><noscript><a href=/n>noscript</a></noscript><p>visible</p></body>",
            "<body><a href=/one>one</a> <a href=/two>t<i>w</i>o</a><form action=' /go ' method=POST></form><form></form><div id=x></div><span id=x></span></body>",
            "<html lang=ru><head><meta http-equiv=refresh content='5; url=/later'><meta name=robots content='NOINDEX, follow'><script type=application/ld+json>{\"a\": \"&amp;\"}</script></head><body>Привет, мир!</body></html>",
            "<head><link rel=alternate hreflang=en href=/en><link rel='next' href='?p=2'><link rel=stylesheet href=s.css></head><body><link rel=canonical href=/late><iframe src=/frame></iframe><img srcset='x.png 1x, y.png 2x'></body>",
            "text before<title>late title</title><p>after</p>",
            "<svg><title>icon</title></svg><title>real</title><p>body</p>",
            "<p>word<span>glued</span> apart</p><p>&lt;tag&gt; &amp;&amp; &#x41;&#66;</p>",
        ] {
            agrees(html);
        }
    }
}
