use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};
use wreq::IntoEmulation;
use wreq_util::{Emulation, Platform, Profile};

use crate::error::{Result, ScoutError};

/// The built-in catalog, copied to ~/.webscout/identities/ on first use so it can be edited.
const BUILTIN: [(&str, &str); 3] = [
    ("pcs.toml", include_str!("../identities/pcs.toml")),
    ("phones.toml", include_str!("../identities/phones.toml")),
    ("tablets.toml", include_str!("../identities/tablets.toml")),
];
pub const BROWSERS: [&str; 6] = ["chrome", "edge", "opera", "brave", "firefox", "safari"];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind { Desktop, Phone, Tablet }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gpu {
    pub vendor: String,
    pub renderer: String,
}

/// One entry of identities/*.toml (field docs at the top of pcs.toml).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub kind: Kind,
    pub vendor: String,
    #[serde(default)]
    pub model: String,
    pub os: Platform,
    pub os_version: String,
    pub browsers: Vec<String>,
    pub screen: [u32; 2],
    pub dpr: f32,
    pub cpu_cores: u32,
    pub ram_gb: u32,
    #[serde(default)]
    pub touch: u32,
    pub arch: String,
    pub gpu: Gpu,
}

#[derive(Deserialize)]
struct CatalogFile {
    device: Vec<Device>,
}

/// One browser on one device, consistent at every layer: `profile` + the device's OS drive
/// the TLS, HTTP/2 and header fingerprint (wreq-util); `user_agent` / `sec_ch_ua` override
/// that profile's headers; `device` feeds the JS side (screen, GPU, cores, touch...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub browser: String,
    pub profile: Profile,
    pub device: Device,
    pub user_agent: String,
    /// Chromium only.
    pub sec_ch_ua: Option<String>,
    pub accept_language: String,
    /// Where requests exit: http(s):// or socks5:// proxy URL. The only thing that really
    /// changes the IP sites see; anti-bot systems key on the connection's address.
    #[serde(default)]
    pub proxy: Option<String>,
    /// Opt-in X-Forwarded-For / X-Real-IP value. Only fools servers that trust client-sent
    /// forwarding headers (no CDN in front); real browsers never send these, so bot walls
    /// (Cloudflare, Akamai, DataDome...) treat it as a bot signal. Off by default.
    #[serde(default)]
    pub forwarded_for: Option<String>,
    /// Captured from a real browser (handoff): never re-derived or version-bumped, since
    /// the site's clearance is bound to exactly this UA and fingerprint.
    #[serde(default)]
    pub pinned: bool,
}

fn catalog_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var("HOME").ok()?).join(".webscout/identities"))
}

/// All devices from ~/.webscout/identities/*.toml (seeded from the built-ins when missing).
pub fn catalog() -> Result<Vec<Device>> {
    let err = |e: String| ScoutError::Session(format!("identities: {e}"));
    let mut files: Vec<(String, String)> = Vec::new();
    match catalog_dir() {
        Some(dir) => {
            if !dir.exists() {
                std::fs::create_dir_all(&dir).map_err(|e| err(e.to_string()))?;
                for (name, src) in BUILTIN {
                    std::fs::write(dir.join(name), src).map_err(|e| err(e.to_string()))?;
                }
            }
            for entry in std::fs::read_dir(&dir).map_err(|e| err(e.to_string()))?.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "toml") {
                    let src = std::fs::read_to_string(&path).map_err(|e| err(e.to_string()))?;
                    files.push((path.display().to_string(), src));
                }
            }
        }
        None => return Ok(builtin()),
    }
    let mut devices = Vec::new();
    for (path, src) in files {
        let file: CatalogFile = toml::from_str(&src).map_err(|e| err(format!("{path}: {e}")))?;
        devices.extend(file.device);
    }
    Ok(devices)
}

pub fn builtin() -> Vec<Device> {
    BUILTIN.iter().flat_map(|(_, s)| toml::from_str::<CatalogFile>(s).expect("built-in identities parse").device).collect()
}

impl Identity {
    /// Random identity for this machine: `WEBSCOUT_IDENTITY` if set, else a desktop running
    /// the host's OS. The TCP/IP fingerprint (TTL, window, option order) comes from the real
    /// kernel, and e.g. a Windows UA over a macOS TCP stack is a known bot tell.
    pub fn generate() -> Self {
        let host = if cfg!(target_os = "macos") { "macos" } else if cfg!(target_os = "windows") { "windows" } else { "linux" };
        let spec = std::env::var("WEBSCOUT_IDENTITY").unwrap_or_else(|_| format!("desktop {host}"));
        Self::pick(&spec).or_else(|_| Self::pick("")).expect("identity catalog is empty")
    }

