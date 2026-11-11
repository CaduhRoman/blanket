//! Centralized, user-editable configuration (`blanket.toml`).
//!
//! Every tunable used by the resource managers lives here so that behaviour can be
//! adjusted (and benchmarked) without recompiling. Unknown keys are ignored and missing
//! keys take their default, so old config files keep working across versions.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::error::{CoreError, CoreResult};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BlanketConfig {
    pub general: GeneralConfig,
    pub memory: MemoryConfig,
    pub scheduler: SchedulerConfig,
    pub cache: CacheConfig,
    pub security: SecurityConfig,
    pub preload: PreloadConfig,
    pub ui: UiConfig,
    pub compat: CompatConfig,
}

/// Compatibility mode: sites that need features Servo does not have yet (Media Source
/// Extensions, DRM — YouTube, Twitch, streaming services) open in a system WebView2 tab
/// instead, automatically. Everything else stays on Servo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompatConfig {
    pub enabled: bool,
    /// Registrable domains; subdomains match too ("youtube.com" covers "m.youtube.com").
    pub domains: Vec<String>,
    /// Which default list `domains` was last brought up to; sites added to the defaults
    /// later are appended to saved lists once (see [`COMPAT_DEFAULTS_ADDED`]).
    #[serde(default)]
    pub defaults_version: u32,
}

/// Sites added to the default list after the first release, by defaults version.
pub const COMPAT_DEFAULTS_ADDED: &[(u32, &[&str])] = &[
    // X: Google sign-in (an Identity Services iframe) does not run on Servo.
    (2, &["x.com", "twitter.com"]),
];

pub const COMPAT_DEFAULTS_VERSION: u32 = 2;

pub const DEFAULT_COMPAT_DOMAINS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "twitch.tv",
    "kick.com",
    "netflix.com",
    "primevideo.com",
    "disneyplus.com",
    "max.com",
    "globoplay.globo.com",
    "crunchyroll.com",
    "vimeo.com",
    "open.spotify.com",
    "tiktok.com",
    "x.com",
    "twitter.com",
];

impl Default for CompatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            domains: DEFAULT_COMPAT_DOMAINS.iter().map(|domain| (*domain).to_owned()).collect(),
            defaults_version: COMPAT_DEFAULTS_VERSION,
        }
    }
}

