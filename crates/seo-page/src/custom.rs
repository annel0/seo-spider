//! Custom extraction and custom search, as in Screaming Frog: values pulled
//! out of each page by CSS selector, XPath 1.0 or regular expression, and
//! text or patterns counted in its HTML or its visible text.
//!
//! The specs come from the crawl settings. `CustomRules::compile` checks them
//! once; `CustomRules::run` applies them to one document, which is parsed
//! into a DOM only when a CSS or XPath extractor, or a search in the visible
//! text, needs one.

use std::collections::HashMap;

use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One value to pull out of every page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtractorSpec {
    /// The column name in the export.
    pub name: String,
    /// Exactly one of `css`, `xpath` and `regex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    /// XPath 1.0. Attribute nodes (`//a/@href`) give their values, and
    /// functions (`count(//a)`) their result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xpath: Option<String>,
    /// Matched against the HTML source: the first capture group when there
    /// is one, else the whole match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// With `css`: the value of this attribute instead of the element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribute: Option<String>,
    /// What a matched element gives.
    #[serde(default)]
    pub output: Output,
    /// At most this many values per page.
    #[serde(default = "default_max")]
    pub max: usize,
}

fn default_max() -> usize {
    10
}

/// What an element matched by CSS or XPath gives.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Output {
    /// Its text, whitespace collapsed.
    #[default]
    Text,
    /// The element itself as HTML.
    Html,
    /// The HTML inside it.
    InnerHtml,
}

/// Text or a pattern to count on every page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchSpec {
    pub name: String,
    /// Exactly one of `text` (case does not matter) and `regex`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// Where to look.
    #[serde(default)]
    pub scope: SearchScope,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    /// The HTML source.
    #[default]
    Html,
    /// The visible text of `<body>` (script, style, noscript and template
    /// left out).
    Text,
}

/// What the custom extractors and searches found on one page, by name in
/// the order of the settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomFacts {
    /// The values each extractor found, in document order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extracted: Vec<(String, Vec<String>)>,
    /// How many times each search was found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub found: Vec<(String, usize)>,
    /// Extractors whose XPath failed to evaluate on this page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<(String, String)>,
}

impl CustomFacts {
    pub fn is_empty(&self) -> bool {
        self.extracted.is_empty() && self.found.is_empty() && self.errors.is_empty()
    }

    /// The values an extractor found.
    pub fn values(&self, name: &str) -> &[String] {
        self.extracted.iter().find(|(have, _)| have == name).map_or(&[], |(_, values)| values.as_slice())
    }

    /// How many times a search was found; `None` when it did not run here.
    pub fn count(&self, name: &str) -> Option<usize> {
        self.found.iter().find(|(have, _)| have == name).map(|(_, count)| *count)
    }
}

/// The specs, checked and compiled, ready to run on documents.
pub struct CustomRules {
    extractors: Vec<Extractor>,
    searches: Vec<Search>,
}

struct Extractor {
    name: String,
    how: How,
    attribute: Option<String>,
    output: Output,
    max: usize,
}

enum How {
    Css(Selector),
    /// Compiled for each document: a compiled XPath cannot cross threads.
    XPath(String),
    Regex(Regex),
}

struct Search {
    name: String,
    /// Lower-cased text, or a pattern.
    matcher: Result<String, Regex>,
    scope: SearchScope,
}

