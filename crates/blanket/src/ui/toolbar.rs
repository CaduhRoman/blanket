//! Tab strip, navigation bar (glass buttons and smart address bar) and bookmarks bar.

use blanket_core::cache::ByteLruCache;
use blanket_core::ids::TabId;
use blanket_core::omnibox::{display_text, search_url};
use blanket_core::security::ConnectionSecurity;
use blanket_core::tabs::{Tab, TabLifecycle};
use egui::text::{CCursor, CCursorRange, LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Frame, Id, Key, Margin, Order, Pos2, Rect, Sense, Stroke, StrokeKind, TextEdit,
    Ui, Vec2, pos2, vec2,
};
use url::Url;

use super::icons::{self, Icon};
use super::theme::{PILL, Palette};
use super::widgets::{GlassState, ellipsize, glass_icon_button, glass_icon_button_sized, paint_glass, progress_bar};
use super::{Disposition, Panel, Texture, UiCommand, UiModel, ZoomChange};

const STRIP_HEIGHT: f32 = 40.0;
const NAV_HEIGHT: f32 = 48.0;
const BOOKMARKS_HEIGHT: f32 = 32.0;
const TAB_MAX_WIDTH: f32 = 224.0;
const TAB_MIN_WIDTH: f32 = 44.0;
const PINNED_WIDTH: f32 = 44.0;
const MAX_SUGGESTIONS: usize = 7;

pub struct Toolbar<'a> {
    pub palette: &'a Palette,
    pub model: &'a UiModel<'a>,
    pub location: &'a mut LocationBar,
    pub strip: &'a mut TabStrip,
    pub favicons: &'a mut ByteLruCache<TabId, Texture>,
    pub thumbnails: &'a mut ByteLruCache<TabId, Texture>,
    pub open_panel: Option<super::Panel>,
    pub commands: &'a mut Vec<UiCommand>,
    pub time: f64,
}

pub fn show(ui: &mut Ui, toolbar: &mut Toolbar<'_>) {
    let palette = *toolbar.palette;
    egui::Panel::top("tab-strip")
        .exact_size(STRIP_HEIGHT)
        .frame(Frame::NONE.fill(palette.strip).inner_margin(Margin {
            left: 8,
            right: 8,
            top: 6,
            bottom: 0,
        }))
        .show_inside(ui, |ui| tab_strip(ui, toolbar));
    egui::Panel::top("nav-bar")
        .exact_size(NAV_HEIGHT)
        .frame(Frame::NONE.fill(palette.chrome).inner_margin(Margin::symmetric(8, 0)))
        .show_inside(ui, |ui| nav_bar(ui, toolbar));
    if toolbar.model.config.ui.show_bookmarks_bar {
        egui::Panel::top("bookmarks-bar")
            .exact_size(BOOKMARKS_HEIGHT)
            .frame(Frame::NONE.fill(palette.chrome).inner_margin(Margin::symmetric(10, 0)))
            .show_inside(ui, |ui| bookmarks_bar(ui, toolbar));
    }
    // Hairline between chrome and content.
    let line_y = ui.min_rect().max.y;
    let width = ui.max_rect().x_range();
    ui.painter().hline(width, line_y, Stroke::new(1.0, palette.separator));
}

// ----- tab strip -------------------------------------------------------------------------

#[derive(Default)]
pub struct TabStrip {
    dragging: Option<TabId>,
}

