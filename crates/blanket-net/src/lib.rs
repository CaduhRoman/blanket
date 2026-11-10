//! Off-main-thread networking for Blanket.
//!
//! Servo performs all page loads with its own network stack. This crate covers the
//! browser features that Servo does not provide to embedders:
//!
//! * **Downloads**: Servo has no download manager API, so files are fetched here with
//!   `reqwest` (rustls + the operating system's certificate verifier), forwarding the
//!   page's cookies so authenticated downloads work.
//! * **DNS pre-resolution** for Predictive Preloading.
//!
//! The Tokio runtime is created lazily on first use (Lazy Initialization): a session in
//! which nothing is downloaded never starts these threads.

mod download;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use blanket_core::config::SecurityConfig;
use blanket_core::ids::DownloadId;
use blanket_core::security::DownloadRisk;
use crossbeam_channel::{Receiver, Sender};
use tokio::runtime::Runtime;
use tokio::sync::{oneshot, watch};
use tracing::{debug, error};
use url::Url;

/// Callback used to wake the UI event loop when an event is available.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Everything needed to start a download.
#[derive(Clone, Debug)]
pub struct DownloadRequest {
    pub id: DownloadId,
    pub url: Url,
    pub directory: PathBuf,
    /// `Cookie` header value for the URL, taken from the engine's cookie store.
    pub cookie_header: Option<String>,
    pub referrer: Option<Url>,
    pub user_agent: String,
    pub security: SecurityConfig,
}

/// Progress notifications sent to the UI thread.
#[derive(Clone, Debug, PartialEq)]
pub enum DownloadEvent {
    /// Response headers arrived and the download needs user confirmation.
    AwaitingConfirmation { id: DownloadId, file_name: String, risk: DownloadRisk },
    /// The transfer started, writing to `destination`.
    Started { id: DownloadId, file_name: String, destination: PathBuf, total: Option<u64> },
    Progress { id: DownloadId, received: u64, total: Option<u64> },
    Completed { id: DownloadId, bytes: u64 },
    Failed { id: DownloadId, error: String },
    Cancelled { id: DownloadId },
}

impl DownloadEvent {
    pub fn id(&self) -> DownloadId {
        match self {
            Self::AwaitingConfirmation { id, .. } |
            Self::Started { id, .. } |
            Self::Progress { id, .. } |
            Self::Completed { id, .. } |
            Self::Failed { id, .. } |
            Self::Cancelled { id } => *id,
        }
    }
}

struct DownloadControl {
    cancel: watch::Sender<bool>,
    confirm: Option<oneshot::Sender<bool>>,
}

struct Shared {
    events: Sender<DownloadEvent>,
    waker: Waker,
    controls: Mutex<HashMap<DownloadId, DownloadControl>>,
}

impl Shared {
    fn emit(&self, event: DownloadEvent) {
        if self.events.send(event).is_ok() {
            (self.waker)();
        }
    }

    fn finished(&self, id: DownloadId) {
        self.controls.lock().unwrap_or_else(|poison| poison.into_inner()).remove(&id);
    }
}

/// Handle to the networking service. Cheap to keep around; threads start on first use.
pub struct NetService {
    runtime: OnceLock<Runtime>,
    client: OnceLock<reqwest::Client>,
    shared: Arc<Shared>,
    receiver: Receiver<DownloadEvent>,
}

impl NetService {
    pub fn new(waker: Waker) -> Self {
        let (events, receiver) = crossbeam_channel::unbounded();
        Self {
            runtime: OnceLock::new(),
            client: OnceLock::new(),
            shared: Arc::new(Shared {
                events,
                waker,
                controls: Mutex::new(HashMap::new()),
            }),
            receiver,
        }
    }

    fn runtime(&self) -> &Runtime {
        self.runtime.get_or_init(|| {
            debug!("starting network runtime");
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("blanket-net")
                .enable_all()
                .build()
                .expect("failed to start the network runtime")
        })
    }

    fn client(&self) -> &reqwest::Client {
        self.client.get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(120))
                .https_only(false)
                .build()
                .unwrap_or_else(|error| {
                    error!(%error, "failed to build HTTP client with custom settings, using defaults");
                    reqwest::Client::new()
                })
        })
    }

    /// Whether the background runtime has been started.
    pub fn is_started(&self) -> bool {
        self.runtime.get().is_some()
    }

    /// Drain pending events without blocking.
    pub fn poll_events(&self) -> impl Iterator<Item = DownloadEvent> + '_ {
        self.receiver.try_iter()
    }

    pub fn start_download(&self, request: DownloadRequest) {
        let (cancel, cancel_receiver) = watch::channel(false);
        let (confirm, confirm_receiver) = oneshot::channel();
        self.shared.controls.lock().unwrap_or_else(|poison| poison.into_inner()).insert(
            request.id,
            DownloadControl {
                cancel,
                confirm: Some(confirm),
            },
        );
        let client = self.client().clone();
        let shared = self.shared.clone();
        self.runtime().spawn(async move {
            let id = request.id;
            let outcome = download::run(&client, request, cancel_receiver, confirm_receiver, &shared).await;
            shared.finished(id);
            shared.emit(outcome);
        });
    }

    /// Cancel a download. Partially written data is removed.
    pub fn cancel_download(&self, id: DownloadId) {
        if let Some(control) = self.shared.controls.lock().unwrap_or_else(|poison| poison.into_inner()).get(&id) {
            let _ = control.cancel.send(true);
        }
    }

    /// Answer an [`DownloadEvent::AwaitingConfirmation`] prompt.
    pub fn confirm_download(&self, id: DownloadId, allow: bool) {
        let mut controls = self.shared.controls.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(sender) = controls.get_mut(&id).and_then(|control| control.confirm.take()) {
            let _ = sender.send(allow);
        }
    }

    /// Resolve `host` in the background so the OS resolver cache is warm when the user
    /// navigates there. Failures are irrelevant and only logged.
    pub fn prefetch_dns(&self, host: String) {
        self.runtime().spawn(async move {
            match tokio::net::lookup_host((host.as_str(), 443)).await {
                Ok(addresses) => debug!(%host, count = addresses.count(), "dns pre-resolved"),
                Err(error) => debug!(%host, %error, "dns pre-resolution failed"),
            }
        });
    }
}

impl Drop for NetService {
    fn drop(&mut self) {
        let controls = self.shared.controls.lock().unwrap_or_else(|poison| poison.into_inner());
        for control in controls.values() {
            let _ = control.cancel.send(true);
        }
        drop(controls);
        if let Some(runtime) = self.runtime.take() {
            // Give in-flight downloads a moment to delete their partial files.
            runtime.shutdown_timeout(Duration::from_secs(2));
        }
    }
}
