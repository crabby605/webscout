use thiserror::Error;

#[derive(Debug, Error)]
pub enum ScoutError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("TLS error: {0}")]
    Tls(String),
    #[error("Timeout error: {0}")]
    Timeout(String),
    #[error("Parse error: {0}")]
    Parse(String),
    #[error("Challenge error: {0}")]
    Challenge(String),
    #[error("Session error: {0}")]
    Session(String),
    #[error("Cache error: {0}")]
    Cache(String),
    #[error("Unsupported error: {0}")]
    Unsupported(String),
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("Cookie store error: {0}")]
    Cookie(String),
}

pub type Result<T> = std::result::Result<T, ScoutError>;

