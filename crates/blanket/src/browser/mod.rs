//! Browser Core: owns the Servo instance, the window, the tabs and every manager, and
//! connects them. Split by concern into submodules.

mod delegate;
mod downloads;
mod input;
mod maintenance;
mod navigation;
mod servo_events;
mod suspension;
mod tabs;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use blanket_core::bookmarks::Bookmarks;
use blanket_core::config::BlanketConfig;
use blanket_core::downloads::DownloadList;
use blanket_core::history::History;
use blanket_core::ids::TabId;
use blanket_core::memory::{MemoryPolicy, PressureTracker};
use blanket_core::omnibox::OmniboxTarget;
use blanket_core::paths::ProfilePaths;
use blanket_core::preload::Predictor;
use blanket_core::scheduler::{FramePacer, TaskScheduler, WaitMode};
use blanket_core::security::{PermissionStore, connection_security};
use blanket_core::session::Session;
use blanket_core::storage::load_json_or_default;
use blanket_core::sysmon::SystemMonitor;
use blanket_core::tabs::{OpenPosition, SuspendReason, TabLifecycle, TabManager};
use blanket_core::time::unix_now;
use blanket_net::NetService;
use euclid::Scale;
use servo::protocol_handler::ProtocolRegistry;
use servo::{
    Cursor, DeviceIndependentPixel, DevicePixel, OffscreenRenderingContext, Opts, RenderingContext, Servo,
    ServoBuilder, WebView, WebViewId, WindowRenderingContext,
};
use thiserror::Error;
use tracing::{debug, error, info, warn};
use url::Url;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Icon, Window};

pub use self::maintenance::PerfSnapshot;

use self::delegate::Delegate;
use self::input::InputState;
use self::maintenance::Task;
use self::suspension::AsyncResult;
use crate::app::StartupOptions;
use crate::events::Waker;
use crate::metrics::Metrics;
use crate::protocol::{self, BlanketProtocolHandler, NewTabData};
use crate::ui::{ActiveView, Disposition, Gui, UiCommand, UiModel};

const MIN_WINDOW_SIZE: LogicalSize<f64> = LogicalSize::new(480.0, 320.0);
const INITIAL_WINDOW_SIZE: LogicalSize<f64> = LogicalSize::new(1280.0, 800.0);

