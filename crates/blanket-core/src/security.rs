//! Security decisions that are the embedder's responsibility.
//!
//! Servo itself enforces the web platform's security model (same-origin policy, CSP,
//! mixed-content blocking, TLS certificate validation). This module covers what Servo
//! delegates to the browser: which navigations between privilege levels are allowed,
//! what the address bar shows, permission prompts, and download safety. Nothing here
//! loosens a check performed by the engine.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

/// Security state shown in the address bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionSecurity {
    /// `https:` (TLS validated by the engine).
    Secure,
    /// `http:` to a non-local host.
    Insecure,
    /// `http:` to localhost / loopback, which browsers treat as potentially trustworthy.
    LocalHttp,
    /// `file:` URLs.
    LocalFile,
    /// Blanket's own pages.
    Internal,
    /// `about:`, `data:` and similar.
    Other,
}

pub fn connection_security(url: &Url) -> ConnectionSecurity {
    match url.scheme() {
        "https" | "wss" => ConnectionSecurity::Secure,
        "http" | "ws" => {
            if is_loopback(url) {
                ConnectionSecurity::LocalHttp
            } else {
                ConnectionSecurity::Insecure
            }
        },
        "file" => ConnectionSecurity::LocalFile,
        "blanket" => ConnectionSecurity::Internal,
        _ => ConnectionSecurity::Other,
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain == "localhost" || domain.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Trust level of a document, used to stop less-trusted content from navigating into
/// more-trusted contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Privilege {
    Web = 0,
    LocalFile = 1,
    Internal = 2,
}

pub fn privilege_of(url: &Url) -> Privilege {
    match url.scheme() {
        "blanket" => Privilege::Internal,
        "file" => Privilege::LocalFile,
        _ => Privilege::Web,
    }
}

/// Internal pages that expose browser state or controls. Only the user may open them,
/// never a web page.
const PRIVILEGED_INTERNAL_PAGES: &[&str] = &["settings", "history", "bookmarks", "downloads", "performance"];

/// Decide whether content currently showing `from` may navigate its frame to `to`.
///
/// Navigations the *user* starts (address bar, bookmarks, UI buttons) do not go through
/// this check; it applies to navigations initiated by page content.
pub fn content_navigation_allowed(from: Option<&Url>, to: &Url) -> bool {
    let from_privilege = from.map_or(Privilege::Web, privilege_of);
    let to_privilege = privilege_of(to);
    if to_privilege <= from_privilege {
        return true;
    }
    match to_privilege {
        // Web content may link to the public new-tab page, but not to pages exposing state.
        Privilege::Internal => !PRIVILEGED_INTERNAL_PAGES.contains(&to.path()),
        // Remote pages must never open local files (information disclosure).
        Privilege::LocalFile => false,
        Privilege::Web => true,
    }
}

/// Features web content can request permission for, as stored by Blanket.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionKind {
    Geolocation,
    Notifications,
    Camera,
    Microphone,
    Clipboard,
    Bluetooth,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionDecision {
    Allow,
    Deny,
}

/// Remembered per-origin permission decisions. Origins are stored in their ASCII
/// serialization (`https://example.com:8443`). Opaque origins are never remembered.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PermissionStore {
    decisions: HashMap<String, HashMap<PermissionKind, PermissionDecision>>,
}

impl PermissionStore {
    pub fn decision(&self, url: &Url, kind: PermissionKind) -> Option<PermissionDecision> {
        let origin = origin_key(url)?;
        self.decisions.get(&origin)?.get(&kind).copied()
    }

    /// Remember a decision. Returns `false` (and remembers nothing) for opaque origins.
    pub fn remember(&mut self, url: &Url, kind: PermissionKind, decision: PermissionDecision) -> bool {
        let Some(origin) = origin_key(url) else {
            return false;
        };
        self.decisions.entry(origin).or_default().insert(kind, decision);
        true
    }

    pub fn forget_origin(&mut self, url: &Url) {
        if let Some(origin) = origin_key(url) {
            self.decisions.remove(&origin);
        }
    }

    pub fn clear(&mut self) {
        self.decisions.clear();
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, PermissionKind, PermissionDecision)> {
        self.decisions
            .iter()
            .flat_map(|(origin, kinds)| kinds.iter().map(move |(kind, decision)| (origin.as_str(), *kind, *decision)))
    }
}

fn origin_key(url: &Url) -> Option<String> {
    let origin = url.origin();
    origin.is_tuple().then(|| origin.ascii_serialization())
}

/// Risk assessment of a download before it starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRisk {
    /// The file type can execute code when opened.
    pub executable: bool,
    /// The transfer is not protected by TLS.
    pub insecure_transport: bool,
}

impl DownloadRisk {
    pub fn needs_confirmation(&self, config: &crate::config::SecurityConfig) -> bool {
        (self.executable && config.confirm_dangerous_downloads) ||
            (self.insecure_transport && config.confirm_insecure_downloads)
    }
}

