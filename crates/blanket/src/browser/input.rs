//! Window input routing.
//!
//! Browser shortcuts are matched first and never reach the page. Then each event goes
//! either to the egui chrome (pointer over the toolbar, panels or popups; keyboard while
//! an egui widget has focus; everything while a modal dialog is open) or to the active
//! web view, with coordinates translated into the content area.

use blanket_core::ids::TabId;
use euclid::Point2D;
use keyboard_types::{Key, Modifiers, NamedKey, ShortcutMatcher};
use servo::{
    CompositionEvent, CompositionState, Cursor, DevicePixel, DevicePoint, EmbedderControlId, ImeEvent, InputEvent,
    MouseButton as ServoMouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent,
    RenderingContext, WheelDelta, WheelEvent, WheelMode,
};
use url::Url;
use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::ModifiersState;
use winit::window::{CursorIcon, ResizeDirection};

use super::Browser;
use crate::keyutils::{CMD_OR_CONTROL, keyboard_event_from_winit};
use crate::ui::{Disposition, Panel, UiCommand, ZoomChange};

/// Pixels scrolled per wheel "line", matching servoshell.
const LINE_HEIGHT: f32 = 76.0;
const LINE_WIDTH: f32 = 76.0;

#[derive(Default)]
pub struct InputState {
    pub modifiers: ModifiersState,
    /// Cursor position in physical window pixels.
    pub cursor: Option<PhysicalPosition<f64>>,
    pub pointer_over_content: bool,
    /// A mouse button was pressed inside the page; keep routing to it until release so
    /// drags (text selection, scrollbars) work across the chrome.
    pub page_capture: bool,
    pub ime_control: Option<EmbedderControlId>,
    /// The pointer is on the window's resize border.
    pub resize_edge: Option<ResizeDirection>,
}

