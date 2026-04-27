use bytes::Bytes;
use reqwest::header::HeaderMap;

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub final_url: String,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl HttpResponse {
    pub fn text(&self) -> String {
        // Fallback to UTF-8
        String::from_utf8_lossy(&self.body).to_string()
    }

    pub fn is_html(&self) -> bool {
        self.content_type().map(|ct| ct.contains("text/html")).unwrap_or(false)
    }

    pub fn content_type(&self) -> Option<String> {
        self.headers.get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    }

    pub fn classify(&self) -> crate::http::detect::PageState {
        crate::http::detect::classify(self)
    }
}

