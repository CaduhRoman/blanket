//! Native prompts that Servo delegates to the embedder: JavaScript dialogs, file pickers,
//! `<select>` menus, color pickers, HTTP authentication, permissions, context menus, and
//! Blanket's own download / leave-page confirmations.
//!
//! The types and methods used here are those of Servo 0.8 (`FilePicker::select` /
//! `submit` / `dismiss`, `SelectElement::select` with option ids, `SimpleDialog`,
//! `ContextMenu::select`). Dropping an unanswered request uses the engine's default
//! (deny / dismiss).

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll, Waker};

use blanket_core::ids::{DownloadId, TabId};
use blanket_core::security::{DownloadRisk, PermissionDecision, PermissionKind};
use egui::{
    Align2, Area, Color32, CornerRadius, Frame, Id, Key, Margin, Modal, Order, Pos2, Rect, RichText, Sense, Stroke, Ui,
    vec2,
};
use servo::{
    AlertDialog, AllowOrDenyRequest, AuthenticationRequest, ColorPicker, ConfirmDialog, ContextMenu, ContextMenuAction,
    ContextMenuItem, EmbedderControl, EmbedderControlId, FilePicker, PermissionRequest, PromptDialog, RgbColor,
    SelectElement, SelectElementOption, SelectElementOptionOrOptgroup, SimpleDialog,
};
use url::Url;
use winit::window::Window;

use super::panels::permission_label;
use super::theme::Palette;
use super::widgets::{ellipsize, pill_button};
use super::{UiCommand, UiModel};

const CARD_WIDTH: f32 = 420.0;

enum PendingFile {
    One(Pin<Box<dyn Future<Output = Option<rfd::FileHandle>>>>),
    Many(Pin<Box<dyn Future<Output = Option<Vec<rfd::FileHandle>>>>>),
}

pub enum Dialog {
    File {
        id: EmbedderControlId,
        future: PendingFile,
        waker: Waker,
        picker: Option<FilePicker>,
    },
    Alert {
        id: EmbedderControlId,
        dialog: Option<AlertDialog>,
    },
    Confirm {
        id: EmbedderControlId,
        dialog: Option<ConfirmDialog>,
    },
    Prompt {
        id: EmbedderControlId,
        dialog: Option<PromptDialog>,
        value: String,
    },
    Select {
        id: EmbedderControlId,
        prompt: Option<SelectElement>,
        selected: Vec<usize>,
    },
    Color {
        id: EmbedderControlId,
        prompt: Option<ColorPicker>,
        color: Color32,
    },
    Context {
        id: EmbedderControlId,
        menu: Option<ContextMenu>,
        extras: Vec<ContextExtra>,
    },
    Authentication {
        host: String,
        request: Option<AuthenticationRequest>,
        username: String,
        password: String,
    },
    Permission {
        url: Url,
        kind: PermissionKind,
        request: Option<PermissionRequest>,
        remember: bool,
    },
    Unload {
        request: Option<AllowOrDenyRequest>,
    },
    DownloadConfirm {
        id: DownloadId,
        file_name: String,
        source: Url,
        risk: DownloadRisk,
    },
}

struct ContextExtra {
    label: String,
    command: UiCommand,
}

impl Dialog {
    pub fn file_picker(picker: FilePicker, window: &Window, waker: Waker) -> Self {
        let id = picker.id();
        let mut dialog = rfd::AsyncFileDialog::new().set_parent(window).set_title("Choose file");
        if !picker.filter_patterns().is_empty() {
            let filters: Vec<String> = picker.filter_patterns().iter().map(|pattern| pattern.0.clone()).collect();
            dialog = dialog.add_filter("Supported files", &filters);
        }
        let future = if picker.allow_select_multiple() {
            PendingFile::Many(Box::pin(dialog.pick_files()))
        } else {
            PendingFile::One(Box::pin(dialog.pick_file()))
        };
        Self::File {
            id,
            future,
            waker,
            picker: Some(picker),
        }
    }

