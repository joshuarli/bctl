use std::fmt;

/// A URL reduced to what is safe to store or show: scheme, host, port, and path. Userinfo, query, and
/// fragment are gone. `about:` URLs keep their path; other opaque schemes (`data:`, `javascript:`, `blob:`)
/// keep only the scheme, since their payload may be a secret. Text that is not a URL becomes empty.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SafeUrl(String);

impl SafeUrl {
    pub fn new(raw: &str) -> SafeUrl {
        let raw = &raw[..raw.find(['?', '#']).unwrap_or(raw.len())];
        let Some((scheme, rest)) = raw.split_once(':') else { return SafeUrl(String::new()) };
        let valid = scheme.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b));
        if !valid {
            return SafeUrl(String::new());
        }
        let scheme = scheme.to_ascii_lowercase();
        if let Some(hierarchical) = rest.strip_prefix("//") {
            let (authority, path) = hierarchical.split_at(hierarchical.find('/').unwrap_or(hierarchical.len()));
            let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
            return SafeUrl(format!("{scheme}://{host}{path}"));
        }
        if scheme == "about" {
            return SafeUrl(format!("about:{rest}"));
        }
        SafeUrl(format!("{scheme}:"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SafeUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::SafeUrl;

    #[test]
    fn credentials_query_and_fragment_never_survive() {
        for (raw, safe) in [
            ("https://user:secret@example.com:8443/a/b?token=x#frag", "https://example.com:8443/a/b"),
            ("HTTP://example.com", "http://example.com"),
            ("http://example.com/#?x", "http://example.com/"),
            ("http://a@b@example.com/p", "http://example.com/p"),
            ("file:///Users/me/page.html?q", "file:///Users/me/page.html"),
            ("chrome://newtab/", "chrome://newtab/"),
            ("about:blank", "about:blank"),
            ("about:blank#x", "about:blank"),
        ] {
            assert_eq!(SafeUrl::new(raw).as_str(), safe, "{raw}");
        }
    }

    #[test]
    fn opaque_payloads_and_non_urls_are_dropped() {
        for (raw, safe) in [
            ("data:text/html,<p>password</p>", "data:"),
            ("javascript:alert(document.cookie)", "javascript:"),
            ("blob:https://example.com/0f1e", "blob:"),
            ("not a url", ""),
            ("1http://x", ""),
            ("", ""),
        ] {
            assert_eq!(SafeUrl::new(raw).as_str(), safe, "{raw}");
        }
    }
}
