//! Predictive Preloading.
//!
//! Prediction is evidence-based and deliberately conservative: a host is warmed only
//! when the address bar's top suggestion is a host the user has visited repeatedly (or
//! typed before) and the typed text is a prefix of it. The warm-up is a DNS resolution,
//! which costs one small UDP exchange and no page memory; the OS resolver cache then
//! saves the lookup when the navigation actually happens. Pages are never prerendered
//! in the background, as that would multiply memory and network use for guesses.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::config::PreloadConfig;
use crate::history::HistoryEntry;

#[derive(Debug)]
pub struct Predictor {
    config: PreloadConfig,
    recently_warmed: HashMap<String, Instant>,
}

/// Upper bound on the dedupe table so it cannot grow without limit.
const MAX_TRACKED_HOSTS: usize = 256;

impl Predictor {
    pub fn new(config: PreloadConfig) -> Self {
        Self {
            config,
            recently_warmed: HashMap::new(),
        }
    }

    /// Given the typed text and the best history suggestion, return a host worth
    /// resolving now, if any.
    pub fn host_to_warm(&mut self, typed: &str, top_suggestion: Option<&HistoryEntry>, now: Instant) -> Option<String> {
        if !self.config.dns_prefetch {
            return None;
        }
        let typed = typed.trim().to_ascii_lowercase();
        if typed.len() < 2 || typed.contains(char::is_whitespace) {
            return None;
        }
        let entry = top_suggestion?;
        if !matches!(entry.url.scheme(), "http" | "https") {
            return None;
        }
        let host = entry.url.host_str()?.to_ascii_lowercase();
        let bare_host = host.trim_start_matches("www.");
        let typed_host = typed
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_start_matches("www.");
        if !bare_host.starts_with(typed_host) && !typed_host.starts_with(bare_host) {
            return None;
        }
        let confident = entry.typed_count >= 1 || entry.visit_count >= self.config.min_visits;
        if !confident {
            return None;
        }

        let dedupe = Duration::from_secs(self.config.dedupe_secs);
        if let Some(last) = self.recently_warmed.get(&host) {
            if now.saturating_duration_since(*last) < dedupe {
                return None;
            }
        }
        if self.recently_warmed.len() >= MAX_TRACKED_HOSTS {
            self.recently_warmed
                .retain(|_, warmed| now.saturating_duration_since(*warmed) < dedupe);
        }
        self.recently_warmed.insert(host.clone(), now);
        Some(host)
    }
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::*;

    fn entry(url: &str, visits: u32, typed: u32) -> HistoryEntry {
        HistoryEntry {
            url: Url::parse(url).unwrap(),
            title: String::new(),
            visit_count: visits,
            typed_count: typed,
            first_visit: 0,
            last_visit: 0,
        }
    }

    #[test]
    fn warms_frequently_visited_matching_host_once() {
        let mut predictor = Predictor::new(PreloadConfig::default());
        let now = Instant::now();
        let github = entry("https://www.github.com/servo", 10, 0);
        assert_eq!(predictor.host_to_warm("git", Some(&github), now), Some("www.github.com".into()));
        assert_eq!(predictor.host_to_warm("gith", Some(&github), now), None, "deduplicated");
        let later = now + Duration::from_secs(PreloadConfig::default().dedupe_secs + 1);
        assert!(predictor.host_to_warm("gith", Some(&github), later).is_some());
    }

    #[test]
    fn requires_evidence() {
        let mut predictor = Predictor::new(PreloadConfig::default());
        let now = Instant::now();
        assert_eq!(predictor.host_to_warm("ex", Some(&entry("https://example.com", 1, 0)), now), None);
        assert!(predictor.host_to_warm("ex", Some(&entry("https://example.com", 1, 1)), now).is_some());
    }

    #[test]
    fn ignores_non_matching_or_search_input() {
        let mut predictor = Predictor::new(PreloadConfig::default());
        let now = Instant::now();
        let github = entry("https://github.com", 10, 5);
        assert_eq!(predictor.host_to_warm("servo", Some(&github), now), None);
        assert_eq!(predictor.host_to_warm("git hub", Some(&github), now), None);
        assert_eq!(predictor.host_to_warm("g", Some(&github), now), None);
        assert_eq!(predictor.host_to_warm("git", None, now), None);
    }

    #[test]
    fn can_be_disabled() {
        let config = PreloadConfig {
            dns_prefetch: false,
            ..PreloadConfig::default()
        };
        let mut predictor = Predictor::new(config);
        let github = entry("https://github.com", 10, 5);
        assert_eq!(predictor.host_to_warm("git", Some(&github), Instant::now()), None);
    }
}
