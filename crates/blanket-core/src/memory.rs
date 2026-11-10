//! Memory Manager policy: memory pressure detection, adaptive live-tab budget and
//! Smart Tab Suspension planning.
//!
//! The policy only *plans*. The application executes the plan against the engine and,
//! before actually releasing a tab, double-checks with the page that no unsaved user
//! input would be lost (see `blanket::suspension`). A suspended tab keeps its URL,
//! title, back/forward list and a thumbnail, and is recreated on activation.

use std::time::{Duration, Instant};

use crate::config::MemoryConfig;
use crate::ids::TabId;
use crate::tabs::{SuspendReason, TabManager};

/// System-wide memory figures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SystemMemory {
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl SystemMemory {
    pub fn available_percent(&self) -> f32 {
        if self.total_bytes == 0 {
            return 100.0;
        }
        (self.available_bytes as f64 / self.total_bytes as f64 * 100.0) as f32
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemoryPressure {
    Normal,
    Moderate,
    Critical,
}

/// Hysteresis applied when pressure decreases, so the level does not flap when
/// available memory hovers around a threshold.
const HYSTERESIS_MIB: u64 = 150;
const MIB: u64 = 1024 * 1024;

/// Tracks the current memory pressure level with hysteresis.
#[derive(Clone, Debug)]
pub struct PressureTracker {
    level: MemoryPressure,
}

impl Default for PressureTracker {
    fn default() -> Self {
        Self {
            level: MemoryPressure::Normal,
        }
    }
}

impl PressureTracker {
    pub fn level(&self) -> MemoryPressure {
        self.level
    }

    /// Update with a new sample and return the (possibly unchanged) level.
    ///
    /// Pressure is judged on *absolute* free memory, like Chromium's Windows monitor
    /// (moderate below ~1 GB, critical below ~400 MB). A percentage would put an 8 GB
    /// machine — where Windows routinely keeps only 15–20% free — under permanent
    /// "pressure" and make the browser throw tabs away for nothing.
    pub fn update(&mut self, config: &MemoryConfig, memory: SystemMemory) -> MemoryPressure {
        let available = memory.available_bytes / MIB;
        let critical = config.critical_available_mib;
        let moderate = config.moderate_available_mib;
        let raw = classify(available, critical, moderate);
        self.level = if raw >= self.level {
            raw
        } else {
            // Only step down once we are clearly past the threshold.
            classify(available.saturating_sub(HYSTERESIS_MIB), critical, moderate).max(raw)
        };
        self.level
    }
}

fn classify(available_mib: u64, critical: u64, moderate: u64) -> MemoryPressure {
    if available_mib < critical {
        MemoryPressure::Critical
    } else if available_mib < moderate {
        MemoryPressure::Moderate
    } else {
        MemoryPressure::Normal
    }
}

/// What the policy needs to know about one tab.
#[derive(Clone, Debug)]
pub struct TabActivity {
    pub id: TabId,
    pub is_active: bool,
    pub is_live: bool,
    pub pinned: bool,
    pub loading: bool,
    pub media_playing: bool,
    pub idle_for: Duration,
    /// Measured or estimated memory attributed to this tab.
    pub estimated_bytes: Option<u64>,
}

impl TabActivity {
    pub fn collect(tabs: &TabManager, now: Instant) -> Vec<Self> {
        let active = tabs.active_id();
        tabs.tabs()
            .iter()
            .map(|tab| TabActivity {
                id: tab.id,
                is_active: Some(tab.id) == active,
                is_live: tab.lifecycle.is_live(),
                pinned: tab.pinned,
                loading: tab.loading,
                media_playing: tab.media_playing,
                idle_for: if Some(tab.id) == active {
                    Duration::ZERO
                } else {
                    now.saturating_duration_since(tab.last_active)
                },
                estimated_bytes: None,
            })
            .collect()
    }
}

/// An action requested by the memory policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryAction {
    /// Release the engine resources of a background tab.
    SuspendTab { id: TabId, reason: SuspendReason },
    /// Shrink in-browser caches (thumbnails, favicons).
    TrimBrowserCaches,
    /// Drop the engine's in-memory HTTP cache.
    ClearHttpCache,
}

/// Bounds on the adaptive live-tab budget.
const MIN_LIVE_TABS: usize = 3;
const MAX_LIVE_TABS: usize = 64;

#[derive(Clone, Debug)]
pub struct MemoryPolicy {
    config: MemoryConfig,
}

impl MemoryPolicy {
    pub fn new(config: MemoryConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    /// How many tabs may be awake at once: only a ceiling the user set explicitly. There
    /// is no automatic cap — tabs are released for being idle for a long time or, one
    /// at a time, when the system is really short of memory (see [`Self::plan`]).
    pub fn live_tab_budget(&self) -> Option<usize> {
        (self.config.max_live_tabs > 0).then(|| self.config.max_live_tabs.clamp(MIN_LIVE_TABS, MAX_LIVE_TABS))
    }

    /// Compute the actions for this sampling round, in the spirit of Chrome's Memory
    /// Saver: reloading a tab the user comes back to (losing a feed, a scroll position,
    /// a half-read thread) is expensive, so it is a last resort.
    ///
    /// - Any pressure: tabs idle for `suspend_after_secs` (hours by default) sleep.
    /// - Moderate pressure: only the browser's own caches are trimmed.
    /// - Critical pressure: additionally, the single least recently used tab sleeps per
    ///   sampling round, never one used in the last `min_idle_secs`.
    /// - A user-set `max_live_tabs` puts the least recently used tabs over it to sleep.
    pub fn plan(&self, pressure: MemoryPressure, budget: Option<usize>, tabs: &[TabActivity]) -> Vec<MemoryAction> {
        let mut actions = Vec::new();
        match pressure {
            MemoryPressure::Normal => {},
            MemoryPressure::Moderate => actions.push(MemoryAction::TrimBrowserCaches),
            MemoryPressure::Critical => {
                actions.push(MemoryAction::TrimBrowserCaches);
                actions.push(MemoryAction::ClearHttpCache);
            },
        }
        if !self.config.suspension_enabled {
            return actions;
        }

        let min_idle = Duration::from_secs(self.config.min_idle_secs);
        let long_idle = Duration::from_secs(self.config.suspend_after_secs);

        // Candidates in least-recently-used order.
        let mut candidates: Vec<&TabActivity> = tabs.iter().filter(|tab| is_suspendable(tab, min_idle)).collect();
        candidates.sort_by(|a, b| b.idle_for.cmp(&a.idle_for));

        let live = tabs.iter().filter(|tab| tab.is_live).count();
        let mut over_budget = budget.map_or(0, |budget| live.saturating_sub(budget));
        let mut critical_release = pressure == MemoryPressure::Critical;

        for tab in candidates {
            let reason = if tab.idle_for >= long_idle {
                SuspendReason::Idle
            } else if over_budget > 0 {
                SuspendReason::LiveTabBudget
            } else if critical_release {
                critical_release = false;
                SuspendReason::MemoryPressure
            } else {
                continue;
            };
            over_budget = over_budget.saturating_sub(1);
            actions.push(MemoryAction::SuspendTab { id: tab.id, reason });
        }
        actions
    }
}

/// Tabs that must never be suspended automatically: the foreground tab, pinned tabs,
/// tabs still loading, tabs playing media and tabs the user just left.
fn is_suspendable(tab: &TabActivity, min_idle: Duration) -> bool {
    tab.is_live && !tab.is_active && !tab.pinned && !tab.loading && !tab.media_playing && tab.idle_for >= min_idle
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    fn tab(id: u64, idle_secs: u64) -> TabActivity {
        TabActivity {
            id: TabId(id),
            is_active: false,
            is_live: true,
            pinned: false,
            loading: false,
            media_playing: false,
            idle_for: Duration::from_secs(idle_secs),
            estimated_bytes: None,
        }
    }

    fn suspended_ids(actions: &[MemoryAction]) -> Vec<u64> {
        actions
            .iter()
            .filter_map(|action| match action {
                MemoryAction::SuspendTab { id, .. } => Some(id.0),
                _ => None,
            })
            .collect()
    }

    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;

    fn free_mib(mib: u64) -> SystemMemory {
        SystemMemory {
            total_bytes: 8 * GIB,
            available_bytes: mib * MIB,
        }
    }

    #[test]
    fn pressure_uses_absolute_free_memory_with_hysteresis() {
        let config = MemoryConfig::default();
        let mut tracker = PressureTracker::default();
        // 1.4 GB free on an 8 GB machine (17%) is a normal Windows day, not pressure.
        assert_eq!(tracker.update(&config, free_mib(1400)), MemoryPressure::Normal);
        assert_eq!(tracker.update(&config, free_mib(800)), MemoryPressure::Moderate);
        assert_eq!(tracker.update(&config, free_mib(300)), MemoryPressure::Critical);
        // Just above the critical threshold, but within the hysteresis band.
        assert_eq!(tracker.update(&config, free_mib(450)), MemoryPressure::Critical);
        assert_eq!(tracker.update(&config, free_mib(600)), MemoryPressure::Moderate);
        assert_eq!(tracker.update(&config, free_mib(1100)), MemoryPressure::Moderate);
        assert_eq!(tracker.update(&config, free_mib(1200)), MemoryPressure::Normal);
    }

    #[test]
    fn no_automatic_cap_on_awake_tabs() {
        assert_eq!(MemoryPolicy::new(MemoryConfig::default()).live_tab_budget(), None);
        let config = MemoryConfig {
            max_live_tabs: 5,
            ..MemoryConfig::default()
        };
        assert_eq!(MemoryPolicy::new(config).live_tab_budget(), Some(5));
    }

    #[test]
    fn protected_tabs_are_never_suspended() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let mut active = tab(1, 10 * HOUR);
        active.is_active = true;
        let mut pinned = tab(2, 10 * HOUR);
        pinned.pinned = true;
        let mut loading = tab(3, 10 * HOUR);
        loading.loading = true;
        let mut playing = tab(4, 10 * HOUR);
        playing.media_playing = true;
        let recent = tab(5, 5);
        let mut already = tab(6, 10 * HOUR);
        already.is_live = false;
        let tabs = [active, pinned, loading, playing, recent, already];
        let actions = policy.plan(MemoryPressure::Critical, Some(1), &tabs);
        assert!(suspended_ids(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn only_long_idle_tabs_sleep_normally_and_under_moderate_pressure() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let tabs = [tab(1, 5 * HOUR), tab(2, HOUR), tab(3, 20 * MIN)];
        for pressure in [MemoryPressure::Normal, MemoryPressure::Moderate] {
            let actions = policy.plan(pressure, None, &tabs);
            assert_eq!(suspended_ids(&actions), vec![1], "{pressure:?}");
            assert!(!actions.contains(&MemoryAction::ClearHttpCache));
        }
    }

    #[test]
    fn critical_pressure_releases_one_tab_at_a_time() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let tabs = [tab(1, 20 * MIN), tab(2, 50 * MIN), tab(3, 15 * MIN), tab(4, 2 * MIN)];
        let actions = policy.plan(MemoryPressure::Critical, None, &tabs);
        assert!(actions.contains(&MemoryAction::ClearHttpCache));
        assert!(actions.contains(&MemoryAction::TrimBrowserCaches));
        // The least recently used one; the tab used 2 minutes ago is protected.
        assert_eq!(suspended_ids(&actions), vec![2]);
    }

    #[test]
    fn user_ceiling_suspends_least_recently_used_first() {
        let config = MemoryConfig {
            max_live_tabs: 3,
            ..MemoryConfig::default()
        };
        let policy = MemoryPolicy::new(config);
        let mut active = tab(1, 0);
        active.is_active = true;
        let tabs = [active, tab(2, 15 * MIN), tab(3, 90 * MIN), tab(4, 30 * MIN), tab(5, 12 * MIN)];
        let actions = policy.plan(MemoryPressure::Normal, policy.live_tab_budget(), &tabs);
        assert_eq!(suspended_ids(&actions), vec![3, 4]);
    }

    #[test]
    fn disabled_suspension_only_trims_caches() {
        let config = MemoryConfig {
            suspension_enabled: false,
            ..MemoryConfig::default()
        };
        let policy = MemoryPolicy::new(config);
        let actions = policy.plan(MemoryPressure::Critical, None, &[tab(1, 10 * HOUR)]);
        assert!(suspended_ids(&actions).is_empty());
        assert!(actions.contains(&MemoryAction::ClearHttpCache));
    }
}
