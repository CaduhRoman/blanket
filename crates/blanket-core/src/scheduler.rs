//! CPU Scheduler: decides *when* the browser's own work runs.
//!
//! Two pieces live here:
//!
//! * [`TaskScheduler`] runs browser maintenance (session autosave, memory sampling,
//!   history flushes) at deadlines, coalescing duplicates and postponing work while the
//!   user is interacting so it never competes with input handling or painting.
//! * [`FramePacer`] decides how the UI event loop waits: block indefinitely when
//!   nothing is animating (zero idle CPU), or wake at the frame interval only while the
//!   *foreground* page animates. Background tabs never drive the frame loop.
//!
//! Web content's own timers are scheduled by the engine and are not touched here: pages
//! that legitimately run in the background (music, chat) keep working.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

/// Relative importance of a maintenance task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TaskPriority {
    /// May be postponed while the user is interacting.
    Background,
    /// Runs at its deadline even during interaction (e.g. reacting to memory pressure).
    Urgent,
}

#[derive(Clone, Debug)]
struct ScheduledTask {
    due: Instant,
    priority: TaskPriority,
    /// Re-arm automatically after running.
    repeat: Option<Duration>,
}

/// Deadline-based scheduler for browser maintenance tasks identified by a key.
///
/// Scheduling a key that is already pending keeps a single entry (with the earlier
/// deadline and higher priority), so bursts of "save the session" requests collapse into
/// one write.
#[derive(Debug)]
pub struct TaskScheduler<K: Eq + Hash + Clone> {
    tasks: HashMap<K, ScheduledTask>,
    interaction_grace: Duration,
    last_interaction: Option<Instant>,
    /// Upper bound on how long background work may be postponed by interaction.
    max_postpone: Duration,
}

impl<K: Eq + Hash + Clone> TaskScheduler<K> {
    pub fn new(interaction_grace: Duration) -> Self {
        Self {
            tasks: HashMap::new(),
            interaction_grace,
            last_interaction: None,
            max_postpone: Duration::from_secs(10),
        }
    }

    /// Note that the user just interacted with the browser (input event).
    pub fn note_interaction(&mut self, now: Instant) {
        self.last_interaction = Some(now);
    }

    fn user_is_busy(&self, now: Instant) -> bool {
        self.last_interaction
            .is_some_and(|last| now.saturating_duration_since(last) < self.interaction_grace)
    }

    /// Schedule `key` to run at `due` once.
    pub fn schedule(&mut self, key: K, due: Instant, priority: TaskPriority) {
        self.insert(key, due, priority, None);
    }

    /// Schedule `key` to run every `interval`, first at `now + interval`.
    pub fn schedule_repeating(&mut self, key: K, now: Instant, interval: Duration, priority: TaskPriority) {
        self.insert(key, now + interval, priority, Some(interval));
    }

    fn insert(&mut self, key: K, due: Instant, priority: TaskPriority, repeat: Option<Duration>) {
        self.tasks
            .entry(key)
            .and_modify(|task| {
                task.due = task.due.min(due);
                task.priority = task.priority.max(priority);
                if repeat.is_some() {
                    task.repeat = repeat;
                }
            })
            .or_insert(ScheduledTask { due, priority, repeat });
    }

    pub fn cancel(&mut self, key: &K) {
        self.tasks.remove(key);
    }

    pub fn is_scheduled(&self, key: &K) -> bool {
        self.tasks.contains_key(key)
    }

    /// The earliest instant at which [`Self::take_due`] could return work.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let busy_until = self
            .last_interaction
            .filter(|_| self.user_is_busy(now))
            .map(|last| last + self.interaction_grace);
        self.tasks
            .values()
            .map(|task| match (task.priority, busy_until) {
                (TaskPriority::Background, Some(busy_until)) => {
                    task.due.max(busy_until).min(task.due + self.max_postpone)
                },
                _ => task.due,
            })
            .min()
    }

    /// Remove and return the tasks that should run now, most urgent first.
    pub fn take_due(&mut self, now: Instant) -> Vec<K> {
        let busy = self.user_is_busy(now);
        let max_postpone = self.max_postpone;
        let mut due: Vec<(K, TaskPriority, Instant)> = self
            .tasks
            .iter()
            .filter(|(_, task)| {
                task.due <= now &&
                    (task.priority == TaskPriority::Urgent ||
                        !busy ||
                        now.saturating_duration_since(task.due) >= max_postpone)
            })
            .map(|(key, task)| (key.clone(), task.priority, task.due))
            .collect();
        due.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));

        for (key, _, _) in &due {
            let repeat = self.tasks.get(key).and_then(|task| task.repeat);
            match repeat {
                Some(interval) => {
                    if let Some(task) = self.tasks.get_mut(key) {
                        task.due = now + interval;
                    }
                },
                None => {
                    self.tasks.remove(key);
                },
            }
        }
        due.into_iter().map(|(key, _, _)| key).collect()
    }
}

/// How the UI event loop should wait for the next event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitMode {
    /// Sleep until an OS event or an engine wake-up arrives.
    Indefinitely,
    /// Wake at the given instant at the latest.
    Until(Instant),
}

/// Frame pacing for the browser window.
#[derive(Debug)]
pub struct FramePacer {
    frame_interval: Duration,
    last_frame: Option<Instant>,
    redraw_pending: bool,
}

impl FramePacer {
    pub fn new(frame_interval: Duration) -> Self {
        Self {
            frame_interval,
            last_frame: None,
            redraw_pending: false,
        }
    }

