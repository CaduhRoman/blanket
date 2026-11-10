//! Browsing history with frecency-ranked search for address bar suggestions.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CoreResult;
use crate::storage::{load_json_or_default, save_json_atomic};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub url: Url,
    pub title: String,
    pub visit_count: u32,
    /// Visits started by typing the address (strong signal for prediction).
    pub typed_count: u32,
    pub first_visit: u64,
    pub last_visit: u64,
}

impl HistoryEntry {
    /// Frequency weighted by recency, similar in spirit to Firefox's "frecency".
    pub fn frecency(&self, now: u64) -> f64 {
        let age_days = now.saturating_sub(self.last_visit) as f64 / 86_400.0;
        let recency_weight = match age_days {
            age if age < 1.0 => 4.0,
            age if age < 7.0 => 2.0,
            age if age < 30.0 => 1.0,
            age if age < 90.0 => 0.5,
            _ => 0.2,
        };
        (self.visit_count as f64 + 2.0 * self.typed_count as f64) * recency_weight
    }
}

/// Persistent browsing history keyed by URL.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct History {
    entries: HashMap<String, HistoryEntry>,
    #[serde(skip)]
    dirty: bool,
}

/// An inline address bar completion: the full text to show (typed part + completed
/// part) and where Enter goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Autocomplete {
    pub text: String,
    pub url: Url,
}

fn starts_with_ignore_ascii_case(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len() && text.is_char_boundary(prefix.len()) && text[..prefix.len()].eq_ignore_ascii_case(prefix)
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    starts_with_ignore_ascii_case(text, prefix).then(|| &text[prefix.len()..])
}

/// Only these schemes are recorded. Internal pages and `data:` URLs would clutter the
/// history and suggestions.
fn is_recordable(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "file")
}

fn key_for(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.into()
}

impl History {
    pub fn load(path: &Path) -> CoreResult<Self> {
        load_json_or_default(path)
    }

    /// Persist if anything changed since the last save.
    pub fn save_if_dirty(&mut self, path: &Path) -> CoreResult<bool> {
        if !self.dirty {
            return Ok(false);
        }
        save_json_atomic(path, self)?;
        self.dirty = false;
        Ok(true)
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Record a visit. Returns `false` for URLs that are not recorded.
    pub fn record_visit(&mut self, url: &Url, typed: bool, now: u64) -> bool {
        if !is_recordable(url) {
            return false;
        }
        let entry = self.entries.entry(key_for(url)).or_insert_with(|| HistoryEntry {
            url: url.clone(),
            title: String::new(),
            visit_count: 0,
            typed_count: 0,
            first_visit: now,
            last_visit: now,
        });
        entry.visit_count = entry.visit_count.saturating_add(1);
        if typed {
            entry.typed_count = entry.typed_count.saturating_add(1);
        }
        entry.last_visit = now;
        self.dirty = true;
        true
    }

    pub fn set_title(&mut self, url: &Url, title: &str) {
        if let Some(entry) = self.entries.get_mut(&key_for(url)) {
            if entry.title != title {
                entry.title = title.to_owned();
                self.dirty = true;
            }
        }
    }

    pub fn get(&self, url: &Url) -> Option<&HistoryEntry> {
        self.entries.get(&key_for(url))
    }

    pub fn remove(&mut self, url: &Url) -> bool {
        let removed = self.entries.remove(&key_for(url)).is_some();
        self.dirty |= removed;
        removed
    }

    /// Remove visits newer than `since` (unix seconds), e.g. "clear last hour".
    pub fn clear_since(&mut self, since: u64) {
        let before = self.entries.len();
        self.entries.retain(|_, entry| entry.last_visit < since);
        self.dirty |= before != self.entries.len();
    }

    pub fn clear(&mut self) {
        self.dirty |= !self.entries.is_empty();
        self.entries.clear();
    }

    /// Most recent entries first.
    pub fn recent(&self, limit: usize) -> Vec<&HistoryEntry> {
        let mut entries: Vec<&HistoryEntry> = self.entries.values().collect();
        entries.sort_by(|a, b| b.last_visit.cmp(&a.last_visit));
        entries.truncate(limit);
        entries
    }

    /// Entries matching every whitespace-separated term of `query` in the URL or title,
    /// best matches first. Prefix matches on the host rank highest because that is what
    /// the user is usually typing.
    pub fn search(&self, query: &str, limit: usize, now: u64) -> Vec<&HistoryEntry> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if terms.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(f64, &HistoryEntry)> = self
            .entries
            .values()
            .filter_map(|entry| {
                let url = entry.url.as_str().to_lowercase();
                let title = entry.title.to_lowercase();
                if !terms.iter().all(|term| url.contains(term) || title.contains(term)) {
                    return None;
                }
                let host = entry.url.host_str().unwrap_or_default().trim_start_matches("www.");
                let prefix_bonus = if host.starts_with(terms[0].as_str()) { 10.0 } else { 1.0 };
                Some((entry.frecency(now) * prefix_bonus, entry))
            })
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored.into_iter().take(limit).map(|(_, entry)| entry).collect()
    }