    /// Random identity matching `spec`: words that must all match a device's name, vendor,
    /// kind (desktop/pc, phone/mobile, tablet) or OS, plus optionally a browser name.
    /// e.g. "phone samsung", "ios", "tablet android", "windows edge", "iqoo-13 brave".
    pub fn pick(spec: &str) -> Result<Self> {
        Self::pick_from(catalog()?, spec)
    }

    /// Whether this identity is one `spec` could have picked.
    pub fn matches(&self, spec: &str) -> bool {
        let (browser, terms) = spec_words(spec);
        browser.iter().all(|b| *b == self.browser)
            && terms.iter().all(|t| self.device.name == *t || device_matches(&self.device, t))
    }

    fn pick_from(devices: Vec<Device>, spec: &str) -> Result<Self> {
        let (browser, terms) = spec_words(spec);
        // An exact device name means that device, not every name containing it.
        let exact = |t: &str| devices.iter().any(|d| d.name == t);
        let mut options = Vec::new();
        for device in devices.iter().cloned() {
            if !terms.iter().all(|t| if exact(t) { device.name == **t } else { device_matches(&device, t) }) {
                continue;
            }
            for b in &device.browsers {
                if browser.iter().all(|want| want == b) {
                    options.push((b.clone(), device.clone()));
                }
            }
        }
        let (browser, device) = options.choose(&mut rand::thread_rng())
            .ok_or_else(|| ScoutError::Session(format!("no identity in ~/.webscout/identities matches {spec:?}")))?;
        Self::build(browser, device.clone())
    }

    pub fn build(browser: &str, device: Device) -> Result<Self> {
        use Platform::*;
        use Profile::*;
        let (profile, net_os) = match (browser, device.os, device.kind) {
            (_, IOS, _) if browser != "safari" => return Err(ScoutError::Session(format!("{}: only safari runs on iOS", device.name))),
            ("chrome" | "brave", os, _) => (Chrome149, os),
            ("edge", os, _) => (Edge148, os),
            ("opera", os, _) => (Opera131, os),
            ("firefox", Android, _) => (FirefoxAndroid135, Android),
            ("firefox", os, _) => (Firefox151, os),
            ("safari", IOS, Kind::Phone) => (SafariIos26_2, IOS),
            // iPadOS Safari asks for desktop sites: same UA and headers as macOS Safari.
            ("safari", IOS | MacOS, _) => (Safari26_4, MacOS),
            _ => return Err(ScoutError::Session(format!("{}: {browser} isn't available on {:?}", device.name, device.os))),
        };
        let headers = emulation_of(profile, net_os).into_emulation().headers;
        let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
        let mut ua = header("user-agent").unwrap_or_default();
        let mut sec_ch_ua = header("sec-ch-ua");
        // Chrome/Edge/Firefox/Opera auto-update, so a months-old version is itself a bot signal
        // (Cloudflare and PerimeterX block it). Their TLS fingerprints barely change between
        // majors, so claim the current one. Safari is tied to OS updates and stays as is.
        if let Some(opr) = major_after(&ua, "OPR/") {
            // ponytail: Opera trails Chrome stable by ~2 Chromium majors and numbers itself
            // Chromium - 16 (OPR 131 = Chromium 147); re-check against opera.com if it drifts.
            let old = major_after(&ua, "Chrome/").unwrap();
            let new = current_major(156) - 2;
            ua = ua.replace(&format!("Chrome/{old}."), &format!("Chrome/{new}."))
                .replace(&format!("OPR/{opr}."), &format!("OPR/{}.", new - 16));
            sec_ch_ua = Some(chromium_sec_ch_ua(new, "Opera", new - 16));
        } else if let Some(old) = major_after(&ua, "Chrome/") {
            let new = current_major(156);
            ua = ua.replace(&format!("Chrome/{old}."), &format!("Chrome/{new}."))
                .replace(&format!("Edg/{old}."), &format!("Edg/{new}."))
                .replace(&format!("EdgA/{old}."), &format!("EdgA/{new}."));
            let brand = match browser { "edge" => "Microsoft Edge", "brave" => "Brave", _ => "Google Chrome" };
            sec_ch_ua = Some(chromium_sec_ch_ua(new, brand, new));
            // Chrome on an Android tablet drops "Mobile" (and sends sec-ch-ua-mobile: ?0).
            if device.kind == Kind::Tablet {
                ua = ua.replace(" Mobile Safari/", " Safari/");
            }
        } else if let Some(old) = major_after(&ua, "Firefox/") {
            let new = current_major(158);
            ua = ua.replace(&format!("rv:{old}.0"), &format!("rv:{new}.0"))
                .replace(&format!("Firefox/{old}.0"), &format!("Firefox/{new}.0"))
                .replace(&format!("Gecko/{old}.0"), &format!("Gecko/{new}.0"));
            // Firefox for Android shows the real Android version and form factor.
            if let (Some(v), Some(start)) = (major_after(&ua, "Android "), device.os_version.split('.').next()) {
                ua = ua.replace(&format!("Android {v};"), &format!("Android {start};"));
            }
            if device.kind == Kind::Tablet {
                ua = ua.replace("; Mobile;", "; Tablet;");
            }
        }
        Ok(Identity {
            browser: browser.to_string(),
            profile,
            accept_language: header("accept-language").unwrap_or_else(|| "en-US,en;q=0.9".into()),
            device,
            user_agent: ua,
            sec_ch_ua,
            proxy: None,
            forwarded_for: None,
            pinned: false,
        })
    }

