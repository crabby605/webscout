"""Local test site for webscout: header echo, forms, a JS app and a fake bot wall.

    python3 dev/testsite.py [port]      (default 8787)

/echo            request line + headers in wire order (+ body), as text
/                links + GET search form + POST login form
/login (POST)    sets a session cookie, 303 -> /account
/account         shows who is logged in (from the cookie)
/app             JS app: typing, clicking, fetch(), DOM updates
/api/echo        fetch() target: echoes its own headers as JSON
/wall            "bot wall": 403 challenge unless cookie cleared=1;
                 /wall/solve has the button a human clicks to pass it
"""
import html, json, sys, urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PAGE = "<!doctype html><html><head><title>{title}</title></head><body>{body}</body></html>"

APP = """
<h1 id=status>ready</h1>
<input id=q name=q placeholder=search>
<select id=size><option value=s>S</option><option value=m>M</option></select>
<label><input type=checkbox id=agree> agree</label>
<button id=go>Go</button>
<ul id=out></ul>
<script>
  const q = document.getElementById('q'), out = document.getElementById('out');
  q.addEventListener('input', () => document.getElementById('status').textContent = 'typing: ' + q.value);
  document.getElementById('go').addEventListener('click', async () => {
    const r = await fetch('/api/echo?q=' + encodeURIComponent(q.value), { headers: { 'x-requested-with': 'app' } });
    const j = await r.json();
    out.innerHTML = '<li>q=' + j.query.q + '</li><li>size=' + document.getElementById('size').value +
      '</li><li>agree=' + document.getElementById('agree').checked + '</li><li>fetch-mode=' + j.headers['sec-fetch-mode'] + '</li>';
    document.getElementById('status').textContent = 'done';
  });
</script>"""


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send(self, code, body, ctype="text/html; charset=utf-8", headers=()):
        data = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        for k, v in headers:
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(data)

    def page(self, title, body, code=200, headers=()):
        self.send(code, PAGE.format(title=title, body=body), headers=headers)

    def cookies(self):
        c = self.headers.get("Cookie", "")
        return dict(p.strip().split("=", 1) for p in c.split(";") if "=" in p)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n).decode() if n else ""

    def route(self):
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        body = self.body()
        p = url.path
        if p == "/echo":
            lines = [self.requestline] + [f"{k}: {v}" for k, v in self.headers.items()]
            return self.send(200, "\n".join(lines) + ("\n\n" + body if body else ""), "text/plain")
        if p == "/api/echo":
            return self.send(200, json.dumps({"query": query, "body": body,
                "headers": {k.lower(): v for k, v in self.headers.items()}}), "application/json")
        if p == "/":
            return self.page("Home", """
<a href="/echo">echo</a> <a id=acct href="/account">account</a>
<form id=search action="/echo"><input name=q><button>Search</button></form>
<form id=login method=post action="/login"><input name=user><input type=password name=pass><button id=signin>Sign in</button></form>""")
        if p == "/login" and self.command == "POST":
            user = dict(urllib.parse.parse_qsl(body)).get("user", "?")
            return self.send(303, "", headers=[("Location", "/account"), ("Set-Cookie", f"user={user}; Path=/; HttpOnly")])
        if p == "/account":
            user = self.cookies().get("user")
            return self.page("Account", f"<h1 id=who>{html.escape(user) if user else 'not logged in'}</h1>")
        if p == "/app":
            return self.page("App", APP)
        if p == "/wall":
            if self.cookies().get("cleared") == "1":
                return self.page("Protected", "<h1>secret content</h1>")
            return self.page("Just a moment...", "<h1>Verify you are human</h1><a href=/wall/solve>solve</a>", 403)
        if p == "/wall/solve":
            if self.command == "POST":
                return self.send(303, "", headers=[("Location", "/wall"), ("Set-Cookie", "cleared=1; Path=/; Max-Age=3600")])
            return self.page("Challenge", "<form method=post><button id=human>I am human</button></form>")
        self.page("Not found", "not found", 404)

    do_GET = do_POST = do_HEAD = route

    def log_message(self, fmt, *args):
        sys.stderr.write(f"{self.command} {self.path} <- {self.headers.get('User-Agent', '')[:60]}\n")


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8787
    print(f"testsite on http://127.0.0.1:{port}", flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
