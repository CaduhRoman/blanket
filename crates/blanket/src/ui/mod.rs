//! The browser chrome, drawn with egui around and over the web content.
//!
//! The UI never talks to Servo directly: it renders a [`UiModel`] snapshot and returns
//! [`UiCommand`]s for the browser to execute. The web content itself is Servo's
//! offscreen framebuffer, blitted into the central area by a GL paint callback.

pub mod dialogs;
pub mod icons;
mod panels;
pub mod theme;
mod toolbar;
mod widgets;

use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use blanket_core::bookmarks::Bookmarks;
use blanket_core::cache::{ByteLruCache, CacheStats, Weighted};
use blanket_core::config::BlanketConfig;
use blanket_core::downloads::DownloadList;
use blanket_core::history::History;
use blanket_core::ids::{DownloadId, TabId};
use blanket_core::security::{ConnectionSecurity, PermissionDecision, PermissionKind, PermissionStore};
use blanket_core::tabs::TabManager;
use egui::{Align2, CentralPanel, Color32, ColorImage, CornerRadius, FontId, Frame, Order, Pos2, Rect, TextureOptions};
use egui_glow::EguiGlow;
use servo::{Image, OffscreenRenderingContext, PixelFormat, RenderingContext};
use url::Url;
use winit::dpi::PhysicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use self::dialogs::Dialogs;
use self::icons::Icon;
use self::theme::Palette;
use self::widgets::{format_bytes, pill_button};
use crate::browser::PerfSnapshot;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    CurrentTab,
    NewTab,
    BackgroundTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ZoomChange {
    In,
    Out,
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearRange {
    LastHour,
    LastDay,
    Everything,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClearDataRequest {
    pub range: ClearRange,
    pub history: bool,
    pub cookies: bool,
    pub cache: bool,
    pub permissions: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    History,
    Bookmarks,
    Downloads,
    Settings,
    Performance,
}

#[derive(Debug, Clone)]
pub enum UiCommand {
    NewTab,
    CloseTab(TabId),
    ActivateTab(TabId),
    CycleTab(isize),
    /// `usize::MAX` selects the last tab.
    ActivateIndex(usize),
    MoveTab(TabId, usize),
    PinTab(TabId, bool),
    DuplicateTab(TabId),
    CloseOtherTabs(TabId),
    ReopenClosedTab,
    SuspendTab(TabId),
    WakeTab(TabId),
    Navigate { input: String, disposition: Disposition },
    OpenUrl { url: Url, disposition: Disposition },
    Back,
    Forward,
    Reload,
    Home,
    FocusLocation,
    ToggleBookmark,
    RemoveBookmark(Url),
    RenameBookmark(Url, String),
    ToggleBookmarksBar,
    RemoveHistory(Url),
    ClearData(ClearDataRequest),
    DownloadUrl { url: Url, referrer: Option<Url> },
    CancelDownload(DownloadId),
    ConfirmDownload(DownloadId, bool),
    ShowDownload(DownloadId),
    RemoveDownload(DownloadId),
    ClearFinishedDownloads,
    SavePage,
    Zoom(ZoomChange),
    ToggleFullscreen,
    ExitFullscreen,
    TogglePanel(Panel),
    RememberPermission { url: Url, kind: PermissionKind, decision: PermissionDecision },
    ForgetPermissions(Url),
    UpdateConfig(Box<BlanketConfig>),
    /// The address bar text changed (used for conservative DNS pre-resolution).
    OmniboxInput(String),
    RefreshPerformance,
    FocusContent,
    /// Window controls drawn in the tab strip (the window has no system title bar).
    MinimizeWindow,
    ToggleMaximizeWindow,
    /// Start moving the window with the pointer (drag on empty title bar space).
    DragWindow,
    /// Show the system window menu at a point in the window (points).
    WindowMenu(Pos2),
    Quit,
}

/// State of the active tab as the chrome needs it.
pub struct ActiveView {
    pub id: Option<TabId>,
    pub url: Option<Url>,
    pub security: Option<ConnectionSecurity>,
    pub bookmarked: bool,
    pub loading: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub zoom: f32,
    pub status_text: Option<String>,
    pub has_painted_view: bool,
    pub crashed: Option<String>,
}

pub struct UiModel<'a> {
    pub tabs: &'a TabManager,
    pub active: ActiveView,
    pub history: &'a History,
    pub bookmarks: &'a Bookmarks,
    pub downloads: &'a DownloadList,
    pub config: &'a BlanketConfig,
    pub perf: &'a PerfSnapshot,
    pub permissions: &'a PermissionStore,
    pub now_unix: u64,
    /// Content is in fullscreen: no chrome at all.
    pub chrome_hidden: bool,
    pub window_fullscreen: bool,
    pub window_maximized: bool,
}

struct Texture(egui::TextureHandle);

impl Weighted for Texture {
    fn weight_bytes(&self) -> u64 {
        let [width, height] = self.0.size();
        (width * height * 4) as u64
    }
}

struct Toast {
    text: String,
    until: Instant,
}

const TOAST_DURATION: Duration = Duration::from_millis(2800);
const SIDE_PANEL_WIDTH: f32 = 360.0;

pub struct Gui {
    egui: EguiGlow,
    offscreen: Rc<OffscreenRenderingContext>,
    palette: Palette,
    location: toolbar::LocationBar,
    strip: toolbar::TabStrip,
    panel: Option<Panel>,
    panels: panels::PanelState,
    pub dialogs: Dialogs,
    toasts: VecDeque<Toast>,
    favicons: ByteLruCache<TabId, Texture>,
    thumbnails: ByteLruCache<TabId, Texture>,
    /// Content area in points, as laid out in the last frame.
    content_rect: Rect,
    active_tab: Option<TabId>,
    extended_fonts: bool,
    /// Earliest repaint egui asked for (animations, caret blink, tooltips).
    repaint_at: Arc<Mutex<Option<Instant>>>,
}

impl Gui {
    pub fn new(
        window: &Window,
        event_loop: &ActiveEventLoop,
        offscreen: Rc<OffscreenRenderingContext>,
        config: &BlanketConfig,
    ) -> Self {
        let gl = offscreen.parent_context().glow_gl_api();
        let egui = EguiGlow::new(event_loop, gl, None, None, false);
        let ctx = egui.egui_ctx.clone();
        theme::install_fonts(&ctx);
        let palette = Palette::resolve(config.ui.theme, window.theme());
        theme::apply_style(&ctx, &palette);
        // Browser zoom shortcuts belong to the page, not to the chrome.
        ctx.options_mut(|options| options.zoom_with_keyboard = false);

        let repaint_at = Arc::new(Mutex::new(None::<Instant>));
        let slot = repaint_at.clone();
        ctx.set_request_repaint_callback(move |info| {
            let Some(at) = Instant::now().checked_add(info.delay) else {
                return;
            };
            let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(slot.map_or(at, |current| current.min(at)));
        });

        Self {
            egui,
            offscreen,
            palette,
            location: toolbar::LocationBar::default(),
            strip: toolbar::TabStrip::default(),
            panel: None,
            panels: panels::PanelState::default(),
            dialogs: Dialogs::default(),
            toasts: VecDeque::new(),
            favicons: ByteLruCache::new(config.cache.favicon_budget_kib * 1024),
            thumbnails: ByteLruCache::new(config.cache.thumbnail_budget_kib * 1024),
            content_rect: Rect::NOTHING,
            active_tab: None,
            extended_fonts: false,
            repaint_at,
        }
    }

    fn ctx(&self) -> &egui::Context {
        &self.egui.egui_ctx
    }

    pub fn on_window_event(&mut self, window: &Window, event: &winit::event::WindowEvent) -> egui_winit::EventResponse {
        self.egui.on_window_event(window, event)
    }

    /// An egui text field has keyboard focus.
    pub fn wants_keyboard(&self) -> bool {
        self.ctx().text_edit_focused()
    }

    /// The OS takes over the mouse during a window move or resize and swallows the
    /// button release; tell egui the button is up so no widget stays pressed.
    pub fn release_pointer(&mut self, window: &Window) {
        // SAFETY: `dummy` is only unsafe because the id is not a real device; egui does
        // not look at device ids.
        let device_id = unsafe { winit::event::DeviceId::dummy() };
        let event = winit::event::WindowEvent::MouseInput {
            device_id,
            state: winit::event::ElementState::Released,
            button: winit::event::MouseButton::Left,
        };
        let _ = self.egui.on_window_event(window, &event);
    }

    pub fn surrender_focus(&mut self) {
        self.ctx().memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
    }

    /// A dialog of the active tab (or a browser-wide one) captures input.
    pub fn modal_open(&self) -> bool {
        self.dialogs.captures_input(self.active_tab)
    }

    pub fn content_origin(&self) -> Pos2 {
        if self.content_rect.is_positive() { self.content_rect.min } else { Pos2::ZERO }
    }

    /// Whether a point (in points) belongs to the web page rather than the chrome or an
    /// overlay drawn on top of it.
    pub fn is_page_point(&self, point: Pos2) -> bool {
        if !self.content_rect.contains(point) || self.ctx().is_using_pointer() {
            return false;
        }
        self.ctx()
            .layer_id_at(point)
            .is_none_or(|layer| layer.order == Order::Background)
    }

    pub fn wants_live_performance(&self) -> bool {
        self.panel == Some(Panel::Performance)
    }

    pub fn toggle_panel(&mut self, panel: Panel) {
        self.panel = if self.panel == Some(panel) { None } else { Some(panel) };
        if self.panel == Some(Panel::Settings) {
            self.panels.settings_draft = None;
        }
        self.ctx().request_repaint();
    }

    pub fn next_repaint(&self) -> Option<Instant> {
        *self.repaint_at.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// egui wants another frame right away (an animation is running).
    pub fn needs_repaint(&self) -> bool {
        self.next_repaint().is_some_and(|at| at <= Instant::now() + Duration::from_millis(1))
    }

    // ----- notifications from the browser ------------------------------------------------

    pub fn sync_location(&mut self, url: &Url) {
        self.location.sync(url);
    }

    pub fn focus_location(&mut self) {
        self.location.request_focus();
        self.ctx().request_repaint();
    }

    pub fn on_tab_switched(&mut self, url: Option<&Url>) {
        self.location.reset(url);
    }

    pub fn on_load_started(&mut self, _id: TabId) {
        self.ctx().request_repaint();
    }

    pub fn set_favicon(&mut self, id: TabId, image: ColorImage) {
        let texture = self.ctx().load_texture(format!("favicon-{id}"), image, TextureOptions::LINEAR);
        self.favicons.insert(id, Texture(texture));
    }

    pub fn set_thumbnail(&mut self, id: TabId, rgba: Vec<u8>, width: u32, height: u32) {
        let image = ColorImage::from_rgba_unmultiplied([width as usize, height as usize], &rgba);
        let texture = self.ctx().load_texture(format!("thumbnail-{id}"), image, TextureOptions::LINEAR);
        self.thumbnails.insert(id, Texture(texture));
    }

    pub fn forget_tab(&mut self, id: TabId) {
        self.favicons.remove(&id);
        self.thumbnails.remove(&id);
    }

    pub fn toast(&mut self, text: String) {
        self.toasts.retain(|toast| toast.text != text);
        self.toasts.push_back(Toast {
            text,
            until: Instant::now() + TOAST_DURATION,
        });
        while self.toasts.len() > 3 {
            self.toasts.pop_front();
        }
        self.ctx().request_repaint();
    }

    /// Make sure `text` can be displayed, loading extra fonts on first need.
    pub fn note_text(&mut self, text: &str) {
        if !self.extended_fonts && theme::needs_extended_fonts(text) {
            self.extended_fonts = true;
            theme::install_extended_fonts(self.ctx());
        }
    }

    pub fn apply_config(&mut self, config: &BlanketConfig, system_theme: Option<winit::window::Theme>) {
        self.palette = Palette::resolve(config.ui.theme, system_theme);
        theme::apply_style(self.ctx(), &self.palette);
        self.favicons.evict_to(config.cache.favicon_budget_kib * 1024);
        self.thumbnails.evict_to(config.cache.thumbnail_budget_kib * 1024);
        self.ctx().request_repaint();
    }

    /// Release part of the chrome's caches under memory pressure.
    pub fn trim_caches(&mut self, keep_fraction: f64) {
        self.favicons.trim(keep_fraction);
        self.thumbnails.trim(keep_fraction);
    }

    pub fn cache_stats(&self) -> (CacheStats, CacheStats) {
        (self.favicons.stats(), self.thumbnails.stats())
    }

    // ----- frame -------------------------------------------------------------------------

    /// Lay out the chrome for this frame. `paint_content` is called with the content
    /// area's size in device pixels; it sizes and paints the active web view into the
    /// offscreen framebuffer and returns whether there is a page to show.
    pub fn update(
        &mut self,
        window: &Window,
        model: &UiModel<'_>,
        paint_content: &mut dyn FnMut(PhysicalSize<u32>) -> bool,
    ) -> Vec<UiCommand> {
        *self.repaint_at.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        let now = Instant::now();
        self.toasts.retain(|toast| toast.until > now);
        self.active_tab = model.active.id;
        if !self.location.editing {
            self.location.show_url(model.active.url.as_ref());
        }

        let mut commands = Vec::new();
        let Self {
            egui,
            offscreen,
            palette,
            location,
            strip,
            panel,
            panels,
            dialogs,
            toasts,
            favicons,
            thumbnails,
            content_rect,
            ..
        } = self;
        let palette = *palette;
        egui.run(window, |ui| {
            let ctx = ui.ctx().clone();
            let time = ctx.input(|input| input.time);
            if !model.chrome_hidden {
                toolbar::show(
                    ui,
                    &mut toolbar::Toolbar {
                        palette: &palette,
                        model,
                        location,
                        strip,
                        favicons,
                        thumbnails,
                        open_panel: *panel,
                        commands: &mut commands,
                        time,
                    },
                );
                if let Some(open) = *panel {
                    let close = panels::show(ui, open, &palette, model, panels, &mut commands, SIDE_PANEL_WIDTH);
                    if close {
                        *panel = None;
                    }
                }
            }

            CentralPanel::default().frame(Frame::NONE.fill(palette.page)).show_inside(ui, |ui| {
                let rect = ui.max_rect();
                *content_rect = rect;
                show_content(ui, rect, offscreen, model, &palette, thumbnails, paint_content, &mut commands, time);
            });

            dialogs.show(&ctx, &palette, *content_rect, model, &mut commands);
            show_overlays(&ctx, &palette, *content_rect, model, toasts);
        });
        commands
    }

    pub fn paint(&mut self, window: &Window) {
        let parent = self.offscreen.parent_context();
        parent.prepare_for_rendering();
        let size = window.inner_size();
        let background = self.palette.strip;
        self.egui.painter.clear(
            [size.width, size.height],
            [
                f32::from(background.r()) / 255.0,
                f32::from(background.g()) / 255.0,
                f32::from(background.b()) / 255.0,
                1.0,
            ],
        );
        self.egui.paint(window);
        parent.present();
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        self.egui.destroy();
    }
}

#[allow(clippy::too_many_arguments)]
fn show_content(
    ui: &mut egui::Ui,
    rect: Rect,
    offscreen: &Rc<OffscreenRenderingContext>,
    model: &UiModel<'_>,
    palette: &Palette,
    thumbnails: &mut ByteLruCache<TabId, Texture>,
    paint_content: &mut dyn FnMut(PhysicalSize<u32>) -> bool,
    commands: &mut Vec<UiCommand>,
    time: f64,
) {
    let pixels_per_point = ui.ctx().pixels_per_point();
    let size = PhysicalSize::new(
        ((rect.width() * pixels_per_point).round() as u32).max(1),
        ((rect.height() * pixels_per_point).round() as u32).max(1),
    );
    let active = &model.active;
    if let Some(reason) = &active.crashed {
        show_crash_page(ui, rect, palette, reason, commands);
        return;
    }
    // Sizing and painting happen before the blit callback is created: a resize replaces
    // the offscreen framebuffer the callback refers to.
    let has_page = paint_content(size);
    if has_page && active.has_painted_view {
        if let Some(blit) = offscreen.render_to_parent_callback() {
            let callback = egui_glow::CallbackFn::new(move |info, painter| {
                let viewport = info.viewport_in_pixels();
                let target = euclid::default::Rect::new(
                    euclid::default::Point2D::new(viewport.left_px, viewport.from_bottom_px),
                    euclid::default::Size2D::new(viewport.width_px, viewport.height_px),
                );
                blit(painter.gl(), target);
            });
            ui.painter().add(egui::PaintCallback {
                rect,
                callback: Arc::new(callback),
            });
        }
        return;
    }

    // Nothing painted yet (new or restoring tab): show the last known picture of the
    // page, if any, under a loading indicator.
    let painter = ui.painter_at(rect);
    let thumbnail = active.id.and_then(|id| thumbnails.get(&id)).map(|texture| texture.0.id());
    if let Some(texture) = thumbnail {
        let uv = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
        painter.image(texture, rect, uv, Color32::from_white_alpha(150));
        painter.rect_filled(rect, CornerRadius::ZERO, palette.page.gamma_multiply(0.35));
    }
    if active.loading || thumbnail.is_some() {
        icons::spinner(&painter, icons::icon_rect(rect.center(), 32.0), palette.accent, time);
        ui.ctx().request_repaint();
    }
}

fn show_crash_page(ui: &mut egui::Ui, rect: Rect, palette: &Palette, reason: &str, commands: &mut Vec<UiCommand>) {
    let card = Rect::from_center_size(rect.center(), egui::vec2(420.0_f32.min(rect.width() - 32.0), 220.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
        Frame::NONE
            .fill(palette.card)
            .corner_radius(CornerRadius::same(16))
            .stroke(egui::Stroke::new(1.0, palette.glass_stroke))
            .inner_margin(egui::Margin::same(24))
            .show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    let (icon_rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
                    icons::paint(ui.painter(), icon_rect, Icon::Warning, palette.warning);
                    ui.heading("This page stopped working");
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(widgets::ellipsize(reason, 160)).color(palette.text_dim).small());
                    ui.add_space(14.0);
                    if pill_button(ui, palette, "Reload", true).clicked() {
                        commands.push(UiCommand::Reload);
                    }
                });
            });
    });
}

