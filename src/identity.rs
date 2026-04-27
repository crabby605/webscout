use rand::seq::SliceRandom;

#[derive(Debug, Clone)]
pub struct Identity {
    pub user_agent: String,
    pub accept_language: String,
    pub accept: String,
    pub viewport: String,
    pub header_order: Vec<String>,
    pub sec_ch_ua: Option<String>,
    pub sec_ch_ua_mobile: Option<String>,
    pub sec_ch_ua_platform: Option<String>,
    pub platform: String,
}

impl Identity {
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();

        let chrome_uas = vec![
            ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36", "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\"", "Windows"),
            ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36", "\"Not_A Brand\";v=\"8\", \"Chromium\";v=\"120\", \"Google Chrome\";v=\"120\"", "macOS"),
        ];

        let firefox_uas = vec![
            ("Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:121.0) Gecko/20100101 Firefox/121.0", "Windows"),
            ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:121.0) Gecko/20100101 Firefox/121.0", "macOS"),
        ];

        let safari_uas = vec![
            ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.2 Safari/605.1.15", "macOS"),
        ];

        let browser_type: u8 = rand::random::<u8>() % 3;

        match browser_type {
            0 => { // Chrome
                let (ua, sec_ch, platform) = chrome_uas.choose(&mut rng).unwrap();
                Identity {
                    user_agent: ua.to_string(),
                    accept_language: "en-US,en;q=0.9".to_string(),
                    accept: "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8".to_string(),
                    viewport: "1920x1080".to_string(),
                    header_order: vec!["host".into(), "connection".into(), "sec-ch-ua".into(), "sec-ch-ua-mobile".into(), "sec-ch-ua-platform".into(), "upgrade-insecure-requests".into(), "user-agent".into(), "accept".into(), "sec-fetch-site".into(), "sec-fetch-mode".into(), "sec-fetch-user".into(), "sec-fetch-dest".into(), "accept-encoding".into(), "accept-language".into()],
                    sec_ch_ua: Some(sec_ch.to_string()),
                    sec_ch_ua_mobile: Some("?0".to_string()),
                    sec_ch_ua_platform: Some(format!("\"{}\"", platform)),
                    platform: platform.to_string(),
                }
            },
            1 => { // Firefox
                let (ua, platform) = firefox_uas.choose(&mut rng).unwrap();
                Identity {
                    user_agent: ua.to_string(),
                    accept_language: "en-US,en;q=0.5".to_string(),
                    accept: "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8".to_string(),
                    viewport: "1920x1080".to_string(),
                    header_order: vec!["host".into(), "user-agent".into(), "accept".into(), "accept-language".into(), "accept-encoding".into(), "connection".into(), "upgrade-insecure-requests".into(), "sec-fetch-dest".into(), "sec-fetch-mode".into(), "sec-fetch-site".into(), "sec-fetch-user".into()],
                    sec_ch_ua: None,
                    sec_ch_ua_mobile: None,
                    sec_ch_ua_platform: None,
                    platform: platform.to_string(),
                }
            },
            _ => { // Safari
                let (ua, platform) = safari_uas.choose(&mut rng).unwrap();
                Identity {
                    user_agent: ua.to_string(),
                    accept_language: "en-US,en;q=0.9".to_string(),
                    accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
                    viewport: "1920x1080".to_string(),
                    header_order: vec!["host".into(), "accept".into(), "user-agent".into(), "accept-language".into(), "accept-encoding".into(), "connection".into()],
                    sec_ch_ua: None,
                    sec_ch_ua_mobile: None,
                    sec_ch_ua_platform: None,
                    platform: platform.to_string(),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_coherence() {
        let ident = Identity::generate();
        if ident.user_agent.contains("Chrome") {
            assert!(ident.sec_ch_ua.is_some());
        } else {
            assert!(ident.sec_ch_ua.is_none());
        }
    }
}

