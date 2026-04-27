use std::sync::Arc;
use tokio::time::{sleep, Duration};
use rand::Rng;
use reqwest::{Client, redirect::Policy};
use reqwest::header::{HeaderMap, HeaderValue, HeaderName};
use url::Url;
use std::sync::RwLock;

use crate::session::Session;
use crate::http::HttpResponse;
use crate::error::{Result, ScoutError};

fn build_rustls_config() -> rustls::ClientConfig {
    let mut crypto = rustls::crypto::ring::default_provider();

    crypto.cipher_suites = vec![
        rustls::crypto::ring::cipher_suite::TLS13_AES_128_GCM_SHA256,
        rustls::crypto::ring::cipher_suite::TLS13_AES_256_GCM_SHA384,
        rustls::crypto::ring::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256,
        rustls::crypto::ring::cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
        rustls::crypto::ring::cipher_suite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
        rustls::crypto::ring::cipher_suite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
        rustls::crypto::ring::cipher_suite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
        rustls::crypto::ring::cipher_suite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
        rustls::crypto::ring::cipher_suite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    ];

    let mut config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(crypto))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();

    // Bypass verification explicitly here since we build the config natively
    config.dangerous().set_certificate_verifier(std::sync::Arc::new(NoCertificateVerification));
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    config
}

#[derive(Debug)]
struct NoCertificateVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA1,
            rustls::SignatureScheme::ECDSA_SHA1_Legacy,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}

pub struct HttpClient {
    pub client: Client,
}

struct SharedCookieStore(Arc<RwLock<cookie_store::CookieStore>>);

impl reqwest::cookie::CookieStore for SharedCookieStore {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let mut store = self.0.write().unwrap();
        for header in cookie_headers {
            if let Ok(cookie_str) = header.to_str() {
                if let Ok(cookie) = cookie_store::Cookie::parse(cookie_str, url) {
                    let _ = store.insert_raw(&cookie, url);
                }
            }
        }
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        let store = self.0.read().unwrap();
        let cookies = store.matches(url)
            .into_iter()
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ");

        if cookies.is_empty() {
            None
        } else {
            HeaderValue::from_str(&cookies).ok()
        }
    }
}

impl std::fmt::Debug for SharedCookieStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedCookieStore").finish()
    }
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
         .finish()
    }
}

impl HttpClient {
    #[tracing::instrument(skip(session))]
    pub fn build(session: &Session) -> Result<HttpClient> {
        let mut headers = HeaderMap::new();

        for header_name in &session.identity.header_order {
            let val = match header_name.as_str() {
                "user-agent" => Some(session.identity.user_agent.as_str()),
                "accept" => Some(session.identity.accept.as_str()),
                "accept-language" => Some(session.identity.accept_language.as_str()),
                "sec-ch-ua" => session.identity.sec_ch_ua.as_deref(),
                "sec-ch-ua-mobile" => session.identity.sec_ch_ua_mobile.as_deref(),
                "sec-ch-ua-platform" => session.identity.sec_ch_ua_platform.as_deref(),
                "connection" => Some("keep-alive"),
                "upgrade-insecure-requests" => Some("1"),
                "sec-fetch-dest" => Some("document"),
                "sec-fetch-mode" => Some("navigate"),
                "sec-fetch-site" => Some("none"),
                "sec-fetch-user" => Some("?1"),
                "accept-encoding" => Some("gzip, deflate, br"),
                _ => None,
            };

            if let Some(v) = val {
                if let (Ok(h_name), Ok(h_val)) = (HeaderName::from_bytes(header_name.as_bytes()), HeaderValue::from_str(v)) {
                    headers.insert(h_name, h_val);
                }
            }
        }

        let cookie_provider = Arc::clone(&session.cookies);

        let config = build_rustls_config();

        let client = Client::builder()
            .use_preconfigured_tls(config)
            .gzip(true)
            .brotli(true)
            .deflate(true)
            .http2_initial_stream_window_size(6_291_456)
            .http2_initial_connection_window_size(15_663_105)
            .cookie_provider(Arc::new(SharedCookieStore(cookie_provider)))
            .default_headers(headers)
            .redirect(Policy::limited(10)) // Needs to preserve POST on 307/308, which reqwest does by default
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(ScoutError::Http)?;

        Ok(HttpClient { client })
    }

    async fn delay_if_needed(delay_range: Option<std::ops::Range<u64>>) {
        if let Some(range) = delay_range {
            if range.start < range.end {
                let mut rng = rand::thread_rng();
                let delay = rng.gen_range(range);
                sleep(Duration::from_millis(delay)).await;
            }
        }
    }

    #[tracing::instrument(skip(self))]
    pub async fn get(&self, url: &str, delay_range: Option<std::ops::Range<u64>>) -> Result<HttpResponse> {
        Self::delay_if_needed(delay_range).await;

        let res = self.client.get(url).send().await?;
        let status = res.status().as_u16();
        let final_url = res.url().to_string();
        let headers = res.headers().clone();
        let body = res.bytes().await?;

        Ok(HttpResponse {
            status,
            final_url,
            headers,
            body,
        })
    }

    #[tracing::instrument(skip(self, fields))]
    pub async fn post_form(&self, url: &str, fields: &std::collections::HashMap<&str, &str>, delay_range: Option<std::ops::Range<u64>>) -> Result<HttpResponse> {
        Self::delay_if_needed(delay_range).await;

        let res = self.client.post(url).form(fields).send().await?;
        let status = res.status().as_u16();
        let final_url = res.url().to_string();
        let headers = res.headers().clone();
        let body = res.bytes().await?;

        Ok(HttpResponse {
            status, final_url, headers, body
        })
    }

    #[tracing::instrument(skip(self, body_data))]
    pub async fn post_json(&self, url: &str, body_data: &serde_json::Value, delay_range: Option<std::ops::Range<u64>>) -> Result<HttpResponse> {
        Self::delay_if_needed(delay_range).await;

        let res = self.client.post(url).json(body_data).send().await?;
        let status = res.status().as_u16();
        let final_url = res.url().to_string();
        let headers = res.headers().clone();
        let body = res.bytes().await?;

        Ok(HttpResponse {
            status, final_url, headers, body
        })
    }
}
