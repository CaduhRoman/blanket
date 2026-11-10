//! Tab Manager: ordering, activation, lifecycle and recently-closed tabs.
//!
//! The manager tracks *browser-level* tab state. Whether a tab currently has an engine
//! web view is expressed by [`TabLifecycle`]; the application keeps the actual web view
//! handles keyed by [`TabId`].

use std::collections::VecDeque;
use std::time::Instant;

use url::Url;

use crate::ids::TabId;

/// Why a tab is not currently backed by a live web view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuspendReason {
    /// Background tab idle for longer than the configured threshold.
    Idle,
    /// The system is running low on memory.
    MemoryPressure,
    /// More tabs were live than the adaptive budget allows.
    LiveTabBudget,
    /// Restored from a previous session and not yet viewed (lazy restoration).
    SessionRestore,
    /// The user explicitly asked to suspend the tab.
    User,
}

/// Lifecycle of a tab's engine resources.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabLifecycle {
    /// Backed by a live web view.
    Live,
    /// Engine resources were released; the tab is recreated from its URL on activation.
    Suspended { since: Instant, reason: SuspendReason },
    /// The page's content process or pipeline crashed.
    Crashed { reason: String },
}

impl TabLifecycle {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live)
    }

    pub fn is_suspended(&self) -> bool {
        matches!(self, Self::Suspended { .. })
    }
}

/// A browser tab.
#[derive(Clone, Debug)]
pub struct Tab {
    pub id: TabId,
    pub url: Option<Url>,
    pub title: String,
    pub lifecycle: TabLifecycle,
    pub pinned: bool,
    pub loading: bool,
    /// The page reported an active media session (audio or video playing).
    pub media_playing: bool,
    /// Back/forward list as last reported by the engine.
    pub history: Vec<Url>,
    pub history_index: usize,
    /// Last time the tab was in the foreground (or now, if it is the active tab).
    pub last_active: Instant,
    pub created: Instant,
    /// The tab that opened this one, used to pick the next active tab on close.
    pub opener: Option<TabId>,
}

impl Tab {
    fn new(id: TabId, url: Option<Url>, now: Instant, opener: Option<TabId>) -> Self {
        Self {
            id,
            history: url.iter().cloned().collect(),
            history_index: 0,
            url,
            title: String::new(),
            lifecycle: TabLifecycle::Live,
            pinned: false,
            loading: false,
            media_playing: false,
            last_active: now,
            created: now,
            opener,
        }
    }

    /// Title for display: the page title, else the URL, else "New Tab".
    pub fn display_title(&self) -> &str {
        if !self.title.trim().is_empty() {
            return &self.title;
        }
        match &self.url {
            Some(url) if url.scheme() == "blanket" && url.path() == "newtab" => "New Tab",
            Some(url) => url.host_str().unwrap_or(url.as_str()),
            None => "New Tab",
        }
    }
}

/// Where to insert a newly opened tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenPosition {
    End,
    /// Right after the active tab (links opened in a new tab).
    AfterActive,
    At(usize),
}

/// A recently closed tab that can be reopened.
#[derive(Clone, Debug)]
pub struct ClosedTab {
    pub url: Option<Url>,
    pub title: String,
    pub history: Vec<Url>,
    pub history_index: usize,
    pub index: usize,
    pub pinned: bool,
}

const CLOSED_TABS_LIMIT: usize = 25;

#[derive(Debug, Default)]
pub struct TabManager {
    tabs: Vec<Tab>,
    active: Option<TabId>,
    closed: VecDeque<ClosedTab>,
}

/// Result of closing a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CloseOutcome {
    /// The tab that became active because the active tab was closed.
    pub newly_active: Option<TabId>,
    /// No tabs remain.
    pub now_empty: bool,
}

