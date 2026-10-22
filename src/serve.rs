//! `webscout serve`: the HTTP/JSON daemon that mcp/webscout_mcp.py talks to.

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tracing::info;

use crate::dom::Dom;
use crate::error::ScoutError;
use crate::http::{HttpClient, HttpResponse};
use crate::session::Session;

const DEFAULT_SESSION: &str = "default";

struct Browser {
    session: Session,
    client: HttpClient,
}

/// Named sessions, loaded from disk on first use and kept for the daemon's lifetime.
// ponytail: one global lock around the map only; fetches run outside it.
type Sessions = Arc<Mutex<HashMap<String, Arc<Browser>>>>;

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
}

#[derive(Deserialize)]
struct CookieReq {
    url: String,
    header: String,
    #[serde(default)]
    session: Option<String>,
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
    let sessions: Sessions = Default::default();
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/scrape", post(scrape))
        .route("/extract/links", post(extract_links))
        .route("/extract/forms", post(extract_forms))
        .route("/extract/metadata", post(extract_metadata))
        .route("/cookies/import", post(import_cookies))
        .route("/screenshot", post(screenshot))
        .with_state(sessions);

    let io = |e: std::io::Error| ScoutError::Session(e.to_string());
    let listener = tokio::net::TcpListener::bind((host, port)).await.map_err(io)?;
    let addr = listener.local_addr().map_err(io)?;
    // The MCP wrapper reads exactly this line from stdout to find the port.
    println!("WEBSCOUT_LISTEN={addr}");
    std::io::stdout().flush().map_err(io)?;
    info!("Serving on {addr}");
    axum::serve(listener, app).await.map_err(io)
}

async fn browser(sessions: &Sessions, name: Option<&str>) -> Result<(String, Arc<Browser>), ApiError> {
    let name = name.unwrap_or(DEFAULT_SESSION).to_string();
    let mut map = sessions.lock().await;
    if let Some(b) = map.get(&name) {
        return Ok((name, Arc::clone(b)));
    }
    let session = Session::load(&name)?;
    let client = HttpClient::build(&session)?;
    let b = Arc::new(Browser { session, client });
    map.insert(name.clone(), Arc::clone(&b));
    Ok((name, b))
}

async fn fetch(sessions: &Sessions, req: &FetchReq) -> Result<(HttpResponse, Dom), ApiError> {
    if req.js {
        let _ = req.js_timeout_ms;
        return Err(ScoutError::Unsupported("js=true is not supported yet".into()).into());
    }
    let (name, b) = browser(sessions, req.session.as_deref()).await?;
    // "stealth" = human-ish random pause when the caller didn't set one.
    let delay = req.delay.as_deref()
        .or((req.strategy.as_deref() == Some("stealth")).then_some("300-1200"))
        .and_then(crate::parse_delay);
    let resp = b.client.get(&req.url, delay).await?;
    b.session.save(&name)?;
    let dom = Dom::parse(&resp.text(), &resp.final_url);
    Ok((resp, dom))
}

async fn scrape(State(s): State<Sessions>, Json(req): Json<FetchReq>) -> ApiResult {
    let (resp, dom) = fetch(&s, &req).await?;
    let envelope = json!({
        "url": req.url,
        "final_url": resp.final_url,
        "status": resp.status,
        "state": format!("{:?}", resp.classify()),
    });
    let mut out = envelope.as_object().unwrap().clone();
    match req.format.as_deref().unwrap_or("json") {
        "raw" => {
            out.insert("content".into(), resp.text().into());
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
    let (name, b) = browser(&s, req.session.as_deref()).await?;
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