    pub fn from_control(control: EmbedderControl) -> Self {
        let id = control.id();
        match control {
            EmbedderControl::SimpleDialog(SimpleDialog::Alert(dialog)) => Self::Alert {
                id,
                dialog: Some(dialog),
            },
            EmbedderControl::SimpleDialog(SimpleDialog::Confirm(dialog)) => Self::Confirm {
                id,
                dialog: Some(dialog),
            },
            EmbedderControl::SimpleDialog(SimpleDialog::Prompt(dialog)) => {
                let value = dialog.current_value().to_owned();
                Self::Prompt {
                    id,
                    dialog: Some(dialog),
                    value,
                }
            },
            EmbedderControl::SelectElement(prompt) => {
                let selected = prompt.selected_options();
                Self::Select {
                    id,
                    prompt: Some(prompt),
                    selected,
                }
            },
            EmbedderControl::ColorPicker(prompt) => {
                let color = prompt
                    .current_color()
                    .map(|color| Color32::from_rgb(color.red, color.green, color.blue))
                    .unwrap_or(Color32::WHITE);
                Self::Color {
                    id,
                    prompt: Some(prompt),
                    color,
                }
            },
            EmbedderControl::ContextMenu(menu) => {
                let extras = context_extras(menu.element_info());
                Self::Context {
                    id,
                    menu: Some(menu),
                    extras,
                }
            },
            EmbedderControl::InputMethod(_) | EmbedderControl::FilePicker(_) => {
                unreachable!("input method and file picker are handled by the caller")
            },
        }
    }

    pub fn authentication(host: String, request: AuthenticationRequest) -> Self {
        Self::Authentication {
            host,
            request: Some(request),
            username: String::new(),
            password: String::new(),
        }
    }

    pub fn permission(url: Url, kind: PermissionKind, request: PermissionRequest) -> Self {
        Self::Permission {
            url,
            kind,
            request: Some(request),
            remember: true,
        }
    }

    pub fn unload(request: AllowOrDenyRequest) -> Self {
        Self::Unload { request: Some(request) }
    }

    pub fn download_confirmation(id: DownloadId, file_name: String, source: Url, risk: DownloadRisk) -> Self {
        Self::DownloadConfirm {
            id,
            file_name,
            source,
            risk,
        }
    }

    fn control_id(&self) -> Option<EmbedderControlId> {
        match self {
            Self::File { id, .. } |
            Self::Alert { id, .. } |
            Self::Confirm { id, .. } |
            Self::Prompt { id, .. } |
            Self::Select { id, .. } |
            Self::Color { id, .. } |
            Self::Context { id, .. } => Some(*id),
            _ => None,
        }
    }

    fn is_file_picker(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    fn poll_file(&mut self) -> bool {
        let Self::File {
            future,
            waker,
            picker,
            ..
        } = self
        else {
            return false;
        };
        let Some(active) = picker.as_mut() else {
            return true;
        };
        let mut context = TaskContext::from_waker(waker);
        let ready = match future {
            PendingFile::One(future) => match future.as_mut().poll(&mut context) {
                Poll::Ready(Some(handle)) => Some(vec![handle.path().to_path_buf()]),
                Poll::Ready(None) => Some(Vec::new()),
                Poll::Pending => None,
            },
            PendingFile::Many(future) => match future.as_mut().poll(&mut context) {
                Poll::Ready(Some(handles)) => Some(handles.iter().map(|handle| handle.path().to_path_buf()).collect()),
                Poll::Ready(None) => Some(Vec::new()),
                Poll::Pending => None,
            },
        };
        match ready {
            Some(paths) if paths.is_empty() => {
                picker.take().unwrap().dismiss();
                true
            },
            Some(paths) => {
                active.select(&paths);
                picker.take().unwrap().submit();
                true
            },
            None => false,
        }
    }
}

fn context_extras(info: &servo::ContextMenuElementInformation) -> Vec<ContextExtra> {
    let mut extras = Vec::new();
    if let Some(url) = info.link_url.clone() {
        extras.push(ContextExtra {
            label: "Open link in new tab".into(),
            command: UiCommand::OpenUrl {
                url: url.clone(),
                disposition: super::Disposition::NewTab,
            },
        });
        extras.push(ContextExtra {
            label: "Download link".into(),
            command: UiCommand::DownloadUrl {
                url,
                referrer: None,
            },
        });
    }
    if let Some(url) = info.image_url.clone() {
        extras.push(ContextExtra {
            label: "Open image in new tab".into(),
            command: UiCommand::OpenUrl {
                url: url.clone(),
                disposition: super::Disposition::NewTab,
            },
        });
        extras.push(ContextExtra {
            label: "Save image".into(),
            command: UiCommand::DownloadUrl { url, referrer: None },
        });
    }
    extras
}

#[derive(Default)]
pub struct Dialogs {
    by_tab: HashMap<TabId, Vec<Dialog>>,
    global: Vec<Dialog>,
}

impl Dialogs {
    pub fn push(&mut self, tab: TabId, dialog: Dialog) {
        self.by_tab.entry(tab).or_default().push(dialog);
    }

