//! Colors, typography and egui style. The look follows the Blanket identity: a dark,
//! quiet chrome, translucent "glass" controls, and a single green accent.

use std::path::Path;

use blanket_core::config::ThemePreference;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use tracing::debug;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    /// Behind web content while it has not painted yet.
    pub page: Color32,
    /// Tab strip background.
    pub strip: Color32,
    /// Toolbar and side panels.
    pub chrome: Color32,
    pub tab_active: Color32,
    pub tab_hover: Color32,
    pub card: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_text: Color32,
    pub danger: Color32,
    pub warning: Color32,
    pub glass: Color32,
    pub glass_hover: Color32,
    pub glass_pressed: Color32,
    pub glass_stroke: Color32,
    pub glass_highlight: Color32,
    pub separator: Color32,
    pub scrim: Color32,
    pub shadow: Color32,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            dark: true,
            page: Color32::from_rgb(0x0e, 0x10, 0x13),
            strip: Color32::from_rgb(0x0f, 0x11, 0x14),
            chrome: Color32::from_rgb(0x17, 0x1a, 0x1e),
            tab_active: Color32::from_rgb(0x17, 0x1a, 0x1e),
            tab_hover: Color32::from_rgb(0x1c, 0x20, 0x25),
            card: Color32::from_rgb(0x1e, 0x22, 0x28),
            text: Color32::from_rgb(0xec, 0xee, 0xf1),
            text_dim: Color32::from_rgb(0xa2, 0xa8, 0xb0),
            text_faint: Color32::from_rgb(0x6b, 0x72, 0x7b),
            accent: Color32::from_rgb(0x3d, 0xd6, 0x8c),
            accent_text: Color32::from_rgb(0x06, 0x1a, 0x10),
            danger: Color32::from_rgb(0xf2, 0x6d, 0x6d),
            warning: Color32::from_rgb(0xf5, 0xc1, 0x4b),
            glass: Color32::from_white_alpha(14),
            glass_hover: Color32::from_white_alpha(26),
            glass_pressed: Color32::from_white_alpha(40),
            glass_stroke: Color32::from_white_alpha(24),
            glass_highlight: Color32::from_white_alpha(18),
            separator: Color32::from_white_alpha(16),
            scrim: Color32::from_black_alpha(150),
            shadow: Color32::from_black_alpha(110),
        }
    }

    pub fn light() -> Self {
        Self {
            dark: false,
            page: Color32::from_rgb(0xff, 0xff, 0xff),
            strip: Color32::from_rgb(0xe6, 0xe8, 0xeb),
            chrome: Color32::from_rgb(0xf6, 0xf7, 0xf9),
            tab_active: Color32::from_rgb(0xf6, 0xf7, 0xf9),
            tab_hover: Color32::from_rgb(0xee, 0xf0, 0xf2),
            card: Color32::from_rgb(0xff, 0xff, 0xff),
            text: Color32::from_rgb(0x1a, 0x1c, 0x1f),
            text_dim: Color32::from_rgb(0x55, 0x5b, 0x63),
            text_faint: Color32::from_rgb(0x8d, 0x93, 0x9b),
            accent: Color32::from_rgb(0x0f, 0x9d, 0x63),
            accent_text: Color32::WHITE,
            danger: Color32::from_rgb(0xd1, 0x3b, 0x3b),
            warning: Color32::from_rgb(0xb7, 0x79, 0x00),
            glass: Color32::from_black_alpha(10),
            glass_hover: Color32::from_black_alpha(20),
            glass_pressed: Color32::from_black_alpha(32),
            glass_stroke: Color32::from_black_alpha(20),
            glass_highlight: Color32::from_white_alpha(140),
            separator: Color32::from_black_alpha(22),
            scrim: Color32::from_black_alpha(90),
            shadow: Color32::from_black_alpha(45),
        }
    }

    pub fn resolve(preference: ThemePreference, system: Option<winit::window::Theme>) -> Self {
        if is_dark(preference, system) { Self::dark() } else { Self::light() }
    }
}

pub fn is_dark(preference: ThemePreference, system: Option<winit::window::Theme>) -> bool {
    match preference {
        ThemePreference::Dark => true,
        ThemePreference::Light => false,
        ThemePreference::System => system != Some(winit::window::Theme::Light),
    }
}

/// The color scheme reported to web content (`prefers-color-scheme`).
pub fn resolve_theme(preference: ThemePreference, system: Option<winit::window::Theme>) -> servo::Theme {
    if is_dark(preference, system) { servo::Theme::Dark } else { servo::Theme::Light }
}

pub const RADIUS: u8 = 10;
pub const PILL: u8 = 18;

