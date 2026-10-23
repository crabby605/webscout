//! Human handoff: when a page needs a person (captcha, bot wall, login), open it in a real
//! Chrome, let a human get through, then hand the session that Chrome's cookies *and* its
//! exact fingerprint, so webscout carries on as the same browser the site just cleared.
//!
//! On a desktop the human uses the Chrome window directly. On a headless runner Chrome runs
//! headless and the daemon serves a viewer page (live screenshots, clicks and keys forwarded
//! over CDP) that the human opens from anywhere.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};
use wreq::ws::message::Message;
use wreq_util::{Platform, Profile};

use crate::error::{Result, ScoutError};
use crate::identity::{chromium_sec_ch_ua, Device, Gpu, Identity, Kind};

fn err(e: impl std::fmt::Display) -> ScoutError {
    ScoutError::Session(format!("handoff: {e}"))
}

/// Minimal Chrome DevTools Protocol client: one browser-level websocket, page commands
/// routed through a flattened target session.
#[derive(Clone)]
struct Cdp {
    out: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<std::result::Result<Value, String>>>>>,
    next: Arc<AtomicU64>,
    page: Arc<Mutex<Option<String>>>,
}

impl Cdp {
    async fn connect(ws_url: &str) -> Result<Self> {
        let mut ws = wreq::Client::new().websocket(ws_url).send().await?.into_websocket().await?;
        let (out, mut rx) = mpsc::unbounded_channel::<String>();
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<_>>>> = Default::default();
        let replies = Arc::clone(&pending);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = ws.recv() => {
                        let Some(Ok(Message::Text(text))) = msg else {
                            if matches!(msg, None | Some(Err(_)) | Some(Ok(Message::Close(_)))) { break } else { continue }
                        };
                        let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                        let Some(id) = v["id"].as_u64() else { continue }; // events: unused
                        if let Some(tx) = replies.lock().unwrap().remove(&id) {
                            let _ = tx.send(match v.get("error") { Some(e) => Err(e.to_string()), None => Ok(v["result"].clone()) });
                        }
                    }
                    cmd = rx.recv() => match cmd {
                        Some(c) => if ws.send(Message::text(c)).await.is_err() { break },
                        None => break,
                    },
                }
            }
            replies.lock().unwrap().clear(); // fails every waiting call: Chrome is gone
        });
        Ok(Cdp { out, pending, next: Arc::new(AtomicU64::new(1)), page: Default::default() })
    }

    /// A browser-level command (`page: false`) or one for the attached page.
    async fn call(&self, method: &str, params: Value, page: bool) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if page {
            msg["sessionId"] = json!(self.page.lock().unwrap().clone().ok_or_else(|| err("no page attached"))?);
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.out.send(msg.to_string()).map_err(|_| err("chrome is gone"))?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(err(format!("{method}: {e}"))),
            _ => Err(err(format!("{method}: chrome is gone"))),
        }
    }

    async fn eval(&self, expr: &str) -> Result<Value> {
        let r = self.call("Runtime.evaluate", json!({ "expression": expr, "awaitPromise": true, "returnByValue": true }), true).await?;
        Ok(r["result"]["value"].clone())
    }
}

/// How the handoff ends by itself (else a human/runner calls `done`).
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Until {
    /// Finish once Chrome holds a cookie with this name (e.g. "cf_clearance", "datadome").
    #[serde(default)]
    pub until_cookie: Option<String>,
    /// Finish once the page URL contains this.
    #[serde(default)]
    pub until_url: Option<String>,
}

pub struct Handoff {
    pub id: String,
    pub session: String,
    pub headless: bool,
    pub until: Until,
    cdp: Cdp,
    chrome: Mutex<Option<Child>>,
    /// "waiting" | "done" | "cancelled" | "closed" (window closed before done)
    pub status: Mutex<String>,
}

