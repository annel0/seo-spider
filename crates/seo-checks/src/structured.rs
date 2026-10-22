//! Structured data: JSON-LD and microdata checked against the Schema.org
//! vocabulary and against what Google needs for its rich results, the way
//! Screaming Frog's Structured Data tab validates them.
//!
//! The vocabulary is Schema.org 30.1 (`data/schemaorg.txt`). The rich result
//! rules are the required and recommended properties from Google Search
//! Central's structured data docs for the features Google still shows; the
//! ones it retired (How-to, Course info, Claim review, Book actions, ...)
//! are left out.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use seo_page::PageFacts;
use serde_json::Value;

use crate::page::Ctx;
use crate::Issue;

/// What one page's structured data holds and what is wrong with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuredData {
    /// `JSON-LD`, `Microdata`.
    pub formats: Vec<&'static str>,
    /// Types of the top-level items, in order, each once.
    pub types: Vec<String>,
    /// Rich result features the items are marked up for (in Russian).
    pub features: Vec<&'static str>,
    /// Blocks that are not JSON, or items without a Schema.org `@context`.
    pub parse_errors: Vec<String>,
    /// `@type` values Schema.org does not define.
    pub unknown_types: Vec<String>,
    /// `Type.property` not in Schema.org, not for that type, or with a value
    /// of a type the property does not take.
    pub invalid_properties: Vec<String>,
    /// Properties a rich result needs, by where they are missing.
    pub missing_required: Vec<String>,
    /// Properties a rich result recommends.
    pub missing_recommended: Vec<String>,
}

impl StructuredData {
    /// Validation errors, as Screaming Frog counts them.
    pub fn errors(&self) -> usize {
        self.parse_errors.len() + self.unknown_types.len() + self.invalid_properties.len() + self.missing_required.len()
    }

    pub fn warnings(&self) -> usize {
        self.missing_recommended.len()
    }
}

/// Validates the JSON-LD blocks and microdata items of a document.
pub fn structured_data(facts: &PageFacts) -> StructuredData {
    let mut report = StructuredData::default();
    if !facts.json_ld.is_empty() {
        report.formats.push("JSON-LD");
    }
    if !facts.microdata.is_empty() {
        report.formats.push("Microdata");
    }
    for (format, block) in
        facts.json_ld.iter().map(|block| ("JSON-LD", block)).chain(facts.microdata.iter().map(|block| ("Microdata", block)))
    {
        let value: Value = match serde_json::from_str(unwrap_block(block)) {
            Ok(value) => value,
            Err(error) => {
                report.parse_errors.push(format!("{format}: {error}"));
                continue;
            }
        };
        for (item, context) in top_items(&value, None) {
            let Some(item) = item.as_object() else {
                report.parse_errors.push(format!("{format}: элемент не объект"));
                continue;
            };
            let types = item_types(item);
            for name in &types {
                let name = schema_term(name).unwrap_or(name).to_string();
                if !report.types.contains(&name) {
                    report.types.push(name);
                }
            }
            let schema_context = context.is_some_and(is_schema_context);
            if !schema_context && types.iter().any(|name| schema_term(name).is_some()) {
                report.parse_errors.push(format!("{format}: у {} нет @context schema.org", types.join(", ")));
                continue;
            }
            check_item(item, &types, &mut report, 0);
            for feature in FEATURES {
                if feature.applies_to(&types) {
                    if !report.features.contains(&feature.label) {
                        report.features.push(feature.label);
                    }
                    check_feature(item, feature, &types.join(", "), &mut report);
                }
            }
        }
    }
    for list in [
        &mut report.unknown_types,
        &mut report.invalid_properties,
        &mut report.missing_required,
        &mut report.missing_recommended,
    ] {
        let mut seen = HashSet::new();
        list.retain(|entry| seen.insert(entry.clone()));
    }
    report
}

/// The page check: an issue per kind of problem, detailing the first few.
pub(crate) fn check(ctx: &Ctx, out: &mut Vec<Issue>) {
    let Some(doc) = ctx.doc() else { return };
    if !doc.extracted.structured_data || (doc.json_ld.is_empty() && doc.microdata.is_empty()) {
        return;
    }
    let report = structured_data(doc);
    for (code, found) in [
        ("structured_data_parse_error", &report.parse_errors),
        ("schema_type_unknown", &report.unknown_types),
        ("schema_property_invalid", &report.invalid_properties),
        ("rich_result_required_missing", &report.missing_required),
        ("rich_result_recommended_missing", &report.missing_recommended),
    ] {
        if !found.is_empty() {
            out.push(Issue::with_detail(code, examples(found)));
        }
    }
}

