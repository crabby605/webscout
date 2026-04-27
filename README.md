# WebScout

Custom headless browser engine for LLMs. No Chromium, no Playwright dependency.
Built to look indistinguishable from real browsers (Chrome/Firefox/Safari) at the TLS and HTTP/2 network levels.

## Running Output
```bash
cargo run -- --url "https://nowsecure.nl"
```

## Directory Structure
```text
src/
├── main.rs           # CLI entry
├── error.rs          # unified error type
├── identity.rs       # UA pool, header order, TLS fingerprint
├── session.rs        # per-session cookie jar + identity
└── http/             # HTTP client with identity headers / evasions
    ├── client.rs       
    ├── cache.rs
    ├── detect.rs     # Anti-bot detection routines
    └── response.rs
```

## Phase 1 Readiness (Currently Implemented)
- **Strict Identity Simulation:** Generates fully coherent user-agent, `sec-ch-ua` headers, viewports, platforms, and strictly orders the transmitted HTTP headers.
- **TLS Parameter Tuning:** Reorders `ClientConfig` cipher suites to properly mock Chrome's AES128 precedence natively via `rustls`.
- **HTTP/2 Evasion Tuning:** Spoofs Akamai `INITIAL_WINDOW_SIZE` (6,291,456 bytes) and corresponding HTTP/2 connection updates dynamically via `reqwest` builder extensions. 
- **Persisted Sessions:** Full robust cookie jar generation and isolated session store `UUID` persistence tracking.
- **Passive Bot Bypass:** Out-of-the-box native bypass for passive Cloudflare configurations (no bot challenges triggered on standard fetches like `nowsecure.nl`).
- **Response State Machine:** Automatically classifies blocked patterns to halt requests when interacting with traps (Rate Limit, CF challenge, HCaptcha, JS checks, etc).

## Stack (Currently Implemented)
| Component | Crate |
| --- | --- |
| Async | tokio |
| HTTP | reqwest + hyper |
| TLS Evasion | rustls |
| CLI / Logging | clap + tracing |
| Cookie Management | cookie_store |

---

## Future Architecture (Phase 2 & Beyond Planned)
┌─────────────────────────────────────────┐
│           Transport Layer               │
│   MCP (stdio/JSON-RPC) │ REST (axum)    │
└────────────────┬────────────────────────┘
                 │
┌────────────────▼────────────────────────┐
│              Engine Core                │
│  Identity → HTTP → HTML → JS → Extract │
└─────────────────────────────────────────┘

### Planned Stack Additions
| Component | Crate |
| --- | --- |
| HTML parser | html5ever + scraper |
| JS runtime | rquickjs (QuickJS) |
| Renderer | tiny-skia |
| REST | axum |
| MCP | JSON-RPC stdio |

### Future Setup & Running Tools (Currently Unavailable)
```bash
# install just
cargo install just

# copy config
just init-config
# edit config.toml

# install dev tools
just setup
```

## Future REST API (Planned)
`POST /scrape`
```json
{
  "url": "https://example.com",
  "format": "markdown",   // markdown | raw | json | screenshot
  "js": true,
  "session_id": "..."     // optional, omit for new session
}
```
- `POST /session/new` -> `{ session_id, user_agent }`
- `POST /session/delete` -> `{ ok }`
- `POST /session/cookies` -> `{ ok, cookies }`
- `GET  /health` -> `{ status }`

### Future MCP Tools (Planned)
| Tool | Description |
| --- | --- |
| `scrape` | Fetch URL, return content in chosen format |
| `screenshot` | Fetch URL, return base64 PNG |
| `extract_links` | Return all links from URL |
| `extract_forms` | Return all forms + fields |
| `submit_form` | Submit form with field values |
| `session_new` | Create session with fresh identity |
| `session_delete` | Delete session |

### Future Challenge Solving (Planned)
Supports: Cloudflare Turnstile, hCaptcha, reCAPTCHA v2/v3 via `config.toml` solving:
```toml
[challenge]
enabled = true
provider = "2captcha"    # or "capmonster"
api_key = "your_key"
```

## Internal Engine TODOs
- [ ] Full fontdue text rendering in tiny-skia renderer
- [ ] submit_form tool implementation
- [ ] TLS JA3/JA4 fingerprint via pseudo BoringSSL FFI integration
- [ ] HTTP/2 HPACK header order spoofing natively via hyper
- [ ] CSS visibility evaluation
- [ ] file upload in form submit
- [ ] WebSocket support