/// What `done` hands the session.
pub struct Result_ {
    pub identity: Identity,
    /// Set-Cookie strings with the URL each belongs to.
    pub cookies: Vec<(String, url::Url)>,
}

fn chrome_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WEBSCOUT_CHROME") {
        return Some(p.into());
    }
    let fixed = ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                 "/Applications/Chromium.app/Contents/MacOS/Chromium",
                 r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                 r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe"];
    if let Some(p) = fixed.iter().map(PathBuf::from).find(|p| p.is_file()) {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"].iter()
        .flat_map(|b| std::env::split_paths(&path).map(move |d| d.join(b)))
        .find(|p| p.is_file())
}

/// Headless when there's no display to put a window on (servers, CI, containers).
pub fn default_headless() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none()
}

/// What Chrome reports as UA-CH platformVersion on this machine.
fn host_os_version() -> String {
    let run = |cmd: &str, args: &[&str]| std::process::Command::new(cmd).args(args).output().ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let v = if cfg!(target_os = "macos") { run("sw_vers", &["-productVersion"]) } else if cfg!(target_os = "linux") { run("uname", &["-r"]) } else { String::new() };
    // Chrome pads to three numeric parts ("15.6" -> "15.6.0").
    let mut parts: Vec<&str> = v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).take(3).collect();
    while !parts.is_empty() && parts.len() < 3 { parts.push("0"); }
    parts.join(".")
}

fn host_os() -> Platform {
    if cfg!(target_os = "macos") { Platform::MacOS } else if cfg!(target_os = "windows") { Platform::Windows } else { Platform::Linux }
}

impl Handoff {
    /// Launches Chrome on `url` with this session's own persistent profile (so earlier
    /// handoffs' logins survive) and its proxy, if any.
    pub async fn start(session: &str, url: &str, headless: bool, proxy: Option<&str>, until: Until) -> Result<Self> {
        let chrome = chrome_binary().ok_or_else(|| ScoutError::Unsupported(
            "handoff needs Chrome or Chromium; install it or set WEBSCOUT_CHROME".into()))?;
        let home = std::env::var("HOME").map_err(|_| err("HOME not set"))?;
        let profile = PathBuf::from(home).join(".webscout/chrome").join(session);
        std::fs::create_dir_all(&profile).map_err(err)?;
        let port_file = profile.join("DevToolsActivePort");
        let _ = std::fs::remove_file(&port_file);
        let mut cmd = Command::new(chrome);
        cmd.arg("--remote-debugging-port=0").arg(format!("--user-data-dir={}", profile.display()))
            .args(["--no-first-run", "--no-default-browser-check", "--window-size=1280,900", "about:blank"])
            .kill_on_drop(true).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if headless {
            cmd.arg("--headless=new");
        }
        if let Some(p) = proxy {
            // ponytail: Chrome takes no proxy credentials on the command line; authenticated
            // proxies need an auth extension or a local forwarder.
            cmd.arg(format!("--proxy-server={p}"));
        }
        if std::env::var("USER").as_deref() == Ok("root") {
            cmd.arg("--no-sandbox"); // Chrome refuses to run as root (containers) otherwise
        }
        let child = cmd.spawn().map_err(|e| err(format!("launching chrome: {e}")))?;

        // Chrome writes "<port>\n<browser ws path>" once DevTools is listening.
        let mut ws = None;
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&port_file) {
                if let Some((port, path)) = s.trim().split_once('\n') {
                    ws = Some(format!("ws://127.0.0.1:{port}{path}"));
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let ws = ws.ok_or_else(|| err("chrome didn't open its DevTools port (is another Chrome using this profile?)"))?;
        let cdp = Cdp::connect(&ws).await?;

        let targets = cdp.call("Target.getTargets", json!({}), false).await?;
        let target = targets["targetInfos"].as_array().into_iter().flatten()
            .find(|t| t["type"] == "page").and_then(|t| t["targetId"].as_str()).ok_or_else(|| err("no page in chrome"))?.to_string();
        let attached = cdp.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), false).await?;
        *cdp.page.lock().unwrap() = attached["sessionId"].as_str().map(str::to_string);

