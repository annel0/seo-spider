//! The tabs of the UI, as Screaming Frog has them: which URLs each lists,
//! its filters (built-in ones, and the catalog's checks of its categories),
//! and its columns, each computed from a stored URL. One registry serves
//! the table, the filter counts and the CSV of a tab.

use std::collections::HashMap;

use seo_page::PageFacts;
use seo_store::StoredRow;
use serde::Serialize;
use serde_json::{json, Value};

/// How the UI draws a column's values.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Kind {
    Url,
    Status,
    Text,
    Number,
    /// Characters, good between the bounds.
    Length { low: u32, high: u32 },
    Ms,
    Bytes,
    /// true, false or empty.
    Bool,
    /// indexable, noindex, canonicalised, redirect, error, blocked.
    Indexable,
    Issues,
}

#[derive(Debug, Clone, Serialize)]
pub struct Column {
    pub key: String,
    pub title: String,
    pub kind: Kind,
    /// The store's sort key, when the column can be sorted.
    #[serde(skip)]
    pub sort: Option<&'static str>,
    pub sortable: bool,
    pub width: u16,
}

fn column(key: &str, title: &str, kind: Kind, sort: Option<&'static str>, width: u16) -> Column {
    Column { key: key.into(), title: title.into(), kind, sort, sortable: sort.is_some(), width }
}

pub struct Tab {
    pub key: &'static str,
    pub title: &'static str,
    /// A key of the store's SCOPES.
    pub scope: &'static str,
    /// Keys of the store's FILTERS, `all` first.
    pub filters: &'static [&'static str],
    /// Catalog categories whose checks are filters too.
    pub categories: &'static [&'static str],
    /// Further check codes that belong here.
    pub codes: &'static [&'static str],
    pub columns: fn() -> Vec<Column>,
}

use Kind::*;

fn url() -> Column {
    column("url", "Адрес", Url, Some("url"), 340)
}
fn status() -> Column {
    column("status", "Код", Status, Some("status"), 64)
}
fn indexable() -> Column {
    column("indexable", "Индексация", Indexable, None, 116)
}
fn issues() -> Column {
    column("issues", "Проблем", Issues, None, 80)
}

