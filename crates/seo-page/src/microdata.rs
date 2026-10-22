//! Microdata (`itemscope`, `itemprop`) read into the shape of JSON-LD, so the
//! same validator reads both: an item becomes an object with `@type` (a
//! Schema.org name, or the full URL of another vocabulary), `@id` and its
//! properties, a nested item an object in its property.

use std::collections::HashMap;

use scraper::{ElementRef, Html};
use serde_json::{Map, Value};
use url::Url;

/// Items nested deeper than this are cut: `itemref` can make cycles.
const MAX_DEPTH: usize = 16;

/// The top-level items of a document, each as a JSON object.
pub(crate) fn items(html: &str, base: &Url) -> Vec<String> {
    let document = Html::parse_document(html);
    let mut by_id = HashMap::new();
    for element in document.root_element().descendants().filter_map(ElementRef::wrap) {
        if let Some(id) = element.value().attr("id") {
            by_id.entry(id.to_string()).or_insert(element);
        }
    }
    document
        .root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
        .filter(|element| {
            let element = element.value();
            element.attr("itemscope").is_some() && element.attr("itemprop").is_none()
        })
        .map(|element| item(element, base, &by_id, 0).to_string())
        .collect()
}

fn item(element: ElementRef<'_>, base: &Url, by_id: &HashMap<String, ElementRef<'_>>, depth: usize) -> Value {
    let mut object = Map::new();
    let attrs = element.value();
    if let Some(types) = attrs.attr("itemtype") {
        let types: Vec<String> = types.split_whitespace().map(schema_name).collect();
        if types.iter().any(|kind| !kind.contains(':')) {
            object.insert("@context".into(), "https://schema.org".into());
        }
        object.insert(
            "@type".into(),
            match types.len() {
                1 => Value::String(types[0].clone()),
                _ => types.into_iter().map(Value::String).collect(),
            },
        );
    }
    if let Some(id) = attrs.attr("itemid") {
        object.insert("@id".into(), id.trim().into());
    }
    if depth >= MAX_DEPTH {
        return Value::Object(object);
    }
    let mut properties = Vec::new();
    collect_properties(element, &mut properties);
    for id in attrs.attr("itemref").unwrap_or("").split_whitespace() {
        if let Some(&referenced) = by_id.get(id) {
            if referenced.value().attr("itemprop").is_some() {
                properties.push(referenced);
            }
            collect_properties(referenced, &mut properties);
        }
    }
    for property in properties {
        let value = if property.value().attr("itemscope").is_some() {
            item(property, base, by_id, depth + 1)
        } else {
            Value::String(property_value(property, base))
        };
        for name in property.value().attr("itemprop").unwrap_or("").split_whitespace() {
            let name = schema_name(name);
            match object.get_mut(&name) {
                Some(Value::Array(values)) => values.push(value.clone()),
                Some(existing) => *existing = Value::Array(vec![existing.take(), value.clone()]),
                None => {
                    object.insert(name, value.clone());
                }
            }
        }
    }
    Value::Object(object)
}

/// The elements with `itemprop` under `element` that belong to its item: a
/// nested `itemscope` is a property itself, but its own properties are not.
fn collect_properties<'a>(element: ElementRef<'a>, out: &mut Vec<ElementRef<'a>>) {
    for child in element.children().filter_map(ElementRef::wrap) {
        let attrs = child.value();
        if attrs.attr("itemprop").is_some() {
            out.push(child);
        }
        if attrs.attr("itemscope").is_none() {
            collect_properties(child, out);
        }
    }
}

/// A property's value by the element it is on, as the microdata spec reads it.
fn property_value(element: ElementRef<'_>, base: &Url) -> String {
    let attrs = element.value();
    let url = |name: &str| {
        let raw = attrs.attr(name).unwrap_or("").trim();
        base.join(raw).map_or_else(|_| raw.to_string(), |resolved| resolved.to_string())
    };
    match attrs.name() {
        "meta" => attrs.attr("content").unwrap_or("").trim().to_string(),
        "audio" | "embed" | "iframe" | "img" | "source" | "track" | "video" => url("src"),
        "a" | "area" | "link" => url("href"),
        "object" => url("data"),
        "data" | "meter" => attrs.attr("value").unwrap_or("").trim().to_string(),
        "time" if attrs.attr("datetime").is_some() => attrs.attr("datetime").unwrap_or("").trim().to_string(),
        _ => element.text().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

/// `https://schema.org/Product` → `Product`; other vocabularies keep the URL.
fn schema_name(name: &str) -> String {
    for prefix in ["https://schema.org/", "http://schema.org/", "https://www.schema.org/", "http://www.schema.org/"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_nested_items_and_values_by_element() {
        let html = r#"<body>
            <div itemscope itemtype="https://schema.org/Product" itemref="extra">
              <h1 itemprop="name">Чайник</h1>
              <img itemprop="image" src="/kettle.jpg">
              <div itemprop="offers" itemscope itemtype="http://schema.org/Offer">
                <meta itemprop="priceCurrency" content="RUB">
                <span itemprop="price">1990</span>
                <link itemprop="availability" href="https://schema.org/InStock">
              </div>
              <span itemprop="color">белый</span><span itemprop="color">чёрный</span>
            </div>
            <p id="extra"><time itemprop="releaseDate" datetime="2026-01-02">2 января</time></p>
            <div itemscope itemtype="http://data-vocabulary.org/Breadcrumb"><span itemprop="title">Old</span></div>
            </body>"#;
        let items = items(html, &Url::parse("https://shop.example/p/1").unwrap());
        let product: Value = serde_json::from_str(&items[0]).unwrap();
        assert_eq!(product["@type"], "Product");
        assert_eq!(product["@context"], "https://schema.org");
        assert_eq!(product["name"], "Чайник");
        assert_eq!(product["image"], "https://shop.example/kettle.jpg");
        assert_eq!(product["offers"]["@type"], "Offer");
        assert_eq!(product["offers"]["price"], "1990");
        assert_eq!(product["offers"]["priceCurrency"], "RUB");
        assert_eq!(product["offers"]["availability"], "https://schema.org/InStock");
        assert_eq!(product["color"], serde_json::json!(["белый", "чёрный"]));
        assert_eq!(product["releaseDate"], "2026-01-02");
        // The offer's properties belong to the offer only.
        assert!(product.get("price").is_none());
        let old: Value = serde_json::from_str(&items[1]).unwrap();
        assert_eq!(old["@type"], "http://data-vocabulary.org/Breadcrumb");
        assert!(old.get("@context").is_none());
        assert_eq!(items.len(), 2);
    }
}
