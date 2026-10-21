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
| `seo-checks` | 96 checks: the SEOnaut catalog (MIT) plus the JavaScript tab and a few more |
| `seo-store` | SQLite storage: crawls, URLs, facts, links, renders, issues |
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
```

Without `--render-threads` only the raw HTML is read. With it every HTML page
is also rendered and links found only in the rendered DOM are crawled too.
See `seo-spider crawl --help` for limits, robots.txt, nofollow, subdomains,
sitemaps and timeouts.

Export tabs: `summary`, `internal`, `external`, `redirects`, `javascript`
(raw HTML against the rendered DOM), `links`, `issues`.

## Acceptance site

`tests/site/server.py` serves one known case per page; `tests/site/EXPECTED.md`
lists what a crawl must report.

```bash
python3 tests/site/server.py 8099
seo-spider crawl http://127.0.0.1:8099/ --db site.sqlite --render-threads 4 --sitemaps
```
