# WebScout

**A headless browser for LLMs and scrapers, written in Rust, with no Chromium.**

WebScout fetches and runs pages the way a real browser does: Chrome, Edge, Opera, Brave,
Firefox or Safari, on a real desktop, phone or tablet. The TLS handshake, HTTP/2 frames,
header order and JavaScript environment all agree with each other. When a site still
insists on a human (captcha, bot wall, login), WebScout opens a real Chrome for someone to
get through. It then takes over that browser's cookies and fingerprint and carries on.

![WebScout scraping Hacker News, running page JavaScript, browsing as an iPhone and logging in with actions](docs/demo.gif)

## Features

- **Believable identities.** 31 built-in devices. Each has a TLS + HTTP/2 fingerprint, the
  exact header set and order its browser sends for each kind of request, client hints,
  screen, GPU and touch. Browser versions update themselves.
- **Real JavaScript.** QuickJS plus a full DOM: classic and module scripts, `fetch`/XHR,
  cookies, timers, forms. Pages that render client-side work.
- **Page actions.** Type, click, press, select, submit, wait and eval. The navigations these
  cause are followed like a browser tab.
- **Sessions.** Named cookie jars with a fixed identity, saved to disk. A site sees one
  returning visitor, not a new bot on every request.
- **Human handoff.** On a challenge, a real Chrome opens locally, or streams to any browser
  on a headless box. Once a person clears the challenge, the session continues as that browser.
- **Proxies.** http, https and socks5, per session.
- **MCP server.** A drop-in tool server for LLM agents, using only the Python standard library.

## Quick start

```bash
cargo build --release
./target/release/webscout -u https://example.com --text
```

## CLI

```bash
webscout -u https://example.com --text                          # visible text (raw body for non-HTML)
webscout -u https://news.ycombinator.com -s '.titleline > a'    # text of matching elements
webscout -u https://example.com -s main --html                  # outer HTML instead
webscout -u https://example.com --links                         # absolute links
webscout -u https://quotes.toscrape.com/js/ --js -s '.quote .text'
webscout -u https://example.com --identity "phone samsung" --text
webscout -u https://site.test --actions '[{"type":"#q","text":"boots"},{"press":"Enter"}]' --text
webscout -u https://site.test --proxy socks5://user:pass@host:1080 --text

webscout serve --port 8080      # HTTP/JSON daemon
webscout session show mcp       # inspect a saved session (or: session delete mcp)
```

| Flag | Meaning |
|---|---|
| `-s, --select <css>` | Print matching elements' text (`--html` for outer HTML) |
| `--text` / `--links` | Page text / absolute links |
| `--js`, `--js-timeout <ms>` | Run page scripts first (default budget 5000 ms) |
| `--actions <json>` | Interactions to run (implies `--js`) |
| `--identity <spec>` | Which device and browser to be |
| `--proxy <url>` | Route requests through a proxy |
| `-d, --delay <ms>` | Wait before fetching, e.g. `100-500` |

## Identities

Devices live in `~/.webscout/identities/{pcs,phones,tablets}.toml`, copied from
`identities/` on first use. Edit, add or remove devices there. Each device sets its screen,
DPR, CPU cores, RAM, GPU, touch, model, OS version, and the browsers it runs; repeat a
browser to weight it. The fields are documented at the top of `pcs.toml`.

Choose one with a **spec**: words that must match a device's name, vendor, kind
(`desktop`/`pc`, `phone`/`mobile`, `tablet`) or OS, plus an optional browser.

```text
"iphone"   "phone samsung"   "tablet android"   "windows edge"   "linux firefox"   "iqoo-13 brave"
```

With no spec, a session is a desktop running the host's OS (override with `WEBSCOUT_IDENTITY`).
The TCP/IP fingerprint always comes from the real machine, so this keeps the two consistent.

## HTTP API

Start it with `webscout serve`. Any request can name a `session`. `identity`, `proxy` and
`forwarded_for` take effect when that request creates the session. Asking an existing
session to be something else is an error, so you never silently browse as the wrong device.

### `POST /scrape`

```json
{
  "url": "https://example.com",
  "session": "shop",
  "format": "json",
  "identity": "phone samsung",
  "proxy": "http://user:pass@host:8080",
  "js": true,
  "actions": [
    {"type": "input[name=q]", "text": "red shoes"}, {"press": "Enter"},
    {"wait_for": ".results li", "timeout": 10000},
    {"click": "text=Next"}
  ]
}
```

`format` is `json` (title, text, links, metadata; the default), `markdown` or `raw`. The
response includes the `identity` used. A blocked page also carries a `hint` that points to
the handoff.