impl Browser {
    pub fn handle_window_event(&mut self, event: WindowEvent) {
        match &event {
            WindowEvent::RedrawRequested => {
                self.redraw_now();
                return;
            },
            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    self.window_context.resize(*size);
                }
                self.update_screen_geometry();
                self.request_redraw();
            },
            WindowEvent::Moved(_) => self.update_screen_geometry(),
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                let scale = euclid::Scale::new(*scale_factor as f32);
                self.delegate.set_scale(scale);
                for view in self.views.values() {
                    view.webview.set_hidpi_scale_factor(scale);
                }
                self.request_redraw();
            },
            WindowEvent::CloseRequested => {
                self.exit_requested = true;
                return;
            },
            WindowEvent::ThemeChanged(theme) => {
                let config = self.config.clone();
                self.gui_mut().apply_config(&config, Some(*theme));
                let theme = crate::ui::theme::resolve_theme(self.config.ui.theme, Some(*theme));
                for view in self.views.values() {
                    view.webview.notify_theme_change(theme);
                }
                self.request_redraw();
            },
            WindowEvent::ModifiersChanged(modifiers) => self.input.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { .. } |
            WindowEvent::MouseInput { .. } |
            WindowEvent::MouseWheel { .. } |
            WindowEvent::Ime(_) => self.note_interaction(),
            _ => {},
        }

        if let WindowEvent::KeyboardInput { event: key_event, .. } = &event {
            if key_event.state == ElementState::Pressed {
                let keyboard_event = keyboard_event_from_winit(key_event, self.input.modifiers);
                if let Some(command) = self.match_shortcut(&keyboard_event.event) {
                    self.execute(command);
                    return;
                }
            }
        }

        if let WindowEvent::CursorMoved { position, .. } = &event {
            self.input.cursor = Some(*position);
        }
        // A click on the chrome takes keyboard focus back from a compatibility page.
        if matches!(event, WindowEvent::MouseInput { state: ElementState::Pressed, .. }) {
            self.compat.release_focus();
        }
        if self.handle_resize_border(&event) {
            return;
        }

        let route_to_page = self.route_to_page(&event);
        self.update_pointer_target(route_to_page, &event);

        // egui always sees pointer movement so hover states and tooltips stay correct.
        let send_to_gui = !route_to_page || matches!(event, WindowEvent::CursorMoved { .. } | WindowEvent::CursorLeft { .. });
        if send_to_gui {
            let window = &self.window;
            let response = self.gui.as_mut().map(|gui| gui.on_window_event(window, &event));
            if response.is_some_and(|response| response.repaint) {
                self.request_redraw();
            }
        }
        if route_to_page {
            if matches!(event, WindowEvent::MouseInput { state: ElementState::Pressed, .. }) {
                self.gui_mut().surrender_focus();
            }
            self.forward_to_page(event);
        }
    }

    /// The window has no system frame, so the OS offers no border to grab: resize from
    /// a thin band along the window edges ourselves. Returns whether the event was used.
    fn handle_resize_border(&mut self, event: &WindowEvent) -> bool {
        let edge = if self.input.page_capture { None } else { self.resize_edge_at_cursor() };
        match event {
            WindowEvent::CursorMoved { .. } => {
                if edge != self.input.resize_edge {
                    self.input.resize_edge = edge;
                    let cursor = match edge {
                        Some(direction) => resize_cursor(direction),
                        None if self.input.pointer_over_content => {
                            winit_cursor(self.active_view().map_or(Cursor::Default, |view| view.cursor))
                        },
                        None => CursorIcon::Default,
                    };
                    self.window.set_cursor(cursor);
                }
                edge.is_some()
            },
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(direction) = edge else {
                    return false;
                };
                if let Err(error) = self.window.drag_resize_window(direction) {
                    tracing::debug!(%error, "could not start resizing the window");
                }
                let window = &self.window;
                if let Some(gui) = self.gui.as_mut() {
                    gui.release_pointer(window);
                }
                true
            },
            _ => false,
        }
    }

    fn resize_edge_at_cursor(&self) -> Option<ResizeDirection> {
        if self.window_fullscreen || self.window.is_maximized() || !self.window.is_resizable() {
            return None;
        }
        let position = self.input.cursor?;
        let size = self.window.inner_size();
        let border = (5.0 * self.window.scale_factor()).max(4.0);
        let left = position.x < border;
        let right = position.x >= f64::from(size.width) - border;
        let top = position.y < border;
        let bottom = position.y >= f64::from(size.height) - border;
        Some(match (left, right, top, bottom) {
            (true, _, true, _) => ResizeDirection::NorthWest,
            (_, true, true, _) => ResizeDirection::NorthEast,
            (true, _, _, true) => ResizeDirection::SouthWest,
            (_, true, _, true) => ResizeDirection::SouthEast,
            (true, ..) => ResizeDirection::West,
            (_, true, ..) => ResizeDirection::East,
            (_, _, true, _) => ResizeDirection::North,
            (.., true) => ResizeDirection::South,
            _ => return None,
        })
    }

    fn route_to_page(&self, event: &WindowEvent) -> bool {
        let Some(gui) = &self.gui else {
            return false;
        };
        if gui.modal_open() || !self.active_view().is_some_and(|view| view.painted || view.webview.url().is_some()) {
            return false;
        }
        match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => !gui.wants_keyboard(),
            WindowEvent::ModifiersChanged(_) => true,
            WindowEvent::CursorMoved { .. } |
            WindowEvent::MouseInput { .. } |
            WindowEvent::MouseWheel { .. } |
            WindowEvent::CursorLeft { .. } => {
                if self.input.page_capture {
                    return true;
                }
                self.input.cursor.is_some_and(|position| {
                    let scale = self.scale().get();
                    let point = egui::pos2(position.x as f32 / scale, position.y as f32 / scale);
                    gui.is_page_point(point)
                })
            },
            WindowEvent::DroppedFile(_) => true,
            _ => false,
        }
    }

    fn update_pointer_target(&mut self, over_page: bool, event: &WindowEvent) {
        match event {
            WindowEvent::MouseInput { state, .. } => {
                self.input.page_capture = over_page && *state == ElementState::Pressed;
            },
            WindowEvent::CursorMoved { .. } => {
                if over_page != self.input.pointer_over_content {
                    self.input.pointer_over_content = over_page;
                    if over_page {
                        let cursor = self.active_view().map_or(Cursor::Default, |view| view.cursor);
                        self.window.set_cursor(winit_cursor(cursor));
                    } else if let Some(view) = self.active_view() {
                        view.webview.notify_input_event(InputEvent::MouseLeftViewport(MouseLeftViewportEvent::default()));
                    }
                }
            },
            WindowEvent::CursorLeft { .. } => self.input.pointer_over_content = false,
            _ => {},
        }
    }

    /// Cursor position relative to the content area, in device pixels.
    fn page_point(&self) -> Option<Point2D<f32, DevicePixel>> {
        let position = self.input.cursor?;
        let origin = self.gui.as_ref()?.content_origin();
        let scale = self.scale().get();
        Some(Point2D::new(position.x as f32 - origin.x * scale, position.y as f32 - origin.y * scale))
    }

    fn forward_to_page(&mut self, event: WindowEvent) {
        let Some(webview) = self.active_view().map(|view| view.webview.clone()) else {
            return;
        };
        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let keyboard_event = keyboard_event_from_winit(&event, self.input.modifiers);
                webview.notify_input_event(InputEvent::Keyboard(keyboard_event));
            },
            WindowEvent::CursorMoved { .. } => {
                if let Some(point) = self.page_point() {
                    webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(DevicePoint::from(point).into())));
                }
            },
            WindowEvent::CursorLeft { .. } => {
                webview.notify_input_event(InputEvent::MouseLeftViewport(MouseLeftViewportEvent::default()));
            },
            WindowEvent::MouseInput { state, button, .. } => {
                match (button, state) {
                    (MouseButton::Back, ElementState::Pressed) => return self.execute(UiCommand::Back),
                    (MouseButton::Forward, ElementState::Pressed) => return self.execute(UiCommand::Forward),
                    (MouseButton::Back | MouseButton::Forward, ElementState::Released) => return,
                    _ => {},
                }
                let Some(point) = self.page_point() else {
                    return;
                };
                let button = match button {
                    MouseButton::Left => ServoMouseButton::Primary,
                    MouseButton::Right => ServoMouseButton::Secondary,
                    MouseButton::Middle => ServoMouseButton::Auxiliary,
                    MouseButton::Back => ServoMouseButton::Back,
                    MouseButton::Forward => ServoMouseButton::Forward,
                    MouseButton::Other(value) => ServoMouseButton::Other(value),
                };
                let action = match state {
                    ElementState::Pressed => MouseButtonAction::Down,
                    ElementState::Released => MouseButtonAction::Up,
                };
                webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                    action,
                    button,
                    DevicePoint::from(point).into(),
                )));
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (f64::from(x * LINE_WIDTH), f64::from(y * LINE_HEIGHT)),
                    MouseScrollDelta::PixelDelta(delta) => (delta.x, delta.y),
                };
                let Some(point) = self.page_point() else {
                    return;
                };
                webview.notify_input_event(InputEvent::Wheel(WheelEvent::new(
                    WheelDelta {
                        x,
                        y,
                        z: 0.0,
                        mode: WheelMode::DeltaPixel,
                    },
                    DevicePoint::from(point).into(),
                )));
            },
            WindowEvent::Ime(ime) => {
                let composition = |state, data| InputEvent::Ime(ImeEvent::Composition(CompositionEvent { state, data }));
                match ime {
                    Ime::Enabled => {
                        webview.notify_input_event(composition(CompositionState::Start, String::new()));
                    },
                    Ime::Preedit(text, _) => {
                        webview.notify_input_event(composition(CompositionState::Update, text));
                    },
                    Ime::Commit(text) => {
                        webview.notify_input_event(composition(CompositionState::Update, text.clone()));
                        webview.notify_input_event(composition(CompositionState::End, text));
                    },
                    Ime::Disabled => {
                        // Only report dismissals the user made; when Servo itself hid the
                        // IME, `ime_control` was already cleared.
                        if self.input.ime_control.take().is_some() {
                            webview.notify_input_event(InputEvent::Ime(ImeEvent::Dismissed));
                        }
                    },
                }
            },
            WindowEvent::DroppedFile(path) => match Url::from_file_path(&path) {
                Ok(url) => self.execute(UiCommand::OpenUrl {
                    url,
                    disposition: Disposition::CurrentTab,
                }),
                Err(()) => tracing::warn!(path = %path.display(), "cannot open dropped file"),
            },
            _ => {},
        }
    }

    /// Keyboard shortcuts handled by the browser before the page sees the key.
    fn match_shortcut(&self, event: &keyboard_types::KeyboardEvent) -> Option<UiCommand> {
        let tab_command = |offset: isize| UiCommand::CycleTab(offset);
        let active: Option<TabId> = self.tabs.active_id();
        let close_active = move || active.map(UiCommand::CloseTab);
        ShortcutMatcher::from_event(event.clone())
            .shortcut(CMD_OR_CONTROL, 'T', || Some(UiCommand::NewTab))
            .shortcut(CMD_OR_CONTROL | Modifiers::SHIFT, 'T', || Some(UiCommand::ReopenClosedTab))
            .shortcut(CMD_OR_CONTROL, 'W', close_active)
            .shortcut(CMD_OR_CONTROL, Key::Named(NamedKey::F4), close_active)
            .shortcut(Modifiers::CONTROL, Key::Named(NamedKey::Tab), || Some(tab_command(1)))
            .shortcut(Modifiers::CONTROL | Modifiers::SHIFT, Key::Named(NamedKey::Tab), || Some(tab_command(-1)))
            .shortcut(Modifiers::CONTROL, Key::Named(NamedKey::PageDown), || Some(tab_command(1)))
            .shortcut(Modifiers::CONTROL, Key::Named(NamedKey::PageUp), || Some(tab_command(-1)))
            .shortcut(CMD_OR_CONTROL, '1', || Some(UiCommand::ActivateIndex(0)))
            .shortcut(CMD_OR_CONTROL, '2', || Some(UiCommand::ActivateIndex(1)))
            .shortcut(CMD_OR_CONTROL, '3', || Some(UiCommand::ActivateIndex(2)))
            .shortcut(CMD_OR_CONTROL, '4', || Some(UiCommand::ActivateIndex(3)))
            .shortcut(CMD_OR_CONTROL, '5', || Some(UiCommand::ActivateIndex(4)))
            .shortcut(CMD_OR_CONTROL, '6', || Some(UiCommand::ActivateIndex(5)))
            .shortcut(CMD_OR_CONTROL, '7', || Some(UiCommand::ActivateIndex(6)))
            .shortcut(CMD_OR_CONTROL, '8', || Some(UiCommand::ActivateIndex(7)))
            .shortcut(CMD_OR_CONTROL, '9', || Some(UiCommand::ActivateIndex(usize::MAX)))
            .shortcut(CMD_OR_CONTROL, 'L', || Some(UiCommand::FocusLocation))
            .shortcut(Modifiers::ALT, 'D', || Some(UiCommand::FocusLocation))
            .shortcut(Modifiers::empty(), Key::Named(NamedKey::F6), || Some(UiCommand::FocusLocation))
            .shortcut(CMD_OR_CONTROL, 'R', || Some(UiCommand::Reload))
            .shortcut(Modifiers::empty(), Key::Named(NamedKey::F5), || Some(UiCommand::Reload))
            .shortcut(Modifiers::ALT, Key::Named(NamedKey::ArrowLeft), || Some(UiCommand::Back))
            .shortcut(Modifiers::ALT, Key::Named(NamedKey::ArrowRight), || Some(UiCommand::Forward))
            .shortcut(Modifiers::ALT, Key::Named(NamedKey::Home), || Some(UiCommand::Home))
            .shortcut(CMD_OR_CONTROL, 'D', || Some(UiCommand::ToggleBookmark))
            .shortcut(CMD_OR_CONTROL, 'H', || Some(UiCommand::TogglePanel(Panel::History)))
            .shortcut(CMD_OR_CONTROL, 'J', || Some(UiCommand::TogglePanel(Panel::Downloads)))
            .shortcut(CMD_OR_CONTROL | Modifiers::SHIFT, 'O', || Some(UiCommand::TogglePanel(Panel::Bookmarks)))
            .shortcut(CMD_OR_CONTROL | Modifiers::SHIFT, 'B', || Some(UiCommand::ToggleBookmarksBar))
            .shortcut(Modifiers::SHIFT, Key::Named(NamedKey::Escape), || {
                Some(UiCommand::TogglePanel(Panel::Performance))
            })
            .shortcut(CMD_OR_CONTROL, ',', || Some(UiCommand::TogglePanel(Panel::Settings)))
            .shortcut(CMD_OR_CONTROL, 'S', || Some(UiCommand::SavePage))
            .shortcut(CMD_OR_CONTROL, '=', || Some(UiCommand::Zoom(ZoomChange::In)))
            .shortcut(CMD_OR_CONTROL, '+', || Some(UiCommand::Zoom(ZoomChange::In)))
            .shortcut(CMD_OR_CONTROL | Modifiers::SHIFT, '+', || Some(UiCommand::Zoom(ZoomChange::In)))
            .shortcut(CMD_OR_CONTROL, '-', || Some(UiCommand::Zoom(ZoomChange::Out)))
            .shortcut(CMD_OR_CONTROL, '0', || Some(UiCommand::Zoom(ZoomChange::Reset)))
            .shortcut(Modifiers::empty(), Key::Named(NamedKey::F11), || Some(UiCommand::ToggleFullscreen))
            .optional_shortcut(
                self.content_fullscreen || self.window_fullscreen,
                Modifiers::empty(),
                Key::Named(NamedKey::Escape),
                || Some(UiCommand::ExitFullscreen),
            )
            .shortcut(CMD_OR_CONTROL | Modifiers::SHIFT, 'Q', || Some(UiCommand::Quit))
            .otherwise(|| None)
            .flatten()
    }
}

fn resize_cursor(direction: ResizeDirection) -> CursorIcon {
    match direction {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
    }
}

pub fn winit_cursor(cursor: Cursor) -> CursorIcon {
    match cursor {
        Cursor::None | Cursor::Default => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
    }
}
