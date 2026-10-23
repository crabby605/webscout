//! The exact header set and order each browser family sends, per kind of request.
//!
//! Anti-bot vendors compare header *order* and the sec-fetch-* / referer / origin values
//! against what the claimed browser really sends for that kind of request (a navigation
//! looks nothing like a fetch() or a <script> load), so every request is built here
//! instead of reusing one default header set.

use url::Url;
use wreq::header::HeaderMap;

use crate::identity::Identity;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dest {
    /// Top-level navigation: address bar, link click, form submit.
    Document,
    /// fetch() / XMLHttpRequest.
    Empty,
    /// Classic <script src> (no-cors).
    Script,
    /// <script type=module> / import (cors).
    Module,
}

pub struct Req<'a> {
    pub method: &'a str,
    pub url: &'a Url,
    pub dest: Dest,
    /// The page that made the request; None = typed into the address bar.
    pub referrer: Option<&'a Url>,
    /// Page-supplied headers (content-type, authorization, x-requested-with...).
    pub extra: &'a [(String, String)],
}

// Wire order per family, captured from real browsers (Chrome 13x+, Firefox 13x+, Safari 18+).
// "cookie" is a slot: the cookie layer fills it. Names absent for a request are skipped.
const CHROMIUM_NAV: &[&str] = &["cache-control", "sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform", "origin", "content-type",
    "upgrade-insecure-requests", "user-agent", "accept", "sec-gpc", "sec-fetch-site", "sec-fetch-mode", "sec-fetch-user",
    "sec-fetch-dest", "referer", "accept-encoding", "accept-language", "cookie", "priority"];
const CHROMIUM_SUB: &[&str] = &["sec-ch-ua-platform", "user-agent", "sec-ch-ua", "content-type", "sec-ch-ua-mobile", "accept",
    "origin", "sec-gpc", "sec-fetch-site", "sec-fetch-mode", "sec-fetch-dest", "referer", "accept-encoding", "accept-language",
    "cookie", "priority"];
const FIREFOX_NAV: &[&str] = &["user-agent", "accept", "accept-language", "accept-encoding", "content-type", "origin", "referer",
    "cookie", "upgrade-insecure-requests", "sec-fetch-dest", "sec-fetch-mode", "sec-fetch-site", "sec-fetch-user", "priority", "te"];
const FIREFOX_SUB: &[&str] = &["user-agent", "accept", "accept-language", "accept-encoding", "referer", "content-type", "origin",
    "cookie", "sec-fetch-dest", "sec-fetch-mode", "sec-fetch-site", "priority", "te"];
const SAFARI_NAV: &[&str] = &["origin", "content-type", "sec-fetch-dest", "user-agent", "accept", "sec-fetch-site", "sec-fetch-mode",
    "referer", "accept-language", "priority", "accept-encoding", "cookie"];
const SAFARI_SUB: &[&str] = &["sec-fetch-dest", "user-agent", "accept", "content-type", "origin", "sec-fetch-site", "sec-fetch-mode",
    "referer", "accept-language", "priority", "accept-encoding", "cookie"];

/// Ordered (name, value) pairs; value None marks a slot another layer fills (cookie).
pub fn build(id: &Identity, base: &HeaderMap, r: &Req) -> Vec<(String, Option<String>)> {
    let family = match id.browser.as_str() { "firefox" => "firefox", "safari" => "safari", _ => "chromium" };
    let nav = r.dest == Dest::Document;
    let order = match (family, nav) {
        ("firefox", true) => FIREFOX_NAV, ("firefox", false) => FIREFOX_SUB,
        ("safari", true) => SAFARI_NAV, ("safari", false) => SAFARI_SUB,
        (_, true) => CHROMIUM_NAV, (_, false) => CHROMIUM_SUB,
    };
    let extra = |k: &str| r.extra.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.clone());
    let base_val = |k: &str| base.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    let post = !matches!(r.method, "GET" | "HEAD");
    let same_origin = r.referrer.is_some_and(|f| f.origin() == r.url.origin());
    let site = match r.referrer {
        None => "none",
        Some(_) if same_origin => "same-origin",
        Some(f) if registrable(f) == registrable(r.url) => "same-site",
        Some(_) => "cross-site",
    };
    // Default policy strict-origin-when-cross-origin: full URL same-origin, origin only
    // cross-origin, nothing on an https -> http downgrade.
    let referer = r.referrer.and_then(|f| {
        if f.scheme() == "https" && r.url.scheme() == "http" { None }
        else if same_origin { let mut f = f.clone(); f.set_fragment(None); Some(f.to_string()) }
        else { Some(format!("{}/", f.origin().ascii_serialization())) }
    });
    let origin = r.referrer.filter(|_| post || (r.dest != Dest::Document && !same_origin && r.dest != Dest::Script))
        .map(|f| f.origin().ascii_serialization());
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for &name in order {
        let v = match name {
            "cookie" => { out.push(("cookie".into(), None)); continue; }
            "user-agent" => Some(id.user_agent.clone()),
            "sec-ch-ua" => id.sec_ch_ua.clone(),
            "sec-ch-ua-mobile" => id.sec_ch_ua.as_ref().map(|_| if id.device.kind == crate::identity::Kind::Phone { "?1" } else { "?0" }.into()),
            "sec-ch-ua-platform" => id.sec_ch_ua.as_ref().and(base_val(name)),
            "sec-gpc" => (id.browser == "brave").then(|| "1".into()),
            "upgrade-insecure-requests" => (family != "safari").then(|| "1".into()),
            "cache-control" => post.then(|| "max-age=0".into()),
            "accept" => extra(name).or_else(|| Some(if nav { base_val(name).unwrap_or_default() } else { "*/*".into() })),
            "accept-encoding" | "accept-language" => base_val(name),
            "content-type" => extra(name),
            "origin" => origin.clone(),
            "referer" => referer.clone(),
            "sec-fetch-site" => Some(site.into()),
            "sec-fetch-mode" => Some(match r.dest { Dest::Document => "navigate", Dest::Script => "no-cors", _ => "cors" }.into()),
            "sec-fetch-dest" => Some(match r.dest { Dest::Document => "document", Dest::Empty => "empty", _ => "script" }.into()),
            "sec-fetch-user" => nav.then(|| "?1".into()),
            "te" => Some("trailers".into()),
            // ponytail: approximate per-family fetch priorities for subresources.
            "priority" => Some(match (family, r.dest) {
                (_, Dest::Document) => "u=0, i",
                ("firefox", Dest::Empty) => "u=4",
                ("safari", Dest::Empty) => "u=3, i",
                (_, Dest::Empty) => "u=1, i",
                _ => "u=2",
            }.into()),
            _ => None,
        };
        if let Some(v) = v {
            out.push((name.into(), Some(v)));
        }
        // Page-set custom headers (authorization, x-requested-with...) go right after the UA.
        if name == "user-agent" {
            for (k, v) in r.extra {
                let k = k.to_ascii_lowercase();
                if !order.contains(&k.as_str()) && k != "host" {
                    out.push((k, Some(v.clone())));
                }
            }
        }
    }
    if let Some(ip) = &id.forwarded_for {
        // What a forwarding proxy appends: after the browser's own headers.
        out.push(("x-forwarded-for".into(), Some(ip.clone())));
        out.push(("x-real-ip".into(), Some(ip.clone())));
    }
    out
}

