//! `webscout serve`: the HTTP/JSON daemon that mcp/webscout_mcp.py talks to.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use axum::extract::{FromRef, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::info;

use crate::dom::Dom;
use crate::handoff::{self, Handoff, Until};
use crate::error::ScoutError;
use crate::http::{HttpClient, HttpResponse};
use crate::session::{NewSession, Session};

const DEFAULT_SESSION: &str = "default";

struct Browser {
    session: Session,
    client: HttpClient,
}

/// Named sessions, loaded from disk on first use and kept for the daemon's lifetime.
// ponytail: one global lock around the map only; fetches run outside it.
type Sessions = Arc<Mutex<HashMap<String, Arc<Browser>>>>;
type Handoffs = Arc<Mutex<HashMap<String, Arc<Handoff>>>>;

#[derive(Clone, FromRef)]
struct App {
    sessions: Sessions,
    handoffs: Handoffs,
}

#[derive(Deserialize)]
struct FetchReq {
    url: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    js: bool,
    #[serde(default)]
    js_timeout_ms: Option<u64>,
    #[serde(default)]
    delay: Option<String>,
    #[serde(default)]
    session: Option<String>,
    #[serde(default)]
    strategy: Option<String>,
    /// Used only when this request creates the session.
    #[serde(flatten)]
    new: NewSession,
    /// Interactions to run on the page (implies js); see __ws_act in js_prelude.js.
    #[serde(default)]
    actions: Vec<Value>,
}

#[derive(Deserialize)]
struct CookieReq {
    url: String,
    header: String,
    #[serde(default)]
    session: Option<String>,
    #[serde(flatten)]
    new: NewSession,
}

struct ApiError(StatusCode, String);

