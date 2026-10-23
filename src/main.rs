pub mod dom;
pub mod error;
pub mod handoff;
pub mod identity;
pub mod js;
pub mod session;
pub mod http;
pub mod serve;

use clap::{Parser, Subcommand};
use tracing::{info, error};
use tracing_subscriber;

use crate::http::client::HttpClient;
use crate::http::headers::Dest;
use crate::http::HttpResponse;
use crate::session::Session;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None, args_conflicts_with_subcommands = true)]
struct Args {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Target URL to fetch
    #[arg(short, long, required = true)]
    url: Option<String>,

    /// Delay in ms before fetch (e.g., "100-500")
    #[arg(short, long)]
    delay: Option<String>,

    /// Print the visible text of elements matching this CSS selector
    #[arg(short, long)]
    select: Option<String>,

    /// With --select, print outer HTML instead of text
    #[arg(long, requires = "select")]
    html: bool,

    /// Print the page's visible text
    #[arg(long)]
    text: bool,

    /// Run the page's JavaScript against a full DOM before extracting
    #[arg(long)]
    js: bool,

    /// JS time budget in ms
    #[arg(long, default_value_t = 5000)]
    js_timeout: u64,

    /// Print absolute links found on the page
    #[arg(long)]
    links: bool,

    /// JSON array of interactions to run (implies --js), e.g.
    /// '[{"type":"#q","text":"rust"},{"press":"Enter"}]'
    #[arg(long)]
    actions: Option<String>,

    /// Identity spec for this run, e.g. "phone samsung", "windows edge", "ipad"
    #[arg(long)]
    identity: Option<String>,

    /// Send requests through this proxy (http://, https:// or socks5://user:pass@host:port)
    #[arg(long)]
    proxy: Option<String>,

    /// Add X-Forwarded-For / X-Real-IP with this IP ("random" for a random public one).
    /// Only naive servers trust it; bot walls treat it as a bot signal.
    #[arg(long)]
    forwarded_for: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run the HTTP/JSON daemon used by the MCP server
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// 0 = let the OS pick a free port
        #[arg(long, default_value_t = 0)]
        port: u16,
    },
    /// Inspect or delete a saved session
    Session {
        #[command(subcommand)]
        action: SessionCmd,
    },
}

#[derive(Subcommand, Debug)]
enum SessionCmd {
    Show { name: String },
    Delete { name: String },
}

/// Runs page JS (and `actions`) on a blocking thread (QuickJS is synchronous).
pub async fn render_js(html: String, url: String, session: &Session, client: &HttpClient, timeout_ms: u64,
                       actions: Vec<serde_json::Value>) -> error::Result<js::Rendered> {
    let identity = session.identity.clone();
    let cookies = std::sync::Arc::clone(&session.cookies);
    let client = client.clone();
    tokio::task::spawn_blocking(move || js::render(js::Page {
        html: &html,
        url: &url,
        identity: &identity,
        client,
        cookies,
        timeout: std::time::Duration::from_millis(timeout_ms),
        actions: &actions,
    }))
    .await
    .map_err(|e| error::ScoutError::Parse(format!("js thread: {e}")))?
}

/// Most documents one browse may load: JS redirects, link clicks and form submits.
const MAX_NAVIGATIONS: usize = 10;

/// Runs `resp`'s page JS and then `actions`, following the navigations they cause (links,
/// form submits, location changes) like a browser tab, carrying the remaining actions to
/// each new document. Returns the last response and its rendered page.
pub async fn browse(mut resp: HttpResponse, session: &Session, client: &HttpClient, timeout_ms: u64,
                    actions: &[serde_json::Value]) -> error::Result<(HttpResponse, js::Rendered)> {
    let (mut done, mut console, mut results) = (0, Vec::new(), Vec::new());
    for hop in 0.. {
        if !resp.is_html() {
            let html = resp.text();
            return Ok((resp, js::Rendered { html, console, timed_out: false, results, nav: None }));
        }
        let r = render_js(resp.text(), resp.final_url.clone(), session, client, timeout_ms, actions[done..].to_vec()).await?;
        done += r.results.len();
        console.extend(r.console);
        results.extend(r.results);
        match r.nav {
            Some(nav) if hop < MAX_NAVIGATIONS => {
                let extra: Vec<(String, String)> = nav.content_type.into_iter().map(|t| ("content-type".into(), t)).collect();
                resp = client.request(&nav.method, &nav.url, Dest::Document, Some(&resp.final_url), &extra, nav.body).await?;
            }
            _ => return Ok((resp, js::Rendered { html: r.html, console, timed_out: r.timed_out, results, nav: None })),
        }
    }
    unreachable!()
}