impl TabManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn ids(&self) -> impl Iterator<Item = TabId> + '_ {
        self.tabs.iter().map(|tab| tab.id)
    }

    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|tab| tab.id == id)
    }

    pub fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active
    }

    pub fn active(&self) -> Option<&Tab> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active.and_then(|id| self.index_of(id))
    }

    pub fn live_count(&self) -> usize {
        self.tabs.iter().filter(|tab| tab.lifecycle.is_live()).count()
    }

    pub fn suspended_count(&self) -> usize {
        self.tabs.iter().filter(|tab| tab.lifecycle.is_suspended()).count()
    }

    /// Open a new tab and return its id. The tab starts [`TabLifecycle::Live`]; callers
    /// that restore lazily mark it suspended right away.
    pub fn open(&mut self, url: Option<Url>, position: OpenPosition, activate: bool, now: Instant) -> TabId {
        let id = TabId::next();
        let opener = matches!(position, OpenPosition::AfterActive).then_some(self.active).flatten();
        let index = match position {
            OpenPosition::End => self.tabs.len(),
            OpenPosition::At(index) => index.min(self.tabs.len()),
            OpenPosition::AfterActive => self.after_active_insertion_index(),
        };
        self.tabs.insert(index, Tab::new(id, url, now, opener));
        if activate || self.active.is_none() {
            self.activate(id, now);
        }
        id
    }

    /// Tabs opened from the active tab are placed after it and after any tabs it
    /// previously opened, preserving the order in which they were opened.
    fn after_active_insertion_index(&self) -> usize {
        let Some(active_index) = self.active_index() else {
            return self.tabs.len();
        };
        let active = self.active;
        let mut index = active_index + 1;
        while index < self.tabs.len() && self.tabs[index].opener == active {
            index += 1;
        }
        index
    }

    /// Make `id` the foreground tab. Returns the previously active tab, if it changed.
    pub fn activate(&mut self, id: TabId, now: Instant) -> Option<TabId> {
        if self.get(id).is_none() || self.active == Some(id) {
            return None;
        }
        let previous = self.active.replace(id);
        if let Some(previous) = previous.and_then(|previous| self.get_mut(previous)) {
            previous.last_active = now;
        }
        if let Some(tab) = self.get_mut(id) {
            tab.last_active = now;
        }
        previous
    }

    /// Activate the tab at `index`, if it exists.
    pub fn activate_index(&mut self, index: usize, now: Instant) -> Option<TabId> {
        let id = self.tabs.get(index)?.id;
        self.activate(id, now);
        Some(id)
    }

    /// Activate the next (or previous, for negative `offset`) tab, wrapping around.
    pub fn cycle(&mut self, offset: isize, now: Instant) -> Option<TabId> {
        let len = self.tabs.len() as isize;
        if len == 0 {
            return None;
        }
        let current = self.active_index().unwrap_or(0) as isize;
        let index = (current + offset).rem_euclid(len) as usize;
        self.activate_index(index, now)
    }

    /// Close a tab, remembering it so it can be reopened.
    pub fn close(&mut self, id: TabId, now: Instant) -> Option<CloseOutcome> {
        let index = self.index_of(id)?;
        let tab = self.tabs.remove(index);
        self.closed.push_front(ClosedTab {
            url: tab.url.clone(),
            title: tab.title.clone(),
            history: tab.history.clone(),
            history_index: tab.history_index,
            index,
            pinned: tab.pinned,
        });
        self.closed.truncate(CLOSED_TABS_LIMIT);

        for other in &mut self.tabs {
            if other.opener == Some(id) {
                other.opener = tab.opener;
            }
        }

        let mut newly_active = None;
        if self.active == Some(id) {
            self.active = None;
            let next = self.choose_successor(&tab, index);
            if let Some(next) = next {
                self.activate(next, now);
                newly_active = Some(next);
            }
        }
        Some(CloseOutcome {
            newly_active,
            now_empty: self.tabs.is_empty(),
        })
    }

    fn choose_successor(&self, closed: &Tab, index: usize) -> Option<TabId> {
        // Returning to the opener feels natural after closing a tab opened from a link.
        if let Some(opener) = closed.opener.filter(|opener| self.get(*opener).is_some()) {
            let has_sibling = self.tabs.get(index).is_some_and(|tab| tab.opener == closed.opener);
            if !has_sibling {
                return Some(opener);
            }
        }
        self.tabs
            .get(index)
            .or_else(|| index.checked_sub(1).and_then(|index| self.tabs.get(index)))
            .map(|tab| tab.id)
    }

    /// Reopen the most recently closed tab. The tab is created live and activated.
    pub fn reopen_closed(&mut self, now: Instant) -> Option<TabId> {
        let closed = self.closed.pop_front()?;
        let id = self.open(closed.url.clone(), OpenPosition::At(closed.index), true, now);
        if let Some(tab) = self.get_mut(id) {
            tab.title = closed.title;
            tab.pinned = closed.pinned;
            if !closed.history.is_empty() {
                tab.history_index = closed.history_index.min(closed.history.len() - 1);
                tab.history = closed.history;
            }
        }
        Some(id)
    }

    pub fn recently_closed(&self) -> impl Iterator<Item = &ClosedTab> {
        self.closed.iter()
    }

    /// Move a tab to a new position.
    pub fn move_tab(&mut self, id: TabId, new_index: usize) -> bool {
        let Some(index) = self.index_of(id) else {
            return false;
        };
        let tab = self.tabs.remove(index);
        let new_index = new_index.min(self.tabs.len());
        self.tabs.insert(new_index, tab);
        true
    }

    pub fn set_lifecycle(&mut self, id: TabId, lifecycle: TabLifecycle) -> bool {
        match self.get_mut(id) {
            Some(tab) => {
                tab.lifecycle = lifecycle;
                true
            },
            None => false,
        }
    }

    pub fn set_pinned(&mut self, id: TabId, pinned: bool) {
        if let Some(tab) = self.get_mut(id) {
            tab.pinned = pinned;
        }
    }

    /// Record the engine's view of the tab's navigation history.
    pub fn set_history(&mut self, id: TabId, entries: Vec<Url>, current: usize) {
        if let Some(tab) = self.get_mut(id) {
            if let Some(url) = entries.get(current) {
                tab.url = Some(url.clone());
            }
            tab.history_index = current.min(entries.len().saturating_sub(1));
            tab.history = entries;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn url(text: &str) -> Option<Url> {
        Some(Url::parse(text).unwrap())
    }

    #[test]
    fn first_tab_is_activated_automatically() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let id = tabs.open(url("https://a.com"), OpenPosition::End, false, now);
        assert_eq!(tabs.active_id(), Some(id));
        assert_eq!(tabs.live_count(), 1);
    }

    #[test]
    fn link_tabs_open_after_active_in_order() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let a = tabs.open(url("https://a.com"), OpenPosition::End, true, now);
        let z = tabs.open(url("https://z.com"), OpenPosition::End, false, now);
        tabs.activate(a, now);
        let b = tabs.open(url("https://b.com"), OpenPosition::AfterActive, false, now);
        let c = tabs.open(url("https://c.com"), OpenPosition::AfterActive, false, now);
        let order: Vec<_> = tabs.ids().collect();
        assert_eq!(order, vec![a, b, c, z]);
        assert_eq!(tabs.active_id(), Some(a));
    }

    #[test]
    fn activation_updates_last_active_of_both_tabs() {
        let mut tabs = TabManager::new();
        let start = Instant::now();
        let a = tabs.open(url("https://a.com"), OpenPosition::End, true, start);
        let b = tabs.open(url("https://b.com"), OpenPosition::End, false, start);
        let later = start + Duration::from_secs(5);
        assert_eq!(tabs.activate(b, later), Some(a));
        assert_eq!(tabs.get(a).unwrap().last_active, later);
        assert_eq!(tabs.get(b).unwrap().last_active, later);
        assert_eq!(tabs.activate(b, later), None, "re-activating is a no-op");
    }

    #[test]
    fn closing_active_tab_prefers_right_neighbour() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let a = tabs.open(url("https://a.com"), OpenPosition::End, true, now);
        let b = tabs.open(url("https://b.com"), OpenPosition::End, true, now);
        let c = tabs.open(url("https://c.com"), OpenPosition::End, false, now);
        let outcome = tabs.close(b, now).unwrap();
        assert_eq!(outcome.newly_active, Some(c));
        let outcome = tabs.close(c, now).unwrap();
        assert_eq!(outcome.newly_active, Some(a));
        let outcome = tabs.close(a, now).unwrap();
        assert!(outcome.now_empty);
        assert_eq!(tabs.active_id(), None);
    }

    #[test]
    fn closing_link_tab_returns_to_opener() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let a = tabs.open(url("https://a.com"), OpenPosition::End, true, now);
        let _z = tabs.open(url("https://z.com"), OpenPosition::End, false, now);
        let child = tabs.open(url("https://child.com"), OpenPosition::AfterActive, true, now);
        let outcome = tabs.close(child, now).unwrap();
        assert_eq!(outcome.newly_active, Some(a));
    }

    #[test]
    fn closed_tabs_can_be_reopened_in_place() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let _a = tabs.open(url("https://a.com"), OpenPosition::End, true, now);
        let b = tabs.open(url("https://b.com"), OpenPosition::End, false, now);
        let _c = tabs.open(url("https://c.com"), OpenPosition::End, false, now);
        tabs.get_mut(b).unwrap().title = "Bee".into();
        tabs.close(b, now);
        let reopened = tabs.reopen_closed(now).unwrap();
        assert_eq!(tabs.index_of(reopened), Some(1));
        assert_eq!(tabs.get(reopened).unwrap().title, "Bee");
        assert_eq!(tabs.active_id(), Some(reopened));
        assert!(tabs.reopen_closed(now).is_none());
    }

    #[test]
    fn cycling_wraps_around() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let a = tabs.open(url("https://a.com"), OpenPosition::End, true, now);
        let b = tabs.open(url("https://b.com"), OpenPosition::End, false, now);
        assert_eq!(tabs.cycle(1, now), Some(b));
        assert_eq!(tabs.cycle(1, now), Some(a));
        assert_eq!(tabs.cycle(-1, now), Some(b));
    }

    #[test]
    fn display_title_falls_back_sensibly() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let a = tabs.open(url("https://www.servo.org/x"), OpenPosition::End, true, now);
        let n = tabs.open(url("blanket:newtab"), OpenPosition::End, false, now);
        assert_eq!(tabs.get(a).unwrap().display_title(), "www.servo.org");
        assert_eq!(tabs.get(n).unwrap().display_title(), "New Tab");
    }

    #[test]
    fn history_updates_current_url() {
        let mut tabs = TabManager::new();
        let now = Instant::now();
        let a = tabs.open(url("https://a.com"), OpenPosition::End, true, now);
        let entries = vec![Url::parse("https://a.com").unwrap(), Url::parse("https://a.com/2").unwrap()];
        tabs.set_history(a, entries, 1);
        assert_eq!(tabs.get(a).unwrap().url.as_ref().unwrap().as_str(), "https://a.com/2");
    }
}
