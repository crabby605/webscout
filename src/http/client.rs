use std::sync::Arc;
use tokio::time::{sleep, Duration};
use rand::Rng;
use wreq::{Client, Uri, Version, redirect::Policy};
use wreq::cookie::Cookies;
use wreq::header::HeaderValue;
use url::Url;
use std::sync::RwLock;

use crate::session::Session;
use crate::http::HttpResponse;
use crate::error::{Result, ScoutError};

#[derive(Clone)]
pub struct HttpClient {
    pub client: Client,
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
        // The emulation profile supplies TLS, HTTP/2 settings, and headers (UA,
        // sec-ch-ua, accept-*, header order) exactly as that browser sends them.
        // ...except the claimed version, which Identity keeps current.
        let mut version = wreq::header::HeaderMap::new();
        let id = &session.identity;
        for (k, v) in [("user-agent", Some(&id.user_agent)), ("sec-ch-ua", id.sec_ch_ua.as_ref())] {
            if let Some(v) = v.and_then(|v| HeaderValue::from_str(v).ok()) {
                version.insert(k, v);
            }
        }
        let client = Client::builder()
            .emulation(id.emulation())
            .default_headers(version)
            .cookie_provider(Arc::new(SharedCookieStore(Arc::clone(&session.cookies))))
            .redirect(Policy::limited(10))
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(ScoutError::Http)?;

        Ok(HttpClient { client })
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

    #[tracing::instrument(skip(self))]
    pub async fn get(&self, url: &str, delay_range: Option<std::ops::Range<u64>>) -> Result<HttpResponse> {
        Self::delay_if_needed(delay_range).await;

        let res = self.client.get(url).send().await?;
        let status = res.status().as_u16();
        let final_url = res.uri().to_string();
        let headers = res.headers().clone();
        let body = res.bytes().await?;

        Ok(HttpResponse {
            status,
            final_url,
            headers,
            body,
        })
    }

    /// Arbitrary request with per-request header overrides (used by page JS: fetch, XHR, scripts).
    #[tracing::instrument(skip(self, headers, body))]
    pub async fn request(&self, method: &str, url: &str, headers: &[(String, String)], body: Option<String>) -> Result<HttpResponse> {
        let method = wreq::Method::from_bytes(method.to_ascii_uppercase().as_bytes())
            .map_err(|e| ScoutError::Parse(e.to_string()))?;
        let mut req = self.client.request(method, url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
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

    #[tracing::instrument(skip(self, fields))]
    pub async fn post_form(&self, url: &str, fields: &std::collections::HashMap<&str, &str>, delay_range: Option<std::ops::Range<u64>>) -> Result<HttpResponse> {
        Self::delay_if_needed(delay_range).await;

        let res = self.client.post(url).form(fields).send().await?;
        let status = res.status().as_u16();
        let final_url = res.uri().to_string();
        let headers = res.headers().clone();
        let body = res.bytes().await?;

        Ok(HttpResponse {
            status, final_url, headers, body
        })
    }

    #[tracing::instrument(skip(self, body_data))]
    pub async fn post_json(&self, url: &str, body_data: &serde_json::Value, delay_range: Option<std::ops::Range<u64>>) -> Result<HttpResponse> {
        Self::delay_if_needed(delay_range).await;

        let res = self.client.post(url).json(body_data).send().await?;
        let status = res.status().as_u16();
        let final_url = res.uri().to_string();
        let headers = res.headers().clone();
        let body = res.bytes().await?;

        Ok(HttpResponse {
            status, final_url, headers, body
        })
    }
}
