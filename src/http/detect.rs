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

    let body_text = response.text().to_lowercase();

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

    PageState::Normal
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use reqwest::header::HeaderMap;

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
}

