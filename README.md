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

How much is downloaded: `--fetch status|head|full` (head stops at `</head>`),
what is extracted: `--extract all|head|links,headings,text,...`, what is kept:
`--store-html raw|rendered|both`, `--store-links` (link rows for SQL; exports
and checks read links from the stored facts). A crawl that follows links
turns on what that needs: link extraction, or sitemaps when no `<body>` is
read. Checks whose input was not extracted report nothing.

Speed and politeness: `--concurrency` (the site), `--external-concurrency`
(other hosts, at most 4 per host; a host that does not connect is not asked
again), `--max-rps`, robots.txt `Crawl-delay`, `--retries`, `--http2`
(on in the `minimal` preset, where it is several times faster). TLS:
`--ca-cert file.pem` trusts more root certificates (sites on the Russian
Trusted Root CA, such as tbank.ru, fail with UnknownIssuer otherwise),
`--insecure` accepts any certificate (the crawler's requests only; the
renderer trusts `--ca-cert` too). Rendering:
`--render-threads`, settle timings (`--settle-content-quiet-ms`, off by
default: settle once the content is still while animations run), the
per-page layout budget (`--layout-budget-ms`), third-party stall limit,
tracker blocking, the shared script/style cache and the fetch()/XHR
cache, and what the pages see: `--timezone Europe/Moscow --locale ru-RU`
(the locale also becomes the crawl's Accept-Language). `--reuse-documents`
renders the page the crawler downloaded instead of requesting it again
(one request per page less, but the page is the one served to the
crawler's user agent). Ctrl-C stops a crawl and keeps what it got. See
`seo-spider crawl --help`.

Export tabs: `summary`, `internal`, `external`, `redirects`, `javascript`
(raw HTML against the rendered DOM), `links`, `issues`.

## Acceptance site

`tests/site/server.py` serves one known case per page; `tests/site/EXPECTED.md`
lists what a crawl must report.

```bash
python3 tests/site/server.py 8099
seo-spider crawl http://127.0.0.1:8099/ --db site.sqlite --render-threads 4 --sitemaps
```