impl CompatConfig {
    /// Whether `url` should open in compatibility mode.
    pub fn applies_to(&self, url: &url::Url) -> bool {
        if !self.enabled || !matches!(url.scheme(), "http" | "https") {
            return false;
        }
        let Some(host) = url.host_str() else {
            return false;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.domains.iter().any(|domain| {
            let domain = domain.trim().trim_start_matches("*.").trim_end_matches('.').to_ascii_lowercase();
            !domain.is_empty() && (host == domain || host.ends_with(&format!(".{domain}")))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// Page opened by the Home button and for new windows.
    pub homepage: String,
    /// Page opened in new tabs.
    pub new_tab_url: String,
    /// Restore the previous session's tabs on startup.
    pub restore_session: bool,
    pub search_engine: SearchEngine,
    /// Maximum number of history entries kept on disk.
    pub history_limit: usize,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            homepage: "blanket:newtab".into(),
            new_tab_url: "blanket:newtab".into(),
            restore_session: true,
            search_engine: SearchEngine::default(),
            history_limit: 20_000,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchEngine {
    pub name: String,
    /// URL template; `{query}` is replaced by the percent-encoded search terms.
    pub template: String,
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self {
            name: "DuckDuckGo".into(),
            template: "https://duckduckgo.com/?q={query}".into(),
        }
    }
}

/// Bumped when the memory policy changes meaning; older saved values are replaced.
pub const MEMORY_POLICY_VERSION: u32 = 2;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Version of the memory policy these values were written for. Files from an older
    /// policy get the current defaults (see [`BlanketConfig::validated`]).
    #[serde(default)]
    pub policy_version: u32,
    /// Master switch for Smart Tab Suspension.
    pub suspension_enabled: bool,
    /// A background tab idle for this long becomes eligible for suspension under normal
    /// memory conditions.
    pub suspend_after_secs: u64,
    /// Never suspend a tab that was active more recently than this, even under critical
    /// pressure. Protects tabs the user is flipping between.
    pub min_idle_secs: u64,
    /// Below this much free system RAM (MiB) the browser is under *moderate* pressure.
    pub moderate_available_mib: u64,
    /// Below this much free system RAM (MiB) the browser is under *critical* pressure.
    pub critical_available_mib: u64,
    /// Rough cost of a live tab used to derive the adaptive live-tab budget before real
    /// measurements are available.
    pub estimated_tab_cost_mib: u64,
    /// Hard ceiling on simultaneously live tabs, `0` meaning "derive from available RAM".
    pub max_live_tabs: usize,
    /// How often system memory is sampled.
    pub sample_interval_secs: u64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            suspension_enabled: true,
            policy_version: MEMORY_POLICY_VERSION,
            // Like Chrome's Memory Saver ("balanced"): a tab sleeps after hours of
            // disuse, not minutes.
            suspend_after_secs: 4 * 60 * 60,
            min_idle_secs: 10 * 60,
            // Chromium's Windows thresholds.
            moderate_available_mib: 1000,
            critical_available_mib: 400,
            estimated_tab_cost_mib: 120,
            max_live_tabs: 0,
            sample_interval_secs: 10,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchedulerConfig {
    /// Frame interval used while the active page is animating.
    pub animation_frame_interval_ms: u64,
    /// Background maintenance (autosave, memory sampling) is postponed while the user
    /// interacted with the browser within this window.
    pub interaction_grace_ms: u64,
    /// Interval at which the session is autosaved.
    pub session_autosave_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            animation_frame_interval_ms: 16,
            interaction_grace_ms: 400,
            session_autosave_secs: 30,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    /// Memory budget for decoded favicons kept for the tab strip and history.
    pub favicon_budget_kib: u64,
    /// Memory budget for thumbnails of suspended tabs.
    pub thumbnail_budget_kib: u64,
    /// Drop Servo's in-memory HTTP cache when memory pressure becomes critical.
    pub clear_http_cache_on_critical: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            favicon_budget_kib: 2 * 1024,
            thumbnail_budget_kib: 24 * 1024,
            clear_http_cache_on_critical: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    /// Use `https://` for bare host names typed in the address bar.
    pub prefer_https_for_typed_hosts: bool,
    /// Ask before downloading executable / script file types.
    pub confirm_dangerous_downloads: bool,
    /// Ask before downloading anything over plain HTTP.
    pub confirm_insecure_downloads: bool,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            prefer_https_for_typed_hosts: true,
            confirm_dangerous_downloads: true,
            confirm_insecure_downloads: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PreloadConfig {
    /// Pre-resolve DNS for address-bar suggestions the user is very likely to pick.
    pub dns_prefetch: bool,
    /// Minimum number of past visits before a host is considered predictable.
    pub min_visits: u32,
    /// A host is not resolved again within this window.
    pub dedupe_secs: u64,
}

impl Default for PreloadConfig {
    fn default() -> Self {
        Self {
            dns_prefetch: true,
            min_visits: 3,
            dedupe_secs: 120,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    pub theme: ThemePreference,
    pub show_bookmarks_bar: bool,
    /// Show the live resource monitor in the status area.
    pub show_performance_hud: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: ThemePreference::Dark,
            show_bookmarks_bar: true,
            show_performance_hud: false,
        }
    }
}

impl BlanketConfig {
    /// Load the configuration, writing a commented default file on first run.
    pub fn load_or_create(path: &Path) -> CoreResult<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let config: Self = toml::from_str(&text).map_err(|error| CoreError::Parse {
                    path: path.to_owned(),
                    message: error.to_string(),
                })?;
                let migrated = config.memory.policy_version < MEMORY_POLICY_VERSION ||
                    config.compat.defaults_version < COMPAT_DEFAULTS_VERSION;
                let config = config.validated();
                if migrated {
                    if let Err(error) = config.save(path) {
                        warn!(%error, "could not save the migrated configuration");
                    }
                }
                Ok(config)
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let config = Self::default();
                if let Err(error) = config.save(path) {
                    warn!(%error, "could not write default configuration");
                }
                Ok(config)
            },
            Err(error) => Err(CoreError::io(path, error)),
        }
    }

    pub fn save(&self, path: &Path) -> CoreResult<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| CoreError::io(parent, error))?;
        }
        let text = toml::to_string_pretty(self).map_err(|error| CoreError::Serialize {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
        fs::write(path, text).map_err(|error| CoreError::io(path, error))
    }

    /// Clamp values into ranges where the managers behave sensibly. A typo in the config
    /// file must never be able to make the browser suspend the tab being read or spin the
    /// CPU.
    pub fn validated(mut self) -> Self {
        let compat = &mut self.compat;
        for (version, added) in COMPAT_DEFAULTS_ADDED {
            if compat.defaults_version < *version {
                for domain in *added {
                    if !compat.domains.iter().any(|existing| existing.eq_ignore_ascii_case(domain)) {
                        compat.domains.push((*domain).to_owned());
                    }
                }
            }
        }
        compat.defaults_version = compat.defaults_version.max(COMPAT_DEFAULTS_VERSION);

        let memory = &mut self.memory;
        if memory.policy_version < MEMORY_POLICY_VERSION {
            // The old policy released tabs after a minute of disuse on machines with a
            // normal amount of free RAM. Its values are replaced, not reinterpreted; only
            // the user's on/off choice is kept.
            *memory = MemoryConfig {
                suspension_enabled: memory.suspension_enabled,
                ..MemoryConfig::default()
            };
        }
        memory.min_idle_secs = memory.min_idle_secs.max(60);
        memory.suspend_after_secs = memory.suspend_after_secs.max(memory.min_idle_secs).max(15 * 60);
        memory.critical_available_mib = memory.critical_available_mib.clamp(64, 16_384);
        memory.moderate_available_mib = memory.moderate_available_mib.clamp(memory.critical_available_mib, 65_536);
        memory.estimated_tab_cost_mib = memory.estimated_tab_cost_mib.clamp(16, 4096);
        memory.sample_interval_secs = memory.sample_interval_secs.clamp(1, 600);

        let scheduler = &mut self.scheduler;
        scheduler.animation_frame_interval_ms = scheduler.animation_frame_interval_ms.clamp(4, 100);
        scheduler.interaction_grace_ms = scheduler.interaction_grace_ms.min(5_000);
        scheduler.session_autosave_secs = scheduler.session_autosave_secs.clamp(5, 3_600);

        if !self.general.search_engine.template.contains("{query}") {
            warn!("search engine template lacks {{query}}, using the default engine");
            self.general.search_engine = SearchEngine::default();
        }
        self.general.history_limit = self.general.history_limit.clamp(100, 1_000_000);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compat_mode_matches_domains_and_subdomains() {
        let url = |text: &str| url::Url::parse(text).unwrap();
        let mut compat = CompatConfig::default();
        assert!(compat.applies_to(&url("https://www.youtube.com/watch?v=x")));
        assert!(compat.applies_to(&url("https://m.youtube.com/")));
        assert!(compat.applies_to(&url("https://YouTu.be/x")));
        assert!(compat.applies_to(&url("https://www.twitch.tv/")));
        assert!(!compat.applies_to(&url("https://notyoutube.com/")));
        assert!(!compat.applies_to(&url("https://servo.org/")));
        assert!(!compat.applies_to(&url("blanket:newtab")));
        compat.enabled = false;
        assert!(!compat.applies_to(&url("https://www.youtube.com/")));
    }

    #[test]
    fn saved_compat_lists_receive_sites_added_to_the_defaults() {
        let old = "[compat]\nenabled = true\ndomains = [\"youtube.com\", \"my-site.example\"]\n";
        let config = toml::from_str::<BlanketConfig>(old).unwrap().validated();
        let domains = &config.compat.domains;
        assert!(domains.contains(&"my-site.example".to_owned()), "user entries kept");
        assert!(domains.contains(&"x.com".to_owned()) && domains.contains(&"twitter.com".to_owned()));
        assert_eq!(config.compat.defaults_version, COMPAT_DEFAULTS_VERSION);
        // Once up to date, a site the user removed is not added back.
        let mut current = config.clone();
        current.compat.domains.retain(|domain| domain != "x.com");
        let again = current.validated();
        assert!(!again.compat.domains.contains(&"x.com".to_owned()));
    }

    #[test]
    fn old_config_files_get_compat_defaults() {
        let config: BlanketConfig = toml::from_str("[ui]\ntheme = \"dark\"\n").unwrap();
        assert_eq!(config.compat, CompatConfig::default());
    }

    #[test]
    fn default_config_round_trips_through_toml() {
        let config = BlanketConfig::default();
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: BlanketConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn partial_files_fill_in_defaults() {
        let parsed: BlanketConfig = toml::from_str("[memory]\nsuspend_after_secs = 600\n").unwrap();
        assert_eq!(parsed.memory.suspend_after_secs, 600);
        assert_eq!(parsed.cache, CacheConfig::default());
    }

    #[test]
    fn old_memory_policy_files_are_migrated() {
        // What every profile created before the policy change has on disk.
        let old = "[memory]\nsuspension_enabled = false\nsuspend_after_secs = 1800\nmin_idle_secs = 60\n\
                   moderate_available_percent = 25.0\ncritical_available_percent = 12.0\n";
        let config = toml::from_str::<BlanketConfig>(old).unwrap().validated();
        assert_eq!(
            config.memory,
            MemoryConfig {
                suspension_enabled: false,
                ..MemoryConfig::default()
            }
        );
        // Current files keep the user's own values.
        let mut current = BlanketConfig::default();
        current.memory.suspend_after_secs = 2 * 60 * 60;
        let text = toml::to_string(&current).unwrap();
        let parsed = toml::from_str::<BlanketConfig>(&text).unwrap().validated();
        assert_eq!(parsed.memory.suspend_after_secs, 2 * 60 * 60);
    }

    #[test]
    fn validation_clamps_dangerous_values() {
        let mut config = BlanketConfig::default();
        config.memory.min_idle_secs = 0;
        config.memory.suspend_after_secs = 1;
        config.memory.moderate_available_mib = 100;
        config.memory.critical_available_mib = 900;
        config.scheduler.animation_frame_interval_ms = 0;
        config.general.search_engine.template = "https://example.com/".into();
        let config = config.validated();
        assert_eq!(config.memory.min_idle_secs, 60);
        assert_eq!(config.memory.suspend_after_secs, 15 * 60);
        assert!(config.memory.moderate_available_mib >= config.memory.critical_available_mib);
        assert_eq!(config.scheduler.animation_frame_interval_ms, 4);
        assert_eq!(config.general.search_engine, SearchEngine::default());
    }

    #[test]
    fn first_load_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blanket.toml");
        let config = BlanketConfig::load_or_create(&path).unwrap();
        assert!(path.exists());
        assert_eq!(BlanketConfig::load_or_create(&path).unwrap(), config);
    }
}