    pub fn frame_interval(&self) -> Duration {
        self.frame_interval
    }

    /// The engine or UI asked for a redraw. Multiple requests before the next frame are
    /// coalesced into one. Returns `true` if the caller should ask the window system for
    /// a redraw (i.e. one is not already pending).
    pub fn request_redraw(&mut self) -> bool {
        !std::mem::replace(&mut self.redraw_pending, true)
    }

    /// A frame was painted.
    pub fn frame_presented(&mut self, now: Instant) {
        self.last_frame = Some(now);
        self.redraw_pending = false;
    }

    /// Decide how to wait. `foreground_animating` is true only when the *active* tab
    /// (or the browser UI itself) has running animations.
    pub fn wait_mode(&self, now: Instant, foreground_animating: bool, next_task: Option<Instant>) -> WaitMode {
        let next_frame = foreground_animating.then(|| {
            self.last_frame
                .map_or(now, |last| (last + self.frame_interval).max(now))
        });
        match (next_frame, next_task) {
            (Some(frame), Some(task)) => WaitMode::Until(frame.min(task)),
            (Some(at), None) | (None, Some(at)) => WaitMode::Until(at),
            (None, None) => WaitMode::Indefinitely,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum Task {
        Save,
        Sample,
        Purge,
    }

    #[test]
    fn duplicate_requests_coalesce() {
        let now = Instant::now();
        let mut scheduler = TaskScheduler::new(Duration::ZERO);
        scheduler.schedule(Task::Save, now + Duration::from_secs(5), TaskPriority::Background);
        scheduler.schedule(Task::Save, now + Duration::from_secs(2), TaskPriority::Background);
        scheduler.schedule(Task::Save, now + Duration::from_secs(9), TaskPriority::Background);
        assert_eq!(scheduler.next_deadline(now), Some(now + Duration::from_secs(2)));
        assert_eq!(scheduler.take_due(now + Duration::from_secs(2)), vec![Task::Save]);
        assert!(scheduler.take_due(now + Duration::from_secs(10)).is_empty());
    }

    #[test]
    fn background_work_yields_to_interaction() {
        let now = Instant::now();
        let mut scheduler = TaskScheduler::new(Duration::from_millis(400));
        scheduler.schedule(Task::Save, now, TaskPriority::Background);
        scheduler.schedule(Task::Purge, now, TaskPriority::Urgent);
        scheduler.note_interaction(now);
        assert_eq!(scheduler.take_due(now), vec![Task::Purge]);
        assert_eq!(scheduler.next_deadline(now), Some(now + Duration::from_millis(400)));
        assert_eq!(scheduler.take_due(now + Duration::from_millis(400)), vec![Task::Save]);
    }

    #[test]
    fn postponement_is_bounded() {
        let start = Instant::now();
        let mut scheduler = TaskScheduler::new(Duration::from_millis(400));
        scheduler.schedule(Task::Save, start, TaskPriority::Background);
        let mut now = start;
        // The user never stops typing.
        while now < start + Duration::from_secs(9) {
            scheduler.note_interaction(now);
            assert!(scheduler.take_due(now).is_empty());
            now += Duration::from_millis(100);
        }
        let late = start + Duration::from_secs(10);
        scheduler.note_interaction(late);
        assert_eq!(scheduler.take_due(late), vec![Task::Save]);
    }

    #[test]
    fn repeating_tasks_rearm() {
        let now = Instant::now();
        let mut scheduler = TaskScheduler::new(Duration::ZERO);
        scheduler.schedule_repeating(Task::Sample, now, Duration::from_secs(10), TaskPriority::Background);
        assert!(scheduler.take_due(now).is_empty());
        let first = now + Duration::from_secs(10);
        assert_eq!(scheduler.take_due(first), vec![Task::Sample]);
        assert_eq!(scheduler.next_deadline(first), Some(first + Duration::from_secs(10)));
    }

    #[test]
    fn urgent_tasks_run_first() {
        let now = Instant::now();
        let mut scheduler = TaskScheduler::new(Duration::ZERO);
        scheduler.schedule(Task::Save, now, TaskPriority::Background);
        scheduler.schedule(Task::Purge, now + Duration::from_millis(1), TaskPriority::Urgent);
        let order = scheduler.take_due(now + Duration::from_millis(1));
        assert_eq!(order, vec![Task::Purge, Task::Save]);
    }

    #[test]
    fn idle_browser_sleeps_indefinitely() {
        let now = Instant::now();
        let pacer = FramePacer::new(Duration::from_millis(16));
        assert_eq!(pacer.wait_mode(now, false, None), WaitMode::Indefinitely);
    }

    #[test]
    fn animation_paces_frames() {
        let now = Instant::now();
        let mut pacer = FramePacer::new(Duration::from_millis(16));
        pacer.frame_presented(now);
        assert_eq!(
            pacer.wait_mode(now, true, None),
            WaitMode::Until(now + Duration::from_millis(16))
        );
        let task = now + Duration::from_millis(5);
        assert_eq!(pacer.wait_mode(now, true, Some(task)), WaitMode::Until(task));
        assert_eq!(pacer.wait_mode(now, false, Some(task)), WaitMode::Until(task));
    }

    #[test]
    fn redraw_requests_coalesce_until_presented() {
        let mut pacer = FramePacer::new(Duration::from_millis(16));
        assert!(pacer.request_redraw());
        assert!(!pacer.request_redraw());
        pacer.frame_presented(Instant::now());
        assert!(pacer.request_redraw());
    }
}