pub fn apply_style(ctx: &egui::Context, palette: &Palette) {
    let mut visuals = if palette.dark { Visuals::dark() } else { Visuals::light() };
    visuals.panel_fill = palette.chrome;
    visuals.window_fill = palette.card;
    visuals.extreme_bg_color = if palette.dark { Color32::from_rgb(0x12, 0x14, 0x18) } else { Color32::WHITE };
    visuals.faint_bg_color = palette.glass;
    visuals.window_stroke = Stroke::new(1.0, palette.glass_stroke);
    visuals.window_corner_radius = CornerRadius::same(14);
    visuals.menu_corner_radius = CornerRadius::same(12);
    visuals.window_shadow = egui::Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: palette.shadow,
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: palette.shadow,
    };
    visuals.selection.bg_fill = palette.accent.gamma_multiply(0.45);
    visuals.selection.stroke = Stroke::new(1.0, palette.accent);
    visuals.hyperlink_color = palette.accent;
    visuals.override_text_color = Some(palette.text);

    let radius = CornerRadius::same(RADIUS);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = radius;
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.separator);
    visuals.widgets.inactive.weak_bg_fill = palette.glass;
    visuals.widgets.inactive.bg_fill = palette.glass_hover;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, palette.glass_stroke);
    visuals.widgets.hovered.weak_bg_fill = palette.glass_hover;
    visuals.widgets.hovered.bg_fill = palette.glass_hover;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, palette.glass_stroke);
    visuals.widgets.active.weak_bg_fill = palette.glass_pressed;
    visuals.widgets.active.bg_fill = palette.glass_pressed;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, palette.accent);
    visuals.widgets.open.weak_bg_fill = palette.glass_hover;

    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        style.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Heading, FontId::new(18.0, semibold())),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();
    });
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

fn windows_fonts_dir() -> std::path::PathBuf {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    Path::new(&windir).join("Fonts")
}

fn read_font(file: &str) -> Option<FontData> {
    let path = windows_fonts_dir().join(file);
    match std::fs::read(&path) {
        Ok(bytes) => Some(FontData::from_owned(bytes)),
        Err(error) => {
            debug!(path = %path.display(), %error, "system font unavailable");
            None
        },
    }
}

/// Segoe UI (the Windows UI font) in front of egui's bundled fonts, which stay as
/// fallbacks for symbols and other platforms.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut semibold_chain = Vec::new();
    if let Some(regular) = read_font("segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), regular.into());
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "segoe".into());
    }
    if let Some(semibold) = read_font("seguisb.ttf") {
        fonts.font_data.insert("segoe-semibold".into(), semibold.into());
        semibold_chain.push("segoe-semibold".to_owned());
    } else if fonts.font_data.contains_key("segoe") {
        semibold_chain.push("segoe".to_owned());
    }
    semibold_chain.extend(fallbacks);
    fonts.families.insert(semibold(), semibold_chain);
    if let Some(mono) = read_font("consola.ttf") {
        fonts.font_data.insert("consolas".into(), mono.into());
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "consolas".into());
    }
    ctx.set_fonts(fonts);
}

/// Whether `text` contains characters the default fonts cannot show (CJK, Hangul, Thai…).
pub fn needs_extended_fonts(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c as u32,
            0x0E00..=0x0E7F |   // Thai
            0x1100..=0x11FF |   // Hangul Jamo
            0x3000..=0x30FF |   // CJK punctuation, Hiragana, Katakana
            0x3400..=0x4DBF |   // CJK extension A
            0x4E00..=0x9FFF |   // CJK unified ideographs
            0xAC00..=0xD7AF |   // Hangul syllables
            0xF900..=0xFAFF |   // CJK compatibility ideographs
            0xFF00..=0xFFEF     // Half/full-width forms
        )
    })
}

/// Load the large East Asian fonts only once a page title needs them; they cost tens of
/// megabytes, which most sessions never need.
pub fn install_extended_fonts(ctx: &egui::Context) {
    use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
    for (name, file) in [
        ("yahei", "msyh.ttc"),
        ("yugothic", "YuGothM.ttc"),
        ("malgun", "malgun.ttf"),
        ("leelawadee", "LeelawUI.ttf"),
    ] {
        if let Some(data) = read_font(file) {
            ctx.add_font(FontInsert::new(
                name,
                data,
                vec![
                    InsertFontFamily {
                        family: FontFamily::Proportional,
                        priority: FontPriority::Lowest,
                    },
                    InsertFontFamily {
                        family: semibold(),
                        priority: FontPriority::Lowest,
                    },
                ],
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_theme_defaults_to_dark() {
        assert!(is_dark(ThemePreference::System, None));
        assert!(!is_dark(ThemePreference::System, Some(winit::window::Theme::Light)));
        assert!(is_dark(ThemePreference::Dark, Some(winit::window::Theme::Light)));
        assert!(!is_dark(ThemePreference::Light, Some(winit::window::Theme::Dark)));
    }

    #[test]
    fn extended_font_detection() {
        assert!(!needs_extended_fonts("Rust programming language"));
        assert!(!needs_extended_fonts("Ação — café"));
        assert!(needs_extended_fonts("東京"));
        assert!(needs_extended_fonts("한국어"));
    }
}
