use std::fmt;

use http::Uri;

/// A browser websocket URL: `ws://host[:port]/devtools/browser/<id>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    uri: Uri,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    Invalid(String),
    Scheme,
    MissingHost,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EndpointError::Invalid(e) => write!(f, "not a URL: {e}"),
            EndpointError::Scheme => f.write_str("a CDP endpoint must be a ws:// URL"),
            EndpointError::MissingHost => f.write_str("a CDP endpoint needs a host"),
        }
    }
}

impl std::error::Error for EndpointError {}

impl Endpoint {
    pub fn parse(url: &str) -> Result<Endpoint, EndpointError> {
        let uri: Uri = url.parse().map_err(|e: http::uri::InvalidUri| EndpointError::Invalid(e.to_string()))?;
        if uri.scheme_str() != Some("ws") {
            return Err(EndpointError::Scheme);
        }
        if uri.host().is_none_or(str::is_empty) {
            return Err(EndpointError::MissingHost);
        }
        Ok(Endpoint { uri })
    }

    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// The address to dial. RFC 6761 reserves `localhost` and its subdomains
    /// for loopback, so they dial 127.0.0.1 without consulting a resolver.
    pub(crate) fn dial_target(&self) -> (String, u16) {
        let host = self.uri.host().unwrap_or_default();
        let port = self.uri.port_u16().unwrap_or(80);
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let lower = host.to_ascii_lowercase();
        if lower == "localhost" || lower.ends_with(".localhost") {
            return ("127.0.0.1".into(), port);
        }
        (host.to_string(), port)
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.uri.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_capability_hosts_dial_loopback_and_keep_their_host_header() {
        let endpoint = Endpoint::parse("ws://ab12.localhost:4100/devtools/browser/x").unwrap();
        assert_eq!(endpoint.dial_target(), ("127.0.0.1".to_string(), 4100));
        assert_eq!(endpoint.uri().authority().unwrap().as_str(), "ab12.localhost:4100");
        assert_eq!(Endpoint::parse("ws://[::1]:9/devtools/browser/x").unwrap().dial_target(), ("::1".to_string(), 9));
    }

    #[test]
    fn only_plain_websocket_urls_are_endpoints() {
        assert_eq!(Endpoint::parse("http://127.0.0.1:9222/json/version"), Err(EndpointError::Scheme));
        assert_eq!(Endpoint::parse("wss://127.0.0.1:9222/devtools/browser/x"), Err(EndpointError::Scheme));
    }
}
