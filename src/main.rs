pub mod dom;
pub mod error;
pub mod identity;
pub mod session;
pub mod http;

use clap::Parser;
use tracing::{info, error};
use tracing_subscriber;

use crate::http::client::HttpClient;
use crate::session::Session;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Target URL to fetch
    #[arg(short, long)]
    url: String,

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

    /// Print absolute links found on the page
    #[arg(long)]
    links: bool,
}

fn parse_delay(val: &str) -> Option<std::ops::Range<u64>> {
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

    info!("Fetching {}...", args.url);

    // Execute the request
    let response = client.get(&args.url, delay_range).await?;

    info!("Response received. Status: {}, Final URL: {}", response.status, response.final_url);

    // Classify the response page state implicitly
    let state = response.classify();
    info!("Page Classification State: {:?}", state);

    // Output stats rather than raw dumps
    if response.is_html() {
        let html = response.text();
        info!("Received HTML content. Length: {} bytes", html.len());

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
            dom.links().iter().for_each(|l| println!("{l}"));
        }
    } else if let Some(ct) = response.content_type() {
        info!("Received content of type: {}. Length: {} bytes", ct, response.body.len());
    }

    Ok(())
}
