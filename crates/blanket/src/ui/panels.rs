//! Side panels: history, bookmarks, downloads, settings and performance. They are
//! native (egui) rather than web pages so they cost no engine resources and cannot be
//! reached by web content.

use blanket_core::config::{BlanketConfig, SearchEngine, ThemePreference};
use blanket_core::downloads::DownloadState;
use blanket_core::memory::MemoryPressure;
use blanket_core::omnibox::display_text;
use blanket_core::security::{PermissionDecision, PermissionKind};
use blanket_core::tabs::TabLifecycle;
use egui::{Align, CornerRadius, Frame, Layout, Margin, RichText, ScrollArea, Sense, Stroke, Ui, Vec2, vec2};
use url::Url;

use super::icons::{self, Icon};
use super::theme::Palette;
use super::widgets::{format_bytes, glass_icon_button_sized, pill_button, progress_bar, sparkline, time_ago};
use super::{ClearDataRequest, ClearRange, Disposition, Panel, UiCommand, UiModel};

const SEARCH_PRESETS: [(&str, &str); 5] = [
    ("DuckDuckGo", "https://duckduckgo.com/?q={query}"),
    ("Google", "https://www.google.com/search?q={query}"),
    ("Bing", "https://www.bing.com/search?q={query}"),
    ("Brave Search", "https://search.brave.com/search?q={query}"),
    ("Startpage", "https://www.startpage.com/do/search?q={query}"),
];

pub struct PanelState {
    history_query: String,
    bookmark_query: String,
    rename: Option<(Url, String)>,
    clear: ClearDataRequest,
    pub settings_draft: Option<BlanketConfig>,
    /// Site being typed into the compatibility list.
    compat_new: String,
}

/// "https://www.Example.com/path" → "example.com": what the compatibility list stores.
fn normalize_domain(input: &str) -> String {
    let text = input.trim().to_ascii_lowercase();
    let text = text.split_once("://").map_or(text.as_str(), |(_, rest)| rest);
    let host = text.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.split(':').next().unwrap_or_default();
    host.trim_start_matches("www.").trim_matches('.').to_owned()
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            history_query: String::new(),
            bookmark_query: String::new(),
            rename: None,
            clear: ClearDataRequest {
                range: ClearRange::LastHour,
                history: true,
                cookies: false,
                cache: true,
                permissions: false,
            },
            settings_draft: None,
            compat_new: String::new(),
        }
    }
}

/// Draw the open panel. Returns `true` if the user closed it.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    panel: Panel,
    expanded: bool,
    palette: &Palette,
    model: &UiModel<'_>,
    state: &mut PanelState,
    commands: &mut Vec<UiCommand>,
    width: f32,
) -> bool {
    let mut close = false;
    // Slides in and out; while moving it is an empty sheet, the contents appear once open.
    egui::Panel::right("side-panel")
        .exact_size(width)
        .frame(
            Frame::NONE
                .fill(palette.chrome)
                .inner_margin(Margin::symmetric(14, 12))
                .stroke(Stroke::new(1.0, palette.separator)),
        )
        .show_animated_inside(ui, expanded, |ui| {
            let title = match panel {
                Panel::History => "History",
                Panel::Bookmarks => "Bookmarks",
                Panel::Downloads => "Downloads",
                Panel::Settings => "Settings",
                Panel::Performance => "Performance",
            };
            ui.horizontal(|ui| {
                ui.heading(title);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if glass_icon_button_sized(ui, palette, Icon::Close, true, "Close", 28.0, true).clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(8.0);
            match panel {
                Panel::History => history(ui, palette, model, state, commands),
                Panel::Bookmarks => bookmarks(ui, palette, model, state, commands),
                Panel::Downloads => downloads(ui, palette, model, commands),
                Panel::Settings => settings(ui, palette, model, state, commands),
                Panel::Performance => performance(ui, palette, model, commands),
            }
        });
    if expanded && ui.input(|input| input.key_pressed(egui::Key::Escape)) && !ui.ctx().text_edit_focused() {
        close = true;
    }
    close
}

fn search_field(ui: &mut Ui, palette: &Palette, text: &mut String, hint: &str) {
    Frame::NONE
        .fill(palette.glass)
        .stroke(Stroke::new(1.0, palette.glass_stroke))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::symmetric(10, 2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                icons::paint(ui.painter(), rect, Icon::Search, palette.text_faint);
                ui.add(
                    egui::TextEdit::singleline(text)
                        .frame(Frame::NONE)
                        .margin(Margin::symmetric(0, 6))
                        .desired_width(f32::INFINITY)
                        .hint_text(RichText::new(hint).color(palette.text_faint)),
                );
            });
        });
    ui.add_space(6.0);
}

