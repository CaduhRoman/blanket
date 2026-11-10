//! The `blanket:` URL scheme, served from memory.
//!
//! Only `blanket:newtab` is a web page. It contains no browser state other than the
//! user's most visited sites and is rendered without privileged APIs (no
//! `privileged_paths`), under a restrictive Content-Security-Policy. Browser state such
//! as history, bookmarks and settings is shown in native UI panels instead, so no web
//! page (internal or not) can read or modify it.

use std::fmt::Write as _;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

use blanket_core::config::SearchEngine;
use headers::{ContentType, HeaderMapExt, HeaderName, HeaderValue};
use servo::protocol_handler::{
    DoneChannel, FetchContext, NetworkError, ProtocolHandler, Request, ResourceFetchTiming, Response,
    ResponseBody,
};
use url::Url;

pub const SCHEME: &str = "blanket";

const NEWTAB_TEMPLATE: &str = include_str!("../resources/newtab.html");
const SILK_JPEG: &[u8] = include_bytes!("../resources/silk.jpg");
const NEWTAB_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; img-src blanket:; \
                          form-action http: https:; base-uri 'none'";
const TOP_SITES: usize = 8;

/// A site tile on the new tab page.
#[derive(Clone, Debug)]
pub struct TopSite {
    pub title: String,
    pub url: Url,
}

#[derive(Clone, Debug, Default)]
struct NewTabState {
    search: SearchEngine,
    top_sites: Vec<TopSite>,
}

/// Data shared between the UI thread (writer) and Servo's network threads (reader).
#[derive(Clone, Default)]
pub struct NewTabData {
    state: Arc<RwLock<NewTabState>>,
}

impl NewTabData {
    pub fn update(&self, search: &SearchEngine, top_sites: Vec<TopSite>) {
        let mut state = self.state.write().unwrap_or_else(|poison| poison.into_inner());
        state.search = search.clone();
        state.top_sites = top_sites;
        state.top_sites.truncate(TOP_SITES);
    }

    fn render(&self) -> String {
        let state = self.state.read().unwrap_or_else(|poison| poison.into_inner());
        render_newtab(&state.search, &state.top_sites)
    }
}

pub struct BlanketProtocolHandler {
    newtab: NewTabData,
}

impl BlanketProtocolHandler {
    pub fn new(newtab: NewTabData) -> Self {
        Self { newtab }
    }
}

impl ProtocolHandler for BlanketProtocolHandler {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let url = request.current_url();
        let path = url.path().to_owned();
        let response = match path.as_str() {
            "newtab" => {
                let mut response = Response::new(url, ResourceFetchTiming::new(request.timing_type()));
                response.headers.typed_insert(ContentType::html());
                response.headers.insert(
                    HeaderName::from_static("content-security-policy"),
                    HeaderValue::from_static(NEWTAB_CSP),
                );
                *response.body.lock() = ResponseBody::Done(self.newtab.render().into_bytes());
                response
            },
            "silk" => {
                let mut response = Response::new(url, ResourceFetchTiming::new(request.timing_type()));
                response.headers.insert(
                    HeaderName::from_static("content-type"),
                    HeaderValue::from_static("image/jpeg"),
                );
                response.headers.insert(
                    HeaderName::from_static("cache-control"),
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                );
                *response.body.lock() = ResponseBody::Done(SILK_JPEG.to_vec());
                response
            },
            _ => Response::network_error(NetworkError::ResourceLoadError("Unknown blanket: page".to_owned())),
        };
        Box::pin(std::future::ready(response))
    }
}

/// Split a search template such as `https://duckduckgo.com/?q={query}` into a form
/// action, the name of the query field and any fixed hidden fields.
fn search_form(search: &SearchEngine) -> (String, String, Vec<(String, String)>) {
    let fallback = || ("https://duckduckgo.com/".to_owned(), "q".to_owned(), Vec::new());
    let Ok(url) = Url::parse(&search.template.replace("{query}", "BLANKETQUERY")) else {
        return fallback();
    };
    let mut query_name = None;
    let mut hidden = Vec::new();
    for (key, value) in url.query_pairs() {
        if value == "BLANKETQUERY" {
            query_name = Some(key.into_owned());
        } else {
            hidden.push((key.into_owned(), value.into_owned()));
        }
    }
    let Some(query_name) = query_name else {
        return fallback();
    };
    let mut action = url.clone();
    action.set_query(None);
    action.set_fragment(None);
    (action.to_string(), query_name, hidden)
}

