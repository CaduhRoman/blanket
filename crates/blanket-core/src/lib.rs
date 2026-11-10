//! Engine-independent core of the Blanket browser.
//!
//! Everything in this crate is deliberately free of any dependency on Servo, the window
//! system or the GPU. It contains the browser's *decisions* (which tab to suspend, when to
//! run background work, how to interpret what the user typed, what to persist) while the
//! `blanket` application crate carries them out against the real engine. Keeping the two
//! apart makes the policies unit-testable in milliseconds instead of requiring a full
//! engine build.

pub mod bookmarks;
pub mod cache;
pub mod config;
pub mod downloads;
pub mod error;
pub mod history;
pub mod ids;
pub mod memory;
pub mod omnibox;
pub mod paths;
pub mod preload;
pub mod scheduler;
pub mod security;
pub mod session;
pub mod storage;
pub mod sysmon;
pub mod tabs;
pub mod time;

pub use error::{CoreError, CoreResult};
pub use ids::{DownloadId, TabId};