/// A clickable row with a title and a dimmer subtitle. Returns (row, trailing button).
fn entry_row(ui: &mut Ui, palette: &Palette, title: &str, subtitle: &str, trailing: Option<Icon>) -> (egui::Response, bool) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, 44.0), Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(10), palette.glass_hover);
    }
    let text_right = rect.max.x - if trailing.is_some() { 36.0 } else { 8.0 };
    let clip = egui::Rect::from_min_max(rect.min, egui::pos2(text_right, rect.max.y));
    let painter = ui.painter_at(clip);
    painter.text(
        rect.min + vec2(10.0, 13.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(13.5),
        palette.text,
    );
    painter.text(
        rect.min + vec2(10.0, 31.0),
        egui::Align2::LEFT_CENTER,
        subtitle,
        egui::FontId::proportional(11.5),
        palette.text_faint,
    );
    let mut trailing_clicked = false;
    if let Some(icon) = trailing {
        if response.hovered() || ui.rect_contains_pointer(rect) {
            let button = egui::Rect::from_center_size(egui::pos2(rect.max.x - 18.0, rect.center().y), Vec2::splat(24.0));
            let button_response = ui.interact(button, response.id.with("trailing"), Sense::click());
            if button_response.hovered() {
                ui.painter().rect_filled(button, CornerRadius::same(12), palette.glass_pressed);
            }
            icons::paint(ui.painter(), button.shrink(6.0), icon, palette.text_dim);
            trailing_clicked = button_response.clicked();
        }
    }
    (response, trailing_clicked)
}

fn open_disposition(ui: &Ui, response: &egui::Response) -> Option<Disposition> {
    if response.middle_clicked() || (response.clicked() && ui.input(|input| input.modifiers.ctrl)) {
        Some(Disposition::BackgroundTab)
    } else if response.clicked() {
        Some(Disposition::CurrentTab)
    } else {
        None
    }
}

// ----- history -------------------------------------------------------------------------

fn history(ui: &mut Ui, palette: &Palette, model: &UiModel<'_>, state: &mut PanelState, commands: &mut Vec<UiCommand>) {
    search_field(ui, palette, &mut state.history_query, "Search history");
    let query = state.history_query.trim();
    let entries = if query.is_empty() {
        model.history.recent(300)
    } else {
        model.history.search(query, 100, model.now_unix)
    };
    let clear_height = 190.0;
    ScrollArea::vertical()
        .max_height((ui.available_height() - clear_height).max(120.0))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if entries.is_empty() {
                ui.label(RichText::new("Nothing here yet.").color(palette.text_faint));
            }
            for entry in entries {
                let title = if entry.title.is_empty() { display_text(&entry.url) } else { entry.title.clone() };
                let subtitle = format!(
                    "{} · {}",
                    entry.url.host_str().unwrap_or(entry.url.scheme()),
                    time_ago(entry.last_visit, model.now_unix)
                );
                let (response, remove) = entry_row(ui, palette, &title, &subtitle, Some(Icon::Close));
                if remove {
                    commands.push(UiCommand::RemoveHistory(entry.url.clone()));
                } else if let Some(disposition) = open_disposition(ui, &response) {
                    commands.push(UiCommand::OpenUrl {
                        url: entry.url.clone(),
                        disposition,
                    });
                }
            }
        });

    ui.separator();
    ui.label(RichText::new("Clear browsing data").strong());
    ui.horizontal(|ui| {
        for (range, label) in [
            (ClearRange::LastHour, "Last hour"),
            (ClearRange::LastDay, "Last 24 h"),
            (ClearRange::Everything, "All time"),
        ] {
            ui.selectable_value(&mut state.clear.range, range, label);
        }
    });
    ui.checkbox(&mut state.clear.history, "Browsing history");
    ui.checkbox(&mut state.clear.cookies, "Cookies and site data (signs you out)");
    ui.checkbox(&mut state.clear.cache, "Cached images and files");
    ui.checkbox(&mut state.clear.permissions, "Site permissions");
    let anything = state.clear.history || state.clear.cookies || state.clear.cache || state.clear.permissions;
    ui.add_enabled_ui(anything, |ui| {
        if pill_button(ui, palette, "Clear data", true).clicked() {
            commands.push(UiCommand::ClearData(state.clear));
        }
    });
}

