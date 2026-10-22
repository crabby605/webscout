use std::time::{SystemTime, UNIX_EPOCH};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use wreq::IntoEmulation;
use wreq_util::{Emulation, Platform, Profile};

/// One browser, consistent at every layer: `profile` + `os` drive the TLS, HTTP/2
/// and header fingerprint (wreq-util); `user_agent` / `sec_ch_ua` override that
/// profile's headers and feed the JS side (navigator.userAgent etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub profile: Profile,
    pub os: Platform,
    pub user_agent: String,
    /// Chromium only.
    pub sec_ch_ua: Option<String>,
    pub accept_language: String,
    pub viewport: String,
    /// "Windows" | "macOS" | "Linux"
    pub platform: String,
}

impl Identity {
    pub fn generate() -> Self {
        use Platform::*;
        use Profile::*;
        // Claim the host's OS: the TCP/IP fingerprint (TTL, window, option order) comes from
        // the real kernel, and e.g. a Windows UA over a macOS TCP stack is a known bot tell.
        // Newest TLS/HTTP2 profile per family; the version they claim is bumped to current below.
        let (os, profiles): (Platform, &[Profile]) = if cfg!(target_os = "macos") {
            // Safari twice: only exists on macOS, and passed every blocker we tested (2026-10).
            (MacOS, &[Safari26_4, Safari26_4, Chrome149, Firefox151])
        } else if cfg!(target_os = "windows") {
            (Windows, &[Chrome149, Chrome149, Edge148, Firefox151])
        } else {
            (Linux, &[Chrome149, Firefox151])
        };
        Self::from_profile(*profiles.choose(&mut rand::thread_rng()).unwrap(), os)
    }

    pub fn from_profile(profile: Profile, os: Platform) -> Self {
        let headers = Self::emulation_of(profile, os).into_emulation().headers;
        let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
        let mut user_agent = header("user-agent").unwrap_or_default();
        let mut sec_ch_ua = header("sec-ch-ua");
        // Chrome/Edge/Firefox auto-update, so a months-old version is itself a bot signal
        // (Cloudflare and PerimeterX block it). Their TLS fingerprints barely change between
        // majors, so claim the current one. Safari is tied to OS updates and stays as is.
        if let Some(old) = major_after(&user_agent, "Chrome/") {
            let new = current_major(156);
            user_agent = user_agent.replace(&format!("Chrome/{old}."), &format!("Chrome/{new}."))
                .replace(&format!("Edg/{old}."), &format!("Edg/{new}."));
            let brand = if user_agent.contains("Edg/") { "Microsoft Edge" } else { "Google Chrome" };
            sec_ch_ua = Some(chromium_sec_ch_ua(new, brand));
        } else if let Some(old) = major_after(&user_agent, "Firefox/") {
            let new = current_major(158);
            user_agent = user_agent.replace(&format!("rv:{old}.0"), &format!("rv:{new}.0"))
                .replace(&format!("Firefox/{old}.0"), &format!("Firefox/{new}.0"));
        }
        Identity {
            profile,
            os,
            user_agent,
            sec_ch_ua,
            accept_language: header("accept-language").unwrap_or_else(|| "en-US,en;q=0.9".into()),
            viewport: "1920x1080".into(),
            platform: match os { Platform::Windows => "Windows", Platform::Linux => "Linux", _ => "macOS" }.into(),
        }
    }

    pub fn emulation(&self) -> Emulation {
        Self::emulation_of(self.profile, self.os)
    }

    fn emulation_of(profile: Profile, os: Platform) -> Emulation {
        Emulation::builder().profile(profile).platform(os).build()
    }
}

fn major_after(ua: &str, marker: &str) -> Option<u32> {
    let rest = &ua[ua.find(marker)? + marker.len()..];
    rest.split('.').next()?.parse().ok()
}

/// Chrome and Firefox ship a new major every 4 weeks. `base` was current on 2026-10-22.
fn current_major(base: u32) -> u32 {
    const ANCHOR_DAY: u64 = 20748; // 2026-10-22, in days since the epoch
    let today = SystemTime::now().duration_since(UNIX_EPOCH).map_or(ANCHOR_DAY, |d| d.as_secs() / 86400);
    base + (today.saturating_sub(ANCHOR_DAY) / 28) as u32
}

/// Chromium's sec-ch-ua: the GREASE brand, its version and the list order are all
/// seeded by the major version (chromium's GenerateBrandVersionList).
fn chromium_sec_ch_ua(major: u32, brand: &str) -> String {
    const CHARS: [&str; 11] = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"];
    const VERSIONS: [&str; 3] = ["8", "99", "24"];
    const ORDERS: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    let s = major as usize;
    let order = ORDERS[s % 6];
    let mut list = [String::new(), String::new(), String::new()];
    list[order[0]] = format!("\"Not{}A{}Brand\";v=\"{}\"", CHARS[s % 11], CHARS[(s + 1) % 11], VERSIONS[s % 3]);
    list[order[1]] = format!("\"Chromium\";v=\"{major}\"");
    list[order[2]] = format!("\"{brand}\";v=\"{major}\"");
    list.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_coherence() {
        for _ in 0..20 {
            let ident = Identity::generate();
            assert!(ident.user_agent.starts_with("Mozilla/5.0"), "{ident:?}");
            let ua_os = if ident.user_agent.contains("Windows") { "Windows" } else if ident.user_agent.contains("Linux") { "Linux" } else { "macOS" };
            assert_eq!(ua_os, ident.platform, "{ident:?}");
            assert_eq!(ident.sec_ch_ua.is_some(), ident.user_agent.contains("Chrome/"), "{ident:?}");
            if let Some(major) = major_after(&ident.user_agent, "Chrome/") {
                assert!(ident.sec_ch_ua.as_ref().unwrap().contains(&format!("\"Chromium\";v=\"{major}\"")), "{ident:?}");
            }
        }
    }

    #[test]
    fn sec_ch_ua_matches_real_chrome() {
        // Ground truth: the headers wreq-util captured from real Chrome/Edge builds.
        for (profile, brand) in [(Profile::Chrome147, "Google Chrome"), (Profile::Chrome148, "Google Chrome"),
                                 (Profile::Chrome149, "Google Chrome"), (Profile::Edge148, "Microsoft Edge")] {
            let headers = Identity::emulation_of(profile, Platform::Windows).into_emulation().headers;
            let real = headers.get("sec-ch-ua").unwrap().to_str().unwrap();
            let ua = headers.get("user-agent").unwrap().to_str().unwrap();
            assert_eq!(chromium_sec_ch_ua(major_after(ua, "Chrome/").unwrap(), brand), real, "{profile:?}");
        }
    }
}