        if headless {
            // Headless Chrome says "HeadlessChrome" in its UA and brands: present as the
            // regular Chrome build it is, so the site (and later webscout) sees one browser.
            let version = cdp.call("Browser.getVersion", json!({}), false).await?;
            let ua = version["userAgent"].as_str().unwrap_or_default().replace("HeadlessChrome/", "Chrome/");
            let full = version["product"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default().to_string();
            let major: u32 = full.split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
            let brands: Vec<Value> = chromium_sec_ch_ua(major, "Google Chrome", major).split(", ").filter_map(|b| {
                let (name, v) = b.split_once(";v=")?;
                Some(json!({ "brand": name.trim_matches('"'), "version": v.trim_matches('"') }))
            }).collect();
            let full_list: Vec<Value> = brands.iter().map(|b| {
                let v = if b["brand"].as_str().unwrap_or_default().contains("Brand") { format!("{}.0.0.0", b["version"].as_str().unwrap_or_default()) } else { full.clone() };
                json!({ "brand": b["brand"], "version": v })
            }).collect();
            let platform = match host_os() { Platform::MacOS => "macOS", Platform::Windows => "Windows", _ => "Linux" };
            cdp.call("Emulation.setUserAgentOverride", json!({
                "userAgent": ua,
                "userAgentMetadata": { "brands": brands, "fullVersionList": full_list, "fullVersion": full, "platform": platform,
                    "platformVersion": host_os_version(), "architecture": if cfg!(target_arch = "aarch64") { "arm" } else { "x86" },
                    "model": "", "mobile": false, "bitness": "64", "wow64": false },
            }), true).await?;
            // Headless reports an 800x600, 1x screen; claim a common one for this OS instead.
            let (sw, sh, dpr) = if cfg!(target_os = "macos") { (1512, 982, 2) } else { (1920, 1080, 1) };
            cdp.call("Emulation.setDeviceMetricsOverride", json!({ "width": 1280, "height": 800, "deviceScaleFactor": dpr,
                "mobile": false, "screenWidth": sw, "screenHeight": sh }), true).await?;
        }
        cdp.call("Page.navigate", json!({ "url": url }), true).await?;
        Ok(Handoff {
            id: uuid::Uuid::new_v4().simple().to_string(),
            session: session.to_string(),
            headless,
            until,
            cdp,
            chrome: Mutex::new(Some(child)),
            status: Mutex::new("waiting".into()),
        })
    }

    pub fn status(&self) -> String {
        self.status.lock().unwrap().clone()
    }

    /// Current page URL and title, for status polls.
    pub async fn page(&self) -> (Value, Value) {
        match self.cdp.eval("[location.href, document.title]").await {
            Ok(v) => (v[0].clone(), v[1].clone()),
            Err(_) => (Value::Null, Value::Null),
        }
    }

    /// JPEG of the viewport (base64) and its CSS size, for the viewer.
    pub async fn frame(&self) -> Result<Value> {
        let shot = self.cdp.call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": 70 }), true).await?;
        Ok(json!({ "data": shot["data"], "status": self.status() }))
    }