pub static TABS: &[Tab] = &[
    Tab {
        key: "internal",
        title: "Внутренние",
        scope: "internal",
        filters: &["all", "html", "js", "css", "image", "pdf", "font", "data"],
        categories: &[],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("content_type", "Тип", Text, Some("content_type"), 120),
                status(),
                indexable(),
                column("title", "Title", Text, Some("title"), 260),
                column("title_length", "Длина title", Length { low: 30, high: 60 }, Some("title_length"), 84),
                column("description_length", "Длина description", Length { low: 70, high: 160 }, Some("description_length"), 90),
                column("h1", "H1", Text, Some("h1"), 200),
                column("words", "Слов", Number, Some("word_count"), 70),
                column("depth", "Глубина", Number, Some("depth"), 72),
                column("ttfb", "Ответ", Ms, Some("ttfb_ms"), 80),
                column("size", "Размер", Bytes, Some("size"), 84),
                column("links", "Ссылок", Number, Some("links"), 72),
                issues(),
                column("rendered", "JS", Bool, None, 48),
            ]
        },
    },
    Tab {
        key: "external",
        title: "Внешние",
        scope: "external",
        filters: &["all", "html", "image", "js", "css", "2xx", "3xx", "4xx", "5xx", "no_response"],
        categories: &[],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("content_type", "Тип", Text, Some("content_type"), 140),
                status(),
                column("error", "Ошибка", Text, None, 280),
                column("ttfb", "Ответ", Ms, Some("ttfb_ms"), 80),
            ]
        },
    },
    Tab {
        key: "response_codes",
        title: "Коды ответов",
        scope: "all",
        filters: &["all", "internal", "external", "2xx", "3xx", "4xx", "5xx", "no_response", "blocked", "slow"],
        categories: &["response_codes"],
        codes: &[],
        columns: || {
            vec![
                url(),
                status(),
                column("location", "Редирект на", Url, None, 300),
                column("error", "Ошибка", Text, None, 240),
                column("internal", "Внутренний", Bool, None, 90),
                column("ttfb", "Ответ", Ms, Some("ttfb_ms"), 80),
                column("content_type", "Тип", Text, Some("content_type"), 120),
            ]
        },
    },
    Tab {
        key: "url",
        title: "URL",
        scope: "pages",
        filters: &["all"],
        categories: &["url"],
        codes: &[],
        columns: || vec![url(), column("url_length", "Длина", Number, None, 72), status(), column("depth", "Глубина", Number, Some("depth"), 72), issues()],
    },
    Tab {
        key: "titles",
        title: "Title",
        scope: "pages",
        filters: &["all"],
        categories: &["page_titles"],
        codes: &[],
        columns: || {
            vec![
                url(),
                status(),
                column("title", "Title", Text, Some("title"), 380),
                column("title_length", "Длина", Length { low: 30, high: 60 }, Some("title_length"), 70),
                column("title_count", "Тегов", Number, None, 64),
                indexable(),
            ]
        },
    },
    Tab {
        key: "descriptions",
        title: "Description",
        scope: "pages",
        filters: &["all"],
        categories: &["meta_description"],
        codes: &[],
        columns: || {
            vec![
                url(),
                status(),
                column("description", "Description", Text, Some("description"), 420),
                column("description_length", "Длина", Length { low: 70, high: 160 }, Some("description_length"), 70),
                column("description_count", "Тегов", Number, None, 64),
                indexable(),
            ]
        },
    },
    Tab {
        key: "headings",
        title: "H1 и H2",
        scope: "pages",
        filters: &["all"],
        categories: &["headings"],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("h1", "H1", Text, Some("h1"), 280),
                column("h1_length", "Длина", Length { low: 10, high: 70 }, Some("h1_length"), 64),
                column("h1_count", "H1 всего", Number, None, 72),
                column("h1_second", "H1 второй", Text, None, 200),
                column("h2", "H2", Text, None, 260),
                column("h2_count", "H2 всего", Number, None, 72),
            ]
        },
    },
    Tab {
        key: "content",
        title: "Контент",
        scope: "pages",
        filters: &["all"],
        categories: &["content"],
        codes: &[],
        columns: || {
            vec![
                url(),
                status(),
                column("words", "Слов", Number, Some("word_count"), 72),
                column("html_size", "HTML", Bytes, Some("size"), 84),
                column("dom", "Элементов DOM", Number, None, 110),
                column("duplicate_ids", "Повторов id", Number, None, 96),
                column("lang", "Язык", Text, None, 70),
                issues(),
            ]
        },
    },
    Tab {
        key: "images",
        title: "Изображения",
        scope: "images",
        filters: &["all", "internal", "external", "large", "4xx"],
        categories: &["images"],
        codes: &[],
        columns: || {
            vec![
                url(),
                status(),
                column("size", "Размер", Bytes, Some("size"), 90),
                column("content_type", "Тип", Text, Some("content_type"), 120),
                column("images", "Картинок на странице", Number, None, 150),
                column("images_without_alt", "Без alt", Number, None, 80),
            ]
        },
    },
    Tab {
        key: "canonicals",
        title: "Canonical",
        scope: "pages",
        filters: &["all"],
        categories: &["canonicals"],
        codes: &[],
        columns: || {
            vec![
                url(),
                status(),
                column("canonical", "Canonical", Url, Some("canonical"), 360),
                column("canonical_state", "Указывает", Text, None, 110),
                indexable(),
            ]
        },
    },
    Tab {
        key: "directives",
        title: "Директивы",
        scope: "pages",
        filters: &["all", "noindex", "in_sitemap"],
        categories: &["indexability"],
        codes: &[],
        columns: || {
            vec![
                url(),
                status(),
                indexable(),
                column("meta_robots", "Meta robots", Text, None, 200),
                column("x_robots", "X-Robots-Tag", Text, None, 180),
                column("refresh", "Meta refresh", Text, None, 180),
                column("in_sitemap", "В sitemap", Bool, None, 84),
            ]
        },
    },
    Tab {
        key: "hreflang",
        title: "Hreflang",
        scope: "pages",
        filters: &["all"],
        categories: &["hreflang"],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("lang", "Язык", Text, None, 70),
                column("hreflang_count", "Hreflang", Number, None, 84),
                column("hreflangs", "Варианты", Text, None, 520),
            ]
        },
    },
    Tab {
        key: "pagination",
        title: "Пагинация",
        scope: "pages",
        filters: &["all"],
        categories: &[],
        codes: &["pagination_link_missing"],
        columns: || {
            vec![url(), status(), column("next", "rel=next", Url, None, 320), column("prev", "rel=prev", Url, None, 320)]
        },
    },
    Tab {
        key: "links",
        title: "Ссылки",
        scope: "pages",
        filters: &["all"],
        categories: &["links"],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("links", "Ссылок", Number, Some("links"), 72),
                column("links_internal", "Внутренних", Number, None, 92),
                column("links_external", "Внешних", Number, None, 80),
                column("links_nofollow", "nofollow", Number, None, 80),
                column("depth", "Глубина", Number, Some("depth"), 72),
                issues(),
            ]
        },
    },
    Tab {
        key: "javascript",
        title: "JavaScript",
        scope: "pages",
        filters: &["all", "rendered"],
        categories: &["javascript"],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("rendered", "Рендер", Bool, None, 70),
                column("render_error", "Ошибка рендера", Text, None, 200),
                column("settled", "Дождался", Bool, None, 84),
                column("console_errors", "Ошибок консоли", Number, None, 110),
                column("failed_requests", "Запросов с ошибкой", Number, None, 130),
                column("words_raw", "Слов в HTML", Number, None, 96),
                column("words_rendered", "Слов после JS", Number, None, 110),
                column("links_raw", "Ссылок в HTML", Number, None, 104),
                column("links_rendered", "Ссылок после JS", Number, None, 120),
            ]
        },
    },
    Tab {
        key: "structured_data",
        title: "Разметка",
        scope: "pages",
        filters: &["all"],
        categories: &["structured_data"],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("sd_formats", "Форматы", Text, None, 140),
                column("sd_types", "Типы", Text, None, 240),
                column("sd_features", "Расширенные результаты", Text, None, 220),
                column("sd_errors", "Ошибок", Number, None, 72),
                column("sd_warnings", "Предупреждений", Number, None, 110),
            ]
        },
    },
    Tab {
        key: "security",
        title: "Безопасность",
        scope: "pages",
        filters: &["all"],
        categories: &["security", "mobile"],
        codes: &[],
        columns: || {
            vec![
                url(),
                column("https", "HTTPS", Bool, None, 64),
                column("hsts", "HSTS", Bool, None, 64),
                column("csp", "CSP", Bool, None, 60),
                column("xcto", "X-Content-Type-Options", Bool, None, 170),
                column("xfo", "X-Frame-Options", Bool, None, 130),
                column("referrer_policy", "Referrer-Policy", Bool, None, 120),
                column("viewport", "Viewport", Text, None, 220),
            ]
        },
    },
];