    pub fn push_global(&mut self, dialog: Dialog) {
        self.global.push(dialog);
    }

    pub fn remove_tab(&mut self, tab: TabId) {
        self.by_tab.remove(&tab);
    }

    pub fn remove_control(&mut self, tab: TabId, id: EmbedderControlId) {
        if let Some(dialogs) = self.by_tab.get_mut(&tab) {
            dialogs.retain(|dialog| dialog.control_id() != Some(id));
            if dialogs.is_empty() {
                self.by_tab.remove(&tab);
            }
        }
    }

    pub fn captures_input(&self, active: Option<TabId>) -> bool {
        if !self.global.is_empty() {
            return true;
        }
        active
            .and_then(|id| self.by_tab.get(&id))
            .is_some_and(|dialogs| dialogs.iter().any(|dialog| !dialog.is_file_picker()))
    }

    /// Advance native file pickers. Returns true if any picker finished.
    pub fn poll_file_pickers(&mut self) -> bool {
        let mut changed = false;
        for dialogs in self.by_tab.values_mut() {
            dialogs.retain_mut(|dialog| {
                if dialog.is_file_picker() {
                    let done = dialog.poll_file();
                    changed |= done;
                    !done
                } else {
                    true
                }
            });
        }
        self.by_tab.retain(|_, dialogs| !dialogs.is_empty());
        changed
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        palette: &Palette,
        content: Rect,
        model: &UiModel<'_>,
        commands: &mut Vec<UiCommand>,
    ) {
        let mut global_keep = Vec::new();
        for mut dialog in self.global.drain(..) {
            if show_one(ctx, palette, content, model, commands, &mut dialog) {
                global_keep.push(dialog);
            }
        }
        self.global = global_keep;

        let Some(active) = model.active.id else {
            return;
        };
        let Some(mut dialogs) = self.by_tab.remove(&active) else {
            return;
        };
        dialogs.retain_mut(|dialog| show_one(ctx, palette, content, model, commands, dialog));
        if !dialogs.is_empty() {
            self.by_tab.insert(active, dialogs);
        }
    }
}

/// Returns `true` if the dialog should stay open.
fn show_one(
    ctx: &egui::Context,
    palette: &Palette,
    content: Rect,
    model: &UiModel<'_>,
    commands: &mut Vec<UiCommand>,
    dialog: &mut Dialog,
) -> bool {
    match dialog {
        Dialog::File { .. } => true,
        Dialog::Alert { dialog, .. } => show_alert(ctx, palette, dialog),
        Dialog::Confirm { dialog, .. } => show_confirm(ctx, palette, dialog),
        Dialog::Prompt { dialog, value, .. } => show_prompt(ctx, palette, dialog, value),
        Dialog::Select {
            prompt,
            selected,
            ..
        } => show_select(ctx, palette, content, prompt, selected),
        Dialog::Color { prompt, color, .. } => show_color(ctx, palette, content, prompt, color),
        Dialog::Context { menu, extras, .. } => show_context(ctx, palette, content, menu, extras, commands),
        Dialog::Authentication {
            host,
            request,
            username,
            password,
        } => show_authentication(ctx, palette, host, request, username, password),
        Dialog::Permission {
            url,
            kind,
            request,
            remember,
        } => show_permission(ctx, palette, url, *kind, request, remember, commands),
        Dialog::Unload { request } => show_unload(ctx, palette, request),
        Dialog::DownloadConfirm {
            id,
            file_name,
            source,
            risk,
        } => show_download_confirm(ctx, palette, model, *id, file_name, source, risk, commands),
    }
}

fn card_modal<'a>(id: &str, palette: &'a Palette) -> impl FnOnce(&egui::Context, &mut dyn FnMut(&mut Ui)) + 'a {
    let id = id.to_owned();
    move |ctx, body| {
        Modal::new(Id::new(id)).show(ctx, |ui| {
            Frame::NONE
                .fill(palette.card)
                .stroke(Stroke::new(1.0, palette.glass_stroke))
                .corner_radius(CornerRadius::same(16))
                .inner_margin(Margin::same(18))
                .show(ui, |ui| {
                    ui.set_width(CARD_WIDTH.min(ui.ctx().content_rect().width() - 48.0));
                    body(ui);
                });
        });
    }
}

