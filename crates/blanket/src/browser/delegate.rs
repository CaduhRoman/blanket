//! Bridge from Servo's [`WebViewDelegate`] callbacks to the browser.
//!
//! Servo invokes the delegate while [`servo::Servo::spin_event_loop`] runs, with the
//! browser not borrowable. Notifications are therefore queued and drained right after the
//! spin. Decisions that must be made synchronously and depend only on the request
//! (navigation policy, download interception, new web views) are taken here.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::{Rc, Weak};

use blanket_core::security::content_navigation_allowed;
use euclid::Scale;
use servo::{
    AllowOrDenyRequest, AuthenticationRequest, ConsoleLogLevel, CreateNewWebViewRequest, Cursor, DeviceIndependentPixel,
    DevicePixel, EmbedderControl, EmbedderControlId, LoadStatus, MediaSessionEvent, MediaSessionPlaybackState,
    NavigationRequest, OffscreenRenderingContext, PermissionRequest, RenderingContext, ScreenGeometry, WebResourceLoad,
    WebResourceResponse, WebView, WebViewDelegate, WebViewId,
};
use tracing::{debug, info, warn};
use url::Url;

use super::downloads::looks_like_download;

/// A notification or request from the engine, tagged with its web view.
pub enum ServoEvent {
    UrlChanged(Url),
    TitleChanged(Option<String>),
    StatusText(Option<String>),
    LoadStatus(LoadStatus),
    FaviconChanged,
    NewFrame,
    HistoryChanged { entries: Vec<Url>, current: usize },
    Closed,
    Crashed(String),
    MediaPlaying(bool),
    Fullscreen(bool),
    CursorChanged(Cursor),
    /// Web content opened a new top-level web view (`window.open`, `target=_blank`).
    Opened(WebView),
    Permission(PermissionRequest),
    Authentication(AuthenticationRequest),
    Unload(AllowOrDenyRequest),
    ShowControl(EmbedderControl),
    HideControl(EmbedderControlId),
    /// A main-frame navigation was intercepted because it is a file download.
    Download { url: Url, referrer: Option<Url> },
    /// A page navigated to a site that needs compatibility mode.
    CompatNavigation(Url),
}

/// What the delegate needs to create web views for `window.open`.
struct CreationContext {
    rendering_context: Rc<OffscreenRenderingContext>,
    scale: Scale<f32, DeviceIndependentPixel, DevicePixel>,
}

pub struct Delegate {
    queue: RefCell<VecDeque<(WebViewId, ServoEvent)>>,
    /// Navigations started by the user (address bar, bookmarks, history). They bypass the
    /// content navigation policy, which only restricts navigations initiated by pages.
    user_navigations: RefCell<HashMap<WebViewId, Url>>,
    creation: RefCell<Option<CreationContext>>,
    screen: RefCell<Option<ScreenGeometry>>,
    intercept_downloads: Cell<bool>,
    /// Compatibility-mode rules, when the WebView2 runtime is available.
    compat: RefCell<Option<blanket_core::config::CompatConfig>>,
    this: Weak<Delegate>,
}

impl Delegate {
    pub fn new() -> Rc<Self> {
        Rc::new_cyclic(|this| Self {
            queue: RefCell::default(),
            user_navigations: RefCell::default(),
            creation: RefCell::default(),
            screen: RefCell::default(),
            intercept_downloads: Cell::new(true),
            compat: RefCell::default(),
            this: this.clone(),
        })
    }

    pub fn set_creation_context(
        &self,
        rendering_context: Rc<OffscreenRenderingContext>,
        scale: Scale<f32, DeviceIndependentPixel, DevicePixel>,
    ) {
        *self.creation.borrow_mut() = Some(CreationContext { rendering_context, scale });
    }

    pub fn set_scale(&self, scale: Scale<f32, DeviceIndependentPixel, DevicePixel>) {
        if let Some(creation) = self.creation.borrow_mut().as_mut() {
            creation.scale = scale;
        }
    }

    pub fn set_compat(&self, compat: Option<blanket_core::config::CompatConfig>) {
        *self.compat.borrow_mut() = compat;
    }

    pub fn set_screen_geometry(&self, geometry: ScreenGeometry) {
        *self.screen.borrow_mut() = Some(geometry);
    }

    /// Mark a navigation of `webview` to `url` as user-initiated.
    pub fn expect_user_navigation(&self, webview: WebViewId, url: Url) {
        self.user_navigations.borrow_mut().insert(webview, url);
    }

    pub fn forget_webview(&self, webview: WebViewId) {
        self.user_navigations.borrow_mut().remove(&webview);
    }

    pub fn drain(&self) -> VecDeque<(WebViewId, ServoEvent)> {
        std::mem::take(&mut *self.queue.borrow_mut())
    }

    fn push(&self, webview: &WebView, event: ServoEvent) {
        self.queue.borrow_mut().push_back((webview.id(), event));
    }

    fn rc(&self) -> Option<Rc<Delegate>> {
        self.this.upgrade()
    }
}

impl WebViewDelegate for Delegate {
    fn screen_geometry(&self, _webview: WebView) -> Option<ScreenGeometry> {
        *self.screen.borrow()
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        self.push(&webview, ServoEvent::UrlChanged(url));
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        self.push(&webview, ServoEvent::TitleChanged(title));
    }

