# WebScout

Headless browser engine for LLMs and scraping runners. No Chromium needed for normal
browsing: it looks like a real Chrome / Edge / Opera / Brave / Firefox / Safari on a real
desktop, phone or tablet at the TLS, HTTP/2, header and JavaScript layers. When a site
still wants a human (captcha, bot wall, login), it hands off to a real Chrome and takes
over that browser's cookies and fingerprint afterwards.

## CLI
```bash
webscout -u https://news.ycombinator.com -s '.titleline > a'   # CSS selector
webscout -u https://example.com --text                          # visible text (or raw body for non-HTML)
webscout -u https://quotes.toscrape.com/js/ --js -s '.quote .text'
webscout -u https://example.com --identity "phone samsung" --text
webscout -u https://site.test --actions '[{"type":"#q","text":"boots"},{"press":"Enter"}]' --text
webscout -u https://site.test --proxy socks5://user:pass@host:1080 --text

webscout serve --port 8080            # HTTP/JSON daemon (what the MCP server and runners talk to)
webscout session show mcp
```

## Identities
`identities/{pcs,phones,tablets}.toml` are copied to `~/.webscout/identities/` on first use;
edit, add or delete devices there (screen, DPR, CPU cores, RAM, GPU, touch, model, OS
version, and which browsers it runs, repeat one to weight it). Field docs are at the top of
`pcs.toml`. Pick one with a spec: words that must match a device's name, vendor, kind
(`desktop`/`pc`, `phone`/`mobile`, `tablet`) or OS, plus optionally a browser:
`"iphone"`, `"phone samsung"`, `"tablet android"`, `"windows edge"`, `"linux firefox"`, `"iqoo-13 brave"`.
Without one, a session is a desktop running the host's OS (`WEBSCOUT_IDENTITY` overrides),
since the TCP/IP fingerprint always comes from the real host.

Per identity, consistently: TLS + HTTP/2 fingerprint (wreq/BoringSSL), the browser's exact
header set and order for each kind of request (navigation, `fetch`/XHR, scripts) with
correct `sec-fetch-*` / referer / origin, current versions (Chrome/Firefox auto-bump every
4 weeks), and the JS environment (navigator, client hints, screen, touch, media queries, WebGL GPU).

## HTTP API (`webscout serve`)
Every request may name a `session` (identity + cookie jar, persisted in `~/.webscout/sessions/`).
`identity`, `proxy` and `forwarded_for` apply when that request creates the session; asking an
existing session to be something else is an error rather than silently browsing as the wrong device.

`POST /scrape`
```json
{
  "url": "https://example.com", "session": "shop", "format": "markdown",
  "identity": "phone samsung", "proxy": "http://user:pass@host:8080",
  "js": true,
  "actions": [
    {"type": "input[name=q]", "text": "red shoes"}, {"press": "Enter"},
    {"wait_for": ".results li", "timeout": 10000},
    {"click": "text=Next"}, {"select": "#size", "value": "m"}, {"check": "#agree"},
    {"submit": "form#checkout"}, {"wait": 1500}, {"goto": "/cart"},
    {"eval": "document.querySelectorAll('.item').length"}
  ]
}
```
Actions run one at a time (selectors: CSS or `text=...`), and the navigations they cause
(links, form submits, JS redirects) are followed like a browser tab. Results per action come
back in `js.actions`. Blocked pages carry a `hint` pointing at the handoff.

`/extract/links`, `/extract/forms`, `/extract/metadata` take the same body. `/cookies/import`
takes `{url, header, session}`.

### Human handoff
```bash
curl -X POST localhost:8080/handoff -d '{"url":"https://site.test","session":"shop","until_cookie":"cf_clearance"}' \
     -H 'content-type: application/json'
# -> {id, status, view_url, status_url, done_url, ...}
```
A real Chrome (own profile per session, same proxy) opens the URL. On a desktop the window
appears; on a headless machine (or `"headless": true`) open `view_url` from any browser:
it streams the page and forwards clicks, typing and scrolling. Finish with `POST done_url`,
or let `until_cookie` / `until_url` finish it automatically; poll `status_url` meanwhile.
The session then holds Chrome's cookies and is pinned to that Chrome's exact UA, client
hints, screen, GPU and languages, so the site keeps seeing the browser it cleared.
Set `WEBSCOUT_PUBLIC_URL` if humans reach the daemon under another address, `WEBSCOUT_CHROME`
for a non-standard Chrome path. Bind the daemon to a public interface only on a trusted
network: whoever reaches it can drive its sessions.

### IP
`proxy` (http/https/socks5) changes the address sites see. `forwarded_for` (an IP or
`"random"`) only adds `X-Forwarded-For`/`X-Real-IP`; it fools servers that trust those
headers, and bot walls treat it as a bot signal.

## MCP
`mcp/webscout_mcp.py` (stdio) starts the daemon and exposes `scrape` (with actions and
identities), `handoff`, `handoff_status` (with a live frame), `handoff_done`,
`extract_links|forms|metadata`, `import_cookies`, `session_info`, `session_clear`.

## JavaScript
Page scripts run in QuickJS against [linkedom](https://github.com/WebReflection/linkedom)
(`vendor/dom-bundle.js`, rebuild: `cd vendor && bun install && bun run build`), with
`src/js_prelude.js` and `src/js_webapis.js` supplying the browser around it: classic + module
scripts, `fetch`/XHR/cookies through the session, a virtual clock for timers, forms with real
submit semantics. Not supported: layout (sizes are 0), WebAssembly, `crypto.subtle`,
WebSocket/Worker, IndexedDB; challenges that need them (e.g. AWS WAF) go to the handoff.

## Development
```bash
cargo test --release
python3 dev/testsite.py 8787   # local site: ordered header echo, forms, JS app, fake bot wall
```

## Layout
```text
identities/        device catalog (TOML)
src/main.rs        CLI, browse loop (JS + actions + navigations)
src/serve.rs       HTTP API
src/handoff.rs     real-Chrome handoff over CDP + viewer page
src/identity.rs    device/browser identities, version upkeep
src/session.rs     named sessions on disk
src/js.rs          QuickJS runtime, event loop, actions
src/http/          wreq client, per-request browser headers, challenge detection
mcp/               MCP server
dev/testsite.py    local test site
```
