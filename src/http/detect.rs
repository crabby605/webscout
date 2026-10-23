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

    let has_cf_ray = response.headers.contains_key("cf-ray");

    if (response.status == 403 || response.status == 503) && has_cf_ray {
        return PageState::Challenge(ChallengeHint::Cloudflare);
    }

    if response.status == 403 && !has_cf_ray {
        return PageState::Blocked;
    }

    // Plenty of real pages embed a captcha widget (login, newsletter) or carry "enable
    // JavaScript" <noscript> text; an interstitial is a small page or an error status.
    // ponytail: size heuristic; tune the threshold if a wall turns up above it.
    if response.status < 400 && response.body.len() > 30_000 {
        return PageState::Normal;
    }
    let body_text = response.text().to_lowercase();

    if body_text.contains("hcaptcha") {
        return PageState::Challenge(ChallengeHint::HCaptcha);
    }

    if body_text.contains("recaptcha") {
        return PageState::Challenge(ChallengeHint::Recaptcha);
    }

    // DataDome, PerimeterX ("press & hold"), Cloudflare managed challenge.
    if ["captcha-delivery.com", "px-captcha", "challenge-platform", "just a moment"].iter().any(|m| body_text.contains(m)) {
        return PageState::Challenge(ChallengeHint::Unknown);
    }

    if body_text.contains("checking your browser") ||
       body_text.contains("enable javascript") ||
       body_text.contains("cf-browser-verification") {
        return PageState::JsRequired;
    }

    PageState::Normal
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use wreq::header::HeaderMap;

    #[test]
    fn test_classify_normal() {
        let resp = HttpResponse {
            status: 200,
            final_url: "https://example.com".into(),
            headers: HeaderMap::new(),
            body: Bytes::from("Hello World"),
        };
        assert_eq!(classify(&resp), PageState::Normal);
    }

    #[test]
    fn captcha_widget_on_a_real_page_is_not_a_challenge() {
        let page = |status, body: String| HttpResponse { status, final_url: "https://shop.example".into(), headers: HeaderMap::new(), body: Bytes::from(body) };
        let big = format!("<script src=https://www.google.com/recaptcha/api.js></script>{}", "<p>product</p>".repeat(5000));
        assert_eq!(classify(&page(200, big)), PageState::Normal);
        let wall = "<script src=https://www.google.com/recaptcha/api.js></script><p>verify</p>".to_string();
        assert_eq!(classify(&page(200, wall)), PageState::Challenge(ChallengeHint::Recaptcha));
        let dd = "<script src=https://geo.captcha-delivery.com/captcha/></script>".to_string();
        assert_eq!(classify(&page(200, dd)), PageState::Challenge(ChallengeHint::Unknown));
    }
}