/// A tab as one crawl shows it: the registry's tabs, then the custom
/// extraction tab when the crawl had rules.
pub struct TabView {
    pub key: &'static str,
    pub title: &'static str,
    pub scope: &'static str,
    pub filters: &'static [&'static str],
    pub categories: &'static [&'static str],
    pub codes: &'static [&'static str],
    pub columns: Vec<Column>,
}

pub fn tabs_of(config: &Value) -> Vec<TabView> {
    let mut tabs: Vec<TabView> = TABS
        .iter()
        .map(|tab| TabView {
            key: tab.key,
            title: tab.title,
            scope: tab.scope,
            filters: tab.filters,
            categories: tab.categories,
            codes: tab.codes,
            columns: (tab.columns)(),
        })
        .collect();
    let custom = custom_columns(config);
    if custom.len() > 2 {
        tabs.push(TabView {
            key: "custom",
            title: "Своё извлечение",
            scope: "pages",
            filters: &["all"],
            categories: &[],
            codes: &[],
            columns: custom,
        });
    }
    tabs
}

/// The tab an issue belongs to.
pub fn tab_of_issue(check: &seo_checks::CheckDef) -> Option<&'static str> {
    TABS.iter()
        .find(|tab| tab.codes.contains(&check.code))
        .or_else(|| TABS.iter().find(|tab| tab.categories.contains(&check.category)))
        .map(|tab| tab.key)
}

