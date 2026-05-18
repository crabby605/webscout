use crate::http::response::HttpResponse;

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum PageState {
    Normal,
    JsRequired,
    Challenge(ChallengeHint),
    Blocked,
    RateLimit(u64),
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ChallengeHint {
    Cloudflare,
    HCaptcha,
    Recaptcha,
    DataDome,
    PerimeterX,
    Imperva,
    Unknown,
}

pub fn classify(response: &HttpResponse) -> PageState {
    if response.status == 429 {
        let retry_after = response.headers.get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60);
        return PageState::RateLimit(retry_after);
    }

    // DataDome: presence of x-datadome-cid response header is definitive
    if response.headers.contains_key("x-datadome-cid") {
        return PageState::Challenge(ChallengeHint::DataDome);
    }

    // PerimeterX: x-px-* response headers or _px cookie
    let has_px_header = response.headers.keys()
        .any(|k| k.as_str().starts_with("x-px-"));
    if has_px_header {
        return PageState::Challenge(ChallengeHint::PerimeterX);
    }

    // Imperva / Incapsula: x-iinfo header is their fingerprint
    if response.headers.contains_key("x-iinfo") {
        return PageState::Challenge(ChallengeHint::Imperva);
    }

    let has_cf_ray = response.headers.contains_key("cf-ray");

    if (response.status == 403 || response.status == 503) && has_cf_ray {
        return PageState::Challenge(ChallengeHint::Cloudflare);
    }

    let body_text = response.text().to_lowercase();

    if body_text.contains("datadome") {
        return PageState::Challenge(ChallengeHint::DataDome);
    }

    if body_text.contains("px-captcha") || body_text.contains("perimeterx") {
        return PageState::Challenge(ChallengeHint::PerimeterX);
    }

    if body_text.contains("incapsula") || body_text.contains("_incapsula_resource") {
        return PageState::Challenge(ChallengeHint::Imperva);
    }

    if body_text.contains("hcaptcha") {
        return PageState::Challenge(ChallengeHint::HCaptcha);
    }

    if body_text.contains("recaptcha") {
        return PageState::Challenge(ChallengeHint::Recaptcha);
    }

    if body_text.contains("checking your browser") ||
       body_text.contains("enable javascript") ||
       body_text.contains("cf-browser-verification") {
        return PageState::JsRequired;
    }

    if response.status == 403 {
        return PageState::Blocked;
    }

    PageState::Normal
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
    use std::str::FromStr;

    fn make_resp(status: u16, headers: HeaderMap, body: &str) -> HttpResponse {
        HttpResponse {
            status,
            final_url: "https://example.com".into(),
            headers,
            body: Bytes::from(body.to_string()),
        }
    }

    #[test]
    fn test_classify_normal() {
        let resp = make_resp(200, HeaderMap::new(), "Hello World");
        assert_eq!(classify(&resp), PageState::Normal);
    }

    #[test]
    fn test_classify_rate_limit_default() {
        let resp = make_resp(429, HeaderMap::new(), "Too Many Requests");
        assert_eq!(classify(&resp), PageState::RateLimit(60));
    }

    #[test]
    fn test_classify_rate_limit_with_retry_after() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("retry-after").unwrap(),
            HeaderValue::from_str("120").unwrap(),
        );
        let resp = make_resp(429, headers, "Too Many Requests");
        assert_eq!(classify(&resp), PageState::RateLimit(120));
    }

    #[test]
    fn test_classify_cloudflare_via_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("cf-ray").unwrap(),
            HeaderValue::from_str("abc123-LAX").unwrap(),
        );
        let resp = make_resp(403, headers, "");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::Cloudflare));
    }

    #[test]
    fn test_classify_blocked() {
        let resp = make_resp(403, HeaderMap::new(), "Forbidden");
        assert_eq!(classify(&resp), PageState::Blocked);
    }

    #[test]
    fn test_classify_hcaptcha() {
        let resp = make_resp(200, HeaderMap::new(), "<html>please complete hcaptcha</html>");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::HCaptcha));
    }

    #[test]
    fn test_classify_recaptcha() {
        let resp = make_resp(200, HeaderMap::new(), "<script src='https://www.google.com/recaptcha/api.js'></script>");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::Recaptcha));
    }

    #[test]
    fn test_classify_datadome_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("x-datadome-cid").unwrap(),
            HeaderValue::from_str("some-cid-value").unwrap(),
        );
        let resp = make_resp(403, headers, "");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::DataDome));
    }

    #[test]
    fn test_classify_datadome_body() {
        let resp = make_resp(403, HeaderMap::new(), "<html>Protected by DataDome</html>");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::DataDome));
    }

    #[test]
    fn test_classify_perimeterx_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("x-px-enforcer-telemetry").unwrap(),
            HeaderValue::from_str("1").unwrap(),
        );
        let resp = make_resp(403, headers, "");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::PerimeterX));
    }

    #[test]
    fn test_classify_perimeterx_body() {
        let resp = make_resp(403, HeaderMap::new(), "<div id='px-captcha'>complete the challenge</div>");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::PerimeterX));
    }

    #[test]
    fn test_classify_imperva_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_str("x-iinfo").unwrap(),
            HeaderValue::from_str("12-345678901-0").unwrap(),
        );
        let resp = make_resp(403, headers, "");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::Imperva));
    }

    #[test]
    fn test_classify_imperva_body() {
        let resp = make_resp(403, HeaderMap::new(), "<!-- Incapsula Resource ID -->");
        assert_eq!(classify(&resp), PageState::Challenge(ChallengeHint::Imperva));
    }

    #[test]
    fn test_classify_js_required() {
        let resp = make_resp(200, HeaderMap::new(), "Checking your browser before accessing...");
        assert_eq!(classify(&resp), PageState::JsRequired);
    }
}