// ----- bookmarks -----------------------------------------------------------------------

fn bookmarks(ui: &mut Ui, palette: &Palette, model: &UiModel<'_>, state: &mut PanelState, commands: &mut Vec<UiCommand>) {
    search_field(ui, palette, &mut state.bookmark_query, "Search bookmarks");
    let query = state.bookmark_query.trim();
    let items: Vec<_> = if query.is_empty() {
        model.bookmarks.items().iter().collect()
    } else {
        model.bookmarks.search(query, 200)
    };
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if items.is_empty() {
            ui.label(RichText::new("No bookmarks. Press Ctrl+D on a page to add one.").color(palette.text_faint));
        }
        for bookmark in items {
            if let Some((url, title)) = &mut state.rename {
                if *url == bookmark.url {
                    let response = ui.add(egui::TextEdit::singleline(title).desired_width(f32::INFINITY));
                    response.request_focus();
                    let (enter, escape) =
                        ui.input(|input| (input.key_pressed(egui::Key::Enter), input.key_pressed(egui::Key::Escape)));
                    if enter || (response.lost_focus() && !escape) {
                        commands.push(UiCommand::RenameBookmark(url.clone(), title.trim().to_owned()));
                        state.rename = None;
                    } else if escape {
                        state.rename = None;
                    }
                    continue;
                }
            }
            let title = if bookmark.title.is_empty() { display_text(&bookmark.url) } else { bookmark.title.clone() };
            let subtitle = bookmark.url.host_str().unwrap_or(bookmark.url.scheme()).to_owned();
            let (response, remove) = entry_row(ui, palette, &title, &subtitle, Some(Icon::Close));
            if remove {
                commands.push(UiCommand::RemoveBookmark(bookmark.url.clone()));
            } else if let Some(disposition) = open_disposition(ui, &response) {
                commands.push(UiCommand::OpenUrl {
                    url: bookmark.url.clone(),
                    disposition,
                });
            }
            response.context_menu(|ui| {
                if ui.button("Open in new tab").clicked() {
                    commands.push(UiCommand::OpenUrl {
                        url: bookmark.url.clone(),
                        disposition: Disposition::BackgroundTab,
                    });
                    ui.close();
                }
                if ui.button("Rename").clicked() {
                    state.rename = Some((bookmark.url.clone(), bookmark.title.clone()));
                    ui.close();
                }
                if ui.button("Delete").clicked() {
                    commands.push(UiCommand::RemoveBookmark(bookmark.url.clone()));
                    ui.close();
                }
            });
        }
    });
}

// ----- downloads -----------------------------------------------------------------------