fn tab_strip(ui: &mut Ui, toolbar: &mut Toolbar<'_>) {
    let palette = *toolbar.palette;
    let area = ui.max_rect();
    let tabs = toolbar.model.tabs.tabs();
    let active = toolbar.model.tabs.active_id();
    let new_tab_side = 30.0;
    let available = area.width() - new_tab_side - 8.0;
    let pinned = tabs.iter().filter(|tab| tab.pinned).count();
    let unpinned = tabs.len() - pinned;
    let flexible = (available - pinned as f32 * PINNED_WIDTH).max(0.0);
    let tab_width = if unpinned == 0 {
        TAB_MAX_WIDTH
    } else {
        (flexible / unpinned as f32).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH)
    };

    let mut x = area.min.x;
    let mut slots = Vec::with_capacity(tabs.len());
    for tab in tabs {
        let width = if tab.pinned { PINNED_WIDTH } else { tab_width };
        slots.push(Rect::from_min_size(pos2(x, area.min.y), vec2(width, area.height())));
        x += width;
    }

    let pointer_x = ui.input(|input| input.pointer.hover_pos()).map(|pos| pos.x);
    let mut dragged_rect = None;
    for (index, (tab, slot)) in tabs.iter().zip(&slots).enumerate() {
        let mut rect = *slot;
        let is_dragged = toolbar.strip.dragging == Some(tab.id);
        let response = ui.interact(rect, Id::new(("tab", tab.id)), Sense::click_and_drag());
        if response.drag_started() {
            toolbar.strip.dragging = Some(tab.id);
        }
        if is_dragged && response.dragged() {
            rect = rect.translate(vec2(response.total_drag_delta().map_or(0.0, |delta| delta.x), 0.0));
            rect.min.x = rect.min.x.clamp(area.min.x, area.max.x - rect.width());
            rect.max.x = rect.min.x + slot.width();
            dragged_rect = Some((tab, rect));
            continue;
        }
        if is_dragged && response.drag_stopped() {
            toolbar.strip.dragging = None;
            if let Some(pointer_x) = pointer_x {
                let target = slots
                    .iter()
                    .position(|slot| pointer_x < slot.center().x)
                    .unwrap_or(slots.len().saturating_sub(1));
                let target = if target > index { target.saturating_sub(1).max(index) } else { target };
                if target != index {
                    toolbar.commands.push(UiCommand::MoveTab(tab.id, target));
                }
            }
        }
        let next_is_active = tabs.get(index + 1).is_some_and(|next| Some(next.id) == active);
        paint_tab(ui, toolbar, tab, rect, Some(tab.id) == active, response.hovered(), next_is_active, &response);
        tab_interactions(ui, toolbar, tab, rect, response);
    }
    if let Some((tab, rect)) = dragged_rect {
        let response = ui.interact(rect, Id::new(("tab-drag", tab.id)), Sense::hover());
        paint_tab(ui, toolbar, tab, rect, Some(tab.id) == active, true, false, &response);
        ui.ctx().request_repaint();
    }

    let plus_rect = Rect::from_min_size(
        pos2((x + 4.0).min(area.max.x - new_tab_side), area.min.y + 2.0),
        Vec2::splat(new_tab_side),
    );
    let response = ui
        .scope_builder(egui::UiBuilder::new().max_rect(plus_rect), |ui| {
            glass_icon_button_sized(ui, &palette, Icon::Plus, true, "New tab (Ctrl+T)", new_tab_side - 2.0, true)
        })
        .inner;
    if response.clicked() {
        toolbar.commands.push(UiCommand::NewTab);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_tab(
    ui: &Ui,
    toolbar: &mut Toolbar<'_>,
    tab: &Tab,
    rect: Rect,
    is_active: bool,
    hovered: bool,
    next_is_active: bool,
    response: &egui::Response,
) {
    let palette = toolbar.palette;
    let painter = ui.painter();
    let body = rect.shrink2(vec2(2.0, 0.0));
    if is_active {
        painter.rect_filled(
            body,
            CornerRadius {
                nw: 10,
                ne: 10,
                sw: 0,
                se: 0,
            },
            palette.tab_active,
        );
    } else if hovered || response.is_pointer_button_down_on() {
        painter.rect_filled(body.shrink2(vec2(0.0, 3.0)).translate(vec2(0.0, -2.0)), CornerRadius::same(8), palette.tab_hover);
    } else if !next_is_active {
        painter.vline(rect.max.x - 0.5, (rect.center().y - 8.0)..=(rect.center().y + 6.0), Stroke::new(1.0, palette.separator));
    }

    let compact = rect.width() < 92.0 || tab.pinned;
    let content = body.shrink2(vec2(10.0, 0.0)).translate(vec2(0.0, -2.0));
    let icon_rect = if compact {
        Rect::from_center_size(content.center(), Vec2::splat(16.0))
    } else {
        Rect::from_center_size(pos2(content.min.x + 8.0, content.center().y), Vec2::splat(16.0))
    };

    let suspended = tab.lifecycle.is_suspended();
    let crashed = matches!(tab.lifecycle, TabLifecycle::Crashed { .. });
    if tab.loading {
        icons::spinner(painter, icon_rect, palette.accent, toolbar.time);
        ui.ctx().request_repaint();
    } else if crashed {
        icons::paint(painter, icon_rect, Icon::Warning, palette.warning);
    } else if let Some(texture) = toolbar.favicons.get(&tab.id) {
        let tint = if suspended { Color32::from_white_alpha(120) } else { Color32::WHITE };
        painter.image(texture.0.id(), icon_rect, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), tint);
    } else {
        let color = if suspended { palette.text_faint } else { palette.text_dim };
        icons::paint(painter, icon_rect, Icon::Globe, color);
    }

    if compact {
        return;
    }
    let close_visible = is_active || hovered;
    let mut text_right = content.max.x - if close_visible { 22.0 } else { 2.0 };
    if tab.media_playing {
        let speaker = Rect::from_center_size(pos2(text_right - 8.0, content.center().y), Vec2::splat(14.0));
        icons::paint(painter, speaker, Icon::Speaker, palette.text_dim);
        text_right -= 20.0;
    }
    let text_left = icon_rect.max.x + 8.0;
    let color = if is_active {
        palette.text
    } else if suspended {
        palette.text_faint
    } else {
        palette.text_dim
    };
    let mut job = LayoutJob::simple_singleline(tab.display_title().to_owned(), FontId::proportional(12.5), color);
    job.wrap = TextWrapping::truncate_at_width((text_right - text_left).max(0.0));
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    painter.galley(pos2(text_left, content.center().y - galley.size().y / 2.0), galley, color);
}