impl CustomRules {
    /// Checks every spec; `None` when there is nothing to run. The error
    /// names the spec and what is wrong with it.
    pub fn compile(extractors: &[ExtractorSpec], searches: &[SearchSpec]) -> Result<Option<Self>, String> {
        if extractors.is_empty() && searches.is_empty() {
            return Ok(None);
        }
        let mut names = std::collections::HashSet::new();
        let mut unique = |name: &str| -> Result<(), String> {
            if name.trim().is_empty() {
                return Err("a custom extractor or search has no name".into());
            }
            if !names.insert(name.to_string()) {
                return Err(format!("the name {name:?} is used twice"));
            }
            Ok(())
        };
        let mut compiled = CustomRules { extractors: Vec::new(), searches: Vec::new() };
        for spec in extractors {
            unique(&spec.name)?;
            let how = match (&spec.css, &spec.xpath, &spec.regex) {
                (Some(css), None, None) => How::Css(
                    Selector::parse(css).map_err(|error| format!("extractor {:?}: CSS {css:?}: {error}", spec.name))?,
                ),
                (None, Some(xpath), None) => {
                    compile_xpath(xpath).map_err(|error| format!("extractor {:?}: XPath {xpath:?}: {error}", spec.name))?;
                    How::XPath(xpath.clone())
                }
                (None, None, Some(pattern)) => How::Regex(
                    Regex::new(pattern).map_err(|error| format!("extractor {:?}: regex: {error}", spec.name))?,
                ),
                _ => return Err(format!("extractor {:?} needs exactly one of css, xpath and regex", spec.name)),
            };
            if spec.attribute.is_some() && !matches!(how, How::Css(_)) {
                return Err(format!("extractor {:?}: attribute goes with css (in XPath, select @name)", spec.name));
            }
            compiled.extractors.push(Extractor {
                name: spec.name.clone(),
                how,
                attribute: spec.attribute.clone(),
                output: spec.output,
                max: spec.max.max(1),
            });
        }
        for spec in searches {
            unique(&spec.name)?;
            let matcher = match (&spec.text, &spec.regex) {
                (Some(text), None) if !text.is_empty() => Ok(text.to_lowercase()),
                (None, Some(pattern)) => {
                    Err(Regex::new(pattern).map_err(|error| format!("search {:?}: regex: {error}", spec.name))?)
                }
                _ => return Err(format!("search {:?} needs exactly one of text and regex", spec.name)),
            };
            compiled.searches.push(Search { name: spec.name.clone(), matcher, scope: spec.scope });
        }
        Ok(Some(compiled))
    }

    /// Runs every extractor and search on one HTML document.
    pub fn run(&self, html: &str) -> CustomFacts {
        let needs_dom = self.extractors.iter().any(|extractor| !matches!(extractor.how, How::Regex(_)))
            || self.searches.iter().any(|search| search.scope == SearchScope::Text);
        let dom = needs_dom.then(|| Html::parse_document(html));
        let mut facts = CustomFacts::default();

        let xpaths: Vec<&Extractor> =
            self.extractors.iter().filter(|extractor| matches!(extractor.how, How::XPath(_))).collect();
        let mut xpath_values = match (&dom, xpaths.is_empty()) {
            (Some(dom), false) => evaluate_xpaths(dom, &xpaths),
            _ => HashMap::new(),
        };
        for extractor in &self.extractors {
            let values = match &extractor.how {
                How::Css(selector) => {
                    let dom = dom.as_ref().expect("parsed for CSS");
                    dom.select(selector)
                        .filter_map(|element| match &extractor.attribute {
                            Some(name) => element.value().attr(name).map(|value| value.trim().to_string()),
                            None => Some(element_output(element, extractor.output)),
                        })
                        .filter(|value| !value.is_empty())
                        .take(extractor.max)
                        .collect()
                }
                How::XPath(_) => match xpath_values.remove(&extractor.name) {
                    Some(Ok(values)) => values.into_iter().take(extractor.max).collect(),
                    Some(Err(error)) => {
                        facts.errors.push((extractor.name.clone(), error));
                        Vec::new()
                    }
                    None => Vec::new(),
                },
                How::Regex(pattern) => pattern
                    .captures_iter(html)
                    .filter_map(|captures| captures.get(1).or_else(|| captures.get(0)))
                    .map(|found| found.as_str().trim().to_string())
                    .filter(|value| !value.is_empty())
                    .take(extractor.max)
                    .collect(),
            };
            facts.extracted.push((extractor.name.clone(), values));
        }

        if !self.searches.is_empty() {
            let text = dom
                .as_ref()
                .filter(|_| self.searches.iter().any(|search| search.scope == SearchScope::Text))
                .map(crate::extract::visible_text);
            let mut lowered: [Option<String>; 2] = [None, None];
            for search in &self.searches {
                let haystack = match search.scope {
                    SearchScope::Html => html,
                    SearchScope::Text => text.as_deref().unwrap_or(""),
                };
                let count = match &search.matcher {
                    Ok(needle) => {
                        let at = search.scope as usize;
                        let lowered = lowered[at].get_or_insert_with(|| haystack.to_lowercase());
                        lowered.matches(needle.as_str()).count()
                    }
                    Err(pattern) => pattern.find_iter(haystack).count(),
                };
                facts.found.push((search.name.clone(), count));
            }
        }
        facts
    }
}

