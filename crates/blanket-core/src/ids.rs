use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Stable identifier of a browser tab. Unlike an engine `WebViewId` it survives tab
/// suspension: a suspended tab has no web view but keeps its `TabId`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TabId(pub u64);

/// Identifier of a download in the download manager.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DownloadId(pub u64);

impl fmt::Display for TabId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "tab#{}", self.0)
    }
}

impl fmt::Display for DownloadId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "download#{}", self.0)
    }
}

static NEXT_TAB_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_DOWNLOAD_ID: AtomicU64 = AtomicU64::new(1);

impl TabId {
    /// Allocate a process-unique tab id.
    pub fn next() -> Self {
        Self(NEXT_TAB_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl DownloadId {
    /// Allocate a process-unique download id.
    pub fn next() -> Self {
        Self(NEXT_DOWNLOAD_ID.fetch_add(1, Ordering::Relaxed))
    }

    /// Make sure ids allocated from now on are greater than `id`, so records loaded
    /// from disk never collide with new downloads.
    pub fn reserve_through(id: DownloadId) {
        NEXT_DOWNLOAD_ID.fetch_max(id.0.saturating_add(1), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_download_ids_are_skipped() {
        DownloadId::reserve_through(DownloadId(10_000));
        assert!(DownloadId::next().0 > 10_000);
    }
}
