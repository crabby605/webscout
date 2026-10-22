//! Runs a page's JavaScript against a full DOM (linkedom in QuickJS) and returns the resulting HTML.
//!
//! Blocking: call from `tokio::task::spawn_blocking`. Page fetch/XHR/script loads go
//! through the session's HttpClient (same identity + cookie jar) via `Handle::block_on`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use cookie_store::CookieStore;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::context::EvalOptions;
use rquickjs::{CatchResultExt, Context, Ctx, Exception, Function, Module, Runtime};
use serde::Serialize;
use serde_json::json;
use tokio::runtime::Handle;
use url::Url;

use crate::error::{Result, ScoutError};
use crate::http::{HttpClient, HttpResponse};
use crate::identity::Identity;

const DOM_BUNDLE: &str = include_str!("../vendor/dom-bundle.js");
const PRELUDE: &str = include_str!("js_prelude.js");
const WEB_APIS: &str = include_str!("js_webapis.js");
const MEMORY_LIMIT: usize = 256 * 1024 * 1024;
const MAX_LOGS: usize = 200;

#[derive(Debug, Serialize)]
pub struct Rendered {
    pub html: String,
    /// console output, script errors and attempted navigations, as "level: message".
    pub console: Vec<String>,
    /// True if the wall-clock budget ran out before the page went idle.
    pub timed_out: bool,
}

pub struct Page<'a> {
    pub html: &'a str,
    pub url: &'a str,
    pub identity: &'a Identity,
    pub client: HttpClient,
    pub cookies: Arc<RwLock<CookieStore>>,
    pub timeout: Duration,
}

/// Blocking HTTP for page JS, bounded by the render deadline.
#[derive(Clone)]
struct Net {
    client: HttpClient,
    handle: Handle,
    deadline: Instant,
}

impl Net {
    fn request(&self, method: &str, url: &str, headers: &[(String, String)], body: Option<String>) -> std::result::Result<HttpResponse, String> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        self.handle.block_on(async {
            tokio::time::timeout(remaining, self.client.request(method, url, headers, body)).await
        })
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())
    }
}

/// ES module imports resolve like a browser: relative or absolute URLs only (no bare specifiers).
struct UrlResolver;

impl Resolver for UrlResolver {
    fn resolve<'js>(&mut self, _ctx: &Ctx<'js>, base: &str, name: &str, _attrs: Option<ImportAttributes<'js>>) -> rquickjs::Result<String> {
        let relative = ["/", "./", "../"].iter().any(|p| name.starts_with(p));
        // ponytail: no import maps, so bare specifiers ("react") fail like in a browser without one.
        let url = if relative { Url::parse(base).and_then(|b| b.join(name)) } else { Url::parse(name) };
        url.map(|u| u.to_string()).map_err(|_| rquickjs::Error::new_resolving(base, name))
    }
}

struct NetLoader(Net);

impl Loader for NetLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str, attrs: Option<ImportAttributes<'js>>) -> rquickjs::Result<Module<'js, Declared>> {
        let headers = [("sec-fetch-dest".to_string(), "script".to_string()), ("sec-fetch-mode".to_string(), "cors".to_string())];
        let r = self.0.request("GET", name, &headers, None)
            .map_err(|e| rquickjs::Error::new_loading_message(name, e))?;
        if r.status >= 400 {
            return Err(rquickjs::Error::new_loading_message(name, format!("HTTP {}", r.status)));
        }
        let is_json = attrs.and_then(|a| a.get_type().ok().flatten()).as_deref() == Some("json");
        let source = if is_json { format!("export default {};", r.text()) } else { r.text() };
        let module = Module::declare(ctx.clone(), name, source)?;
        module.meta()?.set("url", name)?;
        Ok(module)
    }
}

fn js_err(e: impl std::fmt::Display) -> ScoutError {
    ScoutError::Parse(format!("js: {e}"))
}

