#!/usr/bin/env python3
"""Checks crawls of the test site against EXPECTED.md: raw.sqlite (with
--extract-css price=.price --search kettle=kettle, exported as CSV to
out-raw), js.sqlite (rendered) and sm.sqlite (rendered, with --sitemaps),
in the current directory. Prints what does not hold; exits 1 if anything.

    python3 tests/site/check.py
"""
import csv
import sqlite3
import sys

U = "http://127.0.0.1:8099"
fails = []


def expect(cond, what):
    if not cond:
        fails.append(what)


def row(c, path):
    url = U + path if path.startswith("/") else path
    return c.execute("select status, depth, error, in_sitemap, location from urls where url=?", (url,)).fetchone()


def issues_at(c, path):
    return {code for (code,) in c.execute("select i.code from issues i join urls u on u.id = i.url_id where u.url = ?", (U + path,))}


for name in ["raw", "js", "sm"]:
    c = sqlite3.connect(f"{name}.sqlite")
    r = lambda path: row(c, path)
    expect(r("/")[:2] == (200, 0), f"{name}: / 200 depth 0: {r('/')}")
    expect(r("/about")[:2] == (200, 1), f"{name}: /about depth 1: {r('/about')}")
    expect(r("/redirect-1")[:2] == (301, 1), f"{name}: /redirect-1: {r('/redirect-1')}")
    expect(r("/redirect-2")[:2] == (302, 1), f"{name}: /redirect-2: {r('/redirect-2')}")
    expect(r("/missing")[0] == 404, f"{name}: /missing 404")
    secret = r("/private/secret")
    expect(secret and secret[0] == 0 and "robots" in (secret[2] or ""), f"{name}: /private/secret blocked: {secret}")
    expect(r("/nofollow-target") is None, f"{name}: /nofollow-target absent")
    expect(r("https://example.com/") is not None, f"{name}: example.com checked")
    expect((r("/about-refresh") or (0, -1))[1] == 1, f"{name}: /about-refresh depth 1: {r('/about-refresh')}")
    expect(r("/img/ok.png")[:2] == (200, 1), f"{name}: /img/ok.png: {r('/img/ok.png')}")
    expect(r("/img/missing.png")[0] == 404, f"{name}: /img/missing.png 404")
    expect(r("/app.js")[0] == 200, f"{name}: /app.js 200")
    expect(r("/api/items.json") is None, f"{name}: /api/items.json absent")
    facts = c.execute("select count(*) from urls u join facts f on f.url_id = u.id where u.url = ?", (U + "/cp1251",)).fetchone()[0]
    expect(facts > 0, f"{name}: /cp1251 has facts")
    if name == "raw":
        expect(r("/js-only") is None, "raw: /js-only absent")
    else:
        expect((r("/js-only") or (0, -1))[1] == 1, f"{name}: /js-only depth 1: {r('/js-only')}")
        expect((r("/js-deep") or (0, -1))[1] == 2, f"{name}: /js-deep depth 2: {r('/js-deep')}")
        rendered = c.execute("select count(*) from renders where error is null").fetchone()[0]
        expect(rendered > 0, f"{name}: pages rendered: {rendered}")
    if name == "sm":
        orphan = r("/orphan")
        expect(orphan is not None and orphan[1] is None and orphan[3] == 1, f"sm: /orphan in sitemap, no depth: {orphan}")
        expect(r("/")[3] == 1 and r("/about")[3] == 1, "sm: / and /about in sitemap")
    got = issues_at(c, "/product")
    for code in ["structured_data_parse_error", "schema_property_invalid", "rich_result_required_missing"]:
        expect(code in got, f"{name}: /product {code}: {sorted(got)}")
    for path in ["/guide-a", "/guide-b"]:
        got = issues_at(c, path)
        expect("near_duplicate_content" in got and "duplicate_content" not in got, f"{name}: {path} near duplicate: {sorted(got)}")
    scored = c.execute("select count(*) from link_scores where score is not null").fetchone()[0]
    expect(scored > 0, f"{name}: link scores: {scored}")

custom = {r["Address"]: r for r in csv.DictReader(open("out-raw/custom.csv", encoding="utf-8-sig"))}
product = custom.get(U + "/product", {})
expect(product.get("price") == "1990 RUB" and product.get("kettle", "0") not in ("", "0"), f"raw: custom /product: {product}")
structured = {r["Address"]: r for r in csv.DictReader(open("out-raw/structured_data.csv", encoding="utf-8-sig"))}
product = structured.get(U + "/product", {})
expect(
    product.get("Formats") == "JSON-LD, Microdata" and "Product" in product.get("Types", "") and "Event" in product.get("Types", ""),
    f"raw: structured_data /product: {product}",
)

print("\n".join(fails) if fails else "the test site: all expectations hold")
sys.exit(1 if fails else 0)