    /// Same browser and device, current version: a saved session "auto-updates" like the
    /// real browser would, instead of claiming a version that gets staler every month.
    pub fn refreshed(self) -> Self {
        if self.pinned {
            return self;
        }
        match Self::build(&self.browser, self.device.clone()) {
            Ok(id) => Self { accept_language: self.accept_language, proxy: self.proxy, forwarded_for: self.forwarded_for, ..id },
            Err(_) => self,
        }
    }

    /// Applies the network options of a new session. `forwarded_for: "random"` picks a
    /// random public IPv4 once, so the session keeps claiming the same address.
    pub fn with_network(mut self, proxy: Option<&str>, forwarded_for: Option<&str>) -> Self {
        self.proxy = proxy.or(std::env::var("WEBSCOUT_PROXY").ok().as_deref()).filter(|p| !p.is_empty()).map(str::to_string);
        self.forwarded_for = forwarded_for.map(|f| if f == "random" { random_public_ipv4() } else { f.to_string() });
        self
    }

    /// The network OS: an iPad in desktop mode is macOS Safari on the wire.
    fn net_os(&self) -> Platform {
        if self.profile == Profile::Safari26_4 { Platform::MacOS } else { self.device.os }
    }

    pub fn emulation(&self) -> Emulation {
        emulation_of(self.profile, self.net_os())
    }

    /// The profile's captured header values (accept, accept-encoding, accept-language,
    /// sec-ch-ua-platform); http::headers decides which to send, and in what order.
    pub fn emulation_headers(&self) -> wreq::header::HeaderMap {
        self.emulation().into_emulation().headers
    }

    /// One line for logs and `session show`.
    pub fn summary(&self) -> String {
        format!("{} on {} ({:?}, {:?})", self.browser, self.device.name, self.device.kind, self.device.os)
    }
}

/// Splits an identity spec into (browser names, device terms), lowercased.
fn spec_words(spec: &str) -> (Vec<String>, Vec<String>) {
    spec.split([' ', ',', ':', '+', '/']).filter(|w| !w.is_empty()).map(str::to_lowercase)
        .partition(|w| BROWSERS.contains(&w.as_str()))
}

fn random_public_ipv4() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    loop {
        let [a, b, c, d]: [u8; 4] = rng.r#gen();
        // Skip private, loopback, link-local, CGNAT, multicast and reserved ranges.
        let reserved = matches!(a, 0 | 10 | 127 | 224..=255) || (a == 100 && (64..128).contains(&b))
            || (a == 169 && b == 254) || (a == 172 && (16..32).contains(&b)) || (a == 192 && b == 168);
        if !reserved && d != 0 {
            return format!("{a}.{b}.{c}.{d}");
        }
    }
}

fn device_matches(d: &Device, term: &str) -> bool {
    let kind = match d.kind { Kind::Desktop => ["desktop", "pc"], Kind::Phone => ["phone", "mobile"], Kind::Tablet => ["tablet", "tab"] };
    let os = match d.os { Platform::MacOS => "macos mac", Platform::Windows => "windows win", Platform::Linux => "linux",
                          Platform::Android => "android", Platform::IOS => "ios", _ => "" };
    kind.contains(&term) || os.split(' ').any(|o| o == term) || d.vendor.to_lowercase() == term || d.name.contains(term)
}

fn emulation_of(profile: Profile, os: Platform) -> Emulation {
    Emulation::builder().profile(profile).platform(os).build()
}

fn major_after(ua: &str, marker: &str) -> Option<u32> {
    let rest = &ua[ua.find(marker)? + marker.len()..];
    rest.split(['.', ';', ')']).next()?.parse().ok()
}