fn page_says(ui: &mut Ui, palette: &Palette) {
    ui.label(RichText::new("This page says").small().color(palette.text_faint));
    ui.add_space(4.0);
}

fn show_alert(ctx: &egui::Context, palette: &Palette, dialog: &mut Option<AlertDialog>) -> bool {
    let Some(alert) = dialog.as_ref() else {
        return false;
    };
    let mut close = false;
    card_modal("js-alert", palette)(ctx, &mut |ui| {
        page_says(ui, palette);
        ui.label(alert.message());
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_button(ui, palette, "OK", true).clicked() || ui.input(|input| input.key_pressed(Key::Escape) || input.key_pressed(Key::Enter)) {
                close = true;
            }
        });
    });
    if close {
        if let Some(alert) = dialog.take() {
            alert.confirm();
        }
        false
    } else {
        true
    }
}

fn show_confirm(ctx: &egui::Context, palette: &Palette, dialog: &mut Option<ConfirmDialog>) -> bool {
    let Some(confirm) = dialog.as_ref() else {
        return false;
    };
    let mut action = None;
    card_modal("js-confirm", palette)(ctx, &mut |ui| {
        page_says(ui, palette);
        ui.label(confirm.message());
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_button(ui, palette, "OK", true).clicked() || ui.input(|input| input.key_pressed(Key::Enter)) {
                action = Some(true);
            }
            if pill_button(ui, palette, "Cancel", false).clicked() || ui.input(|input| input.key_pressed(Key::Escape)) {
                action = Some(false);
            }
        });
    });
    match action {
        Some(true) => {
            dialog.take().unwrap().confirm();
            false
        },
        Some(false) => {
            dialog.take().unwrap().dismiss();
            false
        },
        None => true,
    }
}

fn show_prompt(ctx: &egui::Context, palette: &Palette, dialog: &mut Option<PromptDialog>, value: &mut String) -> bool {
    let Some(prompt) = dialog.as_mut() else {
        return false;
    };
    let mut action = None;
    card_modal("js-prompt", palette)(ctx, &mut |ui| {
        page_says(ui, palette);
        ui.label(prompt.message());
        ui.add_space(8.0);
        let response = ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
        response.request_focus();
        prompt.set_current_value(value);
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_button(ui, palette, "OK", true).clicked() || ui.input(|input| input.key_pressed(Key::Enter)) {
                action = Some(true);
            }
            if pill_button(ui, palette, "Cancel", false).clicked() || ui.input(|input| input.key_pressed(Key::Escape)) {
                action = Some(false);
            }
        });
    });
    match action {
        Some(true) => {
            dialog.take().unwrap().confirm();
            false
        },
        Some(false) => {
            dialog.take().unwrap().dismiss();
            false
        },
        None => true,
    }
}