fn show_overlays(
    ctx: &egui::Context,
    palette: &Palette,
    content: Rect,
    model: &UiModel<'_>,
    toasts: &VecDeque<Toast>,
) {
    if !content.is_positive() {
        return;
    }
    // Link target under the pointer, bottom left like every browser.
    if let Some(status) = model.active.status_text.as_deref().filter(|text| !text.is_empty()) {
        let font = FontId::proportional(12.0);
        let text = widgets::ellipsize(status, 120);
        egui::Area::new(egui::Id::new("status-text"))
            .order(Order::Foreground)
            .interactable(false)
            .fixed_pos(egui::pos2(content.min.x + 6.0, content.max.y - 28.0))
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.card)
                    .stroke(egui::Stroke::new(1.0, palette.glass_stroke))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .show(ui, |ui| ui.label(egui::RichText::new(text).font(font).color(palette.text_dim)));
            });
    }

    if !toasts.is_empty() {
        egui::Area::new(egui::Id::new("toasts"))
            .order(Order::Foreground)
            .interactable(false)
            .anchor(Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
            .show(ctx, |ui| {
                for toast in toasts {
                    Frame::NONE
                        .fill(palette.card)
                        .stroke(egui::Stroke::new(1.0, palette.glass_stroke))
                        .corner_radius(CornerRadius::same(18))
                        .shadow(egui::Shadow {
                            offset: [0, 6],
                            blur: 18,
                            spread: 0,
                            color: palette.shadow,
                        })
                        .inner_margin(egui::Margin::symmetric(16, 9))
                        .show(ui, |ui| ui.label(&toast.text));
                    ui.add_space(6.0);
                }
            });
        if let Some(next) = toasts.iter().map(|toast| toast.until).min() {
            ctx.request_repaint_after(next.saturating_duration_since(Instant::now()));
        }
    }

    if model.config.ui.show_performance_hud {
        let perf = model.perf;
        let mut lines = Vec::new();
        if let Some(rss) = perf.process_rss {
            lines.push(format!("RAM {}", format_bytes(rss)));
        }
        if let Some(cpu) = perf.process_cpu {
            lines.push(format!("CPU {cpu:.0}%"));
        }
        lines.push(format!(
            "Tabs {} live / {} asleep",
            model.tabs.live_count(),
            model.tabs.suspended_count()
        ));
        egui::Area::new(egui::Id::new("perf-hud"))
            .order(Order::Foreground)
            .interactable(false)
            .fixed_pos(egui::pos2(content.max.x - 190.0, content.min.y + 8.0))
            .show(ctx, |ui| {
                Frame::NONE
                    .fill(palette.card.gamma_multiply(0.92))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(170.0);
                        for line in &lines {
                            ui.label(egui::RichText::new(line).monospace().size(11.5).color(palette.text_dim));
                        }
                    });
            });
    }
}

/// Convert an engine image (favicons) into an egui image.
pub fn to_color_image(image: &Image) -> ColorImage {
    let size = [image.width as usize, image.height as usize];
    let data = image.data();
    let pixels = size[0] * size[1];
    let mut rgba = Vec::with_capacity(pixels * 4);
    match image.format {
        PixelFormat::RGBA8 => rgba.extend_from_slice(&data[..(pixels * 4).min(data.len())]),
        PixelFormat::BGRA8 => {
            for chunk in data.chunks_exact(4).take(pixels) {
                rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], chunk[3]]);
            }
        },
        PixelFormat::RGB8 => {
            for chunk in data.chunks_exact(3).take(pixels) {
                rgba.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        },
        PixelFormat::K8 => {
            for &value in data.iter().take(pixels) {
                rgba.extend_from_slice(&[value, value, value, 255]);
            }
        },
        PixelFormat::KA8 => {
            for chunk in data.chunks_exact(2).take(pixels) {
                rgba.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
        },
    }
    rgba.resize(pixels * 4, 0);
    ColorImage::from_rgba_unmultiplied(size, &rgba)
}
