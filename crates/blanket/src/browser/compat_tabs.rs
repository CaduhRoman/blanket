//! Compatibility tabs in the browser: choosing the engine per URL and applying the events
//! of WebView2 tabs to browser state, like `servo_events` does for Servo tabs.

use blanket_core::ids::TabId;
use blanket_core::tabs::TabLifecycle;
use blanket_core::time::unix_now;
use tracing::{debug, info};
use url::Url;

use super::Browser;
use super::compat::CompatEvent;
use super::maintenance::Task;
use crate::ui::Disposition;

impl Browser {
    /// Whether `url` should be rendered in compatibility mode.
    pub(super) fn wants_compat(&self, url: &Url) -> bool {
        self.compat.is_available() && self.config.compat.applies_to(url)
    }

    /// Give tab `id` a compatibility view loading `url`, replacing a Servo view if it had
    /// one. Returns `false` if the runtime is unavailable (the tab then stays on Servo).
    pub(super) fn create_compat_view(&mut self, id: TabId, url: &Url) -> bool {
        let dark = crate::ui::theme::is_dark(self.config.ui.theme, self.window.theme());
        if !self.compat.create(id, url, dark) {
            if !self.compat.is_available() {
                self.gui_mut()
                    .toast("Compatibility mode needs the Microsoft Edge WebView2 runtime".to_owned());
            }
            return false;
        }
        if let Some(old) = self.views.remove(&id) {
            self.by_webview.remove(&old.webview.id());
            self.delegate.forget_webview(old.webview.id());
            self.gui_mut().dialogs.remove_tab(id);
        }
        info!(tab = %id, %url, "opened in compatibility mode");
        self.tabs.set_lifecycle(id, TabLifecycle::Live);
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.loading = true;
        }
        true
    }

    pub(super) fn process_compat_events(&mut self) {
        for (id, event) in self.compat.drain_events() {
            if self.tabs.get(id).is_none() || !self.compat.has_view(id) {
                continue;
            }
            self.handle_compat_event(id, event);
        }
    }

    fn handle_compat_event(&mut self, id: TabId, event: CompatEvent) {
        let is_active = self.tabs.active_id() == Some(id);
        debug!(tab = %id, ?event, "compat event");
        match event {
            CompatEvent::UrlChanged(url) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.url = Some(url.clone());
                }
                if is_active {
                    self.gui_mut().sync_location(&url);
                }
                self.session_dirty = true;
            },
            CompatEvent::TitleChanged(title) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    if let Some(url) = &tab.url {
                        self.history.set_title(url, &title);
                    }
                    tab.title = title.clone();
                }
                self.gui_mut().note_text(&title);
                self.session_dirty = true;
            },
            CompatEvent::LoadStarted => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.loading = true;
                }
                self.gui_mut().on_load_started(id);
            },
            CompatEvent::LoadFinished { success } => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.loading = false;
                }
                let typed = self.compat_typed.remove(&id);
                let url = self.tabs.get(id).and_then(|tab| tab.url.clone());
                if let (true, Some(url)) = (success, url) {
                    if self.history.record_visit(&url, typed, unix_now()) {
                        if let Some(title) = self.tabs.get(id).map(|tab| tab.title.clone()).filter(|title| !title.is_empty()) {
                            self.history.set_title(&url, &title);
                        }
                        self.schedule_soon(Task::SaveHistory);
                        self.schedule_soon(Task::RefreshNewTab);
                    }
                }
            },
            CompatEvent::HistoryChanged => {},
            CompatEvent::OpenNewTab { url, background } => {
                let disposition = if background { Disposition::BackgroundTab } else { Disposition::NewTab };
                self.open_url(url, disposition, false);
            },
            CompatEvent::LeaveCompat(url) => self.load_in_tab(id, url, false),
            CompatEvent::Fullscreen(fullscreen) => {
                if is_active {
                    self.set_content_fullscreen(fullscreen);
                }
            },
            CompatEvent::PlayingAudio(playing) => {
                // Shows the speaker on the tab, and keeps the memory saver away from it.
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.media_playing = playing;
                }
            },
            CompatEvent::Favicon { width, height, rgba } => {
                let image = egui::ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
                self.gui_mut().set_favicon(id, image);
            },
            CompatEvent::Command(command) => self.execute(command),
        }
        self.request_redraw();
    }
}