fn show_authentication(
    ctx: &egui::Context,
    palette: &Palette,
    host: &str,
    request: &mut Option<AuthenticationRequest>,
    username: &mut String,
    password: &mut String,
) -> bool {
    let mut action = None;
    card_modal("http-auth", palette)(ctx, &mut |ui| {
        ui.label(RichText::new(format!("Sign in to {host}")).strong());
        ui.add_space(6.0);
        ui.label(RichText::new("This site is asking for a username and password.").color(palette.text_dim));
        ui.add_space(10.0);
        ui.label(RichText::new("Username").small().color(palette.text_dim));
        ui.add(egui::TextEdit::singleline(username).desired_width(f32::INFINITY));
        ui.label(RichText::new("Password").small().color(palette.text_dim));
        ui.add(egui::TextEdit::singleline(password).password(true).desired_width(f32::INFINITY));
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_button(ui, palette, "Sign in", true).clicked() || ui.input(|input| input.key_pressed(Key::Enter)) {
                action = Some(true);
            }
            if pill_button(ui, palette, "Cancel", false).clicked() || ui.input(|input| input.key_pressed(Key::Escape)) {
                action = Some(false);
            }
        });
    });
    match action {
        Some(true) => {
            if let Some(request) = request.take() {
                request.authenticate(username.clone(), password.clone());
            }
            false
        },
        Some(false) => {
            request.take();
            false
        },
        None => true,
    }
}

fn show_permission(
    ctx: &egui::Context,
    palette: &Palette,
    url: &Url,
    kind: PermissionKind,
    request: &mut Option<PermissionRequest>,
    remember: &mut bool,
    commands: &mut Vec<UiCommand>,
) -> bool {
    let mut action = None;
    let host = url.host_str().unwrap_or(url.as_str()).to_owned();
    let feature = permission_label(kind);
    card_modal("permission", palette)(ctx, &mut |ui| {
        ui.label(RichText::new(format!("{host} wants to use {feature}")).strong());
        ui.add_space(8.0);
        ui.checkbox(remember, "Remember this decision");
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_button(ui, palette, "Allow", true).clicked() {
                action = Some(true);
            }
            if pill_button(ui, palette, "Block", false).clicked() || ui.input(|input| input.key_pressed(Key::Escape)) {
                action = Some(false);
            }
        });
    });
    match action {
        Some(allow) => {
            if *remember {
                commands.push(UiCommand::RememberPermission {
                    url: url.clone(),
                    kind,
                    decision: if allow { PermissionDecision::Allow } else { PermissionDecision::Deny },
                });
            }
            if let Some(request) = request.take() {
                if allow { request.allow() } else { request.deny() }
            }
            false
        },
        None => true,
    }
}

fn show_unload(ctx: &egui::Context, palette: &Palette, request: &mut Option<AllowOrDenyRequest>) -> bool {
    let mut action = None;
    card_modal("unload", palette)(ctx, &mut |ui| {
        page_says(ui, palette);
        ui.label("This page is asking you to confirm that you want to leave.");
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_button(ui, palette, "Leave", true).clicked() {
                action = Some(true);
            }
            if pill_button(ui, palette, "Stay", false).clicked() || ui.input(|input| input.key_pressed(Key::Escape)) {
                action = Some(false);
            }
        });
    });
    match action {
        Some(true) => {
            request.take().unwrap().allow();
            false
        },
        Some(false) => {
            request.take().unwrap().deny();
            false
        },
        None => true,
    }
}