    /// Viewer input. `x`/`y` are fractions (0..1) of the viewport, so the viewer can be any size.
    pub async fn input(&self, ev: &Value) -> Result<()> {
        let m = self.cdp.call("Page.getLayoutMetrics", json!({}), true).await?;
        let (w, h) = (m["cssLayoutViewport"]["clientWidth"].as_f64().unwrap_or(1280.0), m["cssLayoutViewport"]["clientHeight"].as_f64().unwrap_or(900.0));
        let (x, y) = (ev["x"].as_f64().unwrap_or(0.5) * w, ev["y"].as_f64().unwrap_or(0.5) * h);
        let mouse = |kind: &str, extra: Value| {
            let mut p = json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": 0, "clickCount": 1 });
            p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            p
        };
        match ev["type"].as_str().unwrap_or_default() {
            "click" => {
                self.cdp.call("Input.dispatchMouseEvent", mouse("mouseMoved", json!({ "button": "none" })), true).await?;
                self.cdp.call("Input.dispatchMouseEvent", mouse("mousePressed", json!({ "buttons": 1 })), true).await?;
                self.cdp.call("Input.dispatchMouseEvent", mouse("mouseReleased", json!({})), true).await?;
            }
            "move" => { self.cdp.call("Input.dispatchMouseEvent", mouse("mouseMoved", json!({ "button": "none" })), true).await?; }
            "scroll" => {
                let dy = ev["dy"].as_f64().unwrap_or(0.0);
                self.cdp.call("Input.dispatchMouseEvent", mouse("mouseWheel", json!({ "button": "none", "deltaX": 0, "deltaY": dy })), true).await?;
            }
            "text" => { self.cdp.call("Input.insertText", json!({ "text": ev["text"] }), true).await?; }
            "key" => {
                let key = ev["key"].as_str().unwrap_or_default();
                let code = match key { "Enter" => 13, "Backspace" => 8, "Tab" => 9, "Escape" => 27, "Delete" => 46,
                    "ArrowLeft" => 37, "ArrowUp" => 38, "ArrowRight" => 39, "ArrowDown" => 40, _ => 0 };
                let text = if key == "Enter" { "\r" } else { "" };
                for kind in ["rawKeyDown", "keyUp"] {
                    self.cdp.call("Input.dispatchKeyEvent", json!({ "type": kind, "key": key, "code": key,
                        "windowsVirtualKeyCode": code, "nativeVirtualKeyCode": code }), true).await?;
                    if kind == "rawKeyDown" && !text.is_empty() {
                        self.cdp.call("Input.dispatchKeyEvent", json!({ "type": "char", "text": text }), true).await?;
                    }
                }
            }
            "back" => { self.cdp.eval("history.back()").await?; }
            "goto" => { self.cdp.call("Page.navigate", json!({ "url": ev["url"] }), true).await?; }
            other => return Err(err(format!("unknown input type {other:?}"))),
        }
        Ok(())
    }

    /// Whether an `until` condition holds yet.
    pub async fn ready(&self) -> bool {
        if let Some(name) = &self.until.until_cookie {
            if let Ok(c) = self.cdp.call("Storage.getCookies", json!({}), false).await {
                if c["cookies"].as_array().into_iter().flatten().any(|c| c["name"] == name.as_str()) {
                    return true;
                }
            }
        }
        if let Some(part) = &self.until.until_url {
            if self.page().await.0.as_str().is_some_and(|u| u.contains(part.as_str())) {
                return true;
            }
        }
        false
    }

    /// Takes Chrome's cookies and fingerprint, then closes it. `prev` keeps the session's
    /// proxy / forwarded_for.
    pub async fn finish(&self, prev: &Identity) -> Result<Result_> {
        let fp = self.cdp.eval(FINGERPRINT).await?;
        let fp: RealChrome = serde_json::from_str(fp.as_str().ok_or_else(|| err("no fingerprint"))?).map_err(err)?;
        let raw = self.cdp.call("Storage.getCookies", json!({}), false).await?;
        let cookies = raw["cookies"].as_array().into_iter().flatten().filter_map(cookie_line).collect();
        let identity = fp.into_identity(prev);
        self.close("done").await;
        Ok(Result_ { identity, cookies })
    }

    pub async fn close(&self, status: &str) {
        *self.status.lock().unwrap() = status.into();
        let _ = self.cdp.call("Browser.close", json!({}), false).await;
        let child = self.chrome.lock().unwrap().take();
        if let Some(mut c) = child {
            if tokio::time::timeout(Duration::from_secs(5), c.wait()).await.is_err() {
                let _ = c.kill().await;
            }
        }
    }

