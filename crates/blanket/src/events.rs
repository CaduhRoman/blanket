//! Waking the winit event loop from Servo, the network service and async UI tasks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Wake;

use tracing::trace;
use winit::event_loop::EventLoopProxy;

/// User events delivered to the winit event loop.
#[derive(Clone, Copy, Debug)]
pub enum AppEvent {
    /// Some producer has work ready: spin Servo and drain queues.
    Wake,
}

/// A cheap, thread-safe handle that schedules [`AppEvent::Wake`].
///
/// Wake-ups are coalesced: while one is queued and not yet handled, further calls do not
/// post more messages, which keeps a busy engine from flooding the window message queue.
#[derive(Clone)]
pub struct Waker {
    inner: Arc<WakerInner>,
}

struct WakerInner {
    proxy: Mutex<EventLoopProxy<AppEvent>>,
    pending: AtomicBool,
}

impl Waker {
    pub fn new(proxy: EventLoopProxy<AppEvent>) -> Self {
        Self {
            inner: Arc::new(WakerInner {
                proxy: Mutex::new(proxy),
                pending: AtomicBool::new(false),
            }),
        }
    }

    pub fn wake(&self) {
        if self.inner.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let proxy = self.inner.proxy.lock().unwrap_or_else(|poison| poison.into_inner());
        if proxy.send_event(AppEvent::Wake).is_err() {
            trace!("event loop closed; dropping wake-up");
        }
    }

    /// Called by the event loop right before it processes work, so producers that become
    /// ready during processing schedule a new wake-up.
    pub fn acknowledge(&self) {
        self.inner.pending.store(false, Ordering::Release);
    }

    pub fn as_net_waker(&self) -> blanket_net::Waker {
        let waker = self.clone();
        Arc::new(move || waker.wake())
    }

    pub fn as_task_waker(&self) -> std::task::Waker {
        std::task::Waker::from(Arc::new(self.clone()))
    }
}

impl servo::EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn servo::EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        Waker::wake(self);
    }
}

impl Wake for Waker {
    fn wake(self: Arc<Self>) {
        Waker::wake(&self);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        Waker::wake(self);
    }
}
