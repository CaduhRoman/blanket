//! User-initiated navigation: address bar, back/forward, reload, clearing data.

use blanket_core::ids::TabId;
use blanket_core::tabs::{OpenPosition, TabLifecycle};
use blanket_core::time::unix_now;
use tracing::{info, warn};
use url::Url;

use super::Browser;
use super::maintenance::Task;
use crate::ui::{ClearDataRequest, ClearRange, Disposition};

impl Browser {
    pub(super) fn navigate_input(&mut self, input: &str, disposition: Disposition) {
        match self.resolve_omnibox(input) {
            Some(target) => self.open_url(target.into_url(), disposition, true),
            None => warn!(%input, "address bar input did not resolve to a URL"),
        }
    }

    pub(super) fn open_url(&mut self, url: Url, disposition: Disposition, typed: bool) {
        match disposition {
            Disposition::CurrentTab => match self.tabs.active_id() {
                Some(id) => self.load_in_tab(id, url, typed),
                None => {
                    self.open_tab(url, OpenPosition::End, true);
                },
            },
            Disposition::NewTab => {
                let id = self.open_tab(url, OpenPosition::AfterActive, true);
                self.mark_typed(id, typed);
            },
            Disposition::BackgroundTab => {
                let id = self.open_tab(url, OpenPosition::AfterActive, false);
                self.mark_typed(id, typed);
            },
        }
    }

    fn mark_typed(&mut self, id: TabId, typed: bool) {
        if let Some(view) = self.views.get_mut(&id) {
            view.typed = typed;
        } else if typed && self.compat.has_view(id) {
            self.compat_typed.insert(id);
        }
    }

    pub(super) fn load_in_tab(&mut self, id: TabId, url: Url, typed: bool) {
        let crashed = self.tabs.get(id).is_some_and(|tab| matches!(tab.lifecycle, TabLifecycle::Crashed { .. }));
        if crashed {
            self.drop_view(id);
        }
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.url = Some(url.clone());
        }
        // The engine is chosen per URL: stay in the current one, or move the tab across.
        let wants_compat = self.wants_compat(&url);
        if wants_compat && self.compat.has_view(id) {
            self.compat.navigate(id, &url);
            self.mark_typed(id, typed);
            self.request_redraw();
            return;
        }
        if wants_compat || self.compat.has_view(id) {
            self.create_view(id, url, true);
            self.mark_typed(id, typed);
            if Some(id) == self.tabs.active_id() {
                self.show_active(None, id);
            }
            self.request_redraw();
            return;
        }
        match self.views.get_mut(&id) {
            Some(view) => {
                self.delegate.expect_user_navigation(view.webview.id(), url.clone());
                view.typed = typed;
                view.webview.load(url);
            },
            None => {
                self.create_view(id, url, true);
                self.mark_typed(id, typed);
                if Some(id) == self.tabs.active_id() {
                    self.show_active(None, id);
                }
            },
        }
        self.request_redraw();
    }

    pub(super) fn go_back(&mut self) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        if self.compat.has_view(id) {
            self.compat.go_back(id);
            return;
        }
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        if view.webview.can_go_back() {
            view.webview.go_back(1);
        } else if let Some(url) = view.history_base.pop() {
            // Earlier entries of a restored tab: navigate to them explicitly.
            self.delegate.expect_user_navigation(view.webview.id(), url.clone());
            view.webview.load(url);
        }
    }

    pub(super) fn go_forward(&mut self) {
        if let Some(id) = self.tabs.active_id().filter(|id| self.compat.has_view(*id)) {
            self.compat.go_forward(id);
            return;
        }
        if let Some(view) = self.active_view() {
            if view.webview.can_go_forward() {
                view.webview.go_forward(1);
            }
        }
    }

    pub(super) fn reload(&mut self) {
        let Some(id) = self.tabs.active_id() else {
            return;
        };
        let crashed = self.tabs.get(id).is_some_and(|tab| matches!(tab.lifecycle, TabLifecycle::Crashed { .. }));
        if crashed {
            info!(tab = %id, "reloading crashed tab in a new web view");
            self.drop_view(id);
            self.tabs.set_lifecycle(id, TabLifecycle::Live);
            let url = self.tabs.get(id).and_then(|tab| tab.url.clone()).unwrap_or_else(|| self.new_tab_url());
            self.create_view(id, url, true);
            self.show_active(None, id);
            return;
        }
        if self.compat.has_view(id) {
            self.compat.reload(id);
        } else if let Some(view) = self.views.get(&id) {
            view.webview.reload();
        } else {
            self.ensure_live(id);
            self.show_active(None, id);
        }
    }

    pub(super) fn clear_browsing_data(&mut self, request: ClearDataRequest) {
        let since = match request.range {
            ClearRange::LastHour => unix_now().saturating_sub(3_600),
            ClearRange::LastDay => unix_now().saturating_sub(86_400),
            ClearRange::Everything => 0,
        };
        if request.history {
            if since == 0 {
                self.history.clear();
            } else {
                self.history.clear_since(since);
            }
            self.schedule_soon(Task::SaveHistory);
            self.refresh_newtab_data();
        }
        if let Some(servo) = &self.servo {
            if request.cookies {
                // Servo's cookie store has no time-range deletion; cookies are cleared fully.
                servo.site_data_manager().clear_cookies(None);
            }
            if request.cache {
                servo.network_manager().clear_cache();
            }
        }
        if request.permissions {
            self.permissions.clear();
            self.schedule_soon(Task::SavePermissions);
        }
        info!(?request, "browsing data cleared");
        self.gui_mut().toast("Browsing data cleared".to_owned());
    }
}