    /// Chrome died or its window was closed before `done`. The cookies stay in this
    /// session's Chrome profile, so starting another handoff picks them back up.
    pub async fn alive(&self) -> bool {
        self.cdp.call("Browser.getVersion", json!({}), false).await.is_ok()
    }
}

/// Everything the site saw: the session must keep claiming exactly this.
const FINGERPRINT: &str = r#"(async () => {
  const c = document.createElement('canvas').getContext('webgl'), d = c && c.getExtension('WEBGL_debug_renderer_info');
  const uad = navigator.userAgentData;
  const h = uad ? await uad.getHighEntropyValues(['platformVersion', 'architecture']) : {};
  return JSON.stringify({ ua: navigator.userAgent, brands: uad ? uad.brands : [], langs: navigator.languages,
    screen: [screen.width, screen.height], dpr: devicePixelRatio, cores: navigator.hardwareConcurrency,
    mem: navigator.deviceMemory || 8, gpu: d ? [c.getParameter(d.UNMASKED_VENDOR_WEBGL), c.getParameter(d.UNMASKED_RENDERER_WEBGL)] : ['', ''],
    platformVersion: h.platformVersion || '', arch: h.architecture || '' });
})()"#;

#[derive(Deserialize)]
struct RealChrome {
    ua: String,
    brands: Vec<Value>,
    langs: Vec<String>,
    screen: [u32; 2],
    dpr: f32,
    cores: u32,
    mem: f64,
    gpu: [String; 2],
    #[serde(rename = "platformVersion")]
    platform_version: String,
    arch: String,
}

impl RealChrome {
    fn into_identity(self, prev: &Identity) -> Identity {
        let ua = self.ua.replace("HeadlessChrome/", "Chrome/");
        let major = ua.split("Chrome/").nth(1).and_then(|v| v.split('.').next()).and_then(|v| v.parse().ok()).unwrap_or(0);
        let sec_ch_ua = if self.brands.is_empty() {
            chromium_sec_ch_ua(major, "Google Chrome", major) // insecure page: no userAgentData
        } else {
            self.brands.iter().map(|b| format!("\"{}\";v=\"{}\"", b["brand"].as_str().unwrap_or_default(), b["version"].as_str().unwrap_or_default()))
                .collect::<Vec<_>>().join(", ")
        };
        // Chrome's Accept-Language: languages in order, q dropping by 0.1 after the first.
        let accept_language = self.langs.iter().enumerate()
            .map(|(i, l)| if i == 0 { l.clone() } else { format!("{l};q={:.1}", (1.0 - 0.1 * i as f64).max(0.1)) })
            .collect::<Vec<_>>().join(",");
        Identity {
            browser: "chrome".into(),
            profile: Profile::Chrome149,
            device: Device {
                name: "handoff-chrome".into(), kind: Kind::Desktop, vendor: "this machine".into(), model: String::new(),
                os: host_os(), os_version: if self.platform_version.is_empty() { host_os_version() } else { self.platform_version }, browsers: vec!["chrome".into()], screen: self.screen,
                dpr: self.dpr, cpu_cores: self.cores, ram_gb: self.mem as u32, touch: 0,
                arch: if self.arch.is_empty() { if cfg!(target_arch = "aarch64") { "arm".into() } else { "x86".into() } } else { self.arch },
                gpu: Gpu { vendor: self.gpu[0].clone(), renderer: self.gpu[1].clone() },
            },
            user_agent: ua,
            sec_ch_ua: Some(sec_ch_ua),
            accept_language: if accept_language.is_empty() { prev.accept_language.clone() } else { accept_language },
            proxy: prev.proxy.clone(),
            forwarded_for: prev.forwarded_for.clone(),
            pinned: true,
        }
    }
}