impl From<ScoutError> for ApiError {
    fn from(e: ScoutError) -> Self {
        let code = match e {
            ScoutError::Parse(_) | ScoutError::InvalidUrl(_) | ScoutError::Session(_) => StatusCode::BAD_REQUEST,
            ScoutError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
            _ => StatusCode::BAD_GATEWAY,
        };
        ApiError(code, e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

type ApiResult = Result<Json<Value>, ApiError>;

pub async fn run(host: &str, port: u16) -> crate::error::Result<()> {
    let app_state = App { sessions: Default::default(), handoffs: Default::default() };
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/scrape", post(scrape))
        .route("/extract/links", post(extract_links))
        .route("/extract/forms", post(extract_forms))
        .route("/extract/metadata", post(extract_metadata))
        .route("/cookies/import", post(import_cookies))
        .route("/screenshot", post(screenshot))
        .route("/handoff", post(handoff_start))
        .route("/handoff/{id}", get(handoff_status))
        .route("/handoff/{id}/view", get(handoff_view))
        .route("/handoff/{id}/frame", get(handoff_frame))
        .route("/handoff/{id}/input", post(handoff_input))
        .route("/handoff/{id}/done", post(handoff_done))
        .route("/handoff/{id}/cancel", post(handoff_cancel))
        .with_state(app_state);

    let io = |e: std::io::Error| ScoutError::Session(e.to_string());
    let listener = tokio::net::TcpListener::bind((host, port)).await.map_err(io)?;
    let addr = listener.local_addr().map_err(io)?;
    // The MCP wrapper reads exactly this line from stdout to find the port.
    println!("WEBSCOUT_LISTEN={addr}");
    std::io::stdout().flush().map_err(io)?;
    info!("Serving on {addr}");
    axum::serve(listener, app).await.map_err(io)
}

async fn browser(sessions: &Sessions, name: Option<&str>, new: &NewSession) -> Result<(String, Arc<Browser>), ApiError> {
    let name = name.unwrap_or(DEFAULT_SESSION).to_string();
    let mut map = sessions.lock().await;
    if let Some(b) = map.get(&name) {
        new.check(&b.session.identity)?;
        return Ok((name, Arc::clone(b)));
    }
    let session = Session::load(&name, new)?;
    let client = HttpClient::build(&session)?;
    let b = Arc::new(Browser { session, client });
    map.insert(name.clone(), Arc::clone(&b));
    Ok((name, b))
}

/// Fetches the page (running its JS if asked) → (response, DOM, final HTML, JS report).
async fn fetch(sessions: &Sessions, req: &FetchReq) -> Result<(HttpResponse, Dom, String, Option<Value>), ApiError> {
    let (name, b) = browser(sessions, req.session.as_deref(), &req.new).await?;
    // "stealth" = human-ish random pause when the caller didn't set one.
    let delay = req.delay.as_deref()
        .or((req.strategy.as_deref() == Some("stealth")).then_some("300-1200"))
        .and_then(crate::parse_delay);
    let mut resp = b.client.get(&req.url, delay).await?;
    let mut html = resp.text();
    let mut console = None;
    if (req.js || req.strategy.as_deref() == Some("js") || !req.actions.is_empty()) && resp.is_html() {
        let (last, r) = crate::browse(resp, &b.session, &b.client, req.js_timeout_ms.unwrap_or(5000), &req.actions).await?;
        resp = last;
        html = r.html;
        console = Some(json!({ "console": r.console, "timed_out": r.timed_out, "actions": r.results }));
    }
    b.session.save(&name)?;
    let dom = Dom::parse(&html, &resp.final_url);
    Ok((resp, dom, html, console))
}

async fn scrape(State(s): State<Sessions>, Json(req): Json<FetchReq>) -> ApiResult {
    let identity = browser(&s, req.session.as_deref(), &req.new).await?.1.session.identity.summary();
    let (resp, dom, html, js) = fetch(&s, &req).await?;
    let envelope = json!({
        "url": req.url,
        "identity": identity,
        "final_url": resp.final_url,
        "status": resp.status,
        "state": format!("{:?}", resp.classify()),
        "js": js,
    });
    let mut out = envelope.as_object().unwrap().clone();
    if matches!(resp.classify(), crate::http::PageState::Challenge(_) | crate::http::PageState::Blocked) {
        out.insert("hint".into(), "blocked: POST /handoff {url, session} to let a human through in a real browser".into());
    }
    match req.format.as_deref().unwrap_or("json") {
        "raw" => {
            out.insert("content".into(), html.into());
        }
        // ponytail: title + visible text, not real HTML→Markdown; convert headings/lists if readers need structure.
        "markdown" => {
            let title = dom.title().map(|t| format!("# {t}\n\n")).unwrap_or_default();
            out.insert("content".into(), format!("{title}{}", dom.text()).into());
        }
        "json" => {
            out.insert("title".into(), json!(dom.title()));
            out.insert("text".into(), dom.text().into());
            out.insert("links".into(), json!(dom.links()));
            out.insert("metadata".into(), dom.metadata());
        }
        f => return Err(ApiError(StatusCode::BAD_REQUEST, format!("unknown format {f:?}"))),
    }
    Ok(Json(out.into()))
}

async fn extract_links(State(s): State<Sessions>, Json(req): Json<FetchReq>) -> ApiResult {
    Ok(Json(json!(fetch(&s, &req).await?.1.links())))
}

async fn extract_forms(State(s): State<Sessions>, Json(req): Json<FetchReq>) -> ApiResult {
    Ok(Json(json!(fetch(&s, &req).await?.1.forms())))
}

async fn extract_metadata(State(s): State<Sessions>, Json(req): Json<FetchReq>) -> ApiResult {
    Ok(Json(fetch(&s, &req).await?.1.metadata()))
}

async fn import_cookies(State(s): State<Sessions>, Json(req): Json<CookieReq>) -> ApiResult {
    let url = url::Url::parse(&req.url).map_err(|e| ScoutError::InvalidUrl(e.to_string()))?;
    let (name, b) = browser(&s, req.session.as_deref(), &req.new).await?;
    let mut imported = 0;
    {
        let mut jar = b.session.cookies.write().unwrap();
        for pair in req.header.split(';').map(str::trim).filter(|p| p.contains('=')) {
            if let Ok(c) = cookie_store::Cookie::parse(pair, &url) {
                if jar.insert_raw(&c, &url).is_ok() {
                    imported += 1;
                }
            }
        }
    }
    b.session.save(&name)?;
    Ok(Json(json!({ "session": name, "imported": imported })))
}

async fn screenshot() -> ApiError {
    ScoutError::Unsupported("screenshots are not supported in this build".into()).into()
}

// --- human handoff -------------------------------------------------------------

#[derive(Deserialize)]
struct HandoffReq {
    url: String,
    #[serde(default)]
    session: Option<String>,
    /// Default: headless only when there's no display (servers, CI).
    #[serde(default)]
    headless: Option<bool>,
    #[serde(flatten)]
    until: Until,
    #[serde(flatten)]
    new: NewSession,
}

async fn find(handoffs: &Handoffs, id: &str) -> Result<Arc<Handoff>, ApiError> {
    handoffs.lock().await.get(id).cloned().ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no handoff {id}")))
}

async fn describe(h: &Handoff, base: Option<String>) -> Value {
    let (url, title) = if h.status() == "waiting" { h.page().await } else { (Value::Null, Value::Null) };
    let mut v = json!({ "id": h.id, "session": h.session, "status": h.status(), "headless": h.headless, "url": url, "title": title,
                        "until_cookie": h.until.until_cookie, "until_url": h.until.until_url });
    if let Some(base) = base {
        v["view_url"] = format!("{base}/handoff/{}/view", h.id).into();
        v["status_url"] = format!("{base}/handoff/{}", h.id).into();
        v["done_url"] = format!("{base}/handoff/{}/done", h.id).into();
    }
    v
}

/// Opens `url` in a real Chrome for a human. Desktop: a Chrome window appears. Headless:
/// open `view_url` from any browser to drive it. Finish with POST done_url, or set
/// until_cookie / until_url to finish automatically; poll status_url meanwhile.
async fn handoff_start(State(app): State<App>, headers: HeaderMap, Json(req): Json<HandoffReq>) -> ApiResult {
    let (name, b) = browser(&app.sessions, req.session.as_deref(), &req.new).await?;
    let mut handoffs = app.handoffs.lock().await;
    if let Some(h) = handoffs.values().find(|h| h.session == name && h.status() == "waiting").cloned() {
        drop(handoffs);
        return Ok(Json(describe(&h, public_base(&headers)).await)); // one Chrome per session profile
    }
    let headless = req.headless.unwrap_or_else(handoff::default_headless);
    let h = Arc::new(Handoff::start(&name, &req.url, headless, b.session.identity.proxy.as_deref(), req.until).await?);
    handoffs.insert(h.id.clone(), Arc::clone(&h));
    drop(handoffs);
    // Watcher: finishes on `until`, notices a closed window.
    let (app2, h2) = (app.clone(), Arc::clone(&h));
    tokio::spawn(async move {
        while h2.status() == "waiting" {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            if h2.status() != "waiting" {
                break;
            }
            if !h2.alive().await {
                *h2.status.lock().unwrap() = "closed".into();
            } else if h2.ready().await {
                if let Err(e) = complete(&app2, &h2).await {
                    info!("handoff {} auto-finish failed: {}", h2.id, e.1);
                }
            }
        }
    });
    info!("handoff {} for session {name} on {} (headless: {headless})", h.id, req.url);
    Ok(Json(describe(&h, public_base(&headers)).await))
}

/// Where a human reaches this daemon: WEBSCOUT_PUBLIC_URL, else the Host the caller used.
fn public_base(headers: &HeaderMap) -> Option<String> {
    std::env::var("WEBSCOUT_PUBLIC_URL").ok().map(|u| u.trim_end_matches('/').to_string())
        .or_else(|| headers.get("host").and_then(|h| h.to_str().ok()).map(|h| format!("http://{h}")))
}

/// Moves Chrome's cookies and real fingerprint into the session, then closes Chrome.
async fn complete(app: &App, h: &Handoff) -> Result<Value, ApiError> {
    {
        let mut st = h.status.lock().unwrap();
        if *st != "waiting" {
            return Err(ApiError(StatusCode::CONFLICT, format!("handoff is {st}")));
        }
        *st = "finishing".into();
    }
    let (name, b) = browser(&app.sessions, Some(&h.session), &NewSession::default()).await?;
    let r = h.finish(&b.session.identity).await?;
    let session = Session { id: b.session.id, identity: r.identity, cookies: Arc::clone(&b.session.cookies) };
    let mut imported = 0;
    {
        let mut jar = session.cookies.write().unwrap();
        for (line, url) in r.cookies {
            if let Ok(c) = cookie_store::Cookie::parse(line, &url) {
                if jar.insert_raw(&c, &url).is_ok() {
                    imported += 1;
                }
            }
        }
    }
    let client = HttpClient::build(&session)?;
    session.save(&name)?;
    let out = json!({ "session": name, "status": "done", "imported": imported,
                      "identity": session.identity.summary(), "user_agent": session.identity.user_agent });
    app.sessions.lock().await.insert(name, Arc::new(Browser { session, client }));
    Ok(out)
}

async fn handoff_status(State(app): State<App>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult {
    Ok(Json(describe(&*find(&app.handoffs, &id).await?, public_base(&headers)).await))
}

async fn handoff_view(State(app): State<App>, Path(id): Path<String>) -> Result<axum::response::Html<&'static str>, ApiError> {
    find(&app.handoffs, &id).await?;
    Ok(axum::response::Html(handoff::VIEWER))
}

async fn handoff_frame(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    Ok(Json(find(&app.handoffs, &id).await?.frame().await?))
}

async fn handoff_input(State(app): State<App>, Path(id): Path<String>, Json(ev): Json<Value>) -> ApiResult {
    find(&app.handoffs, &id).await?.input(&ev).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn handoff_done(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    let h = find(&app.handoffs, &id).await?;
    Ok(Json(complete(&app, &h).await?))
}

async fn handoff_cancel(State(app): State<App>, Path(id): Path<String>) -> ApiResult {
    find(&app.handoffs, &id).await?.close("cancelled").await;
    Ok(Json(json!({ "ok": true })))
}
