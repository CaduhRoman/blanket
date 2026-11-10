//! Compatibility mode: tabs rendered by the system WebView2 (Chromium) runtime.
//!
//! Servo renders every site except those that need features it does not implement yet
//! (Media Source Extensions, DRM): YouTube, Twitch, streaming services. Those open here,
//! automatically, according to `[compat]` in the configuration. The rest of the browser
//! (tab strip, address bar, history, bookmarks) treats these tabs like any other.
//!
//! A WebView2 is a native child window placed over the content area. It receives mouse
//! and keyboard input directly; browser shortcuts typed inside it arrive through
//! `AcceleratorKeyPressed`. Its events are queued here and applied by the browser.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use blanket_core::config::CompatConfig;
use blanket_core::ids::TabId;
use tracing::{debug, info, warn};
use url::Url;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::Win32::Foundation::{E_POINTER, HWND, RECT};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VIRTUAL_KEY};
use windows::core::{BOOL, Interface, PWSTR};

use crate::ui::{Panel, UiCommand};

/// Something a compatibility tab reported, applied by the browser on the UI thread.
#[derive(Debug)]
pub enum CompatEvent {
    UrlChanged(Url),
    TitleChanged(String),
    LoadStarted,
    LoadFinished { success: bool },
    HistoryChanged,
    /// The page asked for a new window (`target=_blank`, "Open link in new window").
    OpenNewTab { url: Url, background: bool },
    /// The user followed a link to a site that does not need compatibility mode: it
    /// continues on Servo in the same tab.
    LeaveCompat(Url),
    Fullscreen(bool),
    /// A browser shortcut pressed while the page had keyboard focus.
    Command(UiCommand),
}

type EventQueue = Rc<RefCell<Vec<(TabId, CompatEvent)>>>;

struct CompatView {
    controller: ICoreWebView2Controller,
    webview: ICoreWebView2,
    /// When the page context menu was last opened: a new window requested right after
    /// comes from "Open link in new tab/window" and opens in the background.
    context_menu_at: Rc<RefCell<Option<Instant>>>,
    visible: bool,
    bounds: RECT,
}

pub struct CompatHost {
    parent: HWND,
    data_dir: std::path::PathBuf,
    environment: Option<ICoreWebView2Environment>,
    /// Creating the runtime failed once (not installed, broken): stop trying.
    unavailable: bool,
    views: HashMap<TabId, CompatView>,
    events: EventQueue,
    config: Rc<RefCell<CompatConfig>>,
    wake: Rc<dyn Fn()>,
}