fn show_download_confirm(
    ctx: &egui::Context,
    palette: &Palette,
    _model: &UiModel<'_>,
    id: DownloadId,
    file_name: &str,
    source: &Url,
    risk: &DownloadRisk,
    commands: &mut Vec<UiCommand>,
) -> bool {
    let mut action = None;
    let host = source.host_str().unwrap_or(source.as_str()).to_owned();
    card_modal("download-confirm", palette)(ctx, &mut |ui| {
        ui.label(RichText::new("Download this file?").strong());
        ui.add_space(6.0);
        ui.label(RichText::new(file_name).strong());
        ui.label(RichText::new(format!("From {host}")).small().color(palette.text_dim));
        if risk.executable {
            ui.add_space(8.0);
            ui.label(RichText::new("This file type can run programs on your computer.").color(palette.warning));
        }
        if risk.insecure_transport {
            ui.add_space(4.0);
            ui.label(RichText::new("The transfer is not encrypted (HTTP).").color(palette.warning));
        }
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if pill_button(ui, palette, "Download", true).clicked() {
                action = Some(true);
            }
            if pill_button(ui, palette, "Cancel", false).clicked() || ui.input(|input| input.key_pressed(Key::Escape)) {
                action = Some(false);
            }
        });
    });
    match action {
        Some(allow) => {
            commands.push(UiCommand::ConfirmDownload(id, allow));
            false
        },
        None => true,
    }
}

fn device_to_points(rect: servo::DeviceIntRect, content: Rect, pixels_per_point: f32) -> Rect {
    let scale = pixels_per_point.max(0.01);
    Rect::from_min_max(
        Pos2::new(content.min.x + rect.min.x as f32 / scale, content.min.y + rect.min.y as f32 / scale),
        Pos2::new(content.min.x + rect.max.x as f32 / scale, content.min.y + rect.max.y as f32 / scale),
    )
}

fn show_select(
    ctx: &egui::Context,
    palette: &Palette,
    content: Rect,
    prompt: &mut Option<SelectElement>,
    selected: &mut Vec<usize>,
) -> bool {
    let Some(element) = prompt.as_mut() else {
        return false;
    };
    let mut open = true;
    let anchor = device_to_points(element.position(), content, ctx.pixels_per_point());
    let multiple = element.allow_select_multiple();
    let response = Area::new(Id::new("select-element"))
        .order(Order::Foreground)
        .fixed_pos(Pos2::new(anchor.min.x, anchor.max.y + 4.0))
        .show(ctx, |ui| {
            Frame::popup(ui.style())
                .fill(palette.card)
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_min_width(anchor.width().max(180.0));
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                        for item in element.options() {
                            match item {
                                SelectElementOptionOrOptgroup::Option(option) => {
                                    select_row(ui, palette, option, selected, multiple, false, &mut open);
                                },
                                SelectElementOptionOrOptgroup::Optgroup { label, options } => {
                                    ui.label(RichText::new(label).small().color(palette.text_faint));
                                    for option in options {
                                        select_row(ui, palette, option, selected, multiple, true, &mut open);
                                    }
                                },
                            }
                        }
                    });
                });
        });
    if ui_clicked_outside(ctx, response.response.rect) || ctx.input(|input| input.key_pressed(Key::Escape)) {
        open = false;
    }
    element.select(selected.clone());
    if !open {
        prompt.take().unwrap().submit();
        false
    } else {
        true
    }
}

fn select_row(
    ui: &mut Ui,
    palette: &Palette,
    option: &SelectElementOption,
    selected: &mut Vec<usize>,
    multiple: bool,
    indented: bool,
    open: &mut bool,
) {
    let checked = selected.contains(&option.id);
    let label = if indented { format!("    {}", option.label) } else { option.label.clone() };
    let text = if option.is_disabled {
        RichText::new(label).strikethrough().color(palette.text_faint)
    } else {
        RichText::new(label).color(palette.text)
    };
    let response = ui.add_enabled(!option.is_disabled, egui::Button::selectable(checked, text));
    if response.clicked() {
        if multiple {
            if let Some(index) = selected.iter().position(|id| *id == option.id) {
                selected.remove(index);
            } else {
                selected.push(option.id);
            }
        } else {
            selected.clear();
            selected.push(option.id);
            *open = false;
        }
    }
}