fn render_newtab(search: &SearchEngine, top_sites: &[TopSite]) -> String {
    let (action, query_name, hidden) = search_form(search);
    let mut fields = String::new();
    for (name, value) in hidden {
        let _ = write!(
            fields,
            r#"<input type="hidden" name="{}" value="{}">"#,
            escape_html(&name),
            escape_html(&value)
        );
    }
    let mut sites = String::new();
    for site in top_sites {
        let label = if site.title.trim().is_empty() || is_error_title(&site.title) {
            site.url.host_str().unwrap_or(site.url.as_str()).to_owned()
        } else {
            site.title.clone()
        };
        let initial = label.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_default();
        let _ = write!(
            sites,
            r#"<a class="site" href="{href}"><span class="badge" style="background:{color}">{initial}</span><span>{label}</span></a>"#,
            href = escape_html(site.url.as_str()),
            color = badge_color(site.url.host_str().unwrap_or_default()),
            initial = escape_html(&initial),
            label = escape_html(&label),
        );
    }
    NEWTAB_TEMPLATE
        .replace("{{SEARCH_ACTION}}", &escape_html(&action))
        .replace("{{QUERY_NAME}}", &escape_html(&query_name))
        .replace("{{ENGINE_NAME}}", &escape_html(&search.name))
        .replace("{{SEARCH_FIELDS}}", &fields)
        .replace("{{TOP_SITES}}", &sites)
        .replace("{{VERSION}}", env!("CARGO_PKG_VERSION"))
}

/// Titles of the engine's error pages, which say nothing about the site; a failed
/// visit should not label its tile.
fn is_error_title(title: &str) -> bool {
    matches!(title.trim(), "Error loading page" | "Page not found" | "Problem loading page")
}

/// A stable, muted color per host for the site badge.
fn badge_color(host: &str) -> &'static str {
    const COLORS: &[&str] = &["#3a7bd5", "#8e5cd9", "#d9534f", "#e08e2b", "#2f9e72", "#1f8fa3", "#c2477f", "#5b6ee1"];
    let hash = host.bytes().fold(0u32, |hash, byte| hash.wrapping_mul(31).wrapping_add(u32::from(byte)));
    COLORS[hash as usize % COLORS.len()]
}

fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_templates_become_forms() {
        let engine = SearchEngine {
            name: "Example".into(),
            template: "https://search.example/find?src=blanket&q={query}".into(),
        };
        let (action, query, hidden) = search_form(&engine);
        assert_eq!(action, "https://search.example/find");
        assert_eq!(query, "q");
        assert_eq!(hidden, vec![("src".to_owned(), "blanket".to_owned())]);
    }

    #[test]
    fn silk_asset_is_a_jpeg() {
        assert!(SILK_JPEG.starts_with(&[0xFF, 0xD8, 0xFF]), "silk.jpg must be a JPEG");
        assert!(SILK_JPEG.len() > 8_000);
    }

    #[test]
    fn page_escapes_untrusted_titles() {
        let sites = vec![TopSite {
            title: "<script>alert(1)</script>".into(),
            url: Url::parse("https://evil.example/\"><x>").unwrap(),
        }];
        let html = render_newtab(&SearchEngine::default(), &sites);
        assert!(!html.contains("<script>alert(1)"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(!html.contains("\"><x>"));
    }

    #[test]
    fn error_page_titles_fall_back_to_host() {
        let sites = vec![TopSite {
            title: "Error loading page".into(),
            url: Url::parse("https://www.youtube.com/").unwrap(),
        }];
        let html = render_newtab(&SearchEngine::default(), &sites);
        assert!(html.contains("<span>www.youtube.com</span>"));
        assert!(!html.contains("Error loading page"));
    }
}
