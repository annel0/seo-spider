"""A small site with one known case per page, for crawler acceptance runs.

    python3 tests/site/server.py [port]

What a crawl from / should report is listed in EXPECTED.md next to this file.
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def page(title, body, head=""):
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>{title}</title>{head}</head><body>{body}</body></html>"""


HOME = page(
    "Home",
    """<header><nav><a href="/about">About</a></nav></header>
<main><h1>Home</h1><p>Every link below is one case for the crawler.</p>
<a href="/redirect-1">redirect chain</a>
<a href="/loop-a">redirect loop</a>
<a href="/missing">missing page</a>
<a href="/noindex">noindex page</a>
<a href="/canonicalised">canonicalised page</a>
<a href="/private/secret">disallowed by robots.txt</a>
<a href="/nofollow-target" rel="nofollow">nofollow link</a>
<a href="https://example.com/">external link</a>
<a href="/refresh">meta refresh</a>
<a href="/cp1251">windows-1251 page</a>
<a href="/about#team">about with a fragment</a>
<img src="/img/ok.png" alt="ok"><img src="/img/missing.png">
</main><footer><a href="/about">About again</a></footer>
<script src="/app.js"></script>""",
)

# Adds a link only after a same-site fetch() answers, like a SPA would.
APP_JS = """fetch('/api/items.json').then(r => r.json()).then(items => {
  for (const item of items) {
    const a = document.createElement('a');
    a.href = item.href;
    a.textContent = item.name;
    document.querySelector('main').appendChild(a);
  }
});"""

PAGES = {
    "/": ("text/html; charset=utf-8", HOME),
    "/about": ("text/html; charset=utf-8", page("About", '<h1>About</h1><a href="/">Home</a>')),
    "/about-refresh": ("text/html; charset=utf-8", page("Refreshed", "<h1>Refreshed</h1>")),
    "/noindex": (
        "text/html; charset=utf-8",
        page("Noindex", "<h1>Noindex</h1>", '<meta name="robots" content="noindex, follow">'),
    ),
    "/canonicalised": (
        "text/html; charset=utf-8",
        page("Canonicalised", "<h1>Canonicalised</h1>", '<link rel="canonical" href="/about">'),
    ),
    "/private/secret": ("text/html; charset=utf-8", page("Secret", "<h1>Secret</h1>")),
    "/nofollow-target": ("text/html; charset=utf-8", page("Nofollow", "<h1>Nofollow</h1>")),
    "/refresh": (
        "text/html; charset=utf-8",
        page("Refresh", "<p>moving</p>", '<meta http-equiv="refresh" content="0; url=/about-refresh">'),
    ),
    "/js-only": (
        "text/html; charset=utf-8",
        page("JS only", '<h1>Found through JavaScript</h1><a href="/js-deep">deeper</a>'),
    ),
    "/js-deep": ("text/html; charset=utf-8", page("JS deep", "<h1>Two clicks past the script</h1>")),
    "/orphan": ("text/html; charset=utf-8", page("Orphan", "<h1>Only in the sitemap</h1>")),
    "/app.js": ("application/javascript", APP_JS),
    "/img/ok.png": ("image/png", "PNG"),
}

REDIRECTS = {
    "/redirect-1": (301, "/redirect-2"),
    "/redirect-2": (302, "/about"),
    "/loop-a": (301, "/loop-b"),
    "/loop-b": (301, "/loop-a"),
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        path = self.path.split("?")[0]
        host = self.headers.get("Host", "127.0.0.1")
        if path in REDIRECTS:
            status, target = REDIRECTS[path]
            self.send_response(status)
            self.send_header("Location", target)
            self.end_headers()
            return
        if path == "/robots.txt":
            body = f"User-agent: *\nDisallow: /private/\nSitemap: http://{host}/sitemap.xml\n"
            return self.reply(200, "text/plain", body.encode())
        if path == "/sitemap.xml":
            locs = "".join(f"<url><loc>http://{host}{p}</loc></url>" for p in ["/", "/about", "/orphan"])
            body = f'<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{locs}</urlset>'
            return self.reply(200, "application/xml", body.encode())
        if path == "/api/items.json":
            time.sleep(0.3)
            return self.reply(200, "application/json", json.dumps([{"href": "/js-only", "name": "JS only"}]).encode())
        if path == "/cp1251":
            body = page("Привет", "<h1>Кириллица в windows-1251</h1>").replace('charset="utf-8"', 'charset="windows-1251"')
            return self.reply(200, "text/html; charset=windows-1251", body.encode("cp1251"))
        if path in PAGES:
            content_type, body = PAGES[path]
            return self.reply(200, content_type, body.encode())
        self.reply(404, "text/html; charset=utf-8", page("Not found", "<h1>404</h1>").encode())

    def reply(self, status, content_type, body):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


class Server(ThreadingHTTPServer):
    # socketserver listens with a backlog of 5: a crawler opening more
    # connections at once gets SYNs dropped and retried a second later.
    request_queue_size = 128
    daemon_threads = True


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8099
    Server(("127.0.0.1", port), Handler).serve_forever()