fn tab_interactions(ui: &mut Ui, toolbar: &mut Toolbar<'_>, tab: &Tab, rect: Rect, response: egui::Response) {
    let palette = *toolbar.palette;
    let is_active = toolbar.model.tabs.active_id() == Some(tab.id);
    let compact = rect.width() < 92.0 || tab.pinned;
    let mut close_clicked = false;
    if !compact && (is_active || response.hovered()) {
        let close_rect = Rect::from_center_size(pos2(rect.max.x - 20.0, rect.center().y - 2.0), Vec2::splat(20.0));
        let close = ui.interact(close_rect, Id::new(("tab-close", tab.id)), Sense::click());
        let state = GlassState::of(&close);
        paint_glass(ui.painter(), close_rect, 10, &palette, state, true);
        icons::paint(
            ui.painter(),
            close_rect.shrink(5.0),
            Icon::Close,
            if close.hovered() { palette.text } else { palette.text_dim },
        );
        close_clicked = close.clicked();
    }
    if close_clicked || response.middle_clicked() {
        toolbar.commands.push(UiCommand::CloseTab(tab.id));
    } else if response.clicked() && !is_active {
        toolbar.commands.push(UiCommand::ActivateTab(tab.id));
    }

    let thumbnail = toolbar.thumbnails.get(&tab.id).map(|texture| (texture.0.id(), texture.0.aspect_ratio()));
    let response = response.on_hover_ui_at_pointer(|ui| {
        ui.set_max_width(280.0);
        ui.label(egui::RichText::new(tab.display_title()).strong());
        if let Some(url) = &tab.url {
            ui.label(egui::RichText::new(ellipsize(&display_text(url), 60)).small().color(palette.text_dim));
        }
        match &tab.lifecycle {
            TabLifecycle::Suspended { .. } => {
                ui.label(egui::RichText::new("Sleeping to save memory. Click to wake.").small().color(palette.accent));
            },
            TabLifecycle::Crashed { .. } => {
                ui.label(egui::RichText::new("This tab crashed").small().color(palette.warning));
            },
            TabLifecycle::Live => {},
        }
        if let Some((texture, aspect)) = thumbnail.filter(|_| !is_active) {
            let size = vec2(260.0, 260.0 / aspect.max(0.1));
            ui.add(egui::Image::new((texture, size)).corner_radius(CornerRadius::same(6)));
        }
    });

    let suspended = tab.lifecycle.is_suspended();
    response.context_menu(|ui| {
        ui.set_min_width(190.0);
        if ui.button("New tab to the right").clicked() {
            toolbar.commands.push(UiCommand::NewTab);
            ui.close();
        }
        if ui.button("Reload").clicked() {
            toolbar.commands.push(UiCommand::ActivateTab(tab.id));
            toolbar.commands.push(UiCommand::Reload);
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            toolbar.commands.push(UiCommand::DuplicateTab(tab.id));
            ui.close();
        }
        if ui.button(if tab.pinned { "Unpin" } else { "Pin" }).clicked() {
            toolbar.commands.push(UiCommand::PinTab(tab.id, !tab.pinned));
            ui.close();
        }
        ui.separator();
        if suspended {
            if ui.button("Wake up").clicked() {
                toolbar.commands.push(UiCommand::WakeTab(tab.id));
                ui.close();
            }
        } else if !is_active && ui.button("Sleep now (free memory)").clicked() {
            toolbar.commands.push(UiCommand::SuspendTab(tab.id));
            ui.close();
        }
        ui.separator();
        if ui.button("Close other tabs").clicked() {
            toolbar.commands.push(UiCommand::CloseOtherTabs(tab.id));
            ui.close();
        }
        if ui.button("Close").clicked() {
            toolbar.commands.push(UiCommand::CloseTab(tab.id));
            ui.close();
        }
    });
}

// ----- navigation bar --------------------------------------------------------------------

