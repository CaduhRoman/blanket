//! Shared widgets: translucent "glass" controls and small formatting helpers.

use egui::{Color32, CornerRadius, Painter, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use super::icons::{self, Icon};
use super::theme::Palette;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum GlassState {
    Idle,
    Hovered,
    Pressed,
    Selected,
}

impl GlassState {
    pub fn of(response: &Response) -> Self {
        if response.is_pointer_button_down_on() {
            Self::Pressed
        } else if response.hovered() {
            Self::Hovered
        } else {
            Self::Idle
        }
    }
}

/// A translucent surface with a hairline border and a soft top highlight, in the
/// spirit of iOS materials. `subtle` surfaces are invisible until hovered.
pub fn paint_glass(painter: &Painter, rect: Rect, radius: u8, palette: &Palette, state: GlassState, subtle: bool) {
    let fill = match state {
        GlassState::Idle if subtle => Color32::TRANSPARENT,
        GlassState::Idle => palette.glass,
        GlassState::Hovered => palette.glass_hover,
        GlassState::Pressed => palette.glass_pressed,
        GlassState::Selected => palette.glass_hover,
    };
    if fill == Color32::TRANSPARENT {
        return;
    }
    let corner = CornerRadius::same(radius);
    painter.rect_filled(rect, corner, fill);
    // Light catches the upper edge of the material.
    let highlight = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.center().y));
    painter.with_clip_rect(highlight).rect_stroke(
        rect.shrink(0.5),
        corner,
        Stroke::new(1.0, palette.glass_highlight),
        StrokeKind::Inside,
    );
    painter.rect_stroke(rect, corner, Stroke::new(1.0, palette.glass_stroke), StrokeKind::Inside);
}

/// Circular glass button holding an icon.
pub fn glass_icon_button(ui: &mut Ui, palette: &Palette, icon: Icon, enabled: bool, tooltip: &str) -> Response {
    glass_icon_button_sized(ui, palette, icon, enabled, tooltip, 32.0, false)
}

pub fn glass_icon_button_sized(
    ui: &mut Ui,
    palette: &Palette,
    icon: Icon,
    enabled: bool,
    tooltip: &str,
    side: f32,
    subtle: bool,
) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), sense);
    if ui.is_rect_visible(rect) {
        let state = if enabled { GlassState::of(&response) } else { GlassState::Idle };
        paint_glass(ui.painter(), rect, (side / 2.0) as u8, palette, state, subtle);
        let color = if enabled { palette.text } else { palette.text_faint };
        icons::paint(ui.painter(), Rect::from_center_size(rect.center(), Vec2::splat(side * 0.5)), icon, color);
    }
    let response = if enabled && !tooltip.is_empty() { response.on_hover_text(tooltip) } else { response };
    if enabled {
        response.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response
    }
}

/// A pill-shaped glass button with text, optionally emphasized with the accent color.
pub fn pill_button(ui: &mut Ui, palette: &Palette, text: &str, primary: bool) -> Response {
    let font = egui::FontId::new(13.0, super::theme::semibold());
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, palette.text);
    let size = vec2(galley.size().x + 28.0, 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    if ui.is_rect_visible(rect) {
        let state = GlassState::of(&response);
        if primary {
            let fill = match state {
                GlassState::Pressed => palette.accent.gamma_multiply(0.8),
                GlassState::Hovered => palette.accent.gamma_multiply(0.92),
                _ => palette.accent,
            };
            ui.painter().rect_filled(rect, CornerRadius::same(15), fill);
        } else {
            paint_glass(ui.painter(), rect, 15, palette, state, false);
        }
        let color = if primary { palette.accent_text } else { palette.text };
        ui.painter().galley(rect.center() - galley.size() / 2.0, galley, color);
    }
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Thin horizontal progress bar; `None` draws an indeterminate sweep.
pub fn progress_bar(painter: &Painter, rect: Rect, progress: Option<f32>, palette: &Palette, time: f64) {
    painter.rect_filled(rect, CornerRadius::same((rect.height() / 2.0) as u8), palette.glass);
    let filled = match progress {
        Some(fraction) => Rect::from_min_size(rect.min, vec2(rect.width() * fraction.clamp(0.0, 1.0), rect.height())),
        None => {
            let phase = (time * 0.9).fract() as f32;
            let width = rect.width() * 0.3;
            let x = rect.min.x - width + (rect.width() + width) * phase;
            Rect::from_min_max(pos2(x.max(rect.min.x), rect.min.y), pos2((x + width).min(rect.max.x), rect.max.y))
        },
    };
    if filled.width() > 0.0 {
        painter.rect_filled(filled, CornerRadius::same((rect.height() / 2.0) as u8), palette.accent);
    }
}

/// Small line chart of `values` scaled to their maximum.
pub fn sparkline(painter: &Painter, rect: Rect, values: &[f32], color: Color32) {
    if values.len() < 2 {
        return;
    }
    let max = values.iter().copied().fold(f32::MIN, f32::max).max(f32::EPSILON);
    let min = values.iter().copied().fold(f32::MAX, f32::min).min(max * 0.8);
    let span = (max - min).max(f32::EPSILON);
    let step = rect.width() / (values.len() - 1) as f32;
    let points: Vec<_> = values
        .iter()
        .enumerate()
        .map(|(i, value)| pos2(rect.min.x + i as f32 * step, rect.max.y - (value - min) / span * rect.height()))
        .collect();
    painter.line(points, Stroke::new(1.5, color));
}

pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn time_ago(then: u64, now: u64) -> String {
    let seconds = now.saturating_sub(then);
    match seconds {
        0..=59 => "just now".to_owned(),
        60..=3_599 => format!("{} min ago", seconds / 60),
        3_600..=86_399 => format!("{} h ago", seconds / 3_600),
        86_400..=604_799 => format!("{} d ago", seconds / 86_400),
        _ => format!("{} wk ago", seconds / 604_800),
    }
}

/// Shorten `text` to at most `max` characters with an ellipsis.
pub fn ellipsize(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(300 * 1024 * 1024), "300 MB");
    }

    #[test]
    fn relative_times() {
        assert_eq!(time_ago(100, 130), "just now");
        assert_eq!(time_ago(0, 7_200), "2 h ago");
        assert_eq!(time_ago(10, 5), "just now");
    }

    #[test]
    fn ellipsis_respects_char_boundaries() {
        assert_eq!(ellipsize("short", 10), "short");
        assert_eq!(ellipsize("ação completa", 5), "ação…");
    }
}
