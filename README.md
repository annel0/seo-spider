# seo-spider

[![build](https://github.com/annel0/seo-spider/actions/workflows/build.yml/badge.svg)](https://github.com/annel0/seo-spider/actions/workflows/build.yml)

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
| `seo-store` | Storage over SQLite, PostgreSQL or MySQL: crawls, URLs, facts (zstd JSON), renders, kept documents, issues |
| `seo-export` | The tabs as CSV files or one XLSX workbook |
| `seo-server` | The web UI's server: crawl control, progress, tabs, exports |
| `seo-cli` | The `seo-spider` binary |

`spike/` is the throwaway benchmark that settled the architecture.

## Download

Binaries for Linux (x64, glibc 2.35+), Windows (x64) and macOS (Apple
silicon), with the web UI built in, are attached to each
[release](https://github.com/annel0/seo-spider/releases); every push builds
them too (the run's artifacts). On macOS a downloaded binary is quarantined
until `xattr -d com.apple.quarantine seo-spider`.

## Build

obscura must be checked out next to this repository (`../obscura`), at the
revision in `OBSCURA_REV`; the workspace repeats its `[patch.crates-io]` and
`panic = "unwind"`. The UI needs Node 20 or later.

```bash
git clone https://github.com/annel0/obscura ../obscura
git -C ../obscura checkout "$(cat OBSCURA_REV)"
(cd ui && npm ci && npm run build)
cargo build --release -p seo-cli --features embed-ui
```

`.github/workflows/build.yml` does the same on Linux, Windows and macOS,
runs the tests, and publishes the binaries on a `v*` tag. Compiled
dependencies are cached, so a change that leaves obscura alone rebuilds
only this workspace's crates.

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
| `auto` | `seo`, rendering only if JavaScript changes what the crawl reads (below); the UI's default |
| `js` | `seo` plus rendering |
| `dump` | `js` keeping the raw and rendered documents (`seo-spider bodies` writes them out) |

The `auto` preset decides on rendering before the crawl: it reads the start
page and a page of each section the start page links to (8 pages in all,
`render.auto_pages`) both as the server sends them and rendered, and turns
rendering on only if JavaScript changes their text (a fifth more words),
links (any internal link no page has in its HTML), title, H1, description,
canonical or noindex. The decision and the pages behind it are kept with
the crawl's settings and shown in the UI. It takes a few seconds: 1 s on
the test site, 8 to 13 s on real ones. fabrikabloknotov.ru: no rendering
(10,364 -> 10,487 words, 1,581 -> 1,561 links on 7 pages); twitch.tv:
rendering (0 -> 643 words on the home page); vuejs.org: rendering (its
navigation menus, 11 links, exist only after JavaScript).

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

## Databases

A crawl goes into a SQLite file by default. `--db` also takes a PostgreSQL
or MySQL database, for every command and the UI:

```
export SEO_SPIDER_DB_PASSWORD=...          # or PGPASSWORD / MYSQL_PWD, or in the address
seo-spider crawl https://example.com/ --db postgres://seo@db.example/seo
seo-spider ui --db mysql://seo@db.example/seo
```

The tables are made on first use (PostgreSQL 12+, MySQL 8.0+), and one
database holds any number of crawls, as a file does. PostgreSQL connects
with TLS when the server offers it (`?sslmode=require` insists); MySQL
with `?sslmode=require`, checked against the web's roots (a server with
its own CA: `&root_cert_path=ca.pem`). The password is never printed:
output and logs show the address with `***`.

The store writes in bulk however the database takes it fastest:
`COPY ... BINARY` into PostgreSQL, multi-row prepared INSERTs into MySQL,
prepared inserts in one transaction into SQLite. The bench site, 100,000
pages (210,000 URLs), crawl and checks:

| store | crawl | checks | all | peak memory |
|---|---|---|---|---|
| SQLite | 6.2 s | 6.0 s | 12.4 s | 0.8 GB |
| PostgreSQL 17 | 9.5 s | 7.0 s | 16.7 s | 0.5 GB |
| MySQL 8.4 | 17.5 s | 12.4 s | 30.3 s | 0.6 GB |

(Servers in Docker on the same machine, default settings; MySQL with the
binary log off and a 2 GB buffer pool gains about a tenth.) The same site
crawled into all three exports the same rows.

## Continuing a crawl

```
seo-spider continue --db crawl.sqlite                  # the latest crawl
seo-spider continue --db crawl.sqlite --crawl 3 --max-urls 50000
```

A crawl that was interrupted (the process killed, the machine restarted)
or stopped (Ctrl-C, Stop in the UI) goes on where it was: what it wrote
stays, the links of its pages are found again from their stored facts,
and what nothing fetched yet is requested. Robots.txt and the sitemaps are
read again, the crawl is closed as one (depths over all its pages) and its
checks run. A finished crawl can be taken further with a higher
`--max-urls`. The queue of 100,000 crawled pages is rebuilt in about 5 s.
In the UI, an interrupted or stopped crawl has a Continue button.

## Web UI

```
cd ui && npm ci && npm run build
seo-spider ui --db crawl.sqlite --ui-dir ui/dist     # http://127.0.0.1:7070/
cargo build --release -p seo-cli --features embed-ui  # one binary, UI inside (after the build above)
```

Start a crawl from a URL and a preset, pause, resume or stop it, and watch
it: a band of answered, failed and queued URLs, and an overview of charts
(status codes and indexability as share bars, depth, response times,
content types, the main issues) that opens the table behind any part of
it.

The tabs follow Screaming Frog: internal, external, response codes, URL,
titles, descriptions, H1 and H2, content, images, canonicals, directives,
hreflang, pagination, links, JavaScript, structured data, security, and
custom extraction when the crawl had rules. Each has its built-in filters
and the catalog's checks of its category, with counts, and its columns
(Glide Data Grid), paged, sorted, filtered and searched on the server; a
row opens its detail panel. The issues view lists every issue found by
importance and opens the tab filtered to it. A tab exports as CSV with its
filter, search and sort; the whole crawl as XLSX.

The settings form is built from the config's JSON Schema
(`GET /api/config/schema`): every field of `CrawlConfig` shows, starting
from the chosen preset, with what differs marked and a way back. The crawl
starts with a JSON merge patch over the preset.

The server listens on localhost only unless given `--token`, which then
goes with every API request. `npm run dev` serves the UI with hot reload
and proxies `/api` to a running `seo-spider ui`. The UI builds where
native binaries may run (esbuild): not on a `noexec` mount.

## Acceptance site

`tests/site/server.py` serves one known case per page; `tests/site/EXPECTED.md`
lists what a crawl must report.

```bash
python3 tests/site/server.py 8099
seo-spider crawl http://127.0.0.1:8099/ --db site.sqlite --render-threads 4 --sitemaps
```