/// ponytail: last two labels as the "site"; wrong for co.uk-style suffixes, use the
/// public suffix list (psl crate) if same-site vs cross-site starts to matter there.
fn registrable(u: &Url) -> String {
    let host = u.host_str().unwrap_or_default();
    let labels: Vec<&str> = host.rsplit('.').take(2).collect();
    format!("{}://{}", u.scheme(), labels.into_iter().rev().collect::<Vec<_>>().join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[(String, Option<String>)]) -> Vec<&str> {
        v.iter().map(|(k, _)| k.as_str()).collect()
    }

    #[test]
    fn navigation_vs_fetch_headers() {
        let dev = crate::identity::builtin().into_iter().find(|d| d.name == "win-desktop-rtx3060").unwrap();
        let id = Identity::build("chrome", dev).unwrap();
        let base = id.emulation_headers();
        let url = Url::parse("https://shop.example.com/p/1").unwrap();
        let nav = build(&id, &base, &Req { method: "GET", url: &url, dest: Dest::Document, referrer: None, extra: &[] });
        assert_eq!(names(&nav), ["sec-ch-ua", "sec-ch-ua-mobile", "sec-ch-ua-platform", "upgrade-insecure-requests", "user-agent",
            "accept", "sec-fetch-site", "sec-fetch-mode", "sec-fetch-user", "sec-fetch-dest", "accept-encoding", "accept-language",
            "cookie", "priority"]);
        let page = Url::parse("https://www.example.com/cart?x=1#top").unwrap();
        let api = Url::parse("https://api.other.com/v1").unwrap();
        let extra = [("Content-Type".to_string(), "application/json".to_string())];
        let xhr = build(&id, &base, &Req { method: "POST", url: &api, dest: Dest::Empty, referrer: Some(&page), extra: &extra });
        let get = |k: &str| xhr.iter().find(|(n, _)| n == k).and_then(|(_, v)| v.clone());
        assert_eq!(get("sec-fetch-site").as_deref(), Some("cross-site"));
        assert_eq!(get("origin").as_deref(), Some("https://www.example.com"));
        assert_eq!(get("referer").as_deref(), Some("https://www.example.com/"));
        assert_eq!(get("accept").as_deref(), Some("*/*"));
        assert_eq!(get("content-type").as_deref(), Some("application/json"));
        let same = build(&id, &base, &Req { method: "GET", url: &url, dest: Dest::Empty, referrer: Some(&page), extra: &[] });
        assert!(same.iter().any(|(k, v)| k == "sec-fetch-site" && v.as_deref() == Some("same-site")));
        assert!(same.iter().any(|(k, _)| k == "origin")); // cross-origin cors requests carry Origin
        let own = Url::parse("https://www.example.com/api").unwrap();
        let own = build(&id, &base, &Req { method: "GET", url: &own, dest: Dest::Empty, referrer: Some(&page), extra: &[] });
        assert!(!own.iter().any(|(k, _)| k == "origin"));
        assert!(own.iter().any(|(k, v)| k == "referer" && v.as_deref() == Some("https://www.example.com/cart?x=1")));
    }
}