fn show_color(
    ctx: &egui::Context,
    palette: &Palette,
    content: Rect,
    prompt: &mut Option<ColorPicker>,
    color: &mut Color32,
) -> bool {
    let Some(picker) = prompt.as_mut() else {
        return false;
    };
    let mut open = true;
    let mut chosen = None;
    let anchor = device_to_points(picker.position(), content, ctx.pixels_per_point());
    let response = Area::new(Id::new("color-picker"))
        .order(Order::Foreground)
        .fixed_pos(Pos2::new(anchor.min.x, anchor.max.y + 4.0))
        .show(ctx, |ui| {
            Frame::popup(ui.style())
                .fill(palette.card)
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    egui::widgets::color_picker::color_picker_color32(
                        ui,
                        color,
                        egui::widgets::color_picker::Alpha::Opaque,
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if pill_button(ui, palette, "Select", true).clicked() {
                            chosen = Some(true);
                            open = false;
                        }
                        if pill_button(ui, palette, "Cancel", false).clicked() {
                            chosen = Some(false);
                            open = false;
                        }
                    });
                });
        });
    if ui_clicked_outside(ctx, response.response.rect) || ctx.input(|input| input.key_pressed(Key::Escape)) {
        chosen = Some(false);
        open = false;
    }
    if let Some(submit) = chosen {
        if submit {
            picker.select(Some(RgbColor {
                red: color.r(),
                green: color.g(),
                blue: color.b(),
            }));
        } else {
            picker.select(None);
        }
        prompt.take().unwrap().submit();
    }
    open
}

fn show_context(
    ctx: &egui::Context,
    palette: &Palette,
    content: Rect,
    menu: &mut Option<ContextMenu>,
    extras: &[ContextExtra],
    commands: &mut Vec<UiCommand>,
) -> bool {
    let Some(context) = menu.as_ref() else {
        return false;
    };
    let mut selected: Option<ContextMenuAction> = None;
    let mut extra: Option<UiCommand> = None;
    let mut open = true;
    let anchor = device_to_points(context.position(), content, ctx.pixels_per_point());
    let response = Area::new(Id::new("page-context-menu"))
        .order(Order::Foreground)
        .fixed_pos(anchor.min)
        .show(ctx, |ui| {
            Frame::popup(ui.style())
                .fill(palette.card)
                .corner_radius(CornerRadius::same(12))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    ui.set_min_width(200.0);
                    for extra_item in extras {
                        if menu_item(ui, palette, &extra_item.label, true) {
                            extra = Some(extra_item.command.clone());
                            open = false;
                        }
                    }
                    if !extras.is_empty() && !context.items().is_empty() {
                        ui.separator();
                    }
                    for item in context.items() {
                        match item {
                            ContextMenuItem::Item { label, action, enabled } => {
                                if menu_item(ui, palette, label, *enabled) {
                                    selected = Some(*action);
                                    open = false;
                                }
                            },
                            ContextMenuItem::Separator => {
                                ui.separator();
                            },
                        }
                    }
                });
        });
    if ui_clicked_outside(ctx, response.response.rect) || ctx.input(|input| input.key_pressed(Key::Escape)) {
        open = false;
    }
    if let Some(action) = selected {
        menu.take().unwrap().select(action);
        return false;
    }
    if extra.is_some() {
        if let Some(command) = extra {
            commands.push(command);
        }
        if let Some(menu) = menu.take() {
            menu.dismiss();
        }
        return false;
    }
    if !open {
        if let Some(menu) = menu.take() {
            menu.dismiss();
        }
        false
    } else {
        true
    }
}

fn menu_item(ui: &mut Ui, palette: &Palette, label: &str, enabled: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width().max(200.0), 28.0), if enabled { Sense::click() } else { Sense::hover() });
    if response.hovered() && enabled {
        ui.painter().rect_filled(rect, CornerRadius::same(8), palette.glass_hover);
    }
    ui.painter().text(
        rect.left_center() + vec2(10.0, 0.0),
        Align2::LEFT_CENTER,
        ellipsize(label, 42),
        egui::FontId::proportional(13.5),
        if enabled { palette.text } else { palette.text_faint },
    );
    enabled && response.clicked()
}

fn ui_clicked_outside(ctx: &egui::Context, rect: Rect) -> bool {
    ctx.input(|input| {
        input.pointer.any_pressed() && input.pointer.interact_pos().is_some_and(|pos| !rect.contains(pos))
    })
}