**Actions** run in order. Selectors are CSS or `text=...`. Each action's result comes back in `js.actions`.

| Action | Does |
|---|---|
| `{"type": sel, "text": "..."}` | Focus and type into a field |
| `{"press": "Enter"}` | Key press (Enter submits the focused form) |
| `{"click": sel}` | Click; follows links and submit buttons |
| `{"select": sel, "value": "m"}` | Pick a `<select>` option |
| `{"check": sel}` | Tick a checkbox or radio |
| `{"submit": sel}` | Submit a form |
| `{"wait_for": sel, "timeout": ms}` | Wait until an element exists |
| `{"wait": ms}` | Let timers run |
| `{"goto": "/path"}` | Navigate |
| `{"eval": "js"}` | Run JS and return the value |

### Other endpoints

| Endpoint | Body |
|---|---|
| `POST /extract/links` · `/extract/forms` · `/extract/metadata` | Same as `/scrape` |
| `POST /cookies/import` | `{url, header, session}`: paste a `Cookie:` header from your browser |
| `GET /health` | `ok` |

### Human handoff

```bash
curl -X POST localhost:8080/handoff -H 'content-type: application/json' \
     -d '{"url":"https://site.test","session":"shop","until_cookie":"cf_clearance"}'
# -> {id, status, view_url, status_url, done_url, ...}
```

1. A real Chrome opens the URL, with its own profile per session and the same proxy.
2. On a desktop the window appears. On a headless machine (or with `"headless": true`), open
   `view_url` in any browser: it streams the page and forwards clicks, typing and scrolling.
3. Finish with `POST done_url`, or let `until_cookie` / `until_url` finish it automatically.
   Poll `status_url` in the meantime.
4. The session now holds Chrome's cookies. It is pinned to that Chrome's exact UA, client
   hints, screen, GPU and languages, so the site keeps seeing the browser it cleared.

Set `WEBSCOUT_PUBLIC_URL` if people reach the daemon at another address, and
`WEBSCOUT_CHROME` if Chrome is installed somewhere non-standard.

> **Security:** only bind the daemon to a public interface on a trusted network. Anyone who
> can reach it can drive its sessions.

### IP address

`proxy` changes the address sites see. `forwarded_for` (an IP or `"random"`) only adds
`X-Forwarded-For` / `X-Real-IP` headers. That fools servers that trust those headers, but
bot walls read it as a bot signal.

## MCP server

`mcp/webscout_mcp.py` is a stdio MCP server (Python 3.8+, no dependencies). It starts one
daemon and keeps all calls in a shared `mcp` session, like one browser tab.

```json
{
  "mcpServers": {
    "webscout": { "command": "python3", "args": ["/path/to/webscout/mcp/webscout_mcp.py"] }
  }
}
```

Tools: `scrape` (with actions, identity, proxy), `handoff`, `handoff_status` (with a live
frame), `handoff_done`, `extract_links`, `extract_forms`, `extract_metadata`,
`import_cookies`, `session_info`, `session_clear`.

It looks for the binary at `$WEBSCOUT_BIN`, then `target/release`, then `target/debug`, then `$PATH`.

## JavaScript support

Page scripts run in QuickJS against [linkedom](https://github.com/WebReflection/linkedom).
`src/js_prelude.js` and `src/js_webapis.js` supply the browser around them.

| Works | Not yet |
|---|---|
| Classic + module scripts, dynamic `import()` | Layout (sizes and positions are 0) |
| `fetch`, XHR, cookies, all through the session | WebAssembly |
| Timers on a virtual clock | WebSocket, Worker, IndexedDB |
| Forms with real submit semantics | `crypto.subtle` beyond digest + HMAC |
| `crypto.subtle` digest (SHA-1/256/384/512), HMAC sign/verify | |

Challenges that need the missing pieces (such as AWS WAF) go to the handoff. To rebuild the
DOM bundle (`vendor/dom-bundle.js`): `cd vendor && bun install && bun run build`.

## Development

```bash
cargo test --release
python3 dev/testsite.py 8787   # local site: header echo, forms, JS app, fake bot wall
```

```text
identities/        device catalog (TOML)
src/main.rs        CLI and browse loop (JS + actions + navigations)
src/serve.rs       HTTP API
src/handoff.rs     real-Chrome handoff over CDP + viewer page
src/identity.rs    device/browser identities, version upkeep
src/session.rs     named sessions on disk
src/js.rs          QuickJS runtime, event loop, actions
src/http/          wreq client, per-request headers, challenge detection
mcp/               MCP server
dev/testsite.py    local test site
```

## License

[MIT](LICENSE)
