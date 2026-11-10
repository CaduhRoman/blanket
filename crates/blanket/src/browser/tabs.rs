//! Tab Manager integration: creating, showing, hiding and closing web views.
//!
//! Only the active tab's web view is shown. Background web views are hidden, which makes
//! Servo's painter skip them entirely, and they never drive the frame loop.

use std::rc::Rc;
use std::time::Instant;

use blanket_core::ids::TabId;
use blanket_core::tabs::{OpenPosition, TabLifecycle};
use servo::{RenderingContext, WebViewBuilder};
use tracing::{debug, info};
use url::Url;

use super::{Browser, LiveView};
use super::maintenance::Task;

impl Browser {
    /// Create (or recreate) the web view for `id`, loading `url`. The view starts hidden.
    pub(super) fn create_view(&mut self, id: TabId, url: Url, user_initiated: bool) {
        // Sites Servo cannot run yet open in compatibility mode, automatically.
        if self.wants_compat(&url) && self.create_compat_view(id, &url) {
            return;
        }
        self.compat.close(id);
        let Some(servo) = &self.servo else {
            return;
        };
        if let Some(old) = self.views.remove(&id) {
            self.by_webview.remove(&old.webview.id());
            self.delegate.forget_webview(old.webview.id());
        }
        let rendering_context: Rc<dyn RenderingContext> = self.offscreen.clone();
        let webview = WebViewBuilder::new(servo, rendering_context)
            .url(url.clone())
            .hidpi_scale_factor(self.scale())
            .delegate(self.delegate.clone())
            .build();
        webview.hide();
        let theme = crate::ui::theme::resolve_theme(self.config.ui.theme, self.window.theme());
        webview.notify_theme_change(theme);
        if user_initiated {
            self.delegate.expect_user_navigation(webview.id(), url);
        }
        debug!(tab = %id, webview = ?webview.id(), "web view created");
        self.by_webview.insert(webview.id(), id);
        let mut view = LiveView::new(webview);
        view.nav_started = Some(Instant::now());
        self.views.insert(id, view);
        self.tabs.set_lifecycle(id, TabLifecycle::Live);
    }

    /// Open a tab with a web view loading `url`.
    pub(super) fn open_tab(&mut self, url: Url, position: OpenPosition, activate: bool) -> TabId {
        let previous = self.tabs.active_id();
        let id = self.tabs.open(Some(url.clone()), position, activate, Instant::now());
        self.create_view(id, url, true);
        if activate || previous.is_none() {
            self.show_active(previous, id);
        }
        self.session_dirty = true;
        id
    }

    pub(super) fn activate_tab(&mut self, id: TabId) {
        let previous = self.tabs.activate(id, Instant::now());
        if previous.is_none() {
            return;
        }
        self.show_active(previous, id);
    }

    /// Switch the visible web view from `previous` to `id` (already active in the
    /// `TabManager`), restoring `id` if it was suspended.
    pub(super) fn show_active(&mut self, previous: Option<TabId>, id: TabId) {
        if let Some(previous) = previous.filter(|previous| *previous != id) {
            self.capture_thumbnail(previous);
            if let Some(view) = self.views.get(&previous) {
                view.webview.set_focused(false);
                view.webview.hide();
            }
        }
        self.ensure_live(id);
        if let Some(view) = self.views.get(&id) {
            view.webview.show();
            view.webview.set_focused(true);
            self.window.set_cursor(super::input::winit_cursor(view.cursor));
        }
        if self.compat.has_view(id) {
            self.compat.focus(id);
        }
        let url = self.tabs.get(id).and_then(|tab| tab.url.clone());
        self.gui_mut().on_tab_switched(url.as_ref());
        self.session_dirty = true;
        self.request_redraw();
    }

    /// Recreate the web view of a suspended or crashed tab.
    pub(super) fn ensure_live(&mut self, id: TabId) {
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let needs_view = match &tab.lifecycle {
            TabLifecycle::Live => !self.views.contains_key(&id) && !self.compat.has_view(id),
            TabLifecycle::Suspended { .. } => true,
            TabLifecycle::Crashed { .. } => false,
        };
        if !needs_view {
            return;
        }
        let url = tab.url.clone().unwrap_or_else(|| self.new_tab_url());
        // Entries before the current one remain reachable through the browser-side list.
        let history_base = if tab.history.is_empty() {
            Vec::new()
        } else {
            tab.history[..tab.history_index.min(tab.history.len() - 1)].to_vec()
        };
        info!(tab = %id, %url, "restoring tab");
        self.pending_suspensions.remove(&id);
        self.restoring.insert(id, Instant::now());
        self.create_view(id, url, true);
        if let Some(view) = self.views.get_mut(&id) {
            view.history_base = history_base;
        }
    }

    pub(super) fn close_tab(&mut self, id: TabId) {
        let Some(outcome) = self.tabs.close(id, Instant::now()) else {
            return;
        };
        self.drop_view(id);
        self.gui_mut().forget_tab(id);
        self.restoring.remove(&id);
        self.saved_scroll.remove(&id);
        self.pending_suspensions.remove(&id);
        if outcome.now_empty {
            // Closing the last tab leaves a fresh new tab rather than closing the window,
            // so a stray Ctrl+W never quits the browser.
            let url = self.new_tab_url();
            self.open_tab(url, OpenPosition::End, true);
        } else if let Some(next) = outcome.newly_active {
            self.show_active(None, next);
        }
        self.session_dirty = true;
        self.schedule_soon(Task::SaveSession);
    }

    pub(super) fn reopen_closed_tab(&mut self) {
        let previous = self.tabs.active_id();
        let Some(id) = self.tabs.reopen_closed(Instant::now()) else {
            return;
        };
        // Recreate lazily through the suspended path so the back list is preserved.
        self.tabs.set_lifecycle(
            id,
            TabLifecycle::Suspended {
                since: Instant::now(),
                reason: blanket_core::tabs::SuspendReason::User,
            },
        );
        self.show_active(previous, id);
    }

    /// Release the engine resources of a tab. Dropping the last `WebView` handle closes
    /// it in Servo's constellation.
    pub(super) fn drop_view(&mut self, id: TabId) {
        self.compat.close(id);
        self.compat_typed.remove(&id);
        if let Some(view) = self.views.remove(&id) {
            let webview_id = view.webview.id();
            self.by_webview.remove(&webview_id);
            self.delegate.forget_webview(webview_id);
            self.gui_mut().dialogs.remove_tab(id);
        }
    }

    /// A web view opened by page content (`window.open`, `target=_blank`).
    pub(super) fn adopt_opened_webview(&mut self, parent: Option<TabId>, webview: servo::WebView) {
        let previous = self.tabs.active_id();
        if let Some(parent) = parent.filter(|parent| Some(*parent) != previous) {
            self.tabs.activate(parent, Instant::now());
        }
        let id = self.tabs.open(None, OpenPosition::AfterActive, true, Instant::now());
        webview.hide();
        self.by_webview.insert(webview.id(), id);
        let mut view = LiveView::new(webview);
        view.nav_started = Some(Instant::now());
        self.views.insert(id, view);
        self.tabs.set_lifecycle(id, TabLifecycle::Live);
        self.show_active(previous, id);
    }
}
