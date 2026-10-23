use std::sync::Arc;
use tokio::time::{sleep, Duration};
use rand::Rng;
use wreq::{Client, Uri, Version, redirect::Policy};
use wreq::cookie::Cookies;
use wreq::header::{HeaderMap, HeaderName, HeaderValue, OrigHeaderMap};
use url::Url;
use std::sync::RwLock;

use crate::identity::Identity;
use crate::session::Session;
use crate::http::HttpResponse;
use crate::http::headers::{self, Dest, Req};
use crate::error::{Result, ScoutError};

#[derive(Clone)]
pub struct HttpClient {
    client: Client,
    identity: Arc<Identity>,
    /// The emulation profile's own header values, for http::headers to draw from.
    base: Arc<HeaderMap>,
}

struct SharedCookieStore(Arc<RwLock<cookie_store::CookieStore>>);

impl wreq::cookie::CookieStore for SharedCookieStore {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, uri: &Uri) {
        let Ok(url) = Url::parse(&uri.to_string()) else { return };
        let mut store = self.0.write().unwrap();
        for header in cookie_headers {
            if let Ok(cookie_str) = header.to_str() {
                if let Ok(cookie) = cookie_store::Cookie::parse(cookie_str, &url) {
                    let _ = store.insert_raw(&cookie, &url);
                }
            }
        }
    }

    fn cookies(&self, uri: &Uri, _version: Version) -> Cookies {
        let Ok(url) = Url::parse(&uri.to_string()) else { return Cookies::Empty };
        let store = self.0.read().unwrap();
        let cookies = store.matches(&url)
            .into_iter()
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ");
        match HeaderValue::from_str(&cookies) {
            Ok(v) if !cookies.is_empty() => Cookies::Compressed(v),
            _ => Cookies::Empty,
        }
    }
}

impl std::fmt::Debug for SharedCookieStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedCookieStore").finish()
    }
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
         .finish()
    }
}

impl HttpClient {
    #[tracing::instrument(skip(session))]
    pub fn build(session: &Session) -> Result<HttpClient> {
        // The emulation profile supplies TLS and HTTP/2 settings; headers are built per
        // request (http::headers) since a navigation, a fetch() and a script load differ.
        let id = &session.identity;
        let mut builder = Client::builder();
        if let Some(p) = &id.proxy {
            builder = builder.proxy(wreq::Proxy::all(p.as_str()).map_err(ScoutError::Http)?);
        }
        let client = builder
            .emulation(id.emulation())
            .cookie_provider(Arc::new(SharedCookieStore(Arc::clone(&session.cookies))))
            .redirect(Policy::limited(10))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(ScoutError::Http)?;
        Ok(HttpClient { client, identity: Arc::new(id.clone()), base: Arc::new(id.emulation_headers()) })
    }

    async fn delay_if_needed(delay_range: Option<std::ops::Range<u64>>) {
        if let Some(range) = delay_range {
            if range.start < range.end {
                // ThreadRng is !Send, so it must not live across the await.
                let delay = rand::thread_rng().gen_range(range);
                sleep(Duration::from_millis(delay)).await;
            }
        }
    }

    /// A navigation typed into the address bar.
    #[tracing::instrument(skip(self))]
    pub async fn get(&self, url: &str, delay_range: Option<std::ops::Range<u64>>) -> Result<HttpResponse> {
        Self::delay_if_needed(delay_range).await;
        self.request("GET", url, Dest::Document, None, &[], None).await
    }

    /// Any request, with the claimed browser's exact headers for its kind (`dest`) and
    /// initiator (`referrer`). `extra` = page-supplied headers (content-type etc.).
    #[tracing::instrument(skip(self, extra, body))]
    pub async fn request(&self, method: &str, url: &str, dest: Dest, referrer: Option<&str>,
                         extra: &[(String, String)], body: Option<String>) -> Result<HttpResponse> {
        let method = method.to_ascii_uppercase();
        let target = Url::parse(url).map_err(|e| ScoutError::InvalidUrl(e.to_string()))?;
        let referrer = referrer.and_then(|r| Url::parse(r).ok());
        let list = headers::build(&self.identity, &self.base, &Req {
            method: &method, url: &target, dest, referrer: referrer.as_ref(), extra,
        });
        let mut map = HeaderMap::new();
        let mut order = OrigHeaderMap::new();
        for (name, value) in list {
            let (Ok(n), Some(Ok(v))) = (HeaderName::from_bytes(name.as_bytes()), value.as_deref().map(HeaderValue::from_str)) else {
                order.insert(name); // a slot (cookie) another layer fills
                continue;
            };
            map.append(n, v);
            order.insert(name);
        }
        let m = wreq::Method::from_bytes(method.as_bytes()).map_err(|e| ScoutError::Parse(e.to_string()))?;
        let mut req = self.client.request(m, url).default_headers(false).headers(map).orig_headers(order);
        if let Some(b) = body {
            req = req.body(b);
        }
        let res = req.send().await?;
        let status = res.status().as_u16();
        let final_url = res.uri().to_string();
        let headers = res.headers().clone();
        let body = res.bytes().await?;
        Ok(HttpResponse { status, final_url, headers, body })
    }
}