pub fn render(page: Page) -> Result<Rendered> {
    let deadline = Instant::now() + page.timeout;
    let rt = Runtime::new().map_err(js_err)?;
    rt.set_memory_limit(MEMORY_LIMIT);
    let net = Net { client: page.client.clone(), handle: Handle::current(), deadline };
    rt.set_loader(UrlResolver, NetLoader(net.clone()));
    // Kill runaway scripts (infinite loops) once the budget is spent.
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let ctx = Context::full(&rt).map_err(js_err)?;
    let logs: Rc<RefCell<Vec<String>>> = Default::default();

    ctx.with(|ctx| -> Result<()> {
        install_host_fns(&ctx, &page, net.clone(), Rc::clone(&logs)).map_err(js_err)?;
        let languages: Vec<&str> = page.identity.accept_language.split(',')
            .map(|l| l.split(';').next().unwrap_or("").trim())
            .filter(|l| !l.is_empty())
            .collect();
        // linkedom's parser isn't spec-compliant (drops content outside <html>, mangles
        // malformed markup), so hand it html5ever's normalized document instead.
        let html = scraper::Html::parse_document(page.html).html();
        let config = json!({
            "html": html,
            "url": page.url,
            "userAgent": page.identity.user_agent,
            "secChUa": page.identity.sec_ch_ua,
            "platform": page.identity.platform,
            "languages": languages,
            "viewport": page.identity.viewport,
            "maxVirtualMs": page.timeout.as_millis() as u64,
        });
        let config = ctx.json_parse(config.to_string()).map_err(js_err)?;
        ctx.globals().set("__ws_config", config).map_err(js_err)?;
        ctx.eval::<(), _>(DOM_BUNDLE).catch(&ctx).map_err(js_err)?;
        ctx.eval::<(), _>(PRELUDE).catch(&ctx).map_err(js_err)?;
        ctx.eval::<(), _>(WEB_APIS).catch(&ctx).map_err(js_err)?;
        Ok(())
    })?;

    // Event loop: drain promise jobs, then run the next timer task, until idle or out of time.
    let mut timed_out = false;
    loop {
        while rt.is_job_pending() {
            if let Err(e) = rt.execute_pending_job() {
                let msg = e.0.with(|ctx| format!("{:?}", ctx.catch()));
                push_log(&logs, "error", &format!("unhandled job: {msg}"));
            }
        }
        if Instant::now() > deadline {
            timed_out = true;
            break;
        }
        let more = ctx.with(|ctx| ctx.eval::<bool, _>("__ws_tick()").catch(&ctx).map_err(|e| e.to_string()));
        match more {
            Ok(true) => continue,
            Ok(false) => break,
            Err(e) => {
                // Interrupted by the deadline, or the tick itself threw.
                push_log(&logs, "error", &e);
                timed_out = Instant::now() > deadline;
                break;
            }
        }
    }

    rt.set_interrupt_handler(None);
    let html = ctx.with(|ctx| ctx.eval::<String, _>("__ws_serialize()").catch(&ctx).map_err(js_err))?;
    let console = logs.borrow().clone();
    Ok(Rendered { html, console, timed_out })
}

fn push_log(logs: &RefCell<Vec<String>>, level: &str, msg: &str) {
    let mut logs = logs.borrow_mut();
    if logs.len() < MAX_LOGS {
        logs.push(format!("{level}: {msg}"));
    }
}