fn downloads(ui: &mut Ui, palette: &Palette, model: &UiModel<'_>, commands: &mut Vec<UiCommand>) {
    let items = model.downloads.items();
    if items.iter().any(|item| item.state.is_finished()) && pill_button(ui, palette, "Clear finished", false).clicked() {
        commands.push(UiCommand::ClearFinishedDownloads);
    }
    ui.add_space(6.0);
    let time = ui.input(|input| input.time);
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if items.is_empty() {
            ui.label(RichText::new("Downloaded files appear here.").color(palette.text_faint));
        }
        for item in items {
            Frame::NONE
                .fill(palette.glass)
                .stroke(Stroke::new(1.0, palette.glass_stroke))
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(&item.file_name).strong());
                    let status = match &item.state {
                        DownloadState::AwaitingConfirmation => "Waiting for your confirmation".to_owned(),
                        DownloadState::InProgress { received, total } => match total {
                            Some(total) => format!("{} of {}", format_bytes(*received), format_bytes(*total)),
                            None => format!("{} received", format_bytes(*received)),
                        },
                        DownloadState::Completed { bytes } => format!("Completed · {}", format_bytes(*bytes)),
                        DownloadState::Failed { error } => format!("Failed: {error}"),
                        DownloadState::Cancelled => "Cancelled".to_owned(),
                    };
                    let color = match item.state {
                        DownloadState::Failed { .. } => palette.danger,
                        _ => palette.text_dim,
                    };
                    ui.label(RichText::new(status).small().color(color));
                    if let DownloadState::InProgress { .. } = item.state {
                        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                        progress_bar(ui.painter(), rect, item.state.progress(), palette, time);
                    }
                    ui.horizontal(|ui| match item.state {
                        DownloadState::InProgress { .. } | DownloadState::AwaitingConfirmation => {
                            if ui.button("Cancel").clicked() {
                                commands.push(UiCommand::CancelDownload(item.id));
                            }
                        },
                        DownloadState::Completed { .. } => {
                            if ui.button("Show in folder").clicked() {
                                commands.push(UiCommand::ShowDownload(item.id));
                            }
                            if ui.button("Remove from list").clicked() {
                                commands.push(UiCommand::RemoveDownload(item.id));
                            }
                        },
                        DownloadState::Failed { .. } | DownloadState::Cancelled => {
                            if ui.button("Retry").clicked() {
                                commands.push(UiCommand::DownloadUrl {
                                    url: item.url.clone(),
                                    referrer: None,
                                });
                            }
                            if ui.button("Remove from list").clicked() {
                                commands.push(UiCommand::RemoveDownload(item.id));
                            }
                        },
                    });
                });
            ui.add_space(6.0);
        }
    });
}

// ----- settings ------------------------------------------------------------------------

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(title).strong());
    ui.add_space(2.0);
}