/// Chrome and Firefox ship a new major every 4 weeks. `base` was current on 2026-10-22.
fn current_major(base: u32) -> u32 {
    const ANCHOR_DAY: u64 = 20748; // 2026-10-22, in days since the epoch
    let today = SystemTime::now().duration_since(UNIX_EPOCH).map_or(ANCHOR_DAY, |d| d.as_secs() / 86400);
    base + (today.saturating_sub(ANCHOR_DAY) / 28) as u32
}

/// Chromium's sec-ch-ua: the GREASE brand, its version and the list order are all
/// seeded by the Chromium major (chromium's GenerateBrandVersionList). `brand_major`
/// differs from it only for browsers with their own numbering (Opera).
pub fn chromium_sec_ch_ua(major: u32, brand: &str, brand_major: u32) -> String {
    const CHARS: [&str; 11] = [" ", "(", ":", "-", ".", "/", ")", ";", "=", "?", "_"];
    const VERSIONS: [&str; 3] = ["8", "99", "24"];
    const ORDERS: [[usize; 3]; 6] = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
    let s = major as usize;
    let order = ORDERS[s % 6];
    let mut list = [String::new(), String::new(), String::new()];
    list[order[0]] = format!("\"Not{}A{}Brand\";v=\"{}\"", CHARS[s % 11], CHARS[(s + 1) % 11], VERSIONS[s % 3]);
    list[order[1]] = format!("\"Chromium\";v=\"{major}\"");
    list[order[2]] = format!("\"{brand}\";v=\"{brand_major}\"");
    list.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_identity_is_coherent() {
        for d in builtin() {
            for b in &d.browsers {
                let id = Identity::build(b, d.clone()).unwrap_or_else(|e| panic!("{e}"));
                let ua = &id.user_agent;
                let ua_os = if ua.contains("Windows") { Platform::Windows } else if ua.contains("Android") { Platform::Android }
                    else if ua.contains("Linux") { Platform::Linux } else if ua.contains("iPhone") { Platform::IOS } else { Platform::MacOS };
                assert_eq!(ua_os, id.net_os(), "{b} on {}: {ua}", d.name);
                assert_eq!(id.sec_ch_ua.is_some(), ua.contains("Chrome/"), "{b} on {}", d.name);
                assert_eq!(ua.contains("Mobile"), d.kind == Kind::Phone, "{b} on {}: {ua}", d.name);
                if let Some(major) = major_after(ua, "Chrome/") {
                    assert!(id.sec_ch_ua.as_ref().unwrap().contains(&format!("\"Chromium\";v=\"{major}\"")), "{id:?}");
                }
            }
        }
        // Real Brave 129: "Brave";v="129", "Not=A?Brand";v="8", "Chromium";v="129"
        assert_eq!(chromium_sec_ch_ua(129, "Brave", 129), r#""Brave";v="129", "Not=A?Brand";v="8", "Chromium";v="129""#);
    }

    #[test]
    fn specs_pick_matching_devices() {
        let pick = |spec| Identity::pick_from(builtin(), spec);
        for _ in 0..20 {
            let id = pick("phone samsung").unwrap();
            assert!(id.device.vendor == "Samsung" && id.device.kind == Kind::Phone, "{id:?}");
            assert_eq!(pick("windows edge").unwrap().browser, "edge");
            assert!(pick("ipad").unwrap().user_agent.contains("Macintosh"));
            assert_eq!(pick("iphone-16-pro").unwrap().device.name, "iphone-16-pro");
            let tab = pick("tablet samsung chrome").unwrap();
            assert!(tab.matches("tablet samsung chrome") && tab.matches("android") && !tab.matches("iphone") && !tab.matches("firefox"));
        }
        assert!(pick("ios chrome").is_err());
    }

    #[test]
    fn sec_ch_ua_matches_real_chrome() {
        // Ground truth: the headers wreq-util captured from real Chrome/Edge/Opera builds.
        for (profile, brand) in [(Profile::Chrome147, "Google Chrome"), (Profile::Chrome148, "Google Chrome"),
                                 (Profile::Chrome149, "Google Chrome"), (Profile::Edge148, "Microsoft Edge"),
                                 (Profile::Opera131, "Opera")] {
            let headers = emulation_of(profile, Platform::Windows).into_emulation().headers;
            let real = headers.get("sec-ch-ua").unwrap().to_str().unwrap();
            let ua = headers.get("user-agent").unwrap().to_str().unwrap();
            let major = major_after(ua, "Chrome/").unwrap();
            let brand_major = major_after(ua, "OPR/").unwrap_or(major);
            assert_eq!(chromium_sec_ch_ua(major, brand, brand_major), real, "{profile:?}");
        }
    }
}
