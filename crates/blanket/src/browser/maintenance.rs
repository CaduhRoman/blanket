//! Background maintenance driven by the CPU scheduler: resource sampling, the memory
//! policy, and persistence. All of it runs on the UI thread between events, is
//! postponed while the user is interacting, and never wakes the loop when nothing is due.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use blanket_core::cache::{CacheStats, HttpCachePolicy};
use blanket_core::memory::{MemoryAction, MemoryPressure, SystemMemory, TabActivity};
use blanket_core::scheduler::TaskPriority;
use blanket_core::session::Session;
use blanket_core::storage::save_json_atomic;
use blanket_core::time::unix_now;
use serde_json::json;
use tracing::{error, info};

use super::Browser;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Task {
    SampleResources,
    SaveSession,
    SaveHistory,
    SaveDownloads,
    SavePermissions,
    RefreshNewTab,
}

const PERF_HISTORY: usize = 120;
/// Faster sampling while the performance panel is visible.
const LIVE_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Figures shown in the performance panel and HUD.
#[derive(Clone, Debug, Default)]
pub struct PerfSnapshot {
    pub process_rss: Option<u64>,
    pub process_cpu: Option<f32>,
    pub system: Option<SystemMemory>,
    pub pressure: Option<MemoryPressure>,
    pub live_budget: Option<usize>,
    pub http_cache_entries: Option<usize>,
    pub favicon_cache: CacheStats,
    pub thumbnail_cache: CacheStats,
    /// Recent (resident bytes, cpu percent) samples, oldest first.
    pub samples: VecDeque<(u64, f32)>,
    pub last_sample: Option<Instant>,
}

impl Browser {
    pub fn run_due_tasks(&mut self) {
        let now = Instant::now();
        if self.gui.as_ref().and_then(crate::ui::Gui::next_repaint).is_some_and(|at| at <= now) {
            self.request_redraw();
        }
        for task in self.scheduler.take_due(now) {
            match task {
                Task::SampleResources => self.sample_resources(),
                Task::SaveSession => self.save_session(),
                Task::SaveHistory => self.save_history(),
                Task::SaveDownloads => {
                    if let Err(error) = self.downloads.save(&self.paths.downloads_log()) {
                        error!(%error, "could not save the download list");
                    }
                },
                Task::SavePermissions => {
                    if let Err(error) = save_json_atomic(&self.paths.permissions(), &self.permissions) {
                        error!(%error, "could not save site permissions");
                    }
                },
                Task::RefreshNewTab => self.refresh_newtab_data(),
            }
        }
        if self.gui.as_ref().is_some_and(|gui| gui.wants_live_performance()) {
            let due = self.perf.last_sample.is_none_or(|last| last.elapsed() >= LIVE_SAMPLE_INTERVAL);
            if due {
                self.sample_resources();
            }
            if let Some(last) = self.perf.last_sample {
                self.scheduler
                    .schedule(Task::SampleResources, last + LIVE_SAMPLE_INTERVAL, TaskPriority::Background);
            }
        }
    }

    /// Note user input so background work yields to it.
    pub(super) fn note_interaction(&mut self) {
        self.scheduler.note_interaction(Instant::now());
    }

    pub(super) fn sample_resources(&mut self) {
        let now = Instant::now();
        let memory = self.monitor.memory();
        let process = self.monitor.process();
        self.perf.last_sample = Some(now);
        self.perf.system = memory;
        self.perf.process_rss = process.map(|usage| usage.resident_bytes);
        self.perf.process_cpu = process.map(|usage| usage.cpu_percent);
        if let Some(usage) = process {
            self.perf.samples.push_back((usage.resident_bytes, usage.cpu_percent));
            while self.perf.samples.len() > PERF_HISTORY {
                self.perf.samples.pop_front();
            }
        }

        let previous_pressure = self.pressure.level();
        if let Some(memory) = memory {
            let pressure = self.pressure.update(&self.config.memory, memory);
            if pressure != previous_pressure {
                info!(?previous_pressure, ?pressure, available_percent = memory.available_percent(), "memory pressure changed");
            }
            let live_bytes = process.map_or(0, |usage| usage.resident_bytes);
            let budget = self.policy.live_tab_budget(memory, pressure, live_bytes);
            self.perf.pressure = Some(pressure);
            self.perf.live_budget = Some(budget);
            let activity = TabActivity::collect(&self.tabs, now);
            let pressure_rose = pressure > previous_pressure;
            for action in self.policy.plan(pressure, budget, &activity) {
                match action {
                    MemoryAction::SuspendTab { id, reason } => self.request_suspend(id, reason),
                    // Cache trimming reacts to a pressure *increase*; repeating it every
                    // sample under sustained pressure would only churn.
                    MemoryAction::TrimBrowserCaches if pressure_rose => {
                        let keep = if pressure == MemoryPressure::Critical { 0.0 } else { 0.5 };
                        self.gui_mut().trim_caches(keep);
                    },
                    MemoryAction::ClearHttpCache if pressure_rose => {
                        let policy = HttpCachePolicy {
                            clear_on_critical: self.config.cache.clear_http_cache_on_critical,
                        };
                        if policy.should_clear(pressure) {
                            if let Some(servo) = &self.servo {
                                info!("clearing the engine's HTTP cache under critical memory pressure");
                                servo.network_manager().clear_cache();
                            }
                        }
                    },
                    MemoryAction::TrimBrowserCaches | MemoryAction::ClearHttpCache => {},
                }
            }
        }

        let gui_wants_details = self.gui.as_ref().is_some_and(|gui| gui.wants_live_performance());
        if gui_wants_details {
            self.perf.http_cache_entries = self.servo.as_ref().map(|servo| servo.network_manager().cache_entries().len());
        }
        if let Some(gui) = &self.gui {
            let (favicons, thumbnails) = gui.cache_stats();
            self.perf.favicon_cache = favicons;
            self.perf.thumbnail_cache = thumbnails;
        }
        if self.metrics.enabled() {
            self.metrics.record(
                "resource_sample",
                json!({
                    "rss": self.perf.process_rss,
                    "cpu": self.perf.process_cpu,
                    "live_tabs": self.tabs.live_count(),
                    "suspended_tabs": self.tabs.suspended_count(),
                    "available_percent": memory.map(|memory| memory.available_percent()),
                }),
            );
        }
        if gui_wants_details || self.config.ui.show_performance_hud {
            self.request_redraw();
        }
    }

    fn save_session(&mut self) {
        if !self.session_dirty {
            return;
        }
        match Session::capture(&self.tabs, false).save(&self.paths.session()) {
            Ok(()) => self.session_dirty = false,
            Err(error) => error!(%error, "could not save the session"),
        }
    }

    fn save_history(&mut self) {
        self.history.prune(self.config.general.history_limit, unix_now());
        if let Err(error) = self.history.save_if_dirty(&self.paths.history()) {
            error!(%error, "could not save history");
        }
    }

    /// Flush everything that may be dirty (used on exit).
    pub(super) fn save_all_state(&mut self) {
        self.save_history();
        if let Err(error) = self.downloads.save(&self.paths.downloads_log()) {
            error!(%error, "could not save the download list");
        }
        if let Err(error) = save_json_atomic(&self.paths.permissions(), &self.permissions) {
            error!(%error, "could not save site permissions");
        }
    }
}
