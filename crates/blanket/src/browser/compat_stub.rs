//! Compatibility mode needs the WebView2 runtime, which only exists on Windows. Elsewhere
//! every tab stays on Servo; this stub keeps the browser code platform-neutral.

use std::path::Path;
use std::rc::Rc;

use blanket_core::config::CompatConfig;
use blanket_core::ids::TabId;
use url::Url;

use crate::ui::UiCommand;

#[allow(dead_code)]
#[derive(Debug)]
pub enum CompatEvent {
    UrlChanged(Url),
    TitleChanged(String),
    LoadStarted,
    LoadFinished { success: bool },
    HistoryChanged,
    OpenNewTab { url: Url, background: bool },
    LeaveCompat(Url),
    Fullscreen(bool),
    PlayingAudio(bool),
    Favicon { width: u32, height: u32, rgba: Vec<u8> },
    Command(UiCommand),
}

pub struct CompatHost;

#[allow(clippy::unused_self)]
impl CompatHost {
    pub fn for_window(_: &winit::window::Window, _: &Path, _: CompatConfig, _: Rc<dyn Fn()>) -> Self {
        Self
    }
    pub fn set_config(&mut self, _: CompatConfig) {}
    pub fn has_view(&self, _: TabId) -> bool {
        false
    }
    pub fn is_available(&self) -> bool {
        false
    }
    pub fn drain_events(&self) -> Vec<(TabId, CompatEvent)> {
        Vec::new()
    }
    pub fn create(&mut self, _: TabId, _: &Url, _: bool) -> bool {
        false
    }
    pub fn close(&mut self, _: TabId) {}
    pub fn close_all(&mut self) {}
    pub fn layout(&mut self, _: Option<TabId>, _: Option<(i32, i32, i32, i32)>) {}
    pub fn navigate(&self, _: TabId, _: &Url) {}
    pub fn go_back(&self, _: TabId) {}
    pub fn go_forward(&self, _: TabId) {}
    pub fn reload(&self, _: TabId) {}
    pub fn can_go_back(&self, _: TabId) -> bool {
        false
    }
    pub fn can_go_forward(&self, _: TabId) -> bool {
        false
    }
    pub fn zoom(&self, _: TabId) -> Option<f32> {
        None
    }
    pub fn set_zoom(&self, _: TabId, _: f32) {}
    pub fn focus(&self, _: TabId) {}
    pub fn release_focus(&self) {}
}