#[derive(Debug, Error)]
pub enum StartupError {
    #[error("could not create the window: {0}")]
    Window(#[from] winit::error::OsError),
    #[error("could not access the native window: {0}")]
    Handle(#[from] winit::raw_window_handle::HandleError),
    #[error("could not create an OpenGL rendering context: {0:?}")]
    Rendering(String),
}

/// Engine state of a tab that currently has a web view.
pub(crate) struct LiveView {
    pub webview: WebView,
    /// Back entries known from before the tab was suspended. The engine cannot restore a
    /// session history, so these are kept by the browser and navigated with `load`.
    pub history_base: Vec<url::Url>,
    pub nav_started: Option<Instant>,
    /// The current navigation was typed by the user (strong signal for history ranking).
    pub typed: bool,
    pub painted: bool,
    pub status_text: Option<String>,
    pub cursor: Cursor,
}

impl LiveView {
    fn new(webview: WebView) -> Self {
        Self {
            webview,
            history_base: Vec::new(),
            nav_started: None,
            typed: false,
            painted: false,
            status_text: None,
            cursor: Cursor::Default,
        }
    }

    fn can_go_back(&self) -> bool {
        self.webview.can_go_back() || !self.history_base.is_empty()
    }
}

pub struct Browser {
    // Engine. Fields are dropped explicitly and in order by `shutdown`.
    servo: Option<Servo>,
    delegate: Rc<Delegate>,
    views: HashMap<TabId, LiveView>,
    by_webview: HashMap<WebViewId, TabId>,

    gui: Option<Gui>,
    offscreen: Rc<OffscreenRenderingContext>,
    window_context: Rc<WindowRenderingContext>,
    window: Window,

    paths: ProfilePaths,
    config: BlanketConfig,
    tabs: TabManager,
    history: History,
    bookmarks: Bookmarks,
    downloads: DownloadList,
    permissions: PermissionStore,
    net: NetService,
    newtab: NewTabData,

    waker: Waker,
    scheduler: TaskScheduler<Task>,
    pacer: FramePacer,
    monitor: SystemMonitor,
    pressure: PressureTracker,
    policy: MemoryPolicy,
    predictor: Predictor,
    perf: PerfSnapshot,
    metrics: Metrics,

    input: InputState,
    async_results: Rc<RefCell<Vec<AsyncResult>>>,
    pending_suspensions: HashSet<TabId>,
    /// Tabs being recreated after suspension, with the time recreation started.
    restoring: HashMap<TabId, Instant>,
    saved_scroll: HashMap<TabId, (f64, f64)>,
    content_fullscreen: bool,
    window_fullscreen: bool,
    ui_animating: bool,
    first_frame_recorded: bool,
    session_dirty: bool,
    exit_requested: bool,
    last_title: String,
}

impl Browser {
    pub fn new(event_loop: &ActiveEventLoop, waker: Waker, options: StartupOptions) -> Result<Self, StartupError> {
        let StartupOptions {
            paths,
            config,
            urls,
            restore_session,
            metrics,
        } = options;

        let window = event_loop.create_window(
            Window::default_attributes()
                .with_title("Blanket")
                .with_inner_size(INITIAL_WINDOW_SIZE)
                .with_min_inner_size(MIN_WINDOW_SIZE)
                .with_window_icon(window_icon())
                .with_visible(false),
        )?;
        let display_handle = event_loop.display_handle()?;
        let window_handle = window.window_handle()?;
        let inner_size = non_zero(window.inner_size());
        let window_context = Rc::new(
            WindowRenderingContext::new(display_handle, window_handle, inner_size)
                .map_err(|error| StartupError::Rendering(format!("{error:?}")))?,
        );
        window_context
            .make_current()
            .map_err(|error| StartupError::Rendering(format!("{error:?}")))?;
        let offscreen = Rc::new(window_context.offscreen_context(inner_size));

        let gui = Gui::new(&window, event_loop, offscreen.clone(), &config);
        window.set_visible(true);
        metrics.record("window_created", serde_json::Value::Null);

        let newtab = NewTabData::default();
        let mut protocols = ProtocolRegistry::default();
        if let Err(error) = protocols.register(protocol::SCHEME, BlanketProtocolHandler::new(newtab.clone())) {
            warn!(?error, "could not register the blanket: scheme");
        }
        let opts = Opts {
            config_dir: Some(paths.engine_config_dir()),
            ..Opts::default()
        };
        let servo = ServoBuilder::default()
            .opts(opts)
            .protocol_registry(protocols)
            .event_loop_waker(Box::new(waker.clone()))
            .build();
        // Logging goes through tracing's `log` bridge; `Servo::setup_logging` would install
        // a competing global logger.

        let delegate = Delegate::new();
        let scale = Scale::new(window.scale_factor() as f32);
        delegate.set_creation_context(offscreen.clone(), scale);

        let history: History = load_or_default("history", History::load(&paths.history()));
        let bookmarks: Bookmarks = load_or_default("bookmarks", Bookmarks::load(&paths.bookmarks()));
        let downloads: DownloadList = load_or_default("downloads", DownloadList::load(&paths.downloads_log()));
        let permissions: PermissionStore = load_or_default("permissions", load_json_or_default(&paths.permissions()));

        let now = Instant::now();
        let mut scheduler = TaskScheduler::new(Duration::from_millis(config.scheduler.interaction_grace_ms));
        scheduler.schedule_repeating(
            Task::SampleResources,
            now,
            Duration::from_secs(config.memory.sample_interval_secs),
            blanket_core::scheduler::TaskPriority::Background,
        );
        scheduler.schedule_repeating(
            Task::SaveSession,
            now,
            Duration::from_secs(config.scheduler.session_autosave_secs),
            blanket_core::scheduler::TaskPriority::Background,
        );

        let mut browser = Self {
            servo: Some(servo),
            delegate,
            views: HashMap::new(),
            by_webview: HashMap::new(),
            gui: Some(gui),
            offscreen,
            window_context,
            window,
            net: NetService::new(waker.as_net_waker()),
            newtab,
            waker,
            pacer: FramePacer::new(Duration::from_millis(config.scheduler.animation_frame_interval_ms)),
            scheduler,
            monitor: SystemMonitor::new(),
            pressure: PressureTracker::default(),
            policy: MemoryPolicy::new(config.memory.clone()),
            predictor: Predictor::new(config.preload.clone()),
            perf: PerfSnapshot::default(),
            metrics,
            paths,
            tabs: TabManager::new(),
            history,
            bookmarks,
            downloads,
            permissions,
            config,
            input: InputState::default(),
            async_results: Rc::default(),
            pending_suspensions: HashSet::new(),
            restoring: HashMap::new(),
            saved_scroll: HashMap::new(),
            content_fullscreen: false,
            window_fullscreen: false,
            ui_animating: false,
            first_frame_recorded: false,
            session_dirty: false,
            exit_requested: false,
            last_title: String::new(),
        };
        browser.update_screen_geometry();
        browser.refresh_newtab_data();
        browser.open_initial_tabs(urls, restore_session);
        browser.request_redraw();
        Ok(browser)
    }

    fn open_initial_tabs(&mut self, urls: Vec<String>, restore_session: bool) {
        let now = Instant::now();
        let session = if restore_session && self.config.general.restore_session {
            Session::load(&self.paths.session()).unwrap_or_else(|error| {
                warn!(%error, "could not load the previous session");
                Session::default()
            })
        } else {
            Session::default()
        };
        if !session.tabs.is_empty() {
            if !session.clean_exit {
                info!("the previous session did not exit cleanly; restoring its tabs");
            }
            session.restore_into(&mut self.tabs, now);
            info!(tabs = self.tabs.len(), "session restored (background tabs load on first use)");
        }

        let mut opened_from_cli = false;
        for input in urls {
            match blanket_core::omnibox::interpret(
                &input,
                &self.config.general.search_engine,
                self.config.security.prefer_https_for_typed_hosts,
            ) {
                Some(target) => {
                    // The first command-line URL becomes active; the others load in the
                    // background, like links opened in new tabs.
                    let id = self.tabs.open(Some(target.into_url()), OpenPosition::End, !opened_from_cli, now);
                    if opened_from_cli {
                        self.tabs.set_lifecycle(id, TabLifecycle::Live);
                    }
                    opened_from_cli = true;
                },
                None => warn!(%input, "ignoring command-line argument that is not a URL"),
            }
        }
        if self.tabs.is_empty() {
            let url = self.new_tab_url();
            self.tabs.open(Some(url), OpenPosition::End, true, now);
        }

        // Create web views for live tabs: the active one first so it gets the network.
        let mut live: Vec<TabId> = self
            .tabs
            .tabs()
            .iter()
            .filter(|tab| tab.lifecycle.is_live())
            .map(|tab| tab.id)
            .collect();
        let active = self.tabs.active_id();
        live.sort_by_key(|id| Some(*id) != active);
        for id in live {
            let url = self.tabs.get(id).and_then(|tab| tab.url.clone()).unwrap_or_else(|| self.new_tab_url());
            self.create_view(id, url, true);
        }
        if let Some(active) = active {
            self.show_active(None, active);
        }
        self.session_dirty = true;
    }

    // ----- accessors used by submodules -------------------------------------------------

    fn servo(&self) -> Option<&Servo> {
        self.servo.as_ref()
    }

    fn gui_mut(&mut self) -> &mut Gui {
        self.gui.as_mut().expect("the GUI lives until shutdown")
    }

    fn scale(&self) -> Scale<f32, DeviceIndependentPixel, DevicePixel> {
        Scale::new(self.window.scale_factor() as f32)
    }

    fn new_tab_url(&self) -> Url {
        Url::parse(&self.config.general.new_tab_url).unwrap_or_else(|_| Url::parse("blanket:newtab").expect("valid URL"))
    }

    fn active_view(&self) -> Option<&LiveView> {
        self.tabs.active_id().and_then(|id| self.views.get(&id))
    }

    pub fn exit_requested(&self) -> bool {
        self.exit_requested
    }

    fn request_redraw(&mut self) {
        if self.pacer.request_redraw() {
            self.window.request_redraw();
        }
    }

    fn schedule_soon(&mut self, task: Task) {
        self.scheduler.schedule(
            task,
            Instant::now() + Duration::from_secs(2),
            blanket_core::scheduler::TaskPriority::Background,
        );
    }

    // ----- event loop integration -------------------------------------------------------

    /// Let Servo process its messages and apply everything that resulted from them.
    pub fn pump(&mut self) {
        self.waker.acknowledge();
        if let Some(servo) = &self.servo {
            servo.spin_event_loop();
        }
        self.process_servo_events();
        self.drain_async_results();
        self.process_download_events();
        self.poll_file_dialogs();
        self.update_window_title();
    }

    /// How the event loop should wait after this iteration.
    pub fn control_flow(&mut self) -> ControlFlow {
        let now = Instant::now();
        // egui asks for delayed repaints (caret blink, tooltips, toasts) instead of
        // continuous frames; those are deadlines like any scheduled task.
        let gui_repaint = self.gui.as_ref().and_then(Gui::next_repaint);
        let next_task = match (self.scheduler.next_deadline(now), gui_repaint) {
            (Some(task), Some(gui)) => Some(task.min(gui)),
            (task, gui) => task.or(gui),
        };
        match self.pacer.wait_mode(now, self.ui_animating, next_task) {
            WaitMode::Indefinitely => ControlFlow::Wait,
            WaitMode::Until(at) => {
                if self.ui_animating && at <= now {
                    self.request_redraw();
                }
                ControlFlow::WaitUntil(at.max(now))
            },
        }
    }

    fn update_window_title(&mut self) {
        let title = match self.tabs.active() {
            Some(tab) => format!("{} - Blanket", tab.display_title()),
            None => "Blanket".to_owned(),
        };
        if title != self.last_title {
            self.window.set_title(&title);
            self.last_title = title;
        }
    }

    // ----- painting ---------------------------------------------------------------------

    fn redraw_now(&mut self) {
        let now = Instant::now();
        let active_id = self.tabs.active_id();
        let active_view = active_id.and_then(|id| self.views.get(&id));
        let active_tab = self.tabs.active();
        let url = active_tab.and_then(|tab| tab.url.clone());
        let active = ActiveView {
            id: active_id,
            security: url.as_ref().map(connection_security),
            bookmarked: url.as_ref().is_some_and(|url| self.bookmarks.contains(url)),
            url,
            loading: active_tab.is_some_and(|tab| tab.loading),
            can_go_back: active_view.is_some_and(LiveView::can_go_back),
            can_go_forward: active_view.is_some_and(|view| view.webview.can_go_forward()),
            zoom: active_view.map_or(1.0, |view| view.webview.page_zoom()),
            status_text: active_view.and_then(|view| view.status_text.clone()),
            has_painted_view: active_view.is_some_and(|view| view.painted),
            crashed: active_tab.and_then(|tab| match &tab.lifecycle {
                TabLifecycle::Crashed { reason } => Some(reason.clone()),
                _ => None,
            }),
        };

        let Self {
            gui,
            window,
            views,
            tabs,
            history,
            bookmarks,
            downloads,
            config,
            perf,
            permissions,
            content_fullscreen,
            window_fullscreen,
            ..
        } = self;
        let Some(gui) = gui.as_mut() else {
            return;
        };
        let model = UiModel {
            tabs,
            active,
            history,
            bookmarks,
            downloads,
            config,
            perf,
            permissions,
            now_unix: unix_now(),
            chrome_hidden: *content_fullscreen,
            window_fullscreen: *window_fullscreen,
        };
        let active_webview = active_id.and_then(|id| views.get(&id)).map(|view| view.webview.clone());
        let mut paint_content = |size: PhysicalSize<u32>| -> bool {
            let Some(webview) = &active_webview else {
                return false;
            };
            let current = webview.size();
            if current.width as u32 != size.width || current.height as u32 != size.height {
                webview.resize(size);
            }
            webview.paint();
            true
        };
        let commands = gui.update(window, &model, &mut paint_content);
        gui.paint(window);
        self.ui_animating = gui.needs_repaint();
        self.pacer.frame_presented(Instant::now());

        if !self.first_frame_recorded {
            self.first_frame_recorded = true;
            let ms = self.metrics.elapsed_ms(now);
            info!(startup_ms = format!("{ms:.0}"), "first frame presented");
            self.metrics.record("first_frame", serde_json::Value::Null);
        }
        for command in commands {
            self.execute(command);
        }
    }

    // ----- UI commands ------------------------------------------------------------------

    fn execute(&mut self, command: UiCommand) {
        debug!(?command, "ui command");
        match command {
            UiCommand::NewTab => {
                let url = self.new_tab_url();
                self.open_tab(url, OpenPosition::End, true);
                self.gui_mut().focus_location();
            },
            UiCommand::CloseTab(id) => self.close_tab(id),
            UiCommand::ActivateTab(id) => self.activate_tab(id),
            UiCommand::CycleTab(offset) => {
                if let Some(id) = self.next_tab_id(offset) {
                    self.activate_tab(id);
                }
            },
            UiCommand::ActivateIndex(index) => {
                let id = if index == usize::MAX {
                    self.tabs.tabs().last().map(|tab| tab.id)
                } else {
                    self.tabs.tabs().get(index).map(|tab| tab.id)
                };
                if let Some(id) = id {
                    self.activate_tab(id);
                }
            },
            UiCommand::MoveTab(id, index) => {
                self.tabs.move_tab(id, index);
                self.session_dirty = true;
            },
            UiCommand::PinTab(id, pinned) => {
                self.tabs.set_pinned(id, pinned);
                let index = if pinned { 0 } else { self.tabs.tabs().iter().filter(|tab| tab.pinned).count() };
                self.tabs.move_tab(id, index);
                self.session_dirty = true;
            },
            UiCommand::DuplicateTab(id) => {
                if let Some(url) = self.tabs.get(id).and_then(|tab| tab.url.clone()) {
                    self.open_tab(url, OpenPosition::AfterActive, true);
                }
            },
            UiCommand::CloseOtherTabs(keep) => {
                let others: Vec<TabId> =
                    self.tabs.tabs().iter().filter(|tab| tab.id != keep && !tab.pinned).map(|tab| tab.id).collect();
                for id in others {
                    self.close_tab(id);
                }
            },
            UiCommand::ReopenClosedTab => self.reopen_closed_tab(),
            UiCommand::SuspendTab(id) => self.request_suspend(id, SuspendReason::User),
            UiCommand::WakeTab(id) => {
                if self.tabs.get(id).is_some_and(|tab| !tab.lifecycle.is_live()) {
                    self.ensure_live(id);
                    if let Some(view) = self.views.get(&id) {
                        if Some(id) != self.tabs.active_id() {
                            view.webview.hide();
                        }
                    }
                }
            },
            UiCommand::Navigate { input, disposition } => self.navigate_input(&input, disposition),
            UiCommand::OpenUrl { url, disposition } => self.open_url(url, disposition, false),
            UiCommand::Back => self.go_back(),
            UiCommand::Forward => self.go_forward(),
            UiCommand::Reload => self.reload(),
            UiCommand::Home => {
                let home = self.config.general.homepage.clone();
                self.navigate_input(&home, Disposition::CurrentTab);
            },
            UiCommand::ToggleBookmark => self.toggle_bookmark(),
            UiCommand::RemoveBookmark(url) => {
                self.bookmarks.remove(&url);
                self.save_bookmarks();
            },
            UiCommand::RenameBookmark(url, title) => {
                self.bookmarks.rename(&url, title);
                self.save_bookmarks();
            },
            UiCommand::RemoveHistory(url) => {
                self.history.remove(&url);
                self.schedule_soon(Task::SaveHistory);
                self.refresh_newtab_data();
            },
            UiCommand::ClearData(request) => self.clear_browsing_data(request),
            UiCommand::DownloadUrl { url, referrer } => self.start_download(url, referrer),
            UiCommand::CancelDownload(id) => self.net.cancel_download(id),
            UiCommand::ConfirmDownload(id, allow) => self.net.confirm_download(id, allow),
            UiCommand::ShowDownload(id) => self.show_download_in_folder(id),
            UiCommand::RemoveDownload(id) => {
                self.downloads.remove(id);
                self.schedule_soon(Task::SaveDownloads);
            },
            UiCommand::ClearFinishedDownloads => {
                self.downloads.clear_finished();
                self.schedule_soon(Task::SaveDownloads);
            },
            UiCommand::SavePage => {
                if let Some(url) = self.tabs.active().and_then(|tab| tab.url.clone()) {
                    self.start_download(url, None);
                }
            },
            UiCommand::Zoom(change) => self.zoom(change),
            UiCommand::ToggleFullscreen => self.set_window_fullscreen(!self.window_fullscreen),
            UiCommand::ExitFullscreen => {
                if self.content_fullscreen {
                    if let Some(view) = self.active_view() {
                        view.webview.exit_fullscreen();
                    }
                }
                if self.window_fullscreen {
                    self.set_window_fullscreen(false);
                }
            },
            UiCommand::RememberPermission { url, kind, decision } => {
                if self.permissions.remember(&url, kind, decision) {
                    self.schedule_soon(Task::SavePermissions);
                }
            },
            UiCommand::ForgetPermissions(url) => {
                self.permissions.forget_origin(&url);
                self.schedule_soon(Task::SavePermissions);
            },
            UiCommand::UpdateConfig(config) => self.apply_config(*config),
            UiCommand::OmniboxInput(text) => self.predict_from_typing(&text),
            UiCommand::RefreshPerformance => self.sample_resources(),
            UiCommand::FocusContent => {
                if let Some(view) = self.active_view() {
                    view.webview.set_focused(true);
                }
            },
            UiCommand::FocusLocation => self.gui_mut().focus_location(),
            UiCommand::TogglePanel(panel) => self.gui_mut().toggle_panel(panel),
            UiCommand::ToggleBookmarksBar => {
                let mut config = self.config.clone();
                config.ui.show_bookmarks_bar = !config.ui.show_bookmarks_bar;
                self.apply_config(config);
            },
            UiCommand::Quit => self.exit_requested = true,
        }
        self.request_redraw();
    }

    fn next_tab_id(&self, offset: isize) -> Option<TabId> {
        let len = self.tabs.len() as isize;
        if len == 0 {
            return None;
        }
        let current = self.tabs.active_index().unwrap_or(0) as isize;
        let index = (current + offset).rem_euclid(len) as usize;
        self.tabs.tabs().get(index).map(|tab| tab.id)
    }

    fn toggle_bookmark(&mut self) {
        let Some(tab) = self.tabs.active() else {
            return;
        };
        let Some(url) = tab.url.clone() else {
            return;
        };
        let title = tab.display_title().to_owned();
        let added = self.bookmarks.toggle(&url, &title, unix_now());
        self.save_bookmarks();
        self.gui_mut()
            .toast(if added { "Bookmark added".to_owned() } else { "Bookmark removed".to_owned() });
    }

    fn save_bookmarks(&mut self) {
        if let Err(error) = self.bookmarks.save(&self.paths.bookmarks()) {
            error!(%error, "could not save bookmarks");
            self.gui_mut().toast("Could not save bookmarks".to_owned());
        }
    }

    fn zoom(&mut self, change: crate::ui::ZoomChange) {
        let Some(view) = self.active_view() else {
            return;
        };
        let current = view.webview.page_zoom();
        let zoom = match change {
            crate::ui::ZoomChange::In => (current * 1.1).min(5.0),
            crate::ui::ZoomChange::Out => (current / 1.1).max(0.25),
            crate::ui::ZoomChange::Reset => 1.0,
        };
        view.webview.set_page_zoom(zoom);
    }

    fn set_window_fullscreen(&mut self, fullscreen: bool) {
        self.window_fullscreen = fullscreen;
        self.window
            .set_fullscreen(fullscreen.then(|| winit::window::Fullscreen::Borderless(self.window.current_monitor())));
    }

    fn set_content_fullscreen(&mut self, fullscreen: bool) {
        self.content_fullscreen = fullscreen;
        if fullscreen != self.window_fullscreen {
            self.set_window_fullscreen(fullscreen);
        }
        if fullscreen {
            self.gui_mut().toast("Press Esc to exit full screen".to_owned());
        }
    }

    fn apply_config(&mut self, config: BlanketConfig) {
        let config = config.validated();
        if let Err(error) = config.save(&self.paths.config()) {
            error!(%error, "could not save settings");
            self.gui_mut().toast("Could not save settings".to_owned());
        }
        self.policy = MemoryPolicy::new(config.memory.clone());
        self.predictor = Predictor::new(config.preload.clone());
        self.pacer = FramePacer::new(Duration::from_millis(config.scheduler.animation_frame_interval_ms));
        let now = Instant::now();
        self.scheduler.cancel(&Task::SampleResources);
        self.scheduler.schedule_repeating(
            Task::SampleResources,
            now,
            Duration::from_secs(config.memory.sample_interval_secs),
            blanket_core::scheduler::TaskPriority::Background,
        );
        self.config = config;
        let system_theme = self.window.theme();
        let theme = crate::ui::theme::resolve_theme(self.config.ui.theme, system_theme);
        for view in self.views.values() {
            view.webview.notify_theme_change(theme);
        }
        let config = self.config.clone();
        self.gui_mut().apply_config(&config, system_theme);
        self.refresh_newtab_data();
    }

    fn predict_from_typing(&mut self, text: &str) {
        if !self.config.preload.dns_prefetch {
            return;
        }
        let top = self.history.search(text, 1, unix_now()).into_iter().next();
        if let Some(host) = self.predictor.host_to_warm(text, top, Instant::now()) {
            debug!(%host, "pre-resolving likely destination");
            self.net.prefetch_dns(host);
        }
    }

    fn refresh_newtab_data(&mut self) {
        let sites = self
            .history
            .top_sites(8, unix_now())
            .into_iter()
            .map(|entry| protocol::TopSite {
                title: entry.title.clone(),
                url: entry.url.clone(),
            })
            .collect();
        self.newtab.update(&self.config.general.search_engine, sites);
    }

    fn update_screen_geometry(&mut self) {
        let Some(monitor) = self.window.current_monitor() else {
            return;
        };
        let monitor_size = monitor.size();
        let size = servo::DeviceIntSize::new(monitor_size.width as i32, monitor_size.height as i32);
        let position = self.window.outer_position().unwrap_or_default();
        let outer = self.window.outer_size();
        self.delegate.set_screen_geometry(servo::ScreenGeometry {
            size,
            available_size: size,
            window_rect: servo::DeviceIntRect::from_origin_and_size(
                servo::DeviceIntPoint::new(position.x, position.y),
                servo::DeviceIntSize::new(outer.width as i32, outer.height as i32),
            ),
        });
    }

    fn resolve_omnibox(&self, input: &str) -> Option<OmniboxTarget> {
        blanket_core::omnibox::interpret(
            input,
            &self.config.general.search_engine,
            self.config.security.prefer_https_for_typed_hosts,
        )
    }

    // ----- shutdown ---------------------------------------------------------------------

    /// Persist state and tear down in dependency order: web views, then the GUI (which
    /// uses the GL context), then Servo (blocks until the engine has shut down), then the
    /// rendering contexts, and the window last.
    pub fn shutdown(mut self: Box<Self>) {
        info!("shutting down");
        let session = Session::capture(&self.tabs, true);
        if let Err(error) = session.save(&self.paths.session()) {
            error!(%error, "could not save the session");
        }
        self.save_all_state();

        self.views.clear();
        self.by_webview.clear();
        if let Some(gui) = self.gui.take() {
            drop(gui);
        }
        if let Some(servo) = self.servo.take() {
            drop(servo);
        }
        let Self {
            offscreen,
            window_context,
            window,
            net,
            ..
        } = *self;
        drop(net);
        drop(offscreen);
        drop(window_context);
        drop(window);
    }
}

fn load_or_default<T: Default>(what: &str, result: Result<T, blanket_core::CoreError>) -> T {
    result.unwrap_or_else(|error| {
        warn!(%error, "could not load {what}; starting empty");
        T::default()
    })
}

/// Rendering contexts cannot be created with a zero-sized surface (minimized window).
fn non_zero(size: PhysicalSize<u32>) -> PhysicalSize<u32> {
    PhysicalSize::new(size.width.max(1), size.height.max(1))
}

fn window_icon() -> Option<Icon> {
    let (rgba, size) = crate::ui::icons::app_icon_rgba(64);
    Icon::from_rgba(rgba, size, size)
        .map_err(|error| warn!(%error, "invalid window icon"))
        .ok()
}