fn compile_xpath(xpath: &str) -> Result<sxd_xpath::XPath, String> {
    sxd_xpath::Factory::new()
        .build(xpath)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "empty expression".to_string())
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn element_output(element: ElementRef<'_>, output: Output) -> String {
    match output {
        Output::Text => collapse(&element.text().collect::<String>()),
        Output::Html => element.html(),
        Output::InnerHtml => element.inner_html().trim().to_string(),
    }
}

/// Where a copied node goes: under the document root or an element.
#[derive(Clone, Copy)]
enum Parent<'d> {
    Root(sxd_document::dom::Root<'d>),
    Element(sxd_document::dom::Element<'d>),
}

/// The values of every XPath extractor, by name. The DOM is copied once into
/// the XML model sxd-xpath evaluates on (elements without a namespace, so
/// `//div` matches as in a browser's devtools); each copied element remembers
/// the element it came from, for HTML output.
fn evaluate_xpaths(dom: &Html, extractors: &[&Extractor]) -> HashMap<String, Result<Vec<String>, String>> {
    let package = sxd_document::Package::new();
    let document = package.as_document();
    // sxd elements hash by node identity, not by content, so filling in the
    // copies after inserting them does not move their keys.
    #[allow(clippy::mutable_key_type)]
    let mut origin = HashMap::new();
    let mut stack = vec![(dom.tree.root(), Parent::Root(document.root()))];
    while let Some((node, parent)) = stack.pop() {
        // Children are appended as they are met, so each parent gets them in
        // document order whatever order the subtrees are copied in.
        for child in node.children() {
            match child.value() {
                Node::Element(element) => {
                    let copy = document.create_element(element.name());
                    for (name, value) in element.attrs() {
                        copy.set_attribute_value(name, value);
                    }
                    match parent {
                        Parent::Root(root) => root.append_child(copy),
                        Parent::Element(element) => element.append_child(copy),
                    }
                    if let Some(original) = ElementRef::wrap(child) {
                        origin.insert(copy, original);
                    }
                    stack.push((child, Parent::Element(copy)));
                }
                Node::Text(text) => {
                    if let Parent::Element(element) = parent {
                        element.append_child(document.create_text(text));
                    }
                }
                _ => {}
            }
        }
    }

    let context = sxd_xpath::Context::new();
    let mut values = HashMap::new();
    for extractor in extractors {
        let How::XPath(source) = &extractor.how else { continue };
        let result = compile_xpath(source).and_then(|xpath| {
            let value = xpath.evaluate(&context, document.root()).map_err(|error| error.to_string())?;
            Ok(match value {
                sxd_xpath::Value::Nodeset(nodes) => nodes
                    .document_order()
                    .into_iter()
                    .map(|node| match node {
                        sxd_xpath::nodeset::Node::Element(element) => match origin.get(&element) {
                            Some(&original) => element_output(original, extractor.output),
                            None => collapse(&node.string_value()),
                        },
                        sxd_xpath::nodeset::Node::Attribute(attribute) => attribute.value().trim().to_string(),
                        other => collapse(&other.string_value()),
                    })
                    .filter(|value| !value.is_empty())
                    .collect(),
                sxd_xpath::Value::String(text) => vec![text.trim().to_string()],
                sxd_xpath::Value::Number(number) => vec![format_number(number)],
                sxd_xpath::Value::Boolean(flag) => vec![flag.to_string()],
            })
        });
        values.insert(extractor.name.clone(), result);
    }
    values
}

/// `2` rather than `2.0` for a whole number, as XPath tools print it.
fn format_number(number: f64) -> String {
    if number.is_finite() && number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{}", number as i64)
    } else {
        number.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extractor(name: &str, how: &str, expression: &str) -> ExtractorSpec {
        ExtractorSpec {
            name: name.into(),
            css: (how == "css").then(|| expression.into()),
            xpath: (how == "xpath").then(|| expression.into()),
            regex: (how == "regex").then(|| expression.into()),
            attribute: None,
            output: Output::Text,
            max: 10,
        }
    }

    const PAGE: &str = r#"<html><head><title>T</title>
        <script>var id = "GTM-ABC123";</script></head>
        <body><h1> Main   title </h1>
        <div class="price" data-sku="s1"><b>100</b> ₽</div>
        <div class="price" data-sku="s2"><b>200</b> ₽</div>
        <a href="/one">One</a><a href="https://two.example/">Two</a>
        <p>Скидка до 50%. скидка!</p></body></html>"#;

    fn run(extractors: &[ExtractorSpec], searches: &[SearchSpec]) -> CustomFacts {
        CustomRules::compile(extractors, searches).unwrap().unwrap().run(PAGE)
    }

    #[test]
    fn css_gives_text_attributes_and_html() {
        let mut attribute = extractor("sku", "css", ".price");
        attribute.attribute = Some("data-sku".into());
        let mut html = extractor("html", "css", ".price");
        html.output = Output::Html;
        html.max = 1;
        let mut inner = extractor("inner", "css", ".price");
        inner.output = Output::InnerHtml;
        inner.max = 1;
        let facts = run(&[extractor("h1", "css", "h1"), extractor("price", "css", ".price b"), attribute, html, inner], &[]);
        assert_eq!(facts.values("h1"), ["Main title"]);
        assert_eq!(facts.values("price"), ["100", "200"]);
        assert_eq!(facts.values("sku"), ["s1", "s2"]);
        assert_eq!(facts.values("html"), [r#"<div class="price" data-sku="s1"><b>100</b> ₽</div>"#]);
        assert_eq!(facts.values("inner"), ["<b>100</b> ₽"]);
    }

    #[test]
    fn xpath_gives_nodes_attributes_and_function_values() {
        let mut html = extractor("second", "xpath", "//div[@data-sku='s2']");
        html.output = Output::Html;
        let facts = run(
            &[
                extractor("h1", "xpath", "//h1"),
                extractor("hrefs", "xpath", "//a/@href"),
                extractor("links", "xpath", "count(//a)"),
                extractor("external", "xpath", "boolean(//a[starts-with(@href, 'https://')])"),
                html,
            ],
            &[],
        );
        assert_eq!(facts.values("h1"), ["Main title"]);
        assert_eq!(facts.values("hrefs"), ["/one", "https://two.example/"]);
        assert_eq!(facts.values("links"), ["2"]);
        assert_eq!(facts.values("external"), ["true"]);
        assert_eq!(facts.values("second"), [r#"<div class="price" data-sku="s2"><b>200</b> ₽</div>"#]);
        assert!(facts.errors.is_empty());
    }

    #[test]
    fn regex_gives_the_first_group_or_the_match() {
        let facts = run(&[extractor("gtm", "regex", r#""(GTM-[A-Z0-9]+)""#), extractor("rub", "regex", r"\d+</b> ₽")], &[]);
        assert_eq!(facts.values("gtm"), ["GTM-ABC123"]);
        assert_eq!(facts.values("rub"), ["100</b> ₽", "200</b> ₽"]);
    }

    #[test]
    fn searches_count_in_html_or_visible_text() {
        let search = |name: &str, text: Option<&str>, regex: Option<&str>, scope| SearchSpec {
            name: name.into(),
            text: text.map(Into::into),
            regex: regex.map(Into::into),
            scope,
        };
        let facts = run(
            &[],
            &[
                search("sale", Some("СКИДКА"), None, SearchScope::Html),
                search("gtm in html", Some("gtm-"), None, SearchScope::Html),
                search("gtm in text", Some("gtm-"), None, SearchScope::Text),
                search("percent", None, Some(r"\d+%"), SearchScope::Text),
            ],
        );
        assert_eq!(facts.count("sale"), Some(2));
        assert_eq!(facts.count("gtm in html"), Some(1));
        assert_eq!(facts.count("gtm in text"), Some(0));
        assert_eq!(facts.count("percent"), Some(1));
    }

    #[test]
    fn bad_specs_are_refused_with_their_name() {
        let both = ExtractorSpec { xpath: Some("//a".into()), ..extractor("both", "css", "a") };
        let errors = [
            CustomRules::compile(&[extractor("bad css", "css", "a[")], &[]).err().unwrap(),
            CustomRules::compile(&[extractor("bad xpath", "xpath", "//a[")], &[]).err().unwrap(),
            CustomRules::compile(&[extractor("bad regex", "regex", "(")], &[]).err().unwrap(),
            CustomRules::compile(&[both], &[]).err().unwrap(),
            CustomRules::compile(&[extractor("twice", "css", "a"), extractor("twice", "css", "b")], &[]).err().unwrap(),
        ];
        assert!(errors[0].contains("bad css"), "{}", errors[0]);
        assert!(errors[1].contains("bad xpath"), "{}", errors[1]);
        assert!(errors[2].contains("bad regex"), "{}", errors[2]);
        assert!(errors[3].contains("exactly one"), "{}", errors[3]);
        assert!(errors[4].contains("twice"), "{}", errors[4]);
        assert!(CustomRules::compile(&[], &[]).unwrap().is_none());
    }
}