const EXECUTABLE_EXTENSIONS: &[&str] = &[
    "exe", "msi", "msix", "appx", "bat", "cmd", "com", "scr", "pif", "cpl", "ps1", "psm1", "vbs", "vbe", "js",
    "jse", "wsf", "wsh", "hta", "jar", "lnk", "reg", "dll", "sys", "iso", "img", "vhd", "vhdx",
];

pub fn assess_download(url: &Url, file_name: &str) -> DownloadRisk {
    let extension = Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    DownloadRisk {
        executable: EXECUTABLE_EXTENSIONS.contains(&extension.as_str()),
        insecure_transport: connection_security(url) == ConnectionSecurity::Insecure,
    }
}

/// Windows reserves these device names regardless of extension.
const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1",
    "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Turn an untrusted, server-provided file name into a safe single path component.
///
/// Removes directory separators and traversal, characters invalid on Windows, control
/// characters, trailing dots/spaces, and avoids reserved device names.
pub fn sanitize_file_name(raw: &str) -> String {
    let last_component = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name: String = last_component
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    while name.ends_with(['.', ' ']) {
        name.pop();
    }
    let name_trimmed = name.trim_start_matches(['.', ' ']).to_owned();
    name = name_trimmed;
    if name.is_empty() {
        return "download".into();
    }
    let stem = name.split('.').next().unwrap_or_default().to_ascii_lowercase();
    if RESERVED_WINDOWS_NAMES.contains(&stem.as_str()) {
        name.insert(0, '_');
    }
    if name.chars().count() > 200 {
        let extension = Path::new(&name)
            .extension()
            .and_then(|extension| extension.to_str())
            .filter(|extension| extension.len() <= 16)
            .map(|extension| format!(".{extension}"))
            .unwrap_or_default();
        let keep = 200 - extension.chars().count();
        name = name.chars().take(keep).collect::<String>() + &extension;
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn connection_security_is_classified() {
        assert_eq!(connection_security(&url("https://a.com")), ConnectionSecurity::Secure);
        assert_eq!(connection_security(&url("http://a.com")), ConnectionSecurity::Insecure);
        assert_eq!(connection_security(&url("http://localhost:8080")), ConnectionSecurity::LocalHttp);
        assert_eq!(connection_security(&url("http://127.0.0.1")), ConnectionSecurity::LocalHttp);
        assert_eq!(connection_security(&url("http://[::1]/")), ConnectionSecurity::LocalHttp);
        assert_eq!(connection_security(&url("file:///C:/x.html")), ConnectionSecurity::LocalFile);
        assert_eq!(connection_security(&url("blanket:newtab")), ConnectionSecurity::Internal);
    }

    #[test]
    fn web_content_cannot_escalate() {
        let web = url("https://evil.example/");
        assert!(!content_navigation_allowed(Some(&web), &url("blanket:settings")));
        assert!(!content_navigation_allowed(Some(&web), &url("blanket:history")));
        assert!(!content_navigation_allowed(Some(&web), &url("file:///C:/Users/")));
        assert!(content_navigation_allowed(Some(&web), &url("blanket:newtab")));
        assert!(content_navigation_allowed(Some(&web), &url("https://other.example/")));
    }

    #[test]
    fn trusted_content_may_navigate_down() {
        let internal = url("blanket:newtab");
        assert!(content_navigation_allowed(Some(&internal), &url("https://servo.org/")));
        assert!(content_navigation_allowed(Some(&internal), &url("blanket:settings")));
        let file = url("file:///C:/a.html");
        assert!(content_navigation_allowed(Some(&file), &url("file:///C:/b.html")));
    }

    #[test]
    fn permissions_are_per_origin() {
        let mut store = PermissionStore::default();
        let page = url("https://maps.example/a/b");
        assert!(store.remember(&page, PermissionKind::Geolocation, PermissionDecision::Allow));
        assert_eq!(
            store.decision(&url("https://maps.example/other"), PermissionKind::Geolocation),
            Some(PermissionDecision::Allow)
        );
        assert_eq!(store.decision(&url("http://maps.example/"), PermissionKind::Geolocation), None);
        assert_eq!(store.decision(&page, PermissionKind::Camera), None);
        assert!(!store.remember(&url("data:text/html,hi"), PermissionKind::Camera, PermissionDecision::Allow));
    }

    #[test]
    fn downloads_are_assessed() {
        let risk = assess_download(&url("http://a.com/setup.EXE"), "setup.EXE");
        assert!(risk.executable && risk.insecure_transport);
        let risk = assess_download(&url("https://a.com/photo.png"), "photo.png");
        assert!(!risk.executable && !risk.insecure_transport);
    }

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("..\\..\\boot.ini"), "boot.ini");
        assert_eq!(sanitize_file_name("a<b>c:d?.txt"), "a_b_c_d_.txt");
        assert_eq!(sanitize_file_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_file_name("report. . "), "report");
        assert_eq!(sanitize_file_name("..."), "download");
        assert_eq!(sanitize_file_name(""), "download");
        assert_eq!(sanitize_file_name(".hidden"), "hidden");
        let long = format!("{}.zip", "x".repeat(400));
        let sanitized = sanitize_file_name(&long);
        assert_eq!(sanitized.chars().count(), 200);
        assert!(sanitized.ends_with(".zip"));
    }
}
