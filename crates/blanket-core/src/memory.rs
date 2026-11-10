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

/// Percentage points of hysteresis applied when pressure decreases, so the level does
/// not flap when available memory hovers around a threshold.
const HYSTERESIS_PERCENT: f32 = 3.0;

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
    pub fn update(&mut self, config: &MemoryConfig, memory: SystemMemory) -> MemoryPressure {
        let available = memory.available_percent();
        let critical = config.critical_available_percent;
        let moderate = config.moderate_available_percent;
        let raw = classify(available, critical, moderate);
        self.level = if raw >= self.level {
            raw
        } else {
            // Only step down once we are clearly past the threshold.
            classify(available - HYSTERESIS_PERCENT, critical, moderate).max(raw)
        };
        self.level
    }
}

fn classify(available_percent: f32, critical: f32, moderate: f32) -> MemoryPressure {
    if available_percent < critical {
        MemoryPressure::Critical
    } else if available_percent < moderate {
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

    /// Adaptive Memory Allocation: how many tabs may be live at once.
    ///
    /// Derived from the memory that is still available plus what live tabs already use,
    /// so the budget shrinks as the rest of the system consumes RAM and grows on large
    /// machines. A configured `max_live_tabs` acts as a ceiling.
    pub fn live_tab_budget(&self, memory: SystemMemory, pressure: MemoryPressure, live_tab_bytes: u64) -> usize {
        let per_tab = self.config.estimated_tab_cost_mib * 1024 * 1024;
        // Leave headroom for the rest of the system: only plan to use a fraction of what
        // is available.
        let usable_fraction = match pressure {
            MemoryPressure::Normal => 0.5,
            MemoryPressure::Moderate => 0.3,
            MemoryPressure::Critical => 0.1,
        };
        let usable = (memory.available_bytes as f64 * usable_fraction) as u64 + live_tab_bytes;
        let mut budget = (usable / per_tab.max(1)) as usize;
        budget = budget.clamp(MIN_LIVE_TABS, MAX_LIVE_TABS);
        if self.config.max_live_tabs > 0 {
            budget = budget.min(self.config.max_live_tabs.max(1));
        }
        budget
    }

    /// Compute the actions for this sampling round.
    pub fn plan(&self, pressure: MemoryPressure, budget: usize, tabs: &[TabActivity]) -> Vec<MemoryAction> {
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
        let idle_threshold = match pressure {
            MemoryPressure::Normal => Duration::from_secs(self.config.suspend_after_secs),
            MemoryPressure::Moderate => Duration::from_secs(self.config.suspend_after_secs / 4).max(min_idle),
            MemoryPressure::Critical => min_idle,
        };

        // Candidates in least-recently-used order.
        let mut candidates: Vec<&TabActivity> = tabs.iter().filter(|tab| is_suspendable(tab, min_idle)).collect();
        candidates.sort_by(|a, b| b.idle_for.cmp(&a.idle_for));

        let live = tabs.iter().filter(|tab| tab.is_live).count();
        let mut over_budget = live.saturating_sub(budget);
        let reason_for_idle = if pressure == MemoryPressure::Normal {
            SuspendReason::Idle
        } else {
            SuspendReason::MemoryPressure
        };

        for tab in candidates {
            if over_budget > 0 {
                over_budget -= 1;
                actions.push(MemoryAction::SuspendTab {
                    id: tab.id,
                    reason: SuspendReason::LiveTabBudget,
                });
            } else if tab.idle_for >= idle_threshold {
                actions.push(MemoryAction::SuspendTab {
                    id: tab.id,
                    reason: reason_for_idle,
                });
            }
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

    fn memory(total_gib: u64, available_gib_tenths: u64) -> SystemMemory {
        SystemMemory {
            total_bytes: total_gib * GIB,
            available_bytes: available_gib_tenths * GIB / 10,
        }
    }

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

    #[test]
    fn pressure_levels_follow_thresholds_with_hysteresis() {
        let config = MemoryConfig::default();
        let mut tracker = PressureTracker::default();
        assert_eq!(tracker.update(&config, memory(10, 50)), MemoryPressure::Normal);
        assert_eq!(tracker.update(&config, memory(10, 20)), MemoryPressure::Moderate);
        assert_eq!(tracker.update(&config, memory(10, 10)), MemoryPressure::Critical);
        // 13% is above the 12% threshold but within the hysteresis band.
        assert_eq!(tracker.update(&config, memory(10, 13)), MemoryPressure::Critical);
        assert_eq!(tracker.update(&config, memory(10, 16)), MemoryPressure::Moderate);
        assert_eq!(tracker.update(&config, memory(10, 26)), MemoryPressure::Moderate);
        assert_eq!(tracker.update(&config, memory(10, 30)), MemoryPressure::Normal);
    }

    #[test]
    fn budget_adapts_to_available_memory() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let big = policy.live_tab_budget(memory(32, 200), MemoryPressure::Normal, 0);
        let small = policy.live_tab_budget(memory(8, 25), MemoryPressure::Moderate, 0);
        let tiny = policy.live_tab_budget(memory(8, 5), MemoryPressure::Critical, 0);
        assert!(big > small, "{big} > {small}");
        assert!(small >= tiny);
        assert_eq!(tiny, MIN_LIVE_TABS);
        assert!(big <= MAX_LIVE_TABS);
    }

    #[test]
    fn configured_ceiling_is_respected() {
        let config = MemoryConfig {
            max_live_tabs: 5,
            ..MemoryConfig::default()
        };
        let policy = MemoryPolicy::new(config);
        assert_eq!(policy.live_tab_budget(memory(64, 600), MemoryPressure::Normal, 0), 5);
    }

    #[test]
    fn protected_tabs_are_never_suspended() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let mut active = tab(1, 10_000);
        active.is_active = true;
        let mut pinned = tab(2, 10_000);
        pinned.pinned = true;
        let mut loading = tab(3, 10_000);
        loading.loading = true;
        let mut playing = tab(4, 10_000);
        playing.media_playing = true;
        let recent = tab(5, 5);
        let mut already = tab(6, 10_000);
        already.is_live = false;
        let tabs = [active, pinned, loading, playing, recent, already];
        let actions = policy.plan(MemoryPressure::Critical, 1, &tabs);
        assert!(suspended_ids(&actions).is_empty(), "{actions:?}");
    }

    #[test]
    fn idle_tabs_are_suspended_under_normal_pressure() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let tabs = [tab(1, 60 * 60), tab(2, 120)];
        let actions = policy.plan(MemoryPressure::Normal, 10, &tabs);
        assert_eq!(suspended_ids(&actions), vec![1]);
        assert!(!actions.contains(&MemoryAction::ClearHttpCache));
    }

    #[test]
    fn budget_overflow_suspends_least_recently_used_first() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let mut active = tab(1, 0);
        active.is_active = true;
        let tabs = [active, tab(2, 100), tab(3, 900), tab(4, 300)];
        let actions = policy.plan(MemoryPressure::Normal, 2, &tabs);
        assert_eq!(suspended_ids(&actions), vec![3, 4]);
    }

    #[test]
    fn critical_pressure_clears_caches_and_lowers_threshold() {
        let policy = MemoryPolicy::new(MemoryConfig::default());
        let tabs = [tab(1, 90), tab(2, 30)];
        let actions = policy.plan(MemoryPressure::Critical, 10, &tabs);
        assert!(actions.contains(&MemoryAction::ClearHttpCache));
        assert!(actions.contains(&MemoryAction::TrimBrowserCaches));
        assert_eq!(suspended_ids(&actions), vec![1]);
    }

    #[test]
    fn disabled_suspension_only_trims_caches() {
        let config = MemoryConfig {
            suspension_enabled: false,
            ..MemoryConfig::default()
        };
        let policy = MemoryPolicy::new(config);
        let actions = policy.plan(MemoryPressure::Critical, 1, &[tab(1, 10_000)]);
        assert!(suspended_ids(&actions).is_empty());
        assert!(actions.contains(&MemoryAction::ClearHttpCache));
    }
}