fn settings(ui: &mut Ui, palette: &Palette, model: &UiModel<'_>, state: &mut PanelState, commands: &mut Vec<UiCommand>) {
    let PanelState {
        settings_draft,
        compat_new,
        ..
    } = state;
    let draft = settings_draft.get_or_insert_with(|| model.config.clone());
    let changed = *draft != *model.config;
    ScrollArea::vertical()
        .max_height(ui.available_height() - 48.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            section(ui, "Appearance");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut draft.ui.theme, ThemePreference::System, "System");
                ui.selectable_value(&mut draft.ui.theme, ThemePreference::Dark, "Dark");
                ui.selectable_value(&mut draft.ui.theme, ThemePreference::Light, "Light");
            });
            ui.checkbox(&mut draft.ui.show_bookmarks_bar, "Show bookmarks bar");
            ui.checkbox(&mut draft.ui.show_performance_hud, "Show performance overlay");

            section(ui, "Start and search");
            ui.checkbox(&mut draft.general.restore_session, "Reopen tabs from last time");
            ui.label(RichText::new("Home page").small().color(palette.text_dim));
            ui.add(egui::TextEdit::singleline(&mut draft.general.homepage).desired_width(f32::INFINITY));
            ui.label(RichText::new("Search engine").small().color(palette.text_dim));
            let current = draft.general.search_engine.name.clone();
            egui::ComboBox::from_id_salt("search-engine")
                .selected_text(current.as_str())
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for (name, template) in SEARCH_PRESETS {
                        if ui.selectable_label(current == name, name).clicked() {
                            draft.general.search_engine = SearchEngine {
                                name: name.to_owned(),
                                template: template.to_owned(),
                            };
                        }
                    }
                    if ui.selectable_label(!SEARCH_PRESETS.iter().any(|(name, _)| *name == current), "Custom").clicked() {
                        draft.general.search_engine.name = "Custom".to_owned();
                    }
                });
            if !SEARCH_PRESETS.iter().any(|(name, _)| *name == draft.general.search_engine.name) {
                ui.add(
                    egui::TextEdit::singleline(&mut draft.general.search_engine.template)
                        .hint_text("https://example.com/search?q={query}")
                        .desired_width(f32::INFINITY),
                );
            }

            section(ui, "Compatibility mode");
            ui.checkbox(&mut draft.compat.enabled, "Open sites that need it in compatibility mode");
            ui.label(
                RichText::new(
                    "Video and streaming sites (YouTube, Twitch, Netflix…) use web features Servo does not support yet. \
                     These sites open automatically on the Microsoft Edge WebView2 engine; every other site stays on \
                     Servo. Sign-ins on these sites are kept separately from Servo tabs.",
                )
                .small()
                .color(palette.text_faint),
            );
            ui.add_enabled_ui(draft.compat.enabled, |ui| {
                let mut remove = None;
                for (index, domain) in draft.compat.domains.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(domain).small());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    });
                }
                if let Some(index) = remove {
                    draft.compat.domains.remove(index);
                }
                ui.horizontal(|ui| {
                    let field = ui.add(
                        egui::TextEdit::singleline(compat_new)
                            .hint_text("Add a site, e.g. example.com")
                            .desired_width(ui.available_width() - 60.0),
                    );
                    let submitted = field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    if ui.small_button("Add").clicked() || submitted {
                        let domain = normalize_domain(&compat_new);
                        if !domain.is_empty() && !draft.compat.domains.contains(&domain) {
                            draft.compat.domains.push(domain);
                        }
                        compat_new.clear();
                    }
                });
                if ui.small_button("Reset list").clicked() {
                    draft.compat.domains = blanket_core::config::CompatConfig::default().domains;
                }
            });

            section(ui, "Memory");
            ui.checkbox(&mut draft.memory.suspension_enabled, "Put inactive tabs to sleep");
            ui.add_enabled_ui(draft.memory.suspension_enabled, |ui| {
                let mut minutes = (draft.memory.suspend_after_secs / 60).max(1);
                ui.add(egui::Slider::new(&mut minutes, 1..=240).text("minutes idle").logarithmic(true));
                draft.memory.suspend_after_secs = minutes * 60;
                let mut max_live = draft.memory.max_live_tabs;
                ui.add(egui::Slider::new(&mut max_live, 0..=60).text("max awake tabs (0 = automatic)"));
                draft.memory.max_live_tabs = max_live;
            });
            ui.label(
                RichText::new(
                    "Sleeping tabs keep their address, history and scroll position. Pages with unsaved form input, \
                     playing media or pinned tabs are never put to sleep automatically.",
                )
                .small()
                .color(palette.text_faint),
            );
            ui.checkbox(&mut draft.cache.clear_http_cache_on_critical, "Drop the web cache when memory is critically low");

            section(ui, "Privacy and security");
            ui.checkbox(&mut draft.security.prefer_https_for_typed_hosts, "Use HTTPS for typed addresses");
            ui.checkbox(&mut draft.security.confirm_dangerous_downloads, "Ask before downloading executable files");
            ui.checkbox(&mut draft.security.confirm_insecure_downloads, "Ask before downloading over HTTP");
            ui.checkbox(&mut draft.preload.dns_prefetch, "Pre-resolve addresses of frequently visited sites");

            section(ui, "Site permissions");
            let entries: Vec<_> = model.permissions.entries().collect();
            if entries.is_empty() {
                ui.label(RichText::new("No saved decisions.").small().color(palette.text_faint));
            }
            for (origin, kind, decision) in entries {
                ui.horizontal(|ui| {
                    let decision = match decision {
                        PermissionDecision::Allow => "allowed",
                        PermissionDecision::Deny => "blocked",
                    };
                    ui.label(RichText::new(format!("{origin} · {} {decision}", permission_label(kind))).small());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Forget").clicked() {
                            if let Ok(url) = Url::parse(origin) {
                                commands.push(UiCommand::ForgetPermissions(url));
                            }
                        }
                    });
                });
            }

            section(ui, "About");
            ui.label(
                RichText::new(format!(
                    "Blanket {} · Built with Rust. Powered by Servo.",
                    env!("CARGO_PKG_VERSION")
                ))
                .small()
                .color(palette.text_dim),
            );
        });
    ui.separator();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(changed, |ui| {
            if pill_button(ui, palette, "Save", true).clicked() {
                commands.push(UiCommand::UpdateConfig(Box::new(draft.clone())));
            }
            if pill_button(ui, palette, "Discard", false).clicked() {
                *draft = model.config.clone();
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.small_button("Restore defaults").clicked() {
                *draft = BlanketConfig::default();
            }
        });
    });
}

pub fn permission_label(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Geolocation => "location",
        PermissionKind::Notifications => "notifications",
        PermissionKind::Camera => "camera",
        PermissionKind::Microphone => "microphone",
        PermissionKind::Clipboard => "clipboard",
        PermissionKind::Bluetooth => "Bluetooth",
        PermissionKind::Other => "other",
    }
}

// ----- performance ---------------------------------------------------------------------

fn stat(ui: &mut Ui, palette: &Palette, label: &str, value: String) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(palette.text_dim));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(value).monospace());
        });
    });
}

