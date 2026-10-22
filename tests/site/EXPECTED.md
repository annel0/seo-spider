# What a crawl of the test site must report

Start: `http://127.0.0.1:8099/`.

## Raw HTML (`--render-threads 0`)

| URL | Expected |
|---|---|
| `/` | 200, depth 0, title "Home", indexable |
| `/about` | 200, depth 1, 3 inlinks (nav, footer, `/about#team`), 1 unique inlink (`/`) |
| `/redirect-1` | 301 → `/redirect-2`, depth 1 |
| `/redirect-2` | 302 → `/about`, depth 1: a redirect is not a click |
| redirects tab | one chain `/redirect-1` → `/redirect-2` → `/about`, 2 hops, final 200; `/loop-a` ↔ `/loop-b` marked as a loop |
| `/missing` | 404, Client Error |
| `/noindex` | Non-Indexable, noindex |
| `/canonicalised` | Non-Indexable, Canonicalised |
| `/private/secret` | not requested, blocked by robots.txt |
| `/nofollow-target` | absent: the only link to it is nofollow |
| `https://example.com/` | in external, status checked |
| `/refresh` | meta refresh `0; /about-refresh`; `/about-refresh` crawled at depth 1 (a refresh is a redirect) |
| `/cp1251` | title "Привет", decoded from windows-1251 |
| `/img/ok.png` | 200 image/png, depth 1 (its page's depth + 1); `/img/missing.png` 404 |
| `/app.js` | 200, internal resource |
| `/js-only`, `/js-deep` | absent: only the script adds the link |
| `/api/items.json` | absent: requested by the script, not linked |
| `/product` | structured_data tab: JSON-LD and Microdata, types Product, Organization, Event; issues `structured_data_parse_error` (the Organization block is not JSON), `schema_property_invalid` (`Product.colour`), `rich_result_required_missing` (offer price, event startDate) |
| `/guide-a`, `/guide-b` | `near_duplicate_content` on both (the same guide but for one sentence, different menus); not `duplicate_content` |
| custom tab | with `--extract-css price=.price --search kettle=kettle`: `/product` price "1990 RUB", kettle found in its HTML |

## Rendered (`--render-threads N`)

Everything above, plus:

- `/` rendered: one link more than raw ("Links Only In Rendered" = 1 → `/js-only`), settled after the same-site fetch.
- `/js-only` crawled at depth 1, `/js-deep` at depth 2.

## With `--sitemaps`

- `/orphan` crawled, in sitemap, depth empty (no link reaches it).
- `/` and `/about` marked as in sitemap.