fn examples(found: &[String]) -> String {
    const SHOWN: usize = 3;
    let mut detail = found.iter().take(SHOWN).cloned().collect::<Vec<_>>().join("; ");
    if found.len() > SHOWN {
        detail.push_str(&format!(" (ещё {})", found.len() - SHOWN));
    }
    detail
}

/// A block as JSON: some sites wrap it in an HTML comment or CDATA.
fn unwrap_block(block: &str) -> &str {
    let mut text = block.trim();
    for (open, close) in [("<!--", "-->"), ("//<![CDATA[", "//]]>"), ("<![CDATA[", "]]>")] {
        if let Some(inner) = text.strip_prefix(open) {
            text = inner.strip_suffix(close).unwrap_or(inner).trim();
        }
    }
    text
}

/// The items of a block with the `@context` each is read in: an array of
/// items, an object with `@graph`, or an item.
fn top_items<'a>(value: &'a Value, context: Option<&'a Value>) -> Vec<(&'a Value, Option<&'a Value>)> {
    match value {
        Value::Array(items) => items.iter().flat_map(|item| top_items(item, context)).collect(),
        Value::Object(object) => {
            let context = object.get("@context").or(context);
            match object.get("@graph") {
                Some(graph) => top_items(graph, context),
                None => vec![(value, context)],
            }
        }
        _ => vec![(value, context)],
    }
}

fn is_schema_context(context: &Value) -> bool {
    match context {
        Value::String(text) => text.contains("schema.org"),
        Value::Array(list) => list.iter().any(is_schema_context),
        Value::Object(map) => map.get("@vocab").is_some_and(is_schema_context),
        _ => false,
    }
}

/// The `@type` values of an item as written.
fn item_types(item: &serde_json::Map<String, Value>) -> Vec<String> {
    match item.get("@type") {
        Some(Value::String(name)) => vec![name.clone()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

/// A Schema.org term from how it is written (`Product`, `schema:Product`,
/// `https://schema.org/Product`); `None` for another vocabulary's.
fn schema_term(name: &str) -> Option<&str> {
    for prefix in ["https://schema.org/", "http://schema.org/", "https://www.schema.org/", "http://www.schema.org/", "schema:"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    (!name.contains(':') && !name.contains('/')).then_some(name)
}

/// Checks an item's types and properties against Schema.org, then its
/// nested items.
fn check_item(item: &serde_json::Map<String, Value>, types: &[String], report: &mut StructuredData, depth: usize) {
    const MAX_DEPTH: usize = 20;
    let vocab = &*VOCAB;
    let mut known = Vec::new();
    for name in types {
        match schema_term(name) {
            Some(term) if vocab.types.contains_key(term) => known.push(term),
            Some(term) => report.unknown_types.push(term.to_string()),
            None => report.unknown_types.push(name.clone()),
        }
    }
    let label = if types.is_empty() { "(без @type)".to_string() } else { types.join(", ") };
    let lineage: HashSet<&str> = known.iter().flat_map(|term| vocab.lineage(term)).collect();
    for (key, value) in item {
        if key.starts_with('@') {
            continue;
        }
        let Some(property) = schema_term(key) else { continue };
        // Actions annotate a property with `-input` or `-output`
        // (`query-input` on a SearchAction), which names the property.
        let property = property.strip_suffix("-input").or_else(|| property.strip_suffix("-output")).unwrap_or(property);
        let Some((domains, ranges)) = vocab.properties.get(property) else {
            report.invalid_properties.push(format!("{label}.{property}: нет в Schema.org"));
            continue;
        };
        if !lineage.is_empty() && !domains.is_empty() && !domains.iter().any(|domain| lineage.contains(domain)) {
            report.invalid_properties.push(format!("{label}.{property}: не для этого типа"));
        }
        let values: Vec<&Value> = match value {
            Value::Array(values) => values.iter().collect(),
            value => vec![value],
        };
        for value in values {
            let Some(nested) = value.as_object() else { continue };
            let nested_types = item_types(nested);
            let nested_known: Vec<&str> = nested_types
                .iter()
                .filter_map(|name| schema_term(name))
                .filter(|term| vocab.types.contains_key(term))
                .collect();
            if !nested_known.is_empty()
                && !ranges.is_empty()
                && !nested_known.iter().any(|term| vocab.lineage(term).iter().any(|kind| ranges.contains(kind)))
            {
                report.invalid_properties.push(format!(
                    "{label}.{property}: ожидается {}, указан {}",
                    ranges.join(" или "),
                    nested_known.join(", ")
                ));
            }
            if depth < MAX_DEPTH {
                check_item(nested, &nested_types, report, depth + 1);
            }
        }
    }
}

/// A rich result and what Google wants for it. A requirement `a|b` is met by
/// either property; `nested` applies a feature to the objects in a property.
struct Feature {
    label: &'static str,
    types: &'static [&'static str],
    /// The types' subtypes count too (NewsArticle for Article).
    subtypes: bool,
    required: &'static [&'static str],
    recommended: &'static [&'static str],
    nested: &'static [(&'static str, &'static Feature)],
}

impl Feature {
    fn applies_to(&self, types: &[String]) -> bool {
        types.iter().filter_map(|name| schema_term(name)).any(|term| {
            if self.subtypes {
                let lineage = VOCAB.lineage(term);
                self.types.iter().any(|kind| lineage.contains(kind))
            } else {
                self.types.contains(&term)
            }
        })
    }
}

const fn part(
    label: &'static str,
    required: &'static [&'static str],
    recommended: &'static [&'static str],
    nested: &'static [(&'static str, &'static Feature)],
) -> Feature {
    Feature { label, types: &[], subtypes: false, required, recommended, nested }
}