fn nav_bar(ui: &mut Ui, toolbar: &mut Toolbar<'_>) {
    let palette = *toolbar.palette;
    let active = &toolbar.model.active;
    let bar = ui.max_rect();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        if glass_icon_button(ui, &palette, Icon::Back, active.can_go_back, "Back (Alt+Left)").clicked() {
            toolbar.commands.push(UiCommand::Back);
        }
        if glass_icon_button(ui, &palette, Icon::Forward, active.can_go_forward, "Forward (Alt+Right)").clicked() {
            toolbar.commands.push(UiCommand::Forward);
        }
        if glass_icon_button(ui, &palette, Icon::Reload, active.url.is_some(), "Reload (Ctrl+R)").clicked() {
            toolbar.commands.push(UiCommand::Reload);
        }
        if glass_icon_button(ui, &palette, Icon::Home, true, "Home (Alt+Home)").clicked() {
            toolbar.commands.push(UiCommand::Home);
        }
        ui.add_space(4.0);

        let right_width = 2.0 * 32.0 + 2.0 * 6.0 + 4.0;
        let width = (ui.available_width() - right_width).max(120.0);
        address_bar(ui, toolbar, width);
        ui.add_space(4.0);

        let downloads = toolbar.model.downloads;
        let active_downloads = downloads.active_count();
        let downloads_response = glass_icon_button(ui, &palette, Icon::Download, true, "Downloads (Ctrl+J)");
        if active_downloads > 0 {
            let progress: Vec<f32> = downloads.items().iter().filter_map(|item| item.state.progress()).collect();
            let fraction = if progress.is_empty() { None } else { Some(progress.iter().sum::<f32>() / progress.len() as f32) };
            let ring = downloads_response.rect.shrink(2.0);
            match fraction {
                Some(fraction) => {
                    let points = (0..=32)
                        .map(|i| {
                            let angle = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * fraction * i as f32 / 32.0;
                            ring.center() + vec2(angle.cos(), angle.sin()) * ring.width() / 2.0
                        })
                        .collect();
                    ui.painter().line(points, Stroke::new(2.0, palette.accent));
                },
                None => icons::spinner(ui.painter(), ring, palette.accent, toolbar.time),
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        if downloads_response.clicked() {
            toolbar.commands.push(UiCommand::TogglePanel(Panel::Downloads));
        }
        main_menu(ui, toolbar);
    });

    if active.loading {
        let line = Rect::from_min_max(pos2(bar.min.x - 8.0, bar.max.y - 2.0), pos2(bar.max.x + 8.0, bar.max.y));
        progress_bar(ui.painter(), line, None, &palette, toolbar.time);
        ui.ctx().request_repaint();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SuggestionKind {
    Search,
    Visit,
    History,
    Bookmark,
}

struct Suggestion {
    kind: SuggestionKind,
    title: String,
    url: Url,
}

#[derive(Default)]
pub struct LocationBar {
    pub text: String,
    pub editing: bool,
    focus_requested: bool,
    select_all: bool,
    selected: Option<usize>,
    suggestions: Vec<Suggestion>,
    suggested_for: Option<String>,
    popup_rect: Option<Rect>,
    menu_open: bool,
}

impl LocationBar {
    pub fn show_url(&mut self, url: Option<&Url>) {
        self.text = url.map(display_text).unwrap_or_default();
    }

    pub fn sync(&mut self, url: &Url) {
        if !self.editing {
            self.text = display_text(url);
        }
    }

    pub fn reset(&mut self, url: Option<&Url>) {
        self.editing = false;
        self.selected = None;
        self.suggestions.clear();
        self.suggested_for = None;
        self.show_url(url);
        // A blank page invites typing right away.
        if url.is_none_or(|url| display_text(url).is_empty()) {
            self.request_focus();
        }
    }

    pub fn request_focus(&mut self) {
        self.focus_requested = true;
        self.select_all = true;
    }
}

fn address_bar(ui: &mut Ui, toolbar: &mut Toolbar<'_>, width: f32) {
    let palette = *toolbar.palette;
    let id = Id::new("address-bar");
    let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
    let focused = ui.memory(|memory| memory.has_focus(id));

    if focused {
        ui.painter().rect_filled(rect, CornerRadius::same(PILL), palette.card);
        ui.painter().rect_stroke(rect, CornerRadius::same(PILL), Stroke::new(1.5, palette.accent), StrokeKind::Inside);
    } else {
        let hovered = ui.rect_contains_pointer(rect);
        paint_glass(
            ui.painter(),
            rect,
            PILL,
            &palette,
            if hovered { GlassState::Hovered } else { GlassState::Idle },
            false,
        );
    }

    // Leading connection indicator.
    let active = &toolbar.model.active;
    let (icon, color, hint) = match active.security {
        Some(ConnectionSecurity::Secure) => (Icon::Lock, palette.text_dim, "Connection is secure (HTTPS)"),
        Some(ConnectionSecurity::Insecure) => (Icon::Warning, palette.warning, "Not secure: this site does not use HTTPS"),
        Some(ConnectionSecurity::LocalHttp) => (Icon::Info, palette.text_dim, "Local network page"),
        Some(ConnectionSecurity::LocalFile) => (Icon::Folder, palette.text_dim, "File on this computer"),
        Some(ConnectionSecurity::Internal) => (Icon::Search, palette.text_dim, "Blanket page"),
        Some(ConnectionSecurity::Other) | None => (Icon::Search, palette.text_dim, ""),
    };
    let icon_rect = Rect::from_center_size(pos2(rect.min.x + 20.0, rect.center().y), Vec2::splat(15.0));
    let shown_icon = if focused { Icon::Search } else { icon };
    icons::paint(ui.painter(), icon_rect, shown_icon, if focused { palette.text_dim } else { color });
    if !hint.is_empty() && !focused {
        ui.interact(icon_rect.expand(4.0), Id::new("security-indicator"), Sense::hover()).on_hover_text(hint);
    }

    // Trailing controls: zoom badge and bookmark star.
    let mut trailing = rect.max.x - 6.0;
    let star_rect = Rect::from_center_size(pos2(trailing - 13.0, rect.center().y), Vec2::splat(26.0));
    if active.url.is_some() {
        let star = ui.interact(star_rect, Id::new("bookmark-star"), Sense::click());
        paint_glass(ui.painter(), star_rect, 13, &palette, GlassState::of(&star), true);
        let (icon, color) = if active.bookmarked { (Icon::StarFilled, palette.accent) } else { (Icon::Star, palette.text_dim) };
        icons::paint(ui.painter(), star_rect.shrink(6.0), icon, color);
        if star
            .on_hover_text(if active.bookmarked { "Remove bookmark (Ctrl+D)" } else { "Bookmark this page (Ctrl+D)" })
            .clicked()
        {
            toolbar.commands.push(UiCommand::ToggleBookmark);
        }
        trailing = star_rect.min.x - 2.0;
    }
    if (active.zoom - 1.0).abs() > 0.01 {
        let label = format!("{:.0}%", active.zoom * 100.0);
        let galley = ui.painter().layout_no_wrap(label, FontId::proportional(11.5), palette.text_dim);
        let badge = Rect::from_center_size(
            pos2(trailing - galley.size().x / 2.0 - 10.0, rect.center().y),
            vec2(galley.size().x + 14.0, 20.0),
        );
        let response = ui.interact(badge, Id::new("zoom-badge"), Sense::click());
        paint_glass(ui.painter(), badge, 10, &palette, GlassState::of(&response), false);
        ui.painter().galley(badge.center() - galley.size() / 2.0, galley, palette.text_dim);
        if response.on_hover_text("Reset zoom (Ctrl+0)").clicked() {
            toolbar.commands.push(UiCommand::Zoom(ZoomChange::Reset));
        }
        trailing = badge.min.x - 4.0;
    }

    let text_rect = Rect::from_min_max(pos2(icon_rect.max.x + 8.0, rect.min.y), pos2(trailing - 4.0, rect.max.y));
    let location = &mut *toolbar.location;
    if location.focus_requested {
        location.focus_requested = false;
        ui.memory_mut(|memory| memory.request_focus(id));
    }
    let output = ui
        .scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
            TextEdit::singleline(&mut location.text)
                .id(id)
                .frame(Frame::NONE)
                .margin(Margin::symmetric(0, 8))
                .desired_width(text_rect.width())
                .font(FontId::proportional(14.0))
                .hint_text(egui::RichText::new("Search or enter address").color(palette.text_faint))
                .show(ui)
        })
        .inner;
    let response = output.response;

    if response.gained_focus() {
        location.editing = true;
        location.text = active.url.as_ref().map(display_text).unwrap_or_default();
        location.select_all = true;
    }
    if location.select_all && response.has_focus() {
        location.select_all = false;
        let mut state = output.state;
        let end = CCursor::new(location.text.chars().count());
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), end)));
        state.store(ui.ctx(), id);
    }
    if response.changed() {
        location.selected = None;
        toolbar.commands.push(UiCommand::OmniboxInput(location.text.clone()));
    }

    let (enter, alt, down, up) = ui.input(|input| {
        (
            input.key_pressed(Key::Enter),
            input.modifiers.alt,
            input.key_pressed(Key::ArrowDown),
            input.key_pressed(Key::ArrowUp),
        )
    });
    if response.has_focus() && !location.suggestions.is_empty() {
        let count = location.suggestions.len();
        if down {
            location.selected = Some(location.selected.map_or(0, |index| (index + 1) % count));
        }
        if up {
            location.selected = Some(location.selected.map_or(count - 1, |index| (index + count - 1) % count));
        }
    }

    let submitted = response.lost_focus() && enter;
    let disposition = if alt { Disposition::NewTab } else { Disposition::CurrentTab };
    if submitted {
        let choice = location.selected.and_then(|index| location.suggestions.get(index));
        match choice {
            Some(suggestion) => toolbar.commands.push(UiCommand::OpenUrl {
                url: suggestion.url.clone(),
                disposition,
            }),
            None if !location.text.trim().is_empty() => toolbar.commands.push(UiCommand::Navigate {
                input: location.text.trim().to_owned(),
                disposition,
            }),
            None => {},
        }
        toolbar.commands.push(UiCommand::FocusContent);
        location.editing = false;
        location.selected = None;
    }

    let pointer_on_popup = location
        .popup_rect
        .zip(ui.input(|input| input.pointer.hover_pos()))
        .is_some_and(|(popup, pointer)| popup.contains(pointer));
    if response.lost_focus() && !submitted && !pointer_on_popup {
        location.editing = false;
        location.selected = None;
    }

    if location.editing && (response.has_focus() || pointer_on_popup) && !location.text.trim().is_empty() {
        update_suggestions(location, toolbar.model);
        suggestions_popup(ui, location, &palette, rect, toolbar.commands);
    } else {
        location.popup_rect = None;
    }
}

