//! Smart Tab Suspension: releasing a background tab's engine resources without losing
//! what the user cares about.
//!
//! Before a tab is suspended the page is asked (through `evaluate_javascript`) whether a
//! form contains user edits. Tabs with unsaved input, tabs whose state cannot be
//! inspected (the script fails), and tabs that became active meanwhile are left alone.
//! The scroll position is saved and restored after the page is recreated. A thumbnail
//! captured when the tab was last visible is shown while it reloads.

use std::time::Instant;

use blanket_core::ids::TabId;
use blanket_core::tabs::{SuspendReason, TabLifecycle};
use serde_json::json;
use servo::{DeviceIntRect, DeviceIntSize, JSValue, RenderingContext, WebViewId};
use tracing::{debug, info, warn};

use super::Browser;

/// Returns `[hasUnsavedInput, scrollX, scrollY]`.
const INSPECT_PAGE_SCRIPT: &str = r#"(function () {
  function edited(element) {
    if (element.disabled || element.readOnly) return false;
    var tag = element.tagName;
    if (tag === "TEXTAREA") return element.value !== element.defaultValue;
    if (tag === "SELECT") {
      for (var i = 0; i < element.options.length; i++) {
        var option = element.options[i];
        if (option.selected !== option.defaultSelected) return true;
      }
      return false;
    }
    var type = (element.type || "text").toLowerCase();
    if (["hidden", "submit", "button", "reset", "image", "file"].indexOf(type) >= 0) return false;
    if (type === "checkbox" || type === "radio") return element.checked !== element.defaultChecked;
    return element.value !== element.defaultValue;
  }
  var dirty = false;
  var fields = document.querySelectorAll("input, textarea, select");
  for (var i = 0; i < fields.length && !dirty; i++) dirty = edited(fields[i]);
  var active = document.activeElement;
  if (!dirty && active && active.isContentEditable) dirty = true;
  return [dirty, window.scrollX || 0, window.scrollY || 0];
})()"#;

/// Results of asynchronous engine requests, delivered during `spin_event_loop`.
pub enum AsyncResult {
    SuspendCheck {
        tab: TabId,
        webview: WebViewId,
        reason: SuspendReason,
        /// `None` if the page could not be inspected.
        outcome: Option<PageState>,
    },
}

pub struct PageState {
    pub has_unsaved_input: bool,
    pub scroll: (f64, f64),
}

fn parse_page_state(value: JSValue) -> Option<PageState> {
    let JSValue::Array(values) = value else {
        return None;
    };
    match values.as_slice() {
        [JSValue::Boolean(dirty), JSValue::Number(x), JSValue::Number(y)] => Some(PageState {
            has_unsaved_input: *dirty,
            scroll: (*x, *y),
        }),
        _ => None,
    }
}

/// Longest edge of stored thumbnails, in pixels.
const THUMBNAIL_WIDTH: u32 = 480;

impl Browser {
    /// Start suspending `id`. The suspension completes in [`Self::drain_async_results`]
    /// once the page reports that nothing would be lost.
    pub(super) fn request_suspend(&mut self, id: TabId, reason: SuspendReason) {
        if self.tabs.active_id() == Some(id) || self.pending_suspensions.contains(&id) {
            return;
        }
        let Some(view) = self.views.get(&id) else {
            return;
        };
        if !self.tabs.get(id).is_some_and(|tab| tab.lifecycle.is_live()) {
            return;
        }
        let webview = view.webview.id();
        let results = self.async_results.clone();
        self.pending_suspensions.insert(id);
        view.webview.evaluate_javascript(INSPECT_PAGE_SCRIPT, move |result| {
            let outcome = result.ok().and_then(parse_page_state);
            results.borrow_mut().push(AsyncResult::SuspendCheck {
                tab: id,
                webview,
                reason,
                outcome,
            });
        });
    }

    pub(super) fn drain_async_results(&mut self) {
        let results = std::mem::take(&mut *self.async_results.borrow_mut());
        for result in results {
            match result {
                AsyncResult::SuspendCheck {
                    tab,
                    webview,
                    reason,
                    outcome,
                } => self.finish_suspend(tab, webview, reason, outcome),
            }
        }
    }