static RATING: Feature = part("Оценка", &["ratingValue"], &[], &[]);
static AGGREGATE_RATING: Feature = part("Сводный рейтинг", &["ratingValue", "ratingCount|reviewCount"], &["bestRating"], &[]);
static REVIEW: Feature = part("Отзыв", &["author", "reviewRating"], &["datePublished"], &[("reviewRating", &RATING)]);
static OFFER: Feature = part("Предложение", &["price|lowPrice|priceSpecification"], &["priceCurrency", "availability"], &[]);
static APP_OFFER: Feature = part("Предложение", &["price|priceSpecification"], &["priceCurrency"], &[]);
static EVENT_OFFER: Feature = part("Билет", &[], &["price", "priceCurrency", "availability", "url", "validFrom"], &[]);
static LIST_ITEM: Feature = part("Элемент списка", &["position", "name|item"], &[], &[]);
static ANSWER: Feature = part("Ответ", &["text"], &[], &[]);
static FAQ_QUESTION: Feature = part("Вопрос", &["name", "acceptedAnswer"], &[], &[("acceptedAnswer", &ANSWER)]);
static QA_QUESTION: Feature = part(
    "Вопрос",
    &["name", "answerCount", "acceptedAnswer|suggestedAnswer"],
    &["text", "author", "datePublished"],
    &[("acceptedAnswer", &ANSWER), ("suggestedAnswer", &ANSWER)],
);