    fn notify_status_text_changed(&self, webview: WebView, status: Option<String>) {
        self.push(&webview, ServoEvent::StatusText(status));
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        self.push(&webview, ServoEvent::LoadStatus(status));
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        self.push(&webview, ServoEvent::CursorChanged(cursor));
    }

    fn notify_favicon_changed(&self, webview: WebView) {
        self.push(&webview, ServoEvent::FaviconChanged);
    }

    fn notify_new_frame_ready(&self, webview: WebView) {
        self.push(&webview, ServoEvent::NewFrame);
    }

    fn notify_history_changed(&self, webview: WebView, entries: Vec<Url>, current: usize) {
        self.push(&webview, ServoEvent::HistoryChanged { entries, current });
    }

    fn notify_closed(&self, webview: WebView) {
        self.push(&webview, ServoEvent::Closed);
    }

    fn notify_crashed(&self, webview: WebView, reason: String, backtrace: Option<String>) {
        warn!(%reason, backtrace = backtrace.as_deref().unwrap_or(""), "web content crashed");
        self.push(&webview, ServoEvent::Crashed(reason));
    }

    fn notify_media_session_event(&self, webview: WebView, event: MediaSessionEvent) {
        if let MediaSessionEvent::PlaybackStateChange(state) = event {
            let playing = matches!(state, MediaSessionPlaybackState::Playing);
            self.push(&webview, ServoEvent::MediaPlaying(playing));
        }
    }

    fn notify_fullscreen_state_changed(&self, webview: WebView, is_fullscreen: bool) {
        self.push(&webview, ServoEvent::Fullscreen(is_fullscreen));
    }

    fn request_navigation(&self, webview: WebView, navigation_request: NavigationRequest) {
        let expected = self.user_navigations.borrow_mut().remove(&webview.id());
        if expected.as_ref() == Some(&navigation_request.url) {
            navigation_request.allow();
            return;
        }
        // Links into sites that need compatibility mode continue there, in the same tab.
        // Only the URL is handed over, so this is only correct for GETs; a POST (a form
        // submission, a sign-in callback) would arrive in WebView2 without its body.
        // Servo 0.8's `NavigationRequest` exposes nothing but `url` (the method and body
        // stay in the constellation's private load data), so the method cannot be checked
        // here. What keeps sign-in flows on one engine instead is that Google's account
        // hosts are themselves compatibility domains (config `COMPAT_DEFAULTS_ADDED`, v3):
        // the flow starts in WebView2 and never reaches this point mid-way. Check again
        // when Servo is upgraded: if the request gains a method, divert only GETs.
        if self.compat.borrow().as_ref().is_some_and(|compat| compat.applies_to(&navigation_request.url)) {
            let url = navigation_request.url.clone();
            navigation_request.deny();
            self.push(&webview, ServoEvent::CompatNavigation(url));
            return;
        }
        let current = webview.url();
        if content_navigation_allowed(current.as_ref(), &navigation_request.url) {
            navigation_request.allow();
        } else {
            warn!(
                from = current.as_ref().map(Url::as_str).unwrap_or("none"),
                to = %navigation_request.url,
                "blocked navigation into a more privileged context"
            );
            navigation_request.deny();
        }
    }

    fn request_unload(&self, webview: WebView, unload_request: AllowOrDenyRequest) {
        self.push(&webview, ServoEvent::Unload(unload_request));
    }

    fn request_create_new(&self, parent_webview: WebView, request: CreateNewWebViewRequest) {
        let creation = self.creation.borrow();
        let (Some(creation), Some(delegate)) = (creation.as_ref(), self.rc()) else {
            warn!("cannot open a new web view before the window exists");
            return;
        };
        let rendering_context: Rc<dyn RenderingContext> = creation.rendering_context.clone();
        let webview = request
            .builder(rendering_context)
            .hidpi_scale_factor(creation.scale)
            .delegate(delegate)
            .build();
        debug!(parent = ?parent_webview.id(), child = ?webview.id(), "page opened a new web view");
        self.push(&parent_webview, ServoEvent::Opened(webview));
    }

    fn request_permission(&self, webview: WebView, request: PermissionRequest) {
        self.push(&webview, ServoEvent::Permission(request));
    }

    fn request_authentication(&self, webview: WebView, authentication_request: AuthenticationRequest) {
        self.push(&webview, ServoEvent::Authentication(authentication_request));
    }

    fn show_embedder_control(&self, webview: WebView, embedder_control: EmbedderControl) {
        self.push(&webview, ServoEvent::ShowControl(embedder_control));
    }

    fn hide_embedder_control(&self, webview: WebView, control_id: EmbedderControlId) {
        self.push(&webview, ServoEvent::HideControl(control_id));
    }

    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        if !self.intercept_downloads.get() {
            return;
        }
        let request = load.request();
        if !request.is_for_main_frame || request.method != http::Method::GET {
            return;
        }
        let url = request.url.clone().into_url();
        if !looks_like_download(&url) {
            return;
        }
        let referrer = request.referrer_url.clone().map(|referrer| referrer.into_url());
        info!(%url, "intercepting navigation to a downloadable file");
        // 204 No Content makes the navigation a no-op: the current page stays.
        let response = WebResourceResponse::new(url.clone())
            .status_code(http::StatusCode::NO_CONTENT)
            .status_message(b"No Content".to_vec());
        load.intercept(response).finish();
        self.push(&webview, ServoEvent::Download { url, referrer });
    }

    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        debug!(target: "blanket::console", ?level, "{message}");
    }
}