impl CompatHost {
    pub fn for_window(
        window: &winit::window::Window,
        profile_root: &Path,
        config: CompatConfig,
        wake: Rc<dyn Fn()>,
    ) -> Self {
        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let parent = match window.window_handle().map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => HWND(handle.hwnd.get() as *mut core::ffi::c_void),
            _ => HWND::default(),
        };
        let mut host = Self::new(parent, profile_root, config, wake);
        host.unavailable = parent.is_invalid();
        host
    }

    fn new(parent: HWND, profile_root: &Path, config: CompatConfig, wake: Rc<dyn Fn()>) -> Self {
        Self {
            parent,
            data_dir: profile_root.join("compat"),
            environment: None,
            unavailable: false,
            views: HashMap::new(),
            events: Rc::default(),
            config: Rc::new(RefCell::new(config)),
            wake,
        }
    }

    pub fn set_config(&mut self, config: CompatConfig) {
        *self.config.borrow_mut() = config;
    }

    pub fn has_view(&self, id: TabId) -> bool {
        self.views.contains_key(&id)
    }

    pub fn is_available(&self) -> bool {
        !self.unavailable
    }

    pub fn drain_events(&self) -> Vec<(TabId, CompatEvent)> {
        std::mem::take(&mut *self.events.borrow_mut())
    }

    fn environment(&mut self) -> Option<ICoreWebView2Environment> {
        if let Some(environment) = &self.environment {
            return Some(environment.clone());
        }
        if self.unavailable {
            return None;
        }
        let started = Instant::now();
        let folder = CoTaskMemPWSTR::from(self.data_dir.to_string_lossy().as_ref());
        let (tx, rx) = mpsc::channel();
        let result = CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                CreateCoreWebView2EnvironmentWithOptions(
                    windows::core::PCWSTR::null(),
                    *folder.as_ref().as_pcwstr(),
                    None,
                    &handler,
                )
                .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error, environment| {
                error?;
                let _ = tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        );
        match result.map_err(|error| format!("{error:?}")).and_then(|()| {
            rx.recv().map_err(|error| error.to_string())?.map_err(|error| error.to_string())
        }) {
            Ok(environment) => {
                info!(ms = started.elapsed().as_millis() as u64, "WebView2 runtime started for compatibility mode");
                self.environment = Some(environment.clone());
                Some(environment)
            },
            Err(error) => {
                warn!(%error, "WebView2 runtime unavailable; compatibility mode is off");
                self.unavailable = true;
                None
            },
        }
    }

    /// Create the compatibility view of tab `id`, loading `url`. Starts hidden.
    pub fn create(&mut self, id: TabId, url: &Url, dark: bool) -> bool {
        self.close(id);
        let Some(environment) = self.environment() else {
            return false;
        };
        let parent = self.parent;
        let (tx, rx) = mpsc::channel();
        let created = CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                environment
                    .CreateCoreWebView2Controller(parent, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error, controller| {
                error?;
                let _ = tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        );
        let controller = match created
            .map_err(|error| format!("{error:?}"))
            .and_then(|()| rx.recv().map_err(|error| error.to_string())?.map_err(|error| error.to_string()))
        {
            Ok(controller) => controller,
            Err(error) => {
                warn!(%error, tab = %id, "could not create a compatibility view");
                return false;
            },
        };
        match self.wire(id, controller, url, dark) {
            Ok(view) => {
                debug!(tab = %id, %url, "compatibility view created");
                self.views.insert(id, view);
                true
            },
            Err(error) => {
                warn!(%error, tab = %id, "could not set up a compatibility view");
                false
            },
        }
    }

    fn wire(&self, id: TabId, controller: ICoreWebView2Controller, url: &Url, dark: bool) -> windows::core::Result<CompatView> {
        let webview = unsafe { controller.CoreWebView2()? };
        unsafe {
            controller.SetIsVisible(false)?;
            if let Ok(webview13) = webview.cast::<ICoreWebView2_13>() {
                let scheme = if dark {
                    COREWEBVIEW2_PREFERRED_COLOR_SCHEME_DARK
                } else {
                    COREWEBVIEW2_PREFERRED_COLOR_SCHEME_LIGHT
                };
                let _ = webview13.Profile().and_then(|profile| profile.SetPreferredColorScheme(scheme));
            }
        }
        let context_menu_at = Rc::new(RefCell::new(None::<Instant>));
        let mut token = 0i64;

        let push = {
            let events = self.events.clone();
            let wake = self.wake.clone();
            move |event: CompatEvent| {
                events.borrow_mut().push((id, event));
                wake();
            }
        };

        unsafe {
            let emit = push.clone();
            webview.add_SourceChanged(
                &SourceChangedEventHandler::create(Box::new(move |sender, _| {
                    if let Some(url) = sender.as_ref().and_then(|webview| read_string(|out| webview.Source(out))) {
                        if let Ok(url) = Url::parse(&url) {
                            emit(CompatEvent::UrlChanged(url));
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let emit = push.clone();
            webview.add_DocumentTitleChanged(
                &DocumentTitleChangedEventHandler::create(Box::new(move |sender, _| {
                    if let Some(title) = sender.as_ref().and_then(|webview| read_string(|out| webview.DocumentTitle(out))) {
                        emit(CompatEvent::TitleChanged(title));
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let emit = push.clone();
            let config = self.config.clone();
            webview.add_NavigationStarting(
                &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let target = read_string(|out| args.Uri(out)).and_then(|uri| Url::parse(&uri).ok());
                    let user_initiated = read_bool(|out| args.IsUserInitiated(out));
                    let redirected = read_bool(|out| args.IsRedirected(out));
                    // Links the user follows to an ordinary site go back to Servo. Redirects
                    // (sign-in flows, consent pages) stay here.
                    if let Some(target) = target {
                        if user_initiated &&
                            !redirected &&
                            matches!(target.scheme(), "http" | "https") &&
                            !config.borrow().applies_to(&target)
                        {
                            args.SetCancel(true)?;
                            emit(CompatEvent::LeaveCompat(target));
                            return Ok(());
                        }
                    }
                    emit(CompatEvent::LoadStarted);
                    Ok(())
                })),
                &mut token,
            )?;

            let emit = push.clone();
            webview.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let success = args.as_ref().is_none_or(|args| read_bool(|out| args.IsSuccess(out)));
                    emit(CompatEvent::LoadFinished { success });
                    Ok(())
                })),
                &mut token,
            )?;

            let emit = push.clone();
            webview.add_HistoryChanged(
                &HistoryChangedEventHandler::create(Box::new(move |_, _| {
                    emit(CompatEvent::HistoryChanged);
                    Ok(())
                })),
                &mut token,
            )?;

            let emit = push.clone();
            webview.add_ContainsFullScreenElementChanged(
                &ContainsFullScreenElementChangedEventHandler::create(Box::new(move |sender, _| {
                    let fullscreen = sender
                        .as_ref()
                        .is_some_and(|webview| read_bool(|out| webview.ContainsFullScreenElement(out)));
                    emit(CompatEvent::Fullscreen(fullscreen));
                    Ok(())
                })),
                &mut token,
            )?;

            if let Ok(webview11) = webview.cast::<ICoreWebView2_11>() {
                let menu_at = context_menu_at.clone();
                webview11.add_ContextMenuRequested(
                    &ContextMenuRequestedEventHandler::create(Box::new(move |_, _| {
                        *menu_at.borrow_mut() = Some(Instant::now());
                        Ok(())
                    })),
                    &mut token,
                )?;
            }

            let emit = push.clone();
            let menu_at = context_menu_at.clone();
            webview.add_NewWindowRequested(
                &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    args.SetHandled(true)?;
                    let Some(url) = read_string(|out| args.Uri(out)).and_then(|uri| Url::parse(&uri).ok()) else {
                        return Ok(());
                    };
                    // "Open link in new tab" from the context menu never steals focus.
                    let from_menu = menu_at
                        .borrow_mut()
                        .take()
                        .is_some_and(|at| at.elapsed() < Duration::from_secs(30));
                    emit(CompatEvent::OpenNewTab {
                        url,
                        background: from_menu,
                    });
                    Ok(())
                })),
                &mut token,
            )?;

            let emit = push.clone();
            controller.add_AcceleratorKeyPressed(
                &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let mut kind = COREWEBVIEW2_KEY_EVENT_KIND::default();
                    args.KeyEventKind(&mut kind)?;
                    if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                        return Ok(());
                    }
                    let mut key = 0u32;
                    args.VirtualKey(&mut key)?;
                    if let Some(command) = shortcut(id, key) {
                        args.SetHandled(true)?;
                        emit(CompatEvent::Command(command));
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            let target = CoTaskMemPWSTR::from(url.as_str());
            webview.Navigate(*target.as_ref().as_pcwstr())?;
        }

        Ok(CompatView {
            controller,
            webview,
            context_menu_at,
            visible: false,
            bounds: RECT::default(),
        })
    }

    pub fn close(&mut self, id: TabId) {
        if let Some(view) = self.views.remove(&id) {
            unsafe {
                let _ = view.controller.SetIsVisible(false);
                let _ = view.controller.Close();
            }
            drop(view.context_menu_at);
        }
    }

    pub fn close_all(&mut self) {
        let ids: Vec<TabId> = self.views.keys().copied().collect();
        for id in ids {
            self.close(id);
        }
    }

    /// Place and show the view of the active tab (if it is a compatibility tab) and hide
    /// every other one. `bounds` is in physical pixels of the parent window.
    pub fn layout(&mut self, active: Option<TabId>, bounds: Option<(i32, i32, i32, i32)>) {
        let bounds = bounds.map(|(left, top, right, bottom)| RECT {
            left,
            top,
            right,
            bottom,
        });
        for (id, view) in &mut self.views {
            let show = Some(*id) == active && bounds.is_some();
            if show {
                let bounds = bounds.expect("checked above");
                if !rect_eq(&view.bounds, &bounds) {
                    view.bounds = bounds;
                    unsafe {
                        let _ = view.controller.SetBounds(bounds);
                    }
                }
            }
            if show != view.visible {
                view.visible = show;
                unsafe {
                    let _ = view.controller.SetIsVisible(show);
                }
            }
        }
    }

    pub fn navigate(&self, id: TabId, url: &Url) {
        if let Some(view) = self.views.get(&id) {
            let target = CoTaskMemPWSTR::from(url.as_str());
            unsafe {
                let _ = view.webview.Navigate(*target.as_ref().as_pcwstr());
            }
        }
    }

    pub fn go_back(&self, id: TabId) {
        if let Some(view) = self.views.get(&id) {
            unsafe {
                let _ = view.webview.GoBack();
            }
        }
    }

    pub fn go_forward(&self, id: TabId) {
        if let Some(view) = self.views.get(&id) {
            unsafe {
                let _ = view.webview.GoForward();
            }
        }
    }

    pub fn reload(&self, id: TabId) {
        if let Some(view) = self.views.get(&id) {
            unsafe {
                let _ = view.webview.Reload();
            }
        }
    }

    pub fn can_go_back(&self, id: TabId) -> bool {
        self.views
            .get(&id)
            .is_some_and(|view| read_bool(|out| unsafe { view.webview.CanGoBack(out) }))
    }

    pub fn can_go_forward(&self, id: TabId) -> bool {
        self.views
            .get(&id)
            .is_some_and(|view| read_bool(|out| unsafe { view.webview.CanGoForward(out) }))
    }

    pub fn zoom(&self, id: TabId) -> Option<f32> {
        let view = self.views.get(&id)?;
        let mut zoom = 1.0f64;
        unsafe { view.controller.ZoomFactor(&mut zoom).ok()? };
        Some(zoom as f32)
    }

    pub fn set_zoom(&self, id: TabId, zoom: f32) {
        if let Some(view) = self.views.get(&id) {
            unsafe {
                let _ = view.controller.SetZoomFactor(f64::from(zoom));
            }
        }
    }

    /// Give keyboard focus to the page.
    pub fn focus(&self, id: TabId) {
        if let Some(view) = self.views.get(&id) {
            unsafe {
                let _ = view.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
            }
        }
    }

    /// Take keyboard focus back for the browser chrome (a click on the toolbar).
    pub fn release_focus(&self) {
        if self.views.values().any(|view| view.visible) {
            unsafe {
                let _ = SetFocus(Some(self.parent));
            }
        }
    }
}

impl Drop for CompatHost {
    fn drop(&mut self) {
        self.close_all();
    }
}

fn rect_eq(a: &RECT, b: &RECT) -> bool {
    a.left == b.left && a.top == b.top && a.right == b.right && a.bottom == b.bottom
}

fn read_string(get: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> Option<String> {
    let mut value = PWSTR::null();
    get(&mut value).ok()?;
    Some(CoTaskMemPWSTR::from(value).to_string())
}

fn read_bool(get: impl FnOnce(*mut BOOL) -> windows::core::Result<()>) -> bool {
    let mut value = BOOL::default();
    get(&mut value).is_ok() && value.as_bool()
}

fn key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(i32::from(key.0)) < 0 }
}

/// Browser shortcuts that must keep working while a compatibility page has focus. Keys
/// the page runtime already handles the same way (F5, Alt+Left, zoom, find) are left to it.
fn shortcut(id: TabId, key: u32) -> Option<UiCommand> {
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_MENU, VK_SHIFT};
    let ctrl = key_down(VK_CONTROL);
    let shift = key_down(VK_SHIFT);
    let alt = key_down(VK_MENU);
    let letter = |c: u8| key == u32::from(c);
    Some(match (ctrl, shift, alt) {
        (true, false, false) if letter(b'T') => UiCommand::NewTab,
        (true, false, false) if letter(b'W') => UiCommand::CloseTab(id),
        (true, false, false) if letter(b'L') => UiCommand::FocusLocation,
        (true, false, false) if letter(b'D') => UiCommand::ToggleBookmark,
        (true, false, false) if letter(b'H') => UiCommand::TogglePanel(Panel::History),
        (true, false, false) if letter(b'J') => UiCommand::TogglePanel(Panel::Downloads),
        (true, false, false) if key == 0x09 => UiCommand::CycleTab(1),
        (true, true, false) if key == 0x09 => UiCommand::CycleTab(-1),
        (true, true, false) if letter(b'T') => UiCommand::ReopenClosedTab,
        (true, true, false) if letter(b'Q') => UiCommand::Quit,
        (true, false, false) if (u32::from(b'1')..=u32::from(b'8')).contains(&key) => {
            UiCommand::ActivateIndex((key - u32::from(b'1')) as usize)
        },
        (true, false, false) if letter(b'9') => UiCommand::ActivateIndex(usize::MAX),
        (false, false, true) if letter(b'D') => UiCommand::FocusLocation,
        (false, false, false) if key == 0x75 => UiCommand::FocusLocation, // F6
        (false, false, false) if key == 0x7A => UiCommand::ToggleFullscreen, // F11
        _ => return None,
    })
}