/// Google's rich result features, top-level items only (a VideoObject or
/// an ImageObject inside an article is not a feature of its own).
static FEATURES: &[Feature] = &[
    Feature {
        label: "Статья",
        types: &["Article"],
        subtypes: true,
        required: &[],
        recommended: &["author", "datePublished", "dateModified", "headline", "image"],
        nested: &[],
    },
    Feature {
        label: "Хлебные крошки",
        types: &["BreadcrumbList"],
        subtypes: false,
        required: &["itemListElement"],
        recommended: &[],
        nested: &[("itemListElement", &LIST_ITEM)],
    },
    Feature {
        label: "Товар",
        types: &["Product"],
        subtypes: true,
        required: &["name", "offers|review|aggregateRating"],
        recommended: &["image", "description", "brand", "sku|gtin|gtin8|gtin12|gtin13|gtin14|mpn"],
        nested: &[("offers", &OFFER), ("review", &REVIEW), ("aggregateRating", &AGGREGATE_RATING)],
    },
    Feature {
        label: "Отзыв",
        types: &["Review"],
        subtypes: false,
        required: &["author", "itemReviewed", "reviewRating"],
        recommended: &["datePublished"],
        nested: &[("reviewRating", &RATING)],
    },
    Feature {
        label: "Сводный рейтинг",
        types: &["AggregateRating"],
        subtypes: false,
        required: &["itemReviewed", "ratingValue", "ratingCount|reviewCount"],
        recommended: &["bestRating"],
        nested: &[],
    },
    Feature {
        label: "FAQ",
        types: &["FAQPage"],
        subtypes: false,
        required: &["mainEntity"],
        recommended: &[],
        nested: &[("mainEntity", &FAQ_QUESTION)],
    },
    Feature {
        label: "Вопрос и ответы",
        types: &["QAPage"],
        subtypes: false,
        required: &["mainEntity"],
        recommended: &[],
        nested: &[("mainEntity", &QA_QUESTION)],
    },
    Feature {
        label: "Рецепт",
        types: &["Recipe"],
        subtypes: false,
        required: &["name", "image"],
        recommended: &[
            "author",
            "datePublished",
            "description",
            "recipeIngredient",
            "recipeInstructions",
            "recipeYield",
            "prepTime|cookTime|totalTime",
            "aggregateRating",
            "nutrition",
            "keywords",
            "recipeCategory",
            "recipeCuisine",
            "video",
        ],
        nested: &[("aggregateRating", &AGGREGATE_RATING), ("review", &REVIEW)],
    },
    Feature {
        label: "Мероприятие",
        types: &["Event"],
        subtypes: true,
        required: &["name", "startDate", "location"],
        recommended: &["description", "endDate", "eventAttendanceMode", "eventStatus", "image", "offers", "organizer", "performer"],
        nested: &[("offers", &EVENT_OFFER)],
    },
    Feature {
        label: "Вакансия",
        types: &["JobPosting"],
        subtypes: false,
        required: &["title", "description", "datePosted", "hiringOrganization", "jobLocation|applicantLocationRequirements"],
        recommended: &["baseSalary", "employmentType", "validThrough", "directApply", "identifier"],
        nested: &[],
    },
    Feature {
        label: "Местная организация",
        types: &["LocalBusiness"],
        subtypes: true,
        required: &["name", "address"],
        recommended: &["telephone", "url", "geo", "openingHoursSpecification", "priceRange", "image"],
        nested: &[("aggregateRating", &AGGREGATE_RATING), ("review", &REVIEW)],
    },
    Feature {
        label: "Организация",
        types: &["Organization", "Corporation"],
        subtypes: false,
        required: &[],
        recommended: &["name", "url", "logo", "sameAs"],
        nested: &[],
    },
    Feature {
        label: "Видео",
        types: &["VideoObject"],
        subtypes: false,
        required: &["name", "thumbnailUrl", "uploadDate"],
        recommended: &["description", "duration", "contentUrl|embedUrl"],
        nested: &[],
    },
    Feature {
        label: "Приложение",
        types: &["SoftwareApplication"],
        subtypes: true,
        required: &["name", "offers", "aggregateRating|review"],
        recommended: &["applicationCategory", "operatingSystem"],
        nested: &[("offers", &APP_OFFER), ("aggregateRating", &AGGREGATE_RATING), ("review", &REVIEW)],
    },
    Feature {
        label: "Набор данных",
        types: &["Dataset"],
        subtypes: false,
        required: &["name", "description"],
        recommended: &["url", "creator", "license", "identifier", "keywords"],
        nested: &[],
    },
    Feature {
        label: "Фильм",
        types: &["Movie"],
        subtypes: false,
        required: &["name", "image"],
        recommended: &["aggregateRating", "dateCreated", "director", "review"],
        nested: &[("aggregateRating", &AGGREGATE_RATING), ("review", &REVIEW)],
    },
    Feature {
        label: "Обсуждение на форуме",
        types: &["DiscussionForumPosting"],
        subtypes: false,
        required: &["author", "datePublished", "text|image|video"],
        recommended: &["headline", "url"],
        nested: &[],
    },
    Feature {
        label: "Профиль",
        types: &["ProfilePage"],
        subtypes: false,
        required: &["mainEntity"],
        recommended: &[],
        nested: &[],
    },
    Feature {
        label: "Метаданные изображения",
        types: &["ImageObject"],
        subtypes: false,
        required: &["contentUrl", "creator|creditText|copyrightNotice|license"],
        recommended: &["acquireLicensePage"],
        nested: &[],
    },
];