    /// Inline completion for address bar input, like Firefox's autofill: "you" completes
    /// to "youtube.com" (the most frecent host starting with the input) and, once the
    /// input has a path, "github.com/se" completes to the most frecent URL starting with
    /// it. The typed part keeps the user's casing. `www.` and the scheme are optional.
    pub fn autocomplete(&self, input: &str, now: u64) -> Option<Autocomplete> {
        if input.is_empty() || input.contains(char::is_whitespace) {
            return None;
        }
        // A typed scheme is fine ("https://you…"), as long as it is a web one.
        let rest = ["https://", "http://"]
            .iter()
            .find_map(|scheme| strip_prefix_ignore_ascii_case(input, scheme))
            .unwrap_or(input);
        if rest.is_empty() || rest.contains("://") {
            return None;
        }
        let typed_www = strip_prefix_ignore_ascii_case(rest, "www.").is_some();
        let web = self.entries.values().filter(|entry| matches!(entry.url.scheme(), "http" | "https"));

        if !rest.contains('/') {
            // Host completion: rank hosts by the frecency of all their pages.
            let mut hosts: HashMap<String, (f64, &HistoryEntry)> = HashMap::new();
            for entry in web {
                let Some(host) = entry.url.host_str() else {
                    continue;
                };
                let host = match entry.url.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host.to_owned(),
                };
                let score = entry.frecency(now);
                let slot = hosts.entry(host).or_insert((0.0, entry));
                slot.0 += score;
                if score > slot.1.frecency(now) {
                    slot.1 = entry;
                }
            }
            let (host, (_, entry)) = hosts
                .iter()
                .filter(|(host, _)| {
                    let shown = if typed_www { host.as_str() } else { host.trim_start_matches("www.") };
                    starts_with_ignore_ascii_case(shown, rest)
                })
                .max_by(|a, b| a.1.0.total_cmp(&b.1.0).then_with(|| b.0.len().cmp(&a.0.len())))?;
            let shown = if typed_www { host.as_str() } else { host.trim_start_matches("www.") };
            let url = Url::parse(&format!("{}://{host}/", entry.url.scheme())).ok()?;
            return Some(Autocomplete {
                text: format!("{input}{}", &shown[rest.len()..]),
                url,
            });
        }

