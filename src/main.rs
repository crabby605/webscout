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
    tracing_subscriber::fmt::init();

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
        info!("Received HTML content. Length: {} bytes", response.text().len());
    } else if let Some(ct) = response.content_type() {
        info!("Received content of type: {}. Length: {} bytes", ct, response.body.len());
    }

    Ok(())
}