fn update_suggestions(location: &mut LocationBar, model: &UiModel<'_>) {
    let query = location.text.trim().to_owned();
    if location.suggested_for.as_deref() == Some(query.as_str()) {
        return;
    }
    let mut suggestions = Vec::new();
    let config = &model.config.general;
    match blanket_core::omnibox::interpret(&query, &config.search_engine, model.config.security.prefer_https_for_typed_hosts)
    {
        Some(blanket_core::omnibox::OmniboxTarget::Navigate(url)) => suggestions.push(Suggestion {
            kind: SuggestionKind::Visit,
            title: display_text(&url),
            url,
        }),
        Some(blanket_core::omnibox::OmniboxTarget::Search(url)) => suggestions.push(Suggestion {
            kind: SuggestionKind::Search,
            title: format!("{query} — search with {}", config.search_engine.name),
            url,
        }),
        None => {},
    }
    for bookmark in model.bookmarks.search(&query, 3) {
        if suggestions.iter().all(|existing| existing.url != bookmark.url) {
            suggestions.push(Suggestion {
                kind: SuggestionKind::Bookmark,
                title: bookmark.title.clone(),
                url: bookmark.url.clone(),
            });
        }
    }
    for entry in model.history.search(&query, MAX_SUGGESTIONS, model.now_unix) {
        if suggestions.len() >= MAX_SUGGESTIONS {
            break;
        }
        if suggestions.iter().all(|existing| existing.url != entry.url) {
            suggestions.push(Suggestion {
                kind: SuggestionKind::History,
                title: entry.title.clone(),
                url: entry.url.clone(),
            });
        }
    }
    // Searching is always possible, even when the input looks like an address.
    if suggestions.first().is_some_and(|first| first.kind == SuggestionKind::Visit) {
        suggestions.push(Suggestion {
            kind: SuggestionKind::Search,
            title: format!("{query} — search with {}", config.search_engine.name),
            url: search_url(&query, &config.search_engine),
        });
    }
    location.suggestions = suggestions;
    location.suggested_for = Some(query);
    location.selected = None;
}