    fn finish_suspend(&mut self, id: TabId, webview: WebViewId, reason: SuspendReason, outcome: Option<PageState>) {
        if !self.pending_suspensions.remove(&id) {
            return;
        }
        let still_eligible = self.tabs.active_id() != Some(id) &&
            self.views.get(&id).is_some_and(|view| view.webview.id() == webview) &&
            self.tabs.get(id).is_some_and(|tab| tab.lifecycle.is_live() && !tab.media_playing);
        if !still_eligible {
            debug!(tab = %id, "suspension skipped: tab changed state");
            return;
        }
        let Some(state) = outcome else {
            debug!(tab = %id, "suspension skipped: page state unavailable");
            if reason == SuspendReason::User {
                self.gui_mut().toast("This tab cannot be suspended right now".to_owned());
            }
            return;
        };
        if state.has_unsaved_input {
            info!(tab = %id, "suspension skipped: the page has unsaved input");
            if reason == SuspendReason::User {
                self.gui_mut().toast("Not suspended: the page has unsaved input".to_owned());
            }
            return;
        }
        if state.scroll != (0.0, 0.0) {
            self.saved_scroll.insert(id, state.scroll);
        }
        self.drop_view(id);
        self.tabs.set_lifecycle(
            id,
            TabLifecycle::Suspended {
                since: Instant::now(),
                reason,
            },
        );
        info!(tab = %id, ?reason, "tab suspended");
        self.metrics.record("tab_suspended", json!({ "tab": id.0, "reason": format!("{reason:?}") }));
        self.session_dirty = true;
        self.request_redraw();
    }

    /// Keep a small picture of a tab that is about to leave the foreground. The offscreen
    /// framebuffer still holds its last frame, so this is a synchronous read-back.
    pub(super) fn capture_thumbnail(&mut self, id: TabId) {
        if !self.config.memory.suspension_enabled || !self.views.get(&id).is_some_and(|view| view.painted) {
            return;
        }
        let started = Instant::now();
        if let Err(error) = self.window_context.make_current() {
            warn!(?error, "cannot make the GL context current for a thumbnail");
            return;
        }
        let size = self.offscreen.size();
        if size.width < 2 || size.height < 2 {
            return;
        }
        let rect = DeviceIntRect::from_size(DeviceIntSize::new(size.width as i32, size.height as i32));
        let Some(image) = self.offscreen.read_to_image(rect) else {
            return;
        };
        let (pixels, width, height) = downscale_rgba(image.as_raw(), image.width(), image.height(), THUMBNAIL_WIDTH);
        self.gui_mut().set_thumbnail(id, pixels, width, height);
        debug!(tab = %id, ms = started.elapsed().as_secs_f64() * 1000.0, "thumbnail captured");
    }
}

/// Box-filter downscale of tightly packed RGBA pixels so that the width is at most
/// `max_width`. Returns the new pixels and dimensions.
pub fn downscale_rgba(pixels: &[u8], width: u32, height: u32, max_width: u32) -> (Vec<u8>, u32, u32) {
    if width <= max_width || width == 0 || height == 0 {
        return (pixels.to_vec(), width, height);
    }
    let factor = width.div_ceil(max_width);
    let out_width = width / factor;
    let out_height = (height / factor).max(1);
    let mut out = vec![0u8; (out_width * out_height * 4) as usize];
    let samples = factor * factor;
    for oy in 0..out_height {
        for ox in 0..out_width {
            let mut sum = [0u32; 4];
            for dy in 0..factor {
                let row = ((oy * factor + dy) * width) as usize;
                for dx in 0..factor {
                    let index = (row + (ox * factor + dx) as usize) * 4;
                    for channel in 0..4 {
                        sum[channel] += u32::from(pixels[index + channel]);
                    }
                }
            }
            let index = ((oy * out_width + ox) * 4) as usize;
            for channel in 0..4 {
                out[index + channel] = (sum[channel] / samples) as u8;
            }
        }
    }
    (out, out_width, out_height)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn page_state_parsing() {
        let state = parse_page_state(JSValue::Array(vec![
            JSValue::Boolean(true),
            JSValue::Number(0.0),
            JSValue::Number(120.5),
        ]))
        .unwrap();
        assert!(state.has_unsaved_input);
        assert_eq!(state.scroll, (0.0, 120.5));
        assert!(parse_page_state(JSValue::Null).is_none());
        assert!(parse_page_state(JSValue::Object(HashMap::new())).is_none());
        assert!(parse_page_state(JSValue::Array(vec![JSValue::String("x".into())])).is_none());
    }

    #[test]
    fn downscale_averages_blocks() {
        // 4x2 image: left half black, right half white.
        let mut pixels = Vec::new();
        for _ in 0..2 {
            for x in 0..4 {
                let value = if x < 2 { 0 } else { 255 };
                pixels.extend_from_slice(&[value, value, value, 255]);
            }
        }
        let (out, width, height) = downscale_rgba(&pixels, 4, 2, 2);
        assert_eq!((width, height), (2, 1));
        assert_eq!(&out[..4], &[0, 0, 0, 255]);
        assert_eq!(&out[4..], &[255, 255, 255, 255]);
        let (same, width, _) = downscale_rgba(&pixels, 4, 2, 8);
        assert_eq!((same.len(), width), (pixels.len(), 4));
    }
}