/// A CDP cookie as a Set-Cookie line plus the URL to store it under.
fn cookie_line(c: &Value) -> Option<(String, url::Url)> {
    let (name, value, domain) = (c["name"].as_str()?, c["value"].as_str()?, c["domain"].as_str()?);
    let path = c["path"].as_str().unwrap_or("/");
    let secure = c["secure"].as_bool().unwrap_or(false);
    let mut line = format!("{name}={value}; Path={path}");
    if domain.starts_with('.') {
        line += &format!("; Domain={domain}"); // host-only cookies have no leading dot
    }
    if let Some(exp) = c["expires"].as_f64().filter(|e| *e > 0.0) {
        let left = exp as i64 - std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
        line += &format!("; Max-Age={}", left.max(0));
    }
    if secure { line += "; Secure"; }
    if c["httpOnly"].as_bool().unwrap_or(false) { line += "; HttpOnly"; }
    let url = url::Url::parse(&format!("{}://{}{}", if secure { "https" } else { "http" }, domain.trim_start_matches('.'), path)).ok()?;
    Some((line, url))
}

/// The page a human opens to drive a headless handoff from anywhere.
pub const VIEWER: &str = r#"<!doctype html><html><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1"><title>webscout handoff</title>
<style>
  body{margin:0;font:14px system-ui,sans-serif;background:#111;color:#eee}
  header{display:flex;gap:8px;align-items:center;padding:8px 12px;flex-wrap:wrap}
  header span{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;opacity:.8}
  button{font:inherit;padding:6px 12px;border-radius:6px;border:1px solid #555;background:#222;color:#eee;cursor:pointer}
  button.go{background:#1a7f37;border-color:#1a7f37}
  #f{display:block;max-width:100%;margin:0 auto;cursor:crosshair;outline:none}
  input{font:inherit;padding:6px;border-radius:6px;border:1px solid #555;background:#000;color:#eee;width:200px}
</style></head><body>
<header><b>Handoff</b><span id=s>connecting…</span>
<input id=t placeholder="type here, Enter sends"><button id=back>Back</button>
<button class=go id=done>I'm through, done</button></header>
<img id=f tabindex=0 alt="remote browser">
<script>
const base = location.pathname.replace(/\/view$/, ''), f = document.getElementById('f'), s = document.getElementById('s');
const send = ev => fetch(base + '/input', {method: 'POST', headers: {'content-type': 'application/json'}, body: JSON.stringify(ev)});
const pos = e => { const r = f.getBoundingClientRect(); return {x: (e.clientX - r.left) / r.width, y: (e.clientY - r.top) / r.height}; };
f.onclick = e => { f.focus(); send({type: 'click', ...pos(e)}); };
f.onwheel = e => { e.preventDefault(); send({type: 'scroll', dy: e.deltaY, ...pos(e)}); };
f.onkeydown = e => { e.preventDefault(); send(e.key.length === 1 ? {type: 'text', text: e.key} : {type: 'key', key: e.key}); };
document.getElementById('t').onkeydown = e => { if (e.key === 'Enter') { send({type: 'text', text: e.target.value}); e.target.value = ''; } };
document.getElementById('back').onclick = () => send({type: 'back'});
document.getElementById('done').onclick = async () => { s.textContent = 'finishing…'; const r = await fetch(base + '/done', {method: 'POST'}); s.textContent = r.ok ? 'done: the session has the cookies. You can close this tab.' : await r.text(); };
(async function loop() {
  try {
    const r = await (await fetch(base + '/frame')).json();
    if (r.data) f.src = 'data:image/jpeg;base64,' + r.data;
    const st = await (await fetch(base)).json();
    s.textContent = st.status === 'waiting' ? (st.title || st.url || '') : st.status;
    if (st.status !== 'waiting') return;
  } catch (e) { s.textContent = 'reconnecting…'; }
  setTimeout(loop, 400);
})();
</script></body></html>"#;
