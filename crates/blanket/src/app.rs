//! winit application handler: creates the [`Browser`] once the event loop is running and
//! forwards window events and wake-ups to it.

use blanket_core::config::BlanketConfig;
use blanket_core::paths::ProfilePaths;
use tracing::error;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::WindowId;

use crate::browser::Browser;
use crate::events::{AppEvent, Waker};
use crate::metrics::Metrics;

/// Everything decided before the event loop starts.
pub struct StartupOptions {
    pub paths: ProfilePaths,
    pub config: BlanketConfig,
    pub urls: Vec<String>,
    pub restore_session: bool,
    pub metrics: Metrics,
    /// This process owns the profile; it serves later launches once running.
    pub instance: crate::single_instance::Primary,
}

enum State {
    Starting(Box<StartupOptions>),
    Running(Box<Browser>),
    Failed,
    Exited,
}

pub struct App {
    waker: Waker,
    state: State,
}

impl App {
    pub fn new(event_loop: &EventLoop<AppEvent>, options: StartupOptions) -> Self {
        Self {
            waker: Waker::new(event_loop.create_proxy()),
            state: State::Starting(Box::new(options)),
        }
    }

    pub fn failed(&self) -> bool {
        matches!(self.state, State::Failed)
    }

    /// Let the browser process pending work, then decide how the loop should wait.
    fn after_event(&mut self, event_loop: &ActiveEventLoop) {
        let State::Running(browser) = &mut self.state else {
            return;
        };
        browser.pump();
        if browser.exit_requested() {
            self.shutdown();
            event_loop.exit();
            return;
        }
        event_loop.set_control_flow(browser.control_flow());
    }

    fn shutdown(&mut self) {
        if let State::Running(browser) = std::mem::replace(&mut self.state, State::Exited) {
            browser.shutdown();
        }
    }
}

impl ApplicationHandler<AppEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !matches!(self.state, State::Starting(_)) {
            return;
        }
        let State::Starting(options) = std::mem::replace(&mut self.state, State::Failed) else {
            return;
        };
        match Browser::new(event_loop, self.waker.clone(), *options) {
            Ok(browser) => {
                self.state = State::Running(Box::new(browser));
                self.after_event(event_loop);
            },
            Err(error) => {
                error!(%error, "failed to start the browser");
                event_loop.exit();
            },
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _window_id: WindowId, event: WindowEvent) {
        if let State::Running(browser) = &mut self.state {
            browser.handle_window_event(event);
            if browser.exit_requested() {
                self.shutdown();
                event_loop.exit();
            }
        }
    }

    // Wake-ups only need to end the wait; the work happens in `about_to_wait`, once per
    // batch of events.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: AppEvent) {}

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let State::Running(browser) = &mut self.state {
            browser.run_due_tasks();
        }
        self.after_event(event_loop);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.shutdown();
    }
}
