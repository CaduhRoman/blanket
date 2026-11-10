//! Applying engine notifications to browser state.

use std::time::Instant;

use blanket_core::ids::TabId;
use blanket_core::security::{PermissionDecision, PermissionKind};
use blanket_core::tabs::TabLifecycle;
use blanket_core::time::unix_now;
use servo::{EmbedderControl, LoadStatus, PermissionFeature, PermissionRequest};
use serde_json::json;
use tracing::{debug, info};

use super::Browser;
use super::delegate::ServoEvent;
use super::maintenance::Task;
use crate::ui::dialogs::Dialog;

impl Browser {
    pub(super) fn process_servo_events(&mut self) {
        let events = self.delegate.drain();
        for (webview_id, event) in events {
            let tab = self.by_webview.get(&webview_id).copied();
            match (event, tab) {
                (ServoEvent::Opened(webview), parent) => self.adopt_opened_webview(parent, webview),
                (event, Some(tab)) => self.handle_tab_event(tab, event),
                // Events of web views that were closed in the meantime. Requests inside
                // them answer themselves with their default on drop.
                (_, None) => {},
            }
        }
    }

    fn handle_tab_event(&mut self, id: TabId, event: ServoEvent) {
        let is_active = self.tabs.active_id() == Some(id);
        match event {
            ServoEvent::UrlChanged(url) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.url = Some(url.clone());
                }
                if is_active {
                    self.gui_mut().sync_location(&url);
                    self.request_redraw();
                }
                self.session_dirty = true;
            },
            ServoEvent::TitleChanged(title) => {
                let title = title.unwrap_or_default();
                if let Some(tab) = self.tabs.get_mut(id) {
                    if let Some(url) = &tab.url {
                        self.history.set_title(url, &title);
                    }
                    tab.title = title;
                }
                let title = self.tabs.get(id).map(|tab| tab.title.clone()).unwrap_or_default();
                self.gui_mut().note_text(&title);
                self.session_dirty = true;
                self.request_redraw();
            },
            ServoEvent::StatusText(text) => {
                if let Some(view) = self.views.get_mut(&id) {
                    view.status_text = text;
                }
                if is_active {
                    self.request_redraw();
                }
            },
            ServoEvent::LoadStatus(status) => self.on_load_status(id, status),
            ServoEvent::FaviconChanged => {
                let image = self.views.get(&id).and_then(|view| view.webview.favicon().map(|image| crate::ui::to_color_image(&image)));
                if let Some(image) = image {
                    self.gui_mut().set_favicon(id, image);
                    self.request_redraw();
                }
            },
            ServoEvent::NewFrame => {
                if let Some(view) = self.views.get_mut(&id) {
                    view.painted = true;
                }
                if let Some(started) = self.restoring.get(&id).copied() {
                    if is_active && self.metrics.enabled() {
                        let ms = started.elapsed().as_secs_f64() * 1000.0;
                        self.metrics.record("tab_restore_first_frame", json!({ "tab": id.0, "ms": ms }));
                    }
                }
                if is_active {
                    self.request_redraw();
                }
            },
            ServoEvent::HistoryChanged { entries, current } => {
                let base = self.views.get(&id).map(|view| view.history_base.clone()).unwrap_or_default();
                let offset = base.len();
                let mut full = base;
                full.extend(entries);
                self.tabs.set_history(id, full, offset + current);
                self.session_dirty = true;
                if is_active {
                    self.request_redraw();
                }
            },
            ServoEvent::Closed => {
                info!(tab = %id, "page closed its tab");
                self.close_tab(id);
            },
            ServoEvent::Crashed(reason) => {
                self.tabs.set_lifecycle(id, TabLifecycle::Crashed { reason });
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.loading = false;
                }
                self.request_redraw();
            },
            ServoEvent::MediaPlaying(playing) => {
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.media_playing = playing;
                }
                self.request_redraw();
            },
            ServoEvent::Fullscreen(fullscreen) => {
                if is_active {
                    self.set_content_fullscreen(fullscreen);
                }
            },
            ServoEvent::CursorChanged(cursor) => {
                if let Some(view) = self.views.get_mut(&id) {
                    view.cursor = cursor;
                }
                if is_active && self.input.pointer_over_content {
                    self.window.set_cursor(super::input::winit_cursor(cursor));
                }
            },
            ServoEvent::Permission(request) => self.on_permission_request(id, request),
            ServoEvent::Authentication(request) => {
                let host = request.url().host_str().unwrap_or_default().to_owned();
                self.gui_mut().dialogs.push(id, Dialog::authentication(host, request));
                self.request_redraw();
            },
            ServoEvent::Unload(request) => {
                self.gui_mut().dialogs.push(id, Dialog::unload(request));
                self.request_redraw();
            },
            ServoEvent::ShowControl(control) => self.show_embedder_control(id, control),
            ServoEvent::HideControl(control_id) => {
                if self.input.ime_control == Some(control_id) {
                    self.input.ime_control = None;
                    self.window.set_ime_allowed(false);
                }
                self.gui_mut().dialogs.remove_control(id, control_id);
                self.request_redraw();
            },
            ServoEvent::Download { url, referrer } => self.start_download(url, referrer),
            ServoEvent::Opened(_) => unreachable!("handled by process_servo_events"),
        }
    }

    fn on_load_status(&mut self, id: TabId, status: LoadStatus) {
        let loading = matches!(status, LoadStatus::Started | LoadStatus::HeadParsed);
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.loading = loading;
        }
        match status {
            LoadStatus::Started => {
                if let Some(view) = self.views.get_mut(&id) {
                    view.nav_started = Some(Instant::now());
                }
                self.gui_mut().on_load_started(id);
            },
            LoadStatus::Complete => self.on_load_complete(id),
            LoadStatus::HeadParsed | LoadStatus::Stopped => {},
        }
        if self.tabs.active_id() == Some(id) {
            self.request_redraw();
        }
    }

    fn on_load_complete(&mut self, id: TabId) {
        let url = self.tabs.get(id).and_then(|tab| tab.url.clone());
        let (typed, started) = match self.views.get_mut(&id) {
            Some(view) => (std::mem::take(&mut view.typed), view.nav_started.take()),
            None => (false, None),
        };
        if let Some(url) = &url {
            if self.history.record_visit(url, typed, unix_now()) {
                if let Some(title) = self.tabs.get(id).map(|tab| tab.title.clone()).filter(|title| !title.is_empty()) {
                    self.history.set_title(url, &title);
                }
                self.schedule_soon(Task::SaveHistory);
                self.schedule_soon(Task::RefreshNewTab);
            }
        }
        let load_ms = started.map(|started| started.elapsed().as_secs_f64() * 1000.0);
        debug!(tab = %id, url = url.as_ref().map(|url| url.as_str()).unwrap_or(""), ?load_ms, "load complete");
        if self.metrics.enabled() {
            self.metrics.record(
                "load_complete",
                json!({
                    "tab": id.0,
                    "url": url.as_ref().map(|url| url.as_str()),
                    "load_ms": load_ms,
                }),
            );
        }
        if let Some(started) = self.restoring.remove(&id) {
            let ms = started.elapsed().as_secs_f64() * 1000.0;
            info!(tab = %id, restore_ms = format!("{ms:.0}"), "suspended tab restored");
            self.metrics.record("tab_restored", json!({ "tab": id.0, "ms": ms }));
            if let Some((x, y)) = self.saved_scroll.remove(&id) {
                if let Some(view) = self.views.get(&id) {
                    view.webview.evaluate_javascript(format!("window.scrollTo({x}, {y});"), |_| {});
                }
            }
        }
    }

    fn on_permission_request(&mut self, id: TabId, request: PermissionRequest) {
        let kind = permission_kind(request.feature());
        let Some(url) = self.tabs.get(id).and_then(|tab| tab.url.clone()) else {
            request.deny();
            return;
        };
        match self.permissions.decision(&url, kind) {
            Some(PermissionDecision::Allow) => request.allow(),
            Some(PermissionDecision::Deny) => request.deny(),
            None => {
                self.gui_mut().dialogs.push(id, Dialog::permission(url, kind, request));
                self.request_redraw();
            },
        }
    }

    fn show_embedder_control(&mut self, id: TabId, control: EmbedderControl) {
        match control {
            EmbedderControl::InputMethod(input_method) => {
                let scale = self.scale().get();
                let origin = self.gui_mut().content_origin();
                let position = input_method.position();
                self.input.ime_control = Some(input_method.id());
                self.window.set_ime_allowed(true);
                self.window.set_ime_cursor_area(
                    winit::dpi::LogicalPosition::new(
                        f64::from(origin.x) + f64::from(position.min.x) / f64::from(scale),
                        f64::from(origin.y) + f64::from(position.min.y) / f64::from(scale),
                    ),
                    winit::dpi::LogicalSize::new(
                        f64::from(position.max.x - position.min.x) / f64::from(scale),
                        f64::from(position.max.y - position.min.y) / f64::from(scale),
                    ),
                );
            },
            EmbedderControl::FilePicker(picker) => {
                let dialog = Dialog::file_picker(picker, &self.window, self.waker.as_task_waker());
                self.gui_mut().dialogs.push(id, dialog);
            },
            other => {
                let dialog = Dialog::from_control(other);
                self.gui_mut().dialogs.push(id, dialog);
            },
        }
        self.request_redraw();
    }

    pub(super) fn poll_file_dialogs(&mut self) {
        if self.gui_mut().dialogs.poll_file_pickers() {
            self.request_redraw();
        }
    }
}

fn permission_kind(feature: PermissionFeature) -> PermissionKind {
    match feature {
        PermissionFeature::Geolocation => PermissionKind::Geolocation,
        PermissionFeature::Notifications | PermissionFeature::Push => PermissionKind::Notifications,
        PermissionFeature::Camera => PermissionKind::Camera,
        PermissionFeature::Microphone | PermissionFeature::Speaker => PermissionKind::Microphone,
        PermissionFeature::Bluetooth => PermissionKind::Bluetooth,
        _ => PermissionKind::Other,
    }
}