fn check_feature(item: &serde_json::Map<String, Value>, feature: &Feature, path: &str, report: &mut StructuredData) {
    for requirement in feature.required {
        if !satisfied(item, requirement) {
            report.missing_required.push(format!("{path}: нет {}", requirement.replace('|', " или ")));
        }
    }
    for requirement in feature.recommended {
        if !satisfied(item, requirement) {
            report.missing_recommended.push(format!("{path}: нет {}", requirement.replace('|', " или ")));
        }
    }
    for (property, nested) in feature.nested {
        for value in values_of(item, property) {
            if let Some(object) = value.as_object() {
                check_feature(object, nested, &format!("{path} → {property}"), report);
            }
        }
    }
}

/// The values of a property, however its key is written.
fn values_of<'a>(item: &'a serde_json::Map<String, Value>, property: &str) -> Vec<&'a Value> {
    item.iter()
        .filter(|(key, _)| schema_term(key) == Some(property))
        .flat_map(|(_, value)| match value {
            Value::Array(values) => values.iter().collect::<Vec<_>>(),
            value => vec![value],
        })
        .collect()
}

/// Whether one of the `|`-separated properties has a value.
fn satisfied(item: &serde_json::Map<String, Value>, requirement: &str) -> bool {
    requirement.split('|').any(|property| {
        values_of(item, property).into_iter().any(|value| match value {
            Value::Null => false,
            Value::String(text) => !text.trim().is_empty(),
            Value::Array(values) => !values.is_empty(),
            _ => true,
        })
    })
}

/// The Schema.org vocabulary: types with their parents, enumeration
/// members, and properties with the types they apply to and take.
struct Vocab {
    types: HashMap<&'static str, Vec<&'static str>>,
    members: HashMap<&'static str, &'static str>,
    properties: HashMap<&'static str, (Vec<&'static str>, Vec<&'static str>)>,
}

static VOCAB: LazyLock<Vocab> = LazyLock::new(|| Vocab::parse(include_str!("../data/schemaorg.txt")));

