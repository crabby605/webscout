pub mod dom;
pub mod error;
pub mod identity;
pub mod js;
pub mod session;
pub mod http;
pub mod serve;

use clap::{Parser, Subcommand};
use tracing::{info, error};
use tracing_subscriber;

use crate::http::client::HttpClient;
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

/// Runs page JS on a blocking thread (QuickJS is synchronous).
pub async fn render_js(html: String, url: String, session: &Session, client: &HttpClient, timeout_ms: u64) -> error::Result<js::Rendered> {
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
    }))
    .await
    .map_err(|e| error::ScoutError::Parse(format!("js thread: {e}")))?
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
            let s = Session::load(&name)?;
            println!("session:  {name}{}", if exists { "" } else { " (not saved yet)" });
            println!("agent:    {}", s.identity.user_agent);
            println!("platform: {}", s.identity.platform);
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
    let session = Session::new();
    info!("Assigned Identity: {} ({})", session.identity.user_agent, session.identity.platform);

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
    if response.is_html() {
        let mut html = response.text();
        info!("Received HTML content. Length: {} bytes", html.len());
        if args.js {
            let r = render_js(html, response.final_url.clone(), &session, &client, args.js_timeout).await?;
            r.console.iter().for_each(|l| info!("[js] {l}"));
            info!("JS done (timed out: {}). Rendered {} bytes", r.timed_out, r.html.len());
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
    } else if let Some(ct) = response.content_type() {
        info!("Received content of type: {}. Length: {} bytes", ct, response.body.len());
    }

    Ok(())
}