fn suggestions_popup(ui: &Ui, location: &mut LocationBar, palette: &Palette, anchor: Rect, commands: &mut Vec<UiCommand>) {
    if location.suggestions.is_empty() {
        location.popup_rect = None;
        return;
    }
    let width = anchor.width();
    let mut chosen = None;
    let response = egui::Area::new(Id::new("address-suggestions"))
        .order(Order::Foreground)
        .fixed_pos(anchor.left_bottom() + vec2(0.0, 6.0))
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style())
                .fill(palette.card)
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(width - 12.0);
                    for (index, suggestion) in location.suggestions.iter().enumerate() {
                        let (row, response) = ui.allocate_exact_size(vec2(width - 12.0, 36.0), Sense::click());
                        let selected = location.selected == Some(index);
                        if selected || response.hovered() {
                            ui.painter().rect_filled(row, CornerRadius::same(10), palette.glass_hover);
                        }
                        let icon = match suggestion.kind {
                            SuggestionKind::Search => Icon::Search,
                            SuggestionKind::Visit => Icon::Globe,
                            SuggestionKind::History => Icon::History,
                            SuggestionKind::Bookmark => Icon::StarFilled,
                        };
                        let icon_rect = Rect::from_center_size(pos2(row.min.x + 18.0, row.center().y), Vec2::splat(15.0));
                        let icon_color = if suggestion.kind == SuggestionKind::Bookmark { palette.accent } else { palette.text_dim };
                        icons::paint(ui.painter(), icon_rect, icon, icon_color);
                        let text_left = row.min.x + 38.0;
                        let max_width = row.max.x - text_left - 10.0;
                        let title = if suggestion.title.is_empty() { display_text(&suggestion.url) } else { suggestion.title.clone() };
                        let mut job = LayoutJob::default();
                        job.append(&title, 0.0, egui::TextFormat::simple(FontId::proportional(13.5), palette.text));
                        if matches!(suggestion.kind, SuggestionKind::History | SuggestionKind::Bookmark) {
                            job.append(
                                &format!("  —  {}", display_text(&suggestion.url)),
                                0.0,
                                egui::TextFormat::simple(FontId::proportional(12.5), palette.text_faint),
                            );
                        }
                        job.wrap = TextWrapping::truncate_at_width(max_width);
                        let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                        ui.painter().galley(pos2(text_left, row.center().y - galley.size().y / 2.0), galley, palette.text);
                        // Act on press: the address field loses focus on the same click.
                        if response.hovered() && ui.input(|input| input.pointer.primary_pressed()) {
                            chosen = Some(index);
                        }
                    }
                });
        });
    location.popup_rect = Some(response.response.rect);
    if let Some(index) = chosen {
        let url = location.suggestions[index].url.clone();
        let disposition = if ui.input(|input| input.modifiers.ctrl) { Disposition::BackgroundTab } else { Disposition::CurrentTab };
        commands.push(UiCommand::OpenUrl { url, disposition });
        commands.push(UiCommand::FocusContent);
        location.editing = false;
        location.popup_rect = None;
        ui.memory_mut(|memory| memory.surrender_focus(Id::new("address-bar")));
    }
}

