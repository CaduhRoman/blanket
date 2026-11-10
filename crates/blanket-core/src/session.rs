//! Session persistence: the open tabs, saved periodically and on exit so a crash or
//! restart loses at most one autosave interval.

use std::path::Path;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::CoreResult;
use crate::storage::{load_json_or_default, save_json_atomic};
use crate::tabs::{OpenPosition, SuspendReason, TabLifecycle, TabManager};

/// Current on-disk format version.
const SESSION_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionTab {
    pub url: Url,
    pub title: String,
    pub pinned: bool,
    #[serde(default)]
    pub history: Vec<Url>,
    #[serde(default)]
    pub history_index: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub tabs: Vec<SessionTab>,
    pub active_index: Option<usize>,
    /// Whether the browser exited cleanly. A `false` value on startup means the previous
    /// run crashed.
    #[serde(default)]
    pub clean_exit: bool,
}

impl Session {
    pub fn load(path: &Path) -> CoreResult<Self> {
        let session: Self = load_json_or_default(path)?;
        Ok(session.sanitized())
    }

    pub fn save(&self, path: &Path) -> CoreResult<()> {
        save_json_atomic(path, self)
    }

    /// Snapshot the current tabs. Tabs without a URL are skipped.
    pub fn capture(tabs: &TabManager, clean_exit: bool) -> Self {
        let mut active_index = None;
        let mut session_tabs = Vec::with_capacity(tabs.len());
        for tab in tabs.tabs() {
            let Some(url) = tab.url.clone() else {
                continue;
            };
            if Some(tab.id) == tabs.active_id() {
                active_index = Some(session_tabs.len());
            }
            session_tabs.push(SessionTab {
                url,
                title: tab.title.clone(),
                pinned: tab.pinned,
                history: tab.history.clone(),
                history_index: tab.history_index,
            });
        }
        Self {
            version: SESSION_VERSION,
            tabs: session_tabs,
            active_index,
            clean_exit,
        }
    }

    fn sanitized(mut self) -> Self {
        for tab in &mut self.tabs {
            if tab.history.is_empty() {
                tab.history.push(tab.url.clone());
                tab.history_index = 0;
            }
            tab.history_index = tab.history_index.min(tab.history.len() - 1);
        }
        if self.active_index.is_some_and(|index| index >= self.tabs.len()) {
            self.active_index = (!self.tabs.is_empty()).then_some(0);
        }
        self
    }

    /// Restore into `tabs` using lazy initialization: only the active tab is created
    /// live; every other tab is restored suspended and loads when first activated.
    pub fn restore_into(&self, tabs: &mut TabManager, now: Instant) {
        let active_index = self.active_index.unwrap_or(0);
        for (index, saved) in self.tabs.iter().enumerate() {
            let activate = index == active_index;
            let id = tabs.open(Some(saved.url.clone()), OpenPosition::End, activate, now);
            if let Some(tab) = tabs.get_mut(id) {
                tab.title = saved.title.clone();
                tab.pinned = saved.pinned;
                tab.history = saved.history.clone();
                tab.history_index = saved.history_index;
            }
            if !activate {
                tabs.set_lifecycle(
                    id,
                    TabLifecycle::Suspended {
                        since: now,
                        reason: SuspendReason::SessionRestore,
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    #[test]
    fn capture_and_lazy_restore() {
        let now = Instant::now();
        let mut tabs = TabManager::new();
        tabs.open(Some(url("https://a.com/")), OpenPosition::End, true, now);
        let b = tabs.open(Some(url("https://b.com/")), OpenPosition::End, true, now);
        tabs.open(Some(url("https://c.com/")), OpenPosition::End, false, now);
        tabs.set_pinned(b, true);

        let session = Session::capture(&tabs, true);
        assert_eq!(session.tabs.len(), 3);
        assert_eq!(session.active_index, Some(1));

        let mut restored = TabManager::new();
        session.restore_into(&mut restored, now);
        assert_eq!(restored.len(), 3);
        assert_eq!(restored.live_count(), 1, "only the active tab is created eagerly");
        let active = restored.active().unwrap();
        assert_eq!(active.url.as_ref().unwrap().as_str(), "https://b.com/");
        assert!(active.pinned);
    }

    #[test]
    fn sanitizes_inconsistent_files() {
        let session = Session {
            version: 1,
            tabs: vec![SessionTab {
                url: url("https://a.com/"),
                title: String::new(),
                pinned: false,
                history: vec![],
                history_index: 7,
            }],
            active_index: Some(4),
            clean_exit: false,
        }
        .sanitized();
        assert_eq!(session.active_index, Some(0));
        assert_eq!(session.tabs[0].history.len(), 1);
        assert_eq!(session.tabs[0].history_index, 0);
    }

    #[test]
    fn persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        let mut tabs = TabManager::new();
        tabs.open(Some(url("https://a.com/")), OpenPosition::End, true, Instant::now());
        let session = Session::capture(&tabs, false);
        session.save(&path).unwrap();
        assert_eq!(Session::load(&path).unwrap(), session);
    }
}
