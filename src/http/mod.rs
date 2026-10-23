pub mod client;
pub mod response;
pub mod detect;
pub mod headers;

pub use client::HttpClient;
pub use response::HttpResponse;
pub use detect::{PageState, ChallengeHint};

