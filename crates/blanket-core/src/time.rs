use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, used for persisted timestamps (history, bookmarks).
/// Monotonic `Instant`s are used for everything that is only meaningful in-process.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
