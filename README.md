# seo-spider

SEO crawler in the spirit of Screaming Frog SEO Spider, in Rust, with
JavaScript rendering on an embedded [obscura](https://github.com/annel0/obscura)
engine (branch `fix/chrome-parity`) instead of headless Chrome.

## Layout

| Crate | What it does |
|---|---|
| `seo-page` | Page model (`HttpInfo`, `PageFacts`) and HTML extraction, shared by raw and rendered documents |
| `seo-render` | Pool of render threads, each with its own obscura `Page`; snapshots once the content settles |
| `seo-crawl` | Frontier, HTTP fetch without following redirects, robots.txt, sitemaps, click depth, checks run |
| `seo-checks` | 100 checks: the SEOnaut catalog (MIT) plus the JavaScript tab and a few more |
| `seo-store` | SQLite storage: crawls, URLs, facts (zstd JSON), renders, kept documents, issues |
| `seo-cli` | The `seo-spider` binary |

`spike/` is the throwaway benchmark that settled the architecture.

## Build

obscura must be checked out next to this repository (`../obscura`); the
workspace repeats its `[patch.crates-io]` and `panic = "unwind"`.

```bash
cargo build --release -p seo-cli
```

## Use

```bash
seo-spider crawl https://example.com/ --db example.sqlite --render-threads 8
seo-spider export --db example.sqlite -o example.xlsx
seo-spider export --db example.sqlite --format csv -o example-csv/
seo-spider check --db example.sqlite
seo-spider report --db example.sqlite
seo-spider compare before.sqlite after.sqlite
seo-spider bodies --db example.sqlite -o docs/
```

Without `--render-threads` only the raw HTML is read. With it every HTML page
is also rendered and links found only in the rendered DOM are crawled too.

### Settings

Settings come in layers: a preset, then a TOML file (`--config`), then flags.
`--print-config` prints the result as TOML, a starting point for a file.

| Preset | Reads |
|---|---|
| `headers` | status and response headers (HEAD, no body) of the start URLs and sitemap URLs |
| `minimal` | status, headers and `<head>` of the start URLs and sitemap URLs; nothing else requested |
| `seo` (default) | whole raw HTML, links followed, resources and external links checked |
| `js` | `seo` plus rendering |
| `dump` | `js` keeping the raw and rendered documents (`seo-spider bodies` writes them out) |

How much is downloaded: `--fetch status|head|full` (head stops at `</head>`;
`--head-drain-kb 512` reads and drops up to 512 KB more so the connection is
reused: 7 sites took 22 s instead of 44 s, reading 302 MB instead of 38),
what is extracted: `--extract all|head|links,headings,text,...`, what is kept:
`--store-html raw|rendered|both`, `--store-links` (link rows for SQL; exports
and checks read links from the stored facts). A crawl that follows links
turns on what that needs: link extraction, or sitemaps when no `<body>` is
read. Checks whose input was not extracted report nothing.

Speed and politeness: `--concurrency` (the site), `--external-concurrency`
(other hosts, at most 4 per host; a host that does not connect is not asked
again), `--max-rps`, robots.txt `Crawl-delay`, automatic slowing down
while the site answers 429 or 503 (`--no-auto-throttle` turns it off;
drom.ru: 79 of 200 URLs failed without it, none with it), `--retries`, `--http2`
(faster for `minimal` where it works, but off everywhere by default: cut
reads reset HTTP/2 streams, and sites guarding against rapid reset, such
as webflow.com and ghost.org, then fail or answer 403). TLS:
`--ca-cert file.pem` trusts more root certificates (sites on the Russian
Trusted Root CA, such as tbank.ru, fail with UnknownIssuer otherwise),
`--insecure` accepts any certificate; both apply to rendering too. `--header` goes to rendering as well:
cookies into the renderer's jar for the start hosts, other headers with
every request, except Authorization, which would reach third parties.
gazeta.ru and lenta.ru send a crawler to a Sber ID page without
`--header "Cookie: unity_pause_sso=1"`. Rendering:
`--render-threads`, settle timings (`--settle-content-quiet-ms`, off by
default: settle once the content is still while animations run), the
per-page layout budget (`--layout-budget-ms`), third-party stall limit,
tracker blocking, the shared script/style cache and the fetch()/XHR
cache, and what the pages see: `--timezone Europe/Moscow --locale ru-RU`
(the locale also becomes the crawl's Accept-Language). Pages see
`navigator.connection.saveData`, so Nuxt, Gatsby and quicklink do not
prefetch every linked page (`--no-save-data` lets them). `--reuse-documents`
renders the page the crawler downloaded instead of requesting it again
(one request per page less, but the page is the one served to the
crawler's user agent). Ctrl-C stops a crawl and keeps what it got. See
`seo-spider crawl --help`.

### Custom extraction and search

Values pulled out of every internal HTML page, and text counted on it, as in
Screaming Frog. On the command line:

```
seo-spider crawl https://example.com/ \
  --extract-css 'price=.product .price' \
  --extract-xpath 'author=//meta[@name="author"]/@content' \
  --extract-xpath 'images=count(//img)' \
  --extract-regex 'gtm=(GTM-[A-Z0-9]+)' \
  --search 'out of stock=нет в наличии' --search-regex 'phone=\+7[\d ()-]{10,}'
```

In `--config`, with attributes, HTML output and a search in the visible
text:

```toml
[[custom.extract]]
name = "og image"
css = 'meta[property="og:image"]'
attribute = "content"
max = 1                 # values per page (10 by default)

[[custom.extract]]
name = "first paragraph"
xpath = "(//main//p)[1]"
output = "html"         # text (default), html or inner_html

[[custom.search]]
name = "sale"
text = "скидка"         # any case; or regex = "..."
scope = "text"          # html (default) or text
```

The rules run on the raw document and, when pages are rendered, on the
rendered one; the `custom` export tab shows the document each page was
judged by, a column per search (how many times it was found) and per value.
A rule that does not compile stops the crawl before it starts. Cost on the
100k-page bench: regex and search nothing measurable, CSS +19% CPU, XPath
+36% (both parse a DOM).

### Near duplicates

Each page's main content (visible text outside header, nav, footer and
aside) gets a 128-bin MinHash signature of its five-word shingles, one hash
per shingle with 16 bits kept a bin: 256 bytes a page, +5% crawl CPU on the
100k-page bench. After the crawl, locality-sensitive hashing (16 bands of
8) picks the pairs to compare, and pages at or above
`checks.near_duplicate_threshold` (`--near-duplicate-threshold`, 0.9 by
default) get `near_duplicate_content` with the closest match and how many
more there are. Exact copies stay `duplicate_content`. `--extract` without
`similarity` skips the signature. allbirds.com: colour variants of a
product at 97-100%; drom.ru: driving-test tickets at 92-96%.

### Structured data

JSON-LD and microdata (read into the same shape) are checked against the
Schema.org 30.1 vocabulary (`crates/seo-checks/data/schemaorg.txt`, CC BY-SA
3.0): unknown types, properties Schema.org does not have or that do not apply
to the type, nested items of a type the property does not take. Items of the
rich results Google still shows (product, review, breadcrumbs, FAQ, Q&A,
recipe, event, job, local business, organization, video, app, dataset, movie,
forum post, profile, image metadata) are checked for its required and
recommended properties. The `structured_data` export tab lists, per page,
the formats, types, rich results, and each problem; the issues are
`structured_data_parse_error`, `schema_type_unknown`,
`schema_property_invalid`, `rich_result_required_missing` and
`rich_result_recommended_missing`. RDFa is not read.

Export tabs: `summary`, `internal`, `external`, `redirects`, `javascript`
(raw HTML against the rendered DOM), `structured_data`, `custom`, `links`,
`issues`.

## Acceptance site

`tests/site/server.py` serves one known case per page; `tests/site/EXPECTED.md`
lists what a crawl must report.

```bash
python3 tests/site/server.py 8099
seo-spider crawl http://127.0.0.1:8099/ --db site.sqlite --render-threads 4 --sitemaps
```
