//! Interpretation of what the user types in the address bar.

use url::Url;

use crate::config::SearchEngine;

/// What the address bar decided to do with the user's input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OmniboxTarget {
    /// The input was an address.
    Navigate(Url),
    /// The input was turned into a search query.
    Search(Url),
}

impl OmniboxTarget {
    pub fn url(&self) -> &Url {
        match self {
            Self::Navigate(url) | Self::Search(url) => url,
        }
    }

    pub fn into_url(self) -> Url {
        match self {
            Self::Navigate(url) | Self::Search(url) => url,
        }
    }
}

/// Schemes the user may type explicitly. `javascript:` is intentionally absent: running
/// script by pasting into the address bar is a classic social-engineering vector, so such
/// input is treated as a search instead.
const TYPED_SCHEMES: &[&str] = &["http", "https", "file", "about", "blanket", "data"];

/// Interpret raw address bar input.
///
/// Returns `None` for blank input.
pub fn interpret(input: &str, search: &SearchEngine, prefer_https: bool) -> Option<OmniboxTarget> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }

    if let Some(url) = parse_explicit_url(input) {
        return Some(OmniboxTarget::Navigate(url));
    }

    if !input.contains(char::is_whitespace) && looks_like_host(input) {
        let scheme = if prefer_https && !is_local_host(input) { "https" } else { "http" };
        if let Ok(url) = Url::parse(&format!("{scheme}://{input}")) {
            return Some(OmniboxTarget::Navigate(url));
        }
    }

    Some(OmniboxTarget::Search(search_url(input, search)))
}

/// Build a search URL for `query` using the configured engine.
pub fn search_url(query: &str, search: &SearchEngine) -> Url {
    let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
    let candidate = search.template.replace("{query}", &encoded);
    Url::parse(&candidate).unwrap_or_else(|_| {
        let fallback = SearchEngine::default().template.replace("{query}", &encoded);
        Url::parse(&fallback).expect("default search template is a valid URL")
    })
}

fn parse_explicit_url(input: &str) -> Option<Url> {
    // `Url::parse` happily accepts "localhost:3000" as scheme "localhost", so only accept
    // schemes that we know about.
    let (scheme, rest) = input.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    if !TYPED_SCHEMES.contains(&scheme.as_str()) {
        return None;
    }
    if matches!(scheme.as_str(), "http" | "https") && !rest.starts_with("//") {
        return None;
    }
    Url::parse(input).ok()
}

fn host_part(input: &str) -> &str {
    let end = input.find(['/', '?', '#']).unwrap_or(input.len());
    let authority = &input[..end];
    // Strip user info, then the port.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    if authority.starts_with('[') {
        return authority.split_once(']').map_or(authority, |(host, _)| host).trim_start_matches('[');
    }
    authority.rsplit_once(':').map_or(authority, |(host, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) { host } else { authority }
    })
}

fn is_local_host(input: &str) -> bool {
    let host = host_part(input).to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost") || host.parse::<std::net::IpAddr>().is_ok()
}

fn looks_like_host(input: &str) -> bool {
    let host = host_part(input);
    if host.is_empty() {
        return false;
    }
    if host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|label| label.is_empty() || label.len() > 63) {
        return false;
    }
    let valid_label = |label: &&str| {
        label.chars().all(|c| c.is_alphanumeric() || c == '-') && !label.starts_with('-') && !label.ends_with('-')
    };
    if !labels.iter().all(valid_label) {
        return false;
    }
    // The top-level domain must be alphabetic ("example.com", not "3.14").
    labels
        .last()
        .is_some_and(|tld| tld.len() >= 2 && tld.chars().all(char::is_alphabetic))
}

/// Text displayed in the address bar for `url`: internal pages are shown compactly and
/// the user's own search terms are not hidden behind a long URL.
pub fn display_text(url: &Url) -> String {
    match url.scheme() {
        "blanket" if url.path() == "newtab" => String::new(),
        _ => url.as_str().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &str) -> Option<OmniboxTarget> {
        interpret(input, &SearchEngine::default(), true)
    }

    fn navigates_to(input: &str, expected: &str) {
        match run(input) {
            Some(OmniboxTarget::Navigate(url)) => assert_eq!(url.as_str(), expected, "input {input:?}"),
            other => panic!("expected navigation for {input:?}, got {other:?}"),
        }
    }

    fn searches(input: &str) {
        match run(input) {
            Some(OmniboxTarget::Search(url)) => assert!(url.as_str().starts_with("https://duckduckgo.com/?q=")),
            other => panic!("expected search for {input:?}, got {other:?}"),
        }
    }

    #[test]
    fn blank_input_does_nothing() {
        assert_eq!(run("   "), None);
    }

    #[test]
    fn explicit_urls_are_kept() {
        navigates_to("https://servo.org/", "https://servo.org/");
        navigates_to("http://example.com/a?b=c", "http://example.com/a?b=c");
        navigates_to("blanket:newtab", "blanket:newtab");
        navigates_to("about:blank", "about:blank");
    }

    #[test]
    fn bare_hosts_prefer_https() {
        navigates_to("servo.org", "https://servo.org/");
        navigates_to("docs.rs/servo/latest", "https://docs.rs/servo/latest");
        navigates_to("münchen.de", "https://xn--mnchen-3ya.de/");
    }

    #[test]
    fn local_hosts_use_http() {
        navigates_to("localhost:8000", "http://localhost:8000/");
        navigates_to("127.0.0.1:3000/x", "http://127.0.0.1:3000/x");
        navigates_to("192.168.0.1", "http://192.168.0.1/");
    }

    #[test]
    fn queries_become_searches() {
        searches("rust servo embedding");
        searches("what is 3.14");
        searches("3.14");
        searches("hello");
        searches("c++ vs rust");
    }

    #[test]
    fn javascript_urls_are_never_executed() {
        searches("javascript:alert(1)");
    }

    #[test]
    fn search_terms_are_encoded() {
        let url = search_url("a&b=c d", &SearchEngine::default());
        assert_eq!(url.as_str(), "https://duckduckgo.com/?q=a%26b%3Dc+d");
    }

    #[test]
    fn new_tab_url_is_displayed_empty() {
        assert_eq!(display_text(&Url::parse("blanket:newtab").unwrap()), "");
    }
}