        // URL completion once a path is being typed.
        let (entry, shown) = web
            .filter_map(|entry| {
                let full = entry.url.as_str();
                let without_scheme = full.split_once("://").map_or(full, |(_, rest)| rest);
                let shown = if typed_www { without_scheme } else { without_scheme.trim_start_matches("www.") };
                starts_with_ignore_ascii_case(shown, rest).then_some((entry, shown))
            })
            .max_by(|a, b| a.0.frecency(now).total_cmp(&b.0.frecency(now)).then_with(|| b.1.len().cmp(&a.1.len())))?;
        Some(Autocomplete {
            text: format!("{input}{}", &shown[rest.len()..]),
            url: entry.url.clone(),
        })
    }

    /// The most frecent entry of each host, best first, for "top sites" tiles.
    pub fn top_sites(&self, limit: usize, now: u64) -> Vec<&HistoryEntry> {
        let mut best: HashMap<&str, (f64, &HistoryEntry)> = HashMap::new();
        for entry in self.entries.values() {
            let Some(host) = entry.url.host_str().filter(|host| !host.is_empty()) else {
                continue;
            };
            let score = entry.frecency(now);
            let slot = best.entry(host).or_insert((score, entry));
            if score > slot.0 {
                *slot = (score, entry);
            }
        }
        let mut sites: Vec<(f64, &HistoryEntry)> = best.into_values().collect();
        sites.sort_by(|a, b| b.0.total_cmp(&a.0));
        sites.into_iter().take(limit).map(|(_, entry)| entry).collect()
    }

    /// Keep at most `limit` entries, dropping the lowest-frecency ones.
    pub fn prune(&mut self, limit: usize, now: u64) {
        if self.entries.len() <= limit {
            return;
        }
        let mut scored: Vec<(f64, String)> = self
            .entries
            .iter()
            .map(|(key, entry)| (entry.frecency(now), key.clone()))
            .collect();
        scored.sort_by(|a, b| a.0.total_cmp(&b.0));
        let excess = self.entries.len() - limit;
        for (_, key) in scored.into_iter().take(excess) {
            self.entries.remove(&key);
        }
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn visits_accumulate_and_ignore_fragments() {
        let mut history = History::default();
        history.record_visit(&url("https://a.com/page#one"), false, 10);
        history.record_visit(&url("https://a.com/page#two"), true, 20);
        assert_eq!(history.len(), 1);
        let entry = history.get(&url("https://a.com/page")).unwrap();
        assert_eq!((entry.visit_count, entry.typed_count, entry.last_visit), (2, 1, 20));
    }

    #[test]
    fn internal_pages_are_not_recorded() {
        let mut history = History::default();
        assert!(!history.record_visit(&url("blanket:newtab"), false, 0));
        assert!(!history.record_visit(&url("data:text/html,x"), false, 0));
        assert!(history.is_empty());
        assert!(!history.is_dirty());
    }

    fn completion(history: &History, input: &str, now: u64) -> Option<(String, String)> {
        history.autocomplete(input, now).map(|done| (done.text, done.url.to_string()))
    }

    #[test]
    fn autocomplete_completes_frequent_hosts() {
        let now = 400 * DAY;
        let mut history = History::default();
        for _ in 0..5 {
            history.record_visit(&url("https://www.youtube.com/watch?v=1"), false, now);
        }
        history.record_visit(&url("https://www.youtube.com/"), true, now);
        history.record_visit(&url("https://yourdictionary.com/x"), false, now - 60 * DAY);

        assert_eq!(
            completion(&history, "you", now),
            Some(("youtube.com".into(), "https://www.youtube.com/".into()))
        );
        // The user's casing is kept for the typed part.
        assert_eq!(completion(&history, "YouT", now).unwrap().0, "YouTube.com");
        // Typing "www." or a scheme still completes.
        assert_eq!(completion(&history, "www.yo", now).unwrap().0, "www.youtube.com");
        assert_eq!(completion(&history, "https://you", now).unwrap().0, "https://youtube.com");
        // A less frecent host wins once the input only matches it.
        assert_eq!(completion(&history, "your", now).unwrap().0, "yourdictionary.com");
    }

    #[test]
    fn autocomplete_completes_urls_once_a_path_is_typed() {
        let now = 400 * DAY;
        let mut history = History::default();
        for _ in 0..3 {
            history.record_visit(&url("https://github.com/servo/servo"), true, now);
        }
        history.record_visit(&url("https://github.com/rust-lang/rust"), false, now);
        assert_eq!(
            completion(&history, "github.com/se", now),
            Some(("github.com/servo/servo".into(), "https://github.com/servo/servo".into()))
        );
    }

    #[test]
    fn autocomplete_ignores_non_matches_and_searches() {
        let now = 400 * DAY;
        let mut history = History::default();
        history.record_visit(&url("https://youtube.com/"), false, now);
        assert_eq!(completion(&history, "tube", now), None);
        assert_eq!(completion(&history, "you tube", now), None);
        assert_eq!(completion(&history, "", now), None);
        assert_eq!(completion(&history, "ftp://you", now), None);
    }

    #[test]
    fn search_ranks_host_prefix_and_frecency() {
        let now = 400 * DAY;
        let mut history = History::default();
        for _ in 0..3 {
            history.record_visit(&url("https://github.com/servo/servo"), true, now);
        }
        history.record_visit(&url("https://example.com/about-github"), false, now);
        history.record_visit(&url("https://old.github.io/"), false, now - 200 * DAY);
        let results = history.search("git", 10, now);
        assert_eq!(results[0].url.as_str(), "https://github.com/servo/servo");
        assert_eq!(results.len(), 3);
        assert!(history.search("servo github", 10, now).len() == 1);
        assert!(history.search("   ", 10, now).is_empty());
    }

    #[test]
    fn titles_are_searchable() {
        let mut history = History::default();
        let page = url("https://a.com/x");
        history.record_visit(&page, false, 0);
        history.set_title(&page, "The Servo Book");
        assert_eq!(history.search("book", 5, 0).len(), 1);
    }

    #[test]
    fn prune_drops_lowest_frecency() {
        let now = 365 * DAY;
        let mut history = History::default();
        history.record_visit(&url("https://keep.com"), true, now);
        history.record_visit(&url("https://stale.com"), false, now - 300 * DAY);
        history.record_visit(&url("https://also-keep.com"), false, now);
        history.prune(2, now);
        assert_eq!(history.len(), 2);
        assert!(history.get(&url("https://stale.com")).is_none());
    }

    #[test]
    fn top_sites_pick_one_entry_per_host() {
        let now = 10 * DAY;
        let mut history = History::default();
        for _ in 0..5 {
            history.record_visit(&url("https://a.com/inbox"), false, now);
        }
        history.record_visit(&url("https://a.com/settings"), false, now);
        for _ in 0..2 {
            history.record_visit(&url("https://b.com/"), false, now);
        }
        history.record_visit(&url("file:///C:/notes.txt"), false, now);
        let top = history.top_sites(10, now);
        let urls: Vec<&str> = top.iter().map(|entry| entry.url.as_str()).collect();
        assert_eq!(urls, vec!["https://a.com/inbox", "https://b.com/"]);
        assert_eq!(history.top_sites(1, now).len(), 1);
    }

    #[test]
    fn clear_since_removes_recent_entries_only() {
        let mut history = History::default();
        history.record_visit(&url("https://old.com"), false, 100);
        history.record_visit(&url("https://new.com"), false, 500);
        history.clear_since(300);
        assert!(history.get(&url("https://old.com")).is_some());
        assert!(history.get(&url("https://new.com")).is_none());
    }

    #[test]
    fn persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.json");
        let mut history = History::default();
        history.record_visit(&url("https://a.com"), true, 42);
        assert!(history.save_if_dirty(&path).unwrap());
        assert!(!history.save_if_dirty(&path).unwrap());
        let loaded = History::load(&path).unwrap();
        assert_eq!(loaded.get(&url("https://a.com")).unwrap().typed_count, 1);
    }
}