fn main_menu(ui: &mut Ui, toolbar: &mut Toolbar<'_>) {
    let palette = *toolbar.palette;
    let button = glass_icon_button(ui, &palette, Icon::Menu, true, "Menu");
    if button.clicked() {
        toolbar.location.menu_open = !toolbar.location.menu_open;
    }
    if !toolbar.location.menu_open {
        return;
    }
    let mut close = false;
    let active_zoom = toolbar.model.active.zoom;
    let open_panel = toolbar.open_panel;
    let commands = &mut *toolbar.commands;
    let area = egui::Area::new(Id::new("main-menu"))
        .order(Order::Foreground)
        .pivot(Align2::RIGHT_TOP)
        .fixed_pos(button.rect.right_bottom() + vec2(0.0, 8.0))
        .show(ui.ctx(), |ui| {
            Frame::popup(ui.style())
                .fill(palette.card)
                .corner_radius(CornerRadius::same(14))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_width(250.0);
                    let mut item = |ui: &mut Ui, icon: Icon, label: &str, shortcut: &str, selected: bool| -> bool {
                        let (row, response) = ui.allocate_exact_size(vec2(250.0, 32.0), Sense::click());
                        if response.hovered() || selected {
                            ui.painter().rect_filled(row, CornerRadius::same(9), palette.glass_hover);
                        }
                        icons::paint(
                            ui.painter(),
                            Rect::from_center_size(pos2(row.min.x + 18.0, row.center().y), Vec2::splat(15.0)),
                            icon,
                            palette.text_dim,
                        );
                        ui.painter().text(
                            pos2(row.min.x + 38.0, row.center().y),
                            Align2::LEFT_CENTER,
                            label,
                            FontId::proportional(13.5),
                            palette.text,
                        );
                        ui.painter().text(
                            pos2(row.max.x - 10.0, row.center().y),
                            Align2::RIGHT_CENTER,
                            shortcut,
                            FontId::proportional(11.5),
                            palette.text_faint,
                        );
                        response.clicked()
                    };
                    let mut run = |command: UiCommand, close: &mut bool| {
                        commands.push(command);
                        *close = true;
                    };
                    if item(ui, Icon::Plus, "New tab", "Ctrl+T", false) {
                        run(UiCommand::NewTab, &mut close);
                    }
                    if item(ui, Icon::Reload, "Reopen closed tab", "Ctrl+Shift+T", false) {
                        run(UiCommand::ReopenClosedTab, &mut close);
                    }
                    ui.separator();
                    for (panel, icon, label, shortcut) in [
                        (Panel::History, Icon::History, "History", "Ctrl+H"),
                        (Panel::Bookmarks, Icon::Bookmarks, "Bookmarks", "Ctrl+Shift+O"),
                        (Panel::Downloads, Icon::Download, "Downloads", "Ctrl+J"),
                    ] {
                        if item(ui, icon, label, shortcut, open_panel == Some(panel)) {
                            run(UiCommand::TogglePanel(panel), &mut close);
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new("Zoom").color(palette.text));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if glass_icon_button_sized(ui, &palette, Icon::Plus, true, "Zoom in (Ctrl+=)", 26.0, false).clicked() {
                                commands.push(UiCommand::Zoom(ZoomChange::In));
                            }
                            if ui
                                .add(egui::Button::new(format!("{:.0}%", active_zoom * 100.0)).frame(false))
                                .on_hover_text("Reset (Ctrl+0)")
                                .clicked()
                            {
                                commands.push(UiCommand::Zoom(ZoomChange::Reset));
                            }
                            let (minus, response) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::click());
                            paint_glass(ui.painter(), minus, 13, &palette, GlassState::of(&response), false);
                            ui.painter().hline(
                                (minus.center().x - 5.0)..=(minus.center().x + 5.0),
                                minus.center().y,
                                Stroke::new(1.5, palette.text),
                            );
                            if response.on_hover_text("Zoom out (Ctrl+-)").clicked() {
                                commands.push(UiCommand::Zoom(ZoomChange::Out));
                            }
                        });
                    });
                    if item(ui, Icon::Gauge, "Full screen", "F11", toolbar.model.window_fullscreen) {
                        commands.push(UiCommand::ToggleFullscreen);
                        close = true;
                    }
                    if item(ui, Icon::Download, "Save page", "Ctrl+S", false) {
                        commands.push(UiCommand::SavePage);
                        close = true;
                    }
                    ui.separator();
                    if item(ui, Icon::Gauge, "Performance", "Shift+Esc", open_panel == Some(Panel::Performance)) {
                        commands.push(UiCommand::TogglePanel(Panel::Performance));
                        close = true;
                    }
                    if item(ui, Icon::Settings, "Settings", "Ctrl+,", open_panel == Some(Panel::Settings)) {
                        commands.push(UiCommand::TogglePanel(Panel::Settings));
                        close = true;
                    }
                    ui.separator();
                    if item(ui, Icon::Close, "Quit Blanket", "Ctrl+Shift+Q", false) {
                        commands.push(UiCommand::Quit);
                        close = true;
                    }
                });
        });
    let clicked_outside = ui.input(|input| {
        input.pointer.any_pressed() &&
            input
                .pointer
                .interact_pos()
                .is_some_and(|pos| !area.response.rect.contains(pos) && !button.rect.contains(pos))
    });
    let escape = ui.input(|input| input.key_pressed(Key::Escape));
    if close || clicked_outside || escape {
        toolbar.location.menu_open = false;
    }
}