fn install_host_fns<'js>(
    ctx: &Ctx<'js>,
    page: &Page,
    net: Net,
    logs: Rc<RefCell<Vec<String>>>,
) -> rquickjs::Result<()> {
    let g = ctx.globals();

    // Classic scripts share one global lexical scope (a top-level `const` in one
    // script is visible to the next). Indirect eval would scope each one separately.
    g.set("__ws_eval", Function::new(ctx.clone(), |ctx: Ctx<'js>, code: String, name: String| {
        let mut opts = EvalOptions::default();
        opts.global = true;
        opts.strict = false;
        opts.filename = Some(name);
        ctx.eval_with_options::<(), _>(code, opts)
    })?)?;

    g.set("__ws_http", Function::new(ctx.clone(), move |ctx: Ctx<'js>, method: String, url: String, headers: String, body: Option<String>| {
        let headers: Vec<(String, String)> = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&headers)
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())))
            .collect();
        match net.request(&method, &url, &headers, body) {
            Ok(r) => {
                let hdrs: Vec<(&str, &str)> = r.headers.iter()
                    .filter_map(|(k, v)| Some((k.as_str(), v.to_str().ok()?)))
                    .collect();
                Ok(json!({ "status": r.status, "url": r.final_url, "headers": hdrs, "body": r.text() }).to_string())
            }
            Err(e) => Err(Exception::throw_message(&ctx, &e)),
        }
    })?)?;

    let jar = Arc::clone(&page.cookies);
    g.set("__ws_cookie_get", Function::new(ctx.clone(), move |url: String| {
        let Ok(url) = Url::parse(&url) else { return String::new() };
        jar.read().unwrap().matches(&url).into_iter()
            .filter(|c| !c.http_only().unwrap_or(false))
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ")
    })?)?;

    let jar = Arc::clone(&page.cookies);
    g.set("__ws_cookie_set", Function::new(ctx.clone(), move |url: String, cookie: String| {
        let Ok(url) = Url::parse(&url) else { return };
        // Scripts can't set HttpOnly cookies.
        if let Ok(c) = cookie_store::Cookie::parse(cookie, &url) {
            if !c.http_only().unwrap_or(false) {
                let _ = jar.write().unwrap().insert_raw(&c, &url);
            }
        }
    })?)?;

    // Runs a <script type=module>: by URL (src) or inline source named after the page.
    g.set("__ws_module", Function::new(ctx.clone(), |ctx: Ctx<'js>, name: String, source: Option<String>| {
        match source {
            Some(src) => {
                // import.meta.url of an inline module is the page URL.
                let page_url = name.split('#').next().unwrap_or_default().to_string();
                let module = Module::declare(ctx, name, src)?;
                module.meta()?.set("url", page_url)?;
                Ok(module.eval()?.1)
            }
            None => Module::import(&ctx, name),
        }
    })?)?;

    // Secure randomness for crypto.getRandomValues / randomUUID (capped like browsers: 64 KiB).
    g.set("__ws_random", Function::new(ctx.clone(), |n: usize| -> Vec<u8> {
        let mut buf = vec![0u8; n.min(65536)];
        rand::Rng::fill(&mut rand::thread_rng(), &mut buf[..]);
        buf
    })?)?;

    g.set("__ws_log", Function::new(ctx.clone(), move |level: String, msg: String| {
        push_log(&logs, &level, &msg);
    })?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    async fn run(html: &'static str, timeout_ms: u64) -> Rendered {
        let session = Session::new();
        let client = HttpClient::build(&session).unwrap();
        let identity = session.identity.clone();
        let cookies = Arc::clone(&session.cookies);
        tokio::task::spawn_blocking(move || render(Page {
            html, url: "https://ex.com/a/b?q=1", identity: &identity, client, cookies,
            timeout: Duration::from_millis(timeout_ms),
        })).await.unwrap().unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn passes_bot_checks() {
        // The probes bot.sannysoft.com / fingerprinters run, plus the DOM gaps that crashed them.
        let r = run(r#"<html><body><pre id=out></pre><script>
            const gl = document.createElement('canvas').getContext('webgl');
            const dbg = gl.getExtension('WEBGL_debug_renderer_info');
            const out = [
              Object.getOwnPropertyNames(navigator).length,
              navigator.webdriver,
              Object.prototype.toString.call(navigator),
              Object.getOwnPropertyDescriptor(Navigator.prototype, 'webdriver').get.toString(),
              navigator.plugins instanceof PluginArray && navigator.plugins[0].toString(),
              navigator.plugins.length,
              navigator.mimeTypes[0].enabledPlugin.name,
              !!gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL),
              (document.body.firstChild.innerHTML = 0, document.body.firstChild.innerHTML),
              document.implementation.createHTMLDocument('').body.nodeName,
              /Chrome/.test(navigator.userAgent) === !!window.chrome,
            ];
            navigator.permissions.query({name: 'notifications'}).then(s => {
              out.push(s.state, Notification.permission);
              document.getElementById('out').textContent = JSON.stringify(out);
            });
            </script></body></html>"#, 5000).await;
        let d = crate::dom::Dom::parse(&r.html, "https://ex.com/");
        let text = d.select_text("#out").unwrap().join("");
        let out: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("no output; console: {:?}", r.console));
        assert_eq!(out, json!([0, false, "[object Navigator]", "function get webdriver() { [native code] }",
            "[object Plugin]", 5, "PDF Viewer", true, "0", "BODY", true, "prompt", "default"]), "{:?}", r.console);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn runs_page_scripts() {
        let r = run(r#"<html><body><div id=app></div>
            <script>
              var order = [];
              document.getElementById('app').innerHTML = '<ul><li class=i>one</li></ul>';
              document.querySelector('ul').appendChild(Object.assign(document.createElement('li'), {className: 'i', textContent: 'two'}));
              setTimeout(() => document.body.setAttribute('data-timer', 'yes'), 3000);
              Promise.resolve().then(() => order.push('micro'));
              document.addEventListener('DOMContentLoaded', () => order.push('dcl'));
              window.addEventListener('load', () => document.body.setAttribute('data-order', order.join(',')));
              document.cookie = 'k=v; path=/';
              document.body.setAttribute('data-cookie', document.cookie);
              document.body.setAttribute('data-env', [location.pathname, new URL('x', location.href).href, typeof fetch, navigator.webdriver, btoa('hi')].join('|'));
              console.log('hello', {a: 1});
            </script>
            <script>document.write('<p id=w>written</p>')</script>
            <script>const shared = 'yes';</script>
            <script>document.body.setAttribute('data-shared', shared)</script>
            <script>throw new Error('boom')</script>
            <script type="application/json">{"not": "run"}</script>
            </body></html>"#, 5000).await;
        let d = crate::dom::Dom::parse(&r.html, "https://ex.com/");
        assert_eq!(d.select_text("li.i").unwrap(), ["one", "two"]);
        assert_eq!(d.select_text("#w").unwrap(), ["written"]);
        let attr = |a: &str| d.select_html(&format!("body[{a}]")).unwrap().len();
        assert_eq!(attr("data-timer=yes"), 1, "virtual-clock timer ran without waiting");
        assert_eq!(attr("data-order='micro,dcl'"), 1, "{:?}", d.select_html("body").unwrap()[0].get(..200));
        assert_eq!(attr("data-cookie='k=v'"), 1);
        assert_eq!(attr("data-shared=yes"), 1, "top-level const visible to later scripts");
        assert_eq!(attr("data-env='/a/b|https://ex.com/a/x|function|false|aGk='"), 1);
        assert!(r.console.iter().any(|l| l == r#"log: hello {"a":1}"#), "{:?}", r.console);
        assert!(r.console.iter().any(|l| l.contains("boom")), "{:?}", r.console);
        assert!(!r.timed_out);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn web_apis() {
        let r = run(r#"<html><body><canvas></canvas><script>
            const out = [];
            out.push(new Intl.NumberFormat('en-US', {style: 'currency', currency: 'USD'}).format(1234.5));
            out.push((1234567.891).toLocaleString());
            out.push(new Intl.DateTimeFormat('en-US', {timeZone: 'UTC'}).format(new Date(Date.UTC(2026, 0, 2))));
            out.push(new Intl.RelativeTimeFormat('en', {numeric: 'auto'}).format(-1, 'day'));
            out.push(new Intl.ListFormat('en').format(['a', 'b', 'c']));
            out.push(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID()));
            out.push(String(document.querySelector).includes('[native code]'));
            out.push(new Image(3, 4).outerHTML);
            out.push(typeof document.querySelector('canvas').getContext('2d').fillRect);
            const ac = new AbortController(); ac.abort();
            fetch('https://ex.invalid/', {signal: ac.signal}).catch(e => out.push(e.name));
            const ch = new MessageChannel();
            ch.port2.onmessage = e => out.push('msg:' + e.data.x);
            ch.port1.postMessage({x: 1});
            new Blob(['ab', 'c']).text().then(t => out.push(t));
            const fd = new FormData(); fd.append('k', 'v'); out.push(fd.get('k'));
            new WebSocket('wss://ex.invalid/').onclose = () => out.push('ws closed');
            setTimeout(() => { document.getElementById('out').textContent = JSON.stringify(out); }, 10);
            </script><pre id=out></pre></body></html>"#, 5000).await;
        let d = crate::dom::Dom::parse(&r.html, "https://ex.com/");
        let text = d.select_text("#out").unwrap().pop().unwrap_or_default();
        let out: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("no output; console: {:?}", r.console));
        assert_eq!(out, json!(["$1,234.50", "1,234,567.891", "1/2/2026", "yesterday", "a, b, and c", true, true,
            // Blob's promise (microtask) settles before the MessageChannel message (task), as in browsers.
            "<img height=\"4\" width=\"3\">", "function", "v", "AbortError", "abc", "msg:1", "ws closed"]), "{:?}", r.console);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn infinite_loop_times_out() {
        let start = Instant::now();
        let r = run("<script>while (true) {}</script><p>still here</p>", 300).await;
        assert!(r.timed_out && start.elapsed() < Duration::from_secs(3));
        assert!(r.html.contains("still here"), "{} {:?}", r.html, r.console);
    }
}