pub fn parse_delay(val: &str) -> Option<std::ops::Range<u64>> {
    let parts: Vec<&str> = val.split('-').collect();
    if parts.len() == 2 {
        if let (Ok(start), Ok(end)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>()) {
            if start < end {
                return Some(start..end);
            }
        }
    }
    None
}

#[tokio::main]
async fn main() -> Result<(), error::ScoutError> {
    // Initialize production-grade logging
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();

    let args = Args::parse();
    match args.cmd {
        Some(Cmd::Serve { host, port }) => return serve::run(&host, port).await,
        Some(Cmd::Session { action: SessionCmd::Show { name } }) => {
            let exists = Session::exists(&name);
            let s = Session::load(&name, &session::NewSession::default())?;
            println!("session:  {name}{}", if exists { "" } else { " (not saved yet)" });
            println!("agent:    {}", s.identity.user_agent);
            println!("identity: {}", s.identity.summary());
            println!("cookies:  {}", s.cookies.read().unwrap().iter_unexpired().count());
            return Ok(());
        }
        Some(Cmd::Session { action: SessionCmd::Delete { name } }) => {
            let existed = Session::delete(&name)?;
            println!("{}", if existed { format!("deleted {name}") } else { format!("{name} did not exist") });
            return Ok(());
        }
        None => {}
    }
    let url = args.url.expect("clap enforces --url");
    let delay_range = args.delay.and_then(|d| parse_delay(&d));

    info!("Initializing WebScout Engine");

    // Create a persistent session (generates a coherent browser profile and cookie jar)
    let opts = session::NewSession { identity: args.identity.clone(), proxy: args.proxy.clone(), forwarded_for: args.forwarded_for.clone() };
    let session = Session { identity: opts.identity()?, ..Session::new() };
    let actions: Vec<serde_json::Value> = match &args.actions {
        Some(a) => serde_json::from_str(a).map_err(|e| error::ScoutError::Parse(format!("--actions: {e}")))?,
        None => Vec::new(),
    };
    info!("Assigned Identity: {} ({})", session.identity.user_agent, session.identity.summary());

    // Build the HTTP client bound strictly to the generated session profile
    let client = match HttpClient::build(&session) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to build HTTP client: {:?}", e);
            return Err(e);
        }
    };

    info!("Fetching {}...", url);

    // Execute the request
    let response = client.get(&url, delay_range).await?;

    info!("Response received. Status: {}, Final URL: {}", response.status, response.final_url);

    // Classify the response page state implicitly
    let state = response.classify();
    info!("Page Classification State: {:?}", state);

    // Output stats rather than raw dumps
    let mut response = response;
    if response.is_html() {
        let mut html = response.text();
        info!("Received HTML content. Length: {} bytes", html.len());
        if args.js || !actions.is_empty() {
            let (last, r) = browse(response, &session, &client, args.js_timeout, &actions).await?;
            response = last;
            r.console.iter().for_each(|l| info!("[js] {l}"));
            r.results.iter().enumerate().for_each(|(i, r)| info!("[action {i}] {r}"));
            info!("JS done (timed out: {}). Ended on {} ({})", r.timed_out, response.final_url, response.status);
            html = r.html;
        }

        let dom = dom::Dom::parse(&html, &response.final_url);
        info!("Title: {}", dom.title().unwrap_or_default());
        if let Some(css) = &args.select {
            let found = if args.html { dom.select_html(css)? } else { dom.select_text(css)? };
            found.iter().for_each(|s| println!("{s}"));
        }
        if args.text {
            println!("{}", dom.text());
        }
        if args.links {
            dom.links().iter().for_each(|l| println!("{}", l.href));
        }
    } else if args.text {
        println!("{}", response.text());
    } else if let Some(ct) = response.content_type() {
        info!("Received content of type: {}. Length: {} bytes", ct, response.body.len());
    }

    Ok(())
}