// ----- bookmarks bar ---------------------------------------------------------------------

fn bookmarks_bar(ui: &mut Ui, toolbar: &mut Toolbar<'_>) {
    let palette = *toolbar.palette;
    let bookmarks = toolbar.model.bookmarks.items();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if bookmarks.is_empty() {
            ui.label(
                egui::RichText::new("Press Ctrl+D to bookmark a page; bookmarks appear here.")
                    .small()
                    .color(palette.text_faint),
            );
            return;
        }
        for bookmark in bookmarks {
            let title = if bookmark.title.is_empty() { display_text(&bookmark.url) } else { bookmark.title.clone() };
            let title = ellipsize(&title, 24);
            let galley = ui.painter().layout_no_wrap(title, FontId::proportional(12.5), palette.text_dim);
            let size = vec2(galley.size().x + 36.0, 24.0);
            if ui.available_width() < size.x + 4.0 {
                break;
            }
            let (rect, response) = ui.allocate_exact_size(size, Sense::click());
            paint_glass(ui.painter(), rect, 12, &palette, GlassState::of(&response), true);
            icons::paint(
                ui.painter(),
                Rect::from_center_size(pos2(rect.min.x + 14.0, rect.center().y), Vec2::splat(13.0)),
                Icon::Globe,
                palette.text_faint,
            );
            ui.painter().galley(pos2(rect.min.x + 26.0, rect.center().y - galley.size().y / 2.0), galley, palette.text_dim);
            let response = response.on_hover_text(bookmark.url.as_str());
            if response.clicked() {
                let disposition = if ui.input(|input| input.modifiers.ctrl) {
                    Disposition::BackgroundTab
                } else {
                    Disposition::CurrentTab
                };
                toolbar.commands.push(UiCommand::OpenUrl {
                    url: bookmark.url.clone(),
                    disposition,
                });
            } else if response.middle_clicked() {
                toolbar.commands.push(UiCommand::OpenUrl {
                    url: bookmark.url.clone(),
                    disposition: Disposition::BackgroundTab,
                });
            }
        }
    });
}