/// The cells of a row, in the order of the columns.
pub fn cells(columns: &[Column], row: &StoredRow) -> Vec<Value> {
    let mut structured = None;
    columns.iter().map(|column| cell(&column.key, row, &mut structured)).collect()
}

/// A cell as CSV text: Russian words where the UI shows labels.
pub fn csv_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "да".into(),
        Value::Bool(false) => "нет".into(),
        Value::String(text) => match text.as_str() {
            "indexable" => "индексируется".into(),
            "noindex" => "noindex".into(),
            "canonicalised" => "canonical на другой URL".into(),
            "redirect" => "редирект".into(),
            "error" => "ошибка".into(),
            "blocked" => "закрыт в robots.txt".into(),
            _ => text.clone(),
        },
        other => other.to_string(),
    }
}

/// The custom extraction tab, when the crawl had rules: a column per
/// search and per extractor, by the names in the crawl's settings.
pub fn custom_columns(config: &Value) -> Vec<Column> {
    let mut columns = vec![url(), status()];
    let named = |kind: &str| -> Vec<String> {
        config["custom"][kind]
            .as_array()
            .map(|specs| specs.iter().filter_map(|spec| spec["name"].as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    for name in named("search") {
        columns.push(Column { key: format!("custom_search:{name}"), title: name, kind: Number, sort: None, sortable: false, width: 120 });
    }
    for name in named("extract") {
        columns.push(Column { key: format!("custom_extract:{name}"), title: name, kind: Text, sort: None, sortable: false, width: 240 });
    }
    columns
}

/// The value of a column for one stored URL.
pub fn cell(key: &str, row: &StoredRow, structured: &mut Option<seo_checks::StructuredData>) -> Value {
    let stored = &row.url;
    let http = &stored.http;
    let facts = stored.facts();
    let with_facts = |read: &dyn Fn(&PageFacts) -> Value| facts.map_or(Value::Null, read);
    let count = |value: usize| json!(value);
    match key {
        "url" => json!(http.url),
        "status" => json!(http.status),
        "error" => json!(http.error),
        "location" => json!(http.location),
        "content_type" => json!(http.media_type()),
        "internal" => json!(stored.internal),
        "depth" => json!(stored.depth),
        "ttfb" => if http.status > 0 { json!(http.ttfb_ms) } else { Value::Null },
        "size" | "html_size" => json!(http.size),
        "in_sitemap" => json!(stored.in_sitemap),
        "indexable" => json!(indexability(row)),
        "issues" => json!(row.issues),
        "url_length" => count(http.url.chars().count()),
        "title" => with_facts(&|facts| json!(facts.title())),
        "title_length" => with_facts(&|facts| json!(facts.title().map(|title| title.chars().count()))),
        "title_count" => with_facts(&|facts| count(facts.titles.len())),
        "description" => with_facts(&|facts| json!(facts.description())),
        "description_length" => with_facts(&|facts| json!(facts.description().map(|text| text.chars().count()))),
        "description_count" => with_facts(&|facts| count(facts.descriptions.len())),
        "h1" => with_facts(&|facts| json!(facts.h1().next())),
        "h1_length" => with_facts(&|facts| json!(facts.h1().next().map(|text| text.chars().count()))),
        "h1_count" => with_facts(&|facts| count(facts.h1().count())),
        "h1_second" => with_facts(&|facts| json!(facts.h1().nth(1))),
        "h2" => with_facts(&|facts| json!(facts.h2().next())),
        "h2_count" => with_facts(&|facts| count(facts.h2().count())),
        "words" => with_facts(&|facts| count(facts.word_count)),
        "dom" => with_facts(&|facts| count(facts.dom_elements)),
        "duplicate_ids" => with_facts(&|facts| count(facts.duplicate_ids.len())),
        "lang" => with_facts(&|facts| json!(facts.lang)),
        "links" => with_facts(&|facts| count(facts.links.len())),
        "links_internal" | "links_external" => with_facts(&|facts| {
            let host = url::Url::parse(&http.url).ok().and_then(|url| url.host_str().map(str::to_string));
            let internal = facts
                .links
                .iter()
                .filter(|link| url::Url::parse(&link.href).ok().and_then(|url| url.host_str().map(str::to_string)) == host)
                .count();
            count(if key == "links_internal" { internal } else { facts.links.len() - internal })
        }),
        "links_nofollow" => with_facts(&|facts| count(facts.links.iter().filter(|link| link.nofollow).count())),
        "images" => with_facts(&|facts| count(facts.images.len())),
        "images_without_alt" => with_facts(&|facts| count(facts.images.iter().filter(|image| image.alt.is_none()).count())),
        "canonical" => with_facts(&|facts| json!(facts.canonicals.first())),
        "canonical_state" => with_facts(&|facts| {
            json!(match facts.canonicals.as_slice() {
                [] => "нет",
                [only] if only == &http.url => "на себя",
                [_] => "на другой URL",
                _ => "несколько",
            })
        }),
        "meta_robots" => with_facts(&|facts| json!((!facts.meta_robots.is_empty()).then(|| facts.meta_robots.join(", ")))),
        "x_robots" => {
            let values: Vec<&str> = http.header_values("x-robots-tag").collect();
            json!((!values.is_empty()).then(|| values.join(", ")))
        }
        "refresh" => with_facts(&|facts| {
            json!(facts.meta_refresh.as_ref().map(|(delay, target)| format!("{delay} с, {}", target.as_deref().unwrap_or(""))))
        }),
        "hreflang_count" => with_facts(&|facts| count(facts.hreflangs.len())),
        "hreflangs" => with_facts(&|facts| {
            json!(facts.hreflangs.iter().map(|item| format!("{} {}", item.lang, item.href)).collect::<Vec<_>>().join("; "))
        }),
        "next" | "prev" => with_facts(&|facts| {
            json!(facts.pagination.iter().find(|item| item.rel == key).map(|item| item.href.clone()))
        }),
        "rendered" => json!(stored.rendered.is_some()),
        "render_error" => json!(stored.render.as_ref().and_then(|render| render.error.clone())),
        "settled" => json!(stored.render.as_ref().map(|render| render.settled)),
        "console_errors" => json!(stored.render.as_ref().map(|render| render.console_errors)),
        "failed_requests" => json!(stored.render.as_ref().map(|render| render.failed_requests)),
        "words_raw" => json!(stored.raw.as_ref().map(|facts| facts.word_count)),
        "words_rendered" => json!(stored.rendered.as_ref().map(|facts| facts.word_count)),
        "links_raw" => json!(stored.raw.as_ref().map(|facts| facts.links.len())),
        "links_rendered" => json!(stored.rendered.as_ref().map(|facts| facts.links.len())),
        "sd_formats" | "sd_types" | "sd_features" | "sd_errors" | "sd_warnings" => {
            let Some(facts) = facts else { return Value::Null };
            let report = structured.get_or_insert_with(|| seo_checks::structured_data(facts));
            match key {
                "sd_formats" => json!(report.formats.join(", ")),
                "sd_types" => json!(report.types.join(", ")),
                "sd_features" => json!(report.features.join(", ")),
                "sd_errors" => json!(report.errors()),
                _ => json!(report.warnings()),
            }
        }
        "https" => json!(http.url.starts_with("https://")),
        "hsts" => json!(http.header("strict-transport-security").is_some()),
        "csp" => json!(http.header("content-security-policy").is_some() || facts.is_some_and(|facts| facts.csp_meta)),
        "xcto" => json!(http.header("x-content-type-options").is_some()),
        "xfo" => json!(http.header("x-frame-options").is_some()),
        "referrer_policy" => json!(http.header("referrer-policy").is_some()),
        "viewport" => with_facts(&|facts| json!(facts.viewport)),
        _ => match key.split_once(':') {
            Some(("custom_search", name)) => with_facts(&|facts| json!(facts.custom.count(name))),
            Some(("custom_extract", name)) => with_facts(&|facts| {
                let values = facts.custom.values(name);
                json!((!values.is_empty()).then(|| values.join(" | ")))
            }),
            _ => Value::Null,
        },
    }
}

/// Whether a page is indexable, as the overview counts it; empty for what
/// is not a page.
fn indexability(row: &StoredRow) -> &'static str {
    let stored = &row.url;
    let http = &stored.http;
    if stored.blocked_by_robots {
        return "blocked";
    }
    if (300..400).contains(&http.status) {
        return "redirect";
    }
    if http.status == 0 || http.status >= 400 {
        return "error";
    }
    let Some(facts) = stored.facts() else { return "" };
    if facts.robots_tokens().any(|token| token == "noindex")
        || http.header_values("x-robots-tag").any(|value| value.to_ascii_lowercase().contains("noindex"))
    {
        return "noindex";
    }
    match facts.canonicals.first() {
        Some(canonical) if canonical != &http.url => "canonicalised",
        _ => "indexable",
    }
}

/// The filters of a tab with their counts: built-in ones from the store's
/// counts, by (scope, filter), then its checks by importance.
pub fn filters(tab: &TabView, counts: &HashMap<(&str, &str), i64>, issues: &HashMap<String, i64>) -> Vec<Value> {
    let mut found: Vec<Value> = tab
        .filters
        .iter()
        .map(|key| json!({ "key": key, "count": counts.get(&(tab.scope, *key)).copied().unwrap_or(0) }))
        .collect();
    let mut checks: Vec<&seo_checks::CheckDef> = seo_checks::CATALOG
        .iter()
        .filter(|check| tab.categories.contains(&check.category) || tab.codes.contains(&check.code))
        .collect();
    checks.sort_by_key(|check| check.severity);
    for check in checks {
        found.push(json!({
            "key": format!("issue:{}", check.code),
            "title": check.title,
            "description": check.description,
            "severity": check.severity.as_str(),
            "count": issues.get(check.code).copied().unwrap_or(0),
        }));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use seo_page::{CustomFacts, HttpInfo, Source};
    use seo_store::StoredUrl;

    fn row(html: &str, headers: &[(&str, &str)]) -> StoredRow {
        let url = "https://example.com/catalog";
        let mut facts = seo_page::extract(html, &url::Url::parse(url).unwrap(), Source::Raw);
        facts.custom = CustomFacts {
            extracted: vec![("price".into(), vec!["10".into(), "12".into()])],
            found: vec![("kettle".into(), 2)],
            errors: Vec::new(),
        };
        let http = HttpInfo {
            url: url.into(),
            status: 200,
            content_type: Some("text/html; charset=utf-8".into()),
            headers: headers.iter().map(|(name, value)| (name.to_string(), value.to_string())).collect(),
            ttfb_ms: 120,
            size: 2048,
            ..Default::default()
        };
        let url = StoredUrl {
            id: 1,
            http,
            internal: true,
            depth: Some(1),
            in_sitemap: true,
            blocked_by_robots: false,
            raw: Some(facts),
            rendered: None,
            render: None,
        };
        StoredRow { url, issues: 3 }
    }

    const PAGE: &str = r#"<html lang="ru"><head>
        <title>Чайники и самовары</title>
        <meta name="description" content="Каталог чайников">
        <meta name="viewport" content="width=device-width">
        <meta name="robots" content="index, follow">
        <link rel="canonical" href="https://example.com/catalog">
        <link rel="alternate" hreflang="en" href="https://example.com/en/catalog">
        <link rel="next" href="https://example.com/catalog?page=2">
        <script type="application/ld+json">{"@context":"https://schema.org","@type":"Product","name":"Чайник"}</script>
        </head><body><h1>Чайники</h1><h1>Самовары</h1><h2>Новинки</h2>
        <p>Здесь продают чайники</p>
        <a href="/kettle">чайник</a><a href="https://other.org/" rel="nofollow">другой сайт</a>
        <img src="/a.jpg"><img src="/b.jpg" alt="b">
        </body></html>"#;

    #[test]
    fn every_column_of_every_tab_has_a_value() {
        let config = json!({ "custom": { "extract": [{ "name": "price", "css": ".price" }], "search": [{ "name": "kettle", "text": "чайник" }] } });
        let page = row(PAGE, &[("strict-transport-security", "max-age=1")]);
        // What a rendered-free, error-free page has no value for.
        let empty = [
            "error", "location", "x_robots", "refresh", "prev", "render_error", "settled", "console_errors",
            "failed_requests", "words_rendered", "links_rendered",
        ];
        let tabs = tabs_of(&config);
        assert_eq!(tabs.last().map(|tab| tab.key), Some("custom"));
        for tab in &tabs {
            assert!(tab.columns.iter().all(|column| column.sort.is_some() == column.sortable));
            for (column, value) in tab.columns.iter().zip(cells(&tab.columns, &page)) {
                assert!(
                    !value.is_null() || empty.contains(&column.key.as_str()),
                    "{}: {} has no value",
                    tab.key,
                    column.key
                );
            }
        }
        let value = |key: &str| cell(key, &page, &mut None);
        assert_eq!(value("title_length"), json!(18));
        assert_eq!(value("h1_count"), json!(2));
        assert_eq!(value("h1_second"), json!("Самовары"));
        assert_eq!(value("indexable"), json!("indexable"));
        assert_eq!(value("canonical_state"), json!("на себя"));
        assert_eq!(value("links_internal"), json!(1));
        assert_eq!(value("links_nofollow"), json!(1));
        assert_eq!(value("images_without_alt"), json!(1));
        assert_eq!(value("next"), json!("https://example.com/catalog?page=2"));
        assert_eq!(value("sd_types"), json!("Product"));
        assert_eq!(value("hsts"), json!(true));
        assert_eq!(value("csp"), json!(false));
        assert_eq!(value("custom_search:kettle"), json!(2));
        assert_eq!(value("custom_extract:price"), json!("10 | 12"));
        assert_eq!(csv_text(&value("in_sitemap")), "да");

        let noindex = row(&PAGE.replace("index, follow", "noindex"), &[]);
        assert_eq!(cell("indexable", &noindex, &mut None), json!("noindex"));
        let elsewhere = row(&PAGE.replace("href=\"https://example.com/catalog\"", "href=\"https://example.com/\""), &[]);
        assert_eq!(cell("indexable", &elsewhere, &mut None), json!("canonicalised"));
    }

    #[test]
    fn issue_filters_come_from_the_catalog() {
        let tab = tabs_of(&Value::Null).into_iter().find(|tab| tab.key == "titles").unwrap();
        let issues = HashMap::from([("title_missing".to_string(), 4)]);
        let filters = filters(&tab, &HashMap::from([(("pages", "all"), 10)]), &issues);
        assert_eq!(filters[0]["count"], 10);
        assert!(filters.iter().any(|filter| filter["key"] == "issue:title_missing" && filter["count"] == 4));
        assert!(filters.iter().skip(1).all(|filter| filter["key"].as_str().unwrap().starts_with("issue:")));
        // Every check of the catalog is a filter of some tab.
        for check in seo_checks::CATALOG {
            assert!(tab_of_issue(check).is_some(), "{} has no tab", check.code);
        }
    }
}