impl Vocab {
    fn parse(text: &'static str) -> Self {
        let list = |field: Option<&'static str>| -> Vec<&'static str> {
            match field {
                None | Some("-") => Vec::new(),
                Some(field) => field.split(',').collect(),
            }
        };
        let mut vocab = Vocab { types: HashMap::new(), members: HashMap::new(), properties: HashMap::new() };
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let mut fields = line.split(' ');
            match (fields.next(), fields.next()) {
                (Some("T"), Some(name)) => {
                    vocab.types.insert(name, list(fields.next()));
                }
                (Some("E"), Some(name)) => {
                    vocab.members.insert(name, fields.next().unwrap_or(""));
                }
                (Some("P"), Some(name)) => {
                    let domains = list(fields.next());
                    vocab.properties.insert(name, (domains, list(fields.next())));
                }
                _ => {}
            }
        }
        vocab
    }

    /// The type and every type it descends from.
    fn lineage(&self, name: &str) -> HashSet<&'static str> {
        let mut seen = HashSet::new();
        let mut queue: Vec<&'static str> = match self.types.get_key_value(name) {
            Some((&name, _)) => vec![name],
            None => return seen,
        };
        while let Some(kind) = queue.pop() {
            if seen.insert(kind) {
                queue.extend(self.types.get(kind).into_iter().flatten().copied());
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn facts(json_ld: &[&str]) -> PageFacts {
        let mut facts =
            seo_page::extract("<html></html>", &Url::parse("https://example.com/").unwrap(), seo_page::Source::Raw);
        facts.json_ld = json_ld.iter().map(|block| block.to_string()).collect();
        facts
    }

    #[test]
    fn vocabulary_knows_types_members_and_lineage() {
        assert!(VOCAB.lineage("NewsArticle").contains("Article"));
        assert!(VOCAB.lineage("Restaurant").contains("LocalBusiness"));
        assert!(VOCAB.lineage("Restaurant").contains("Thing"));
        assert_eq!(VOCAB.members.get("InStock"), Some(&"ItemAvailability"));
        assert!(VOCAB.properties["offers"].0.contains(&"Product"));
    }

    #[test]
    fn a_complete_product_is_clean() {
        let report = structured_data(&facts(&[r#"{
            "@context": "https://schema.org", "@type": "Product", "name": "Чайник",
            "image": "https://example.com/k.jpg", "description": "Белый", "sku": "K1",
            "brand": {"@type": "Brand", "name": "Kettle Co"},
            "offers": {"@type": "Offer", "price": "1990", "priceCurrency": "RUB",
                       "availability": "https://schema.org/InStock"},
            "aggregateRating": {"@type": "AggregateRating", "ratingValue": 4.6, "reviewCount": 12, "bestRating": 5}
        }"#]));
        assert_eq!(report.errors(), 0, "{report:?}");
        assert_eq!(report.warnings(), 0, "{report:?}");
        assert_eq!(report.types, ["Product"]);
        assert_eq!(report.features, ["Товар"]);
        assert_eq!(report.formats, ["JSON-LD"]);
    }

    #[test]
    fn finds_vocabulary_and_rich_result_problems() {
        let report = structured_data(&facts(&[
            r#"{"@context": "https://schema.org", "@type": "Product", "name": "Чайник", "colour": "white",
                "offers": {"@type": "Offer", "priceCurrency": "RUB"},
                "author": {"@type": "Person", "name": "Нет"}}"#,
            r#"{"@context": "http://schema.org", "@type": "Prodct", "name": "x"}"#,
            r#"{"@context": "https://schema.org", "@type": "Article", "author": {"@type": "Offer"}}"#,
            r#"{"@context": "https://schema.org", "@type": "WebSite", "potentialAction": {"@type": "SearchAction",
                "target": "https://example.com/?q={q}", "query-input": "required name=q"}}"#,
            r#"{"@type": "Product", "name": "без контекста"}"#,
            r#"{"@context": "https://schema.org", "@type": "Product", "name": "x",}"#,
        ]));
        assert!(report.invalid_properties.contains(&"Product.colour: нет в Schema.org".to_string()), "{report:?}");
        assert!(report.invalid_properties.contains(&"Product.author: не для этого типа".to_string()), "{report:?}");
        assert!(
            report.invalid_properties.iter().any(|entry| entry.starts_with("Article.author: ожидается")),
            "{report:?}"
        );
        assert_eq!(report.unknown_types, ["Prodct"]);
        assert!(!report.invalid_properties.iter().any(|entry| entry.contains("query")), "{report:?}");
        assert!(
            report.missing_required.contains(&"Product → offers: нет price или lowPrice или priceSpecification".to_string()),
            "{report:?}"
        );
        assert!(report.missing_recommended.contains(&"Product: нет image".to_string()), "{report:?}");
        assert_eq!(report.parse_errors.len(), 2, "{report:?}");
        assert!(report.parse_errors[0].contains("нет @context"), "{report:?}");
    }

    #[test]
    fn reads_graphs_prefixed_terms_and_microdata() {
        let mut page = facts(&[r#"<!-- {"@context": {"@vocab": "https://schema.org/"}, "@graph": [
            {"@type": "BreadcrumbList", "itemListElement": [
                {"@type": "ListItem", "position": 1, "name": "Главная", "item": "https://example.com/"},
                {"@type": "ListItem", "name": "Чайники"}]},
            {"@type": "schema:Organization", "schema:name": "Kettle Co", "url": "https://example.com/",
             "logo": "https://example.com/l.png", "sameAs": ["https://vk.com/kettle"]}
        ]} -->"#]);
        page.microdata = vec![r#"{"@context": "https://schema.org", "@type": "Event", "name": "Ярмарка"}"#.into()];
        let report = structured_data(&page);
        assert_eq!(report.formats, ["JSON-LD", "Microdata"]);
        assert_eq!(report.types, ["BreadcrumbList", "Organization", "Event"]);
        assert_eq!(report.features, ["Хлебные крошки", "Организация", "Мероприятие"]);
        assert_eq!(
            report.missing_required,
            ["BreadcrumbList → itemListElement: нет position", "Event: нет startDate", "Event: нет location"],
        );
        assert!(report.parse_errors.is_empty() && report.invalid_properties.is_empty(), "{report:?}");
    }
}