fn performance(ui: &mut Ui, palette: &Palette, model: &UiModel<'_>, commands: &mut Vec<UiCommand>) {
    let perf = model.perf;
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        section(ui, "Blanket process");
        stat(ui, palette, "Memory (resident)", perf.process_rss.map_or("–".into(), format_bytes));
        stat(ui, palette, "CPU", perf.process_cpu.map_or("–".into(), |cpu| format!("{cpu:.1}%")));
        let samples: Vec<f32> = perf.samples.iter().map(|(rss, _)| *rss as f32).collect();
        if samples.len() > 1 {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
            ui.painter().rect_filled(rect, CornerRadius::same(8), palette.glass);
            sparkline(ui.painter(), rect.shrink(6.0), &samples, palette.accent);
        }

        section(ui, "System");
        if let Some(system) = perf.system {
            stat(
                ui,
                palette,
                "Available memory",
                format!(
                    "{} of {} ({:.0}%)",
                    format_bytes(system.available_bytes),
                    format_bytes(system.total_bytes),
                    system.available_percent()
                ),
            );
        }
        let pressure = match perf.pressure {
            Some(MemoryPressure::Normal) => "Normal",
            Some(MemoryPressure::Moderate) => "Moderate",
            Some(MemoryPressure::Critical) => "Critical",
            None => "–",
        };
        stat(ui, palette, "Memory pressure", pressure.to_owned());
        stat(ui, palette, "Awake tab budget", perf.live_budget.map_or("–".into(), |budget| budget.to_string()));

        section(ui, "Caches");
        if let Some(entries) = perf.http_cache_entries {
            stat(ui, palette, "Web cache entries", entries.to_string());
        }
        for (label, stats) in [("Favicons", perf.favicon_cache), ("Tab previews", perf.thumbnail_cache)] {
            stat(
                ui,
                palette,
                label,
                format!(
                    "{} / {} · {:.0}% hits",
                    format_bytes(stats.bytes),
                    format_bytes(stats.budget_bytes),
                    stats.hit_ratio() * 100.0
                ),
            );
        }

        section(ui, &format!("Tabs ({} awake, {} asleep)", model.tabs.live_count(), model.tabs.suspended_count()));
        let active = model.tabs.active_id();
        for tab in model.tabs.tabs() {
            ui.horizontal(|ui| {
                let (state, color) = match &tab.lifecycle {
                    TabLifecycle::Live if Some(tab.id) == active => ("active", palette.accent),
                    TabLifecycle::Live => ("awake", palette.text_dim),
                    TabLifecycle::Suspended { .. } => ("asleep", palette.text_faint),
                    TabLifecycle::Crashed { .. } => ("crashed", palette.warning),
                };
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                let title = super::widgets::ellipsize(tab.display_title(), 30);
                ui.label(RichText::new(title).small());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    match tab.lifecycle {
                        TabLifecycle::Live if Some(tab.id) != active => {
                            if ui.small_button("Sleep").clicked() {
                                commands.push(UiCommand::SuspendTab(tab.id));
                            }
                        },
                        TabLifecycle::Suspended { .. } => {
                            if ui.small_button("Wake").clicked() {
                                commands.push(UiCommand::WakeTab(tab.id));
                            }
                        },
                        _ => {},
                    }
                    ui.label(RichText::new(state).small().color(color));
                });
            });
        }
        ui.add_space(8.0);
        if pill_button(ui, palette, "Refresh now", false).clicked() {
            commands.push(UiCommand::RefreshPerformance);
        }
        let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
        icons::paint(ui.painter(), rect, Icon::Info, palette.text_faint);
        ui.label(
            RichText::new(
                "Memory is the resident size of the whole Blanket process, including the engine. \
                 Per-tab figures are not shown because the engine shares memory between tabs.",
            )
            .small()
            .color(palette.text_faint),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::normalize_domain;

    #[test]
    fn compat_domains_are_normalized() {
        assert_eq!(normalize_domain("  https://www.Example.com/path?q=1 "), "example.com");
        assert_eq!(normalize_domain("music.youtube.com:443"), "music.youtube.com");
        assert_eq!(normalize_domain("twitch.tv"), "twitch.tv");
        assert_eq!(normalize_domain("   "), "");
    }
}
