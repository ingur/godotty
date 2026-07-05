use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use godot::classes::control::FocusMode;
use godot::classes::display_server::Feature as DisplayFeature;
use godot::classes::notify::ControlNotification;
use godot::classes::object::ConnectFlags;
use godot::classes::{
    Control, DisplayServer, EditorInterface, Engine, IControl, InputEvent, InputEventKey,
    InputEventMouseButton, InputEventMouseMotion, Os, ProjectSettings, RenderingServer,
};
use godot::global::{Key as GKey, MouseButton, MouseButtonMask};
use godot::prelude::*;
use libghostty_vt::key::Mods as VtMods;
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::selection::FormatOptions;
use libghostty_vt::style::{RgbColor, Underline};
use libghostty_vt::terminal::{
    ColorScheme, ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode,
    PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes, SizeReportSize,
};
use libghostty_vt::{Error as VtError, Terminal as Vt, TerminalOptions, mouse, paste};

use crate::font::Fonts;
use crate::input::{Input, MouseGeometry, event_mods};
use crate::pty::{Drained, Pty, Writer};
use crate::theme::{self, Theme};

const PAD: f32 = 4.0;
const SCROLL_LINES: isize = 3;
const TITLE_POLL_SECS: f64 = 0.5;
/// Parse pty output every frame, but re-record the grid at most this often.
const REPAINT_MIN_SECS: f64 = 0.025;
/// Apply reflow and SIGWINCH once the size has stopped changing to avoid
/// spam-drawing the prompt and messing up the scrollback during resize.
const RESIZE_SETTLE_SECS: f64 = 0.1;
const MAX_SCROLLBACK: u32 = 100_000;

#[derive(GodotClass)]
#[class(tool, base = Control)]
pub struct Terminal {
    base: Base<Control>,
    #[export]
    font_size: i32,
    #[export]
    scrollback: u32,
    /// Command to run; empty means $SHELL.
    #[export]
    pub(crate) shell: GString,
    /// Where the shell starts; empty means project root, then $HOME.
    #[export]
    working_directory: GString,
    /// Spawn as a login shell so profiles rebuild the environment.
    #[export]
    login_shell: bool,
    /// Auto follows the editor theme (or the OS dark mode in games).
    #[export(enum = (Auto, Dark, Light))]
    color_scheme: i32,
    /// Set by the editor plugin; nodes in edited scenes stay inert.
    pub(crate) run_in_editor: bool,
    state: Option<State>,
}

#[godot_api]
impl Terminal {
    #[signal]
    fn title_changed(title: GString);
    #[signal]
    fn bell();
    #[signal]
    fn exited(code: i64);

    #[func]
    fn is_exited(&self) -> bool {
        self.state.as_ref().is_none_or(|st| st.exited)
    }

    #[func]
    fn on_editor_settings_changed(&mut self) {
        self.refresh_theme();
    }

    fn refresh_theme(&mut self) {
        if !self.run_in_editor {
            return;
        }
        let pref = self.scheme_pref();
        let Some(st) = self.state.as_mut() else {
            return;
        };
        let theme = theme::resolve(true, pref);
        if theme == st.theme {
            return;
        }
        if let Err(e) = st.apply_theme(theme) {
            godot_error!("[godotty] theme change failed: {e}");
        }
    }
}

#[godot_api]
impl IControl for Terminal {
    fn init(base: Base<Control>) -> Self {
        Self {
            base,
            font_size: 14,
            scrollback: 1000,
            shell: GString::new(),
            working_directory: GString::new(),
            login_shell: true,
            color_scheme: theme::SCHEME_AUTO,
            run_in_editor: false,
            state: None,
        }
    }

    fn ready(&mut self) {
        if Engine::singleton().is_editor_hint() && !self.run_in_editor {
            return;
        }
        self.base_mut().set_focus_mode(FocusMode::ALL);
        self.base_mut().set_clip_contents(true);
        let size = self.base().get_size();
        let parent_canvas = self.base().get_canvas_item();
        let scale = if Engine::singleton().is_editor_hint() {
            EditorInterface::singleton().get_editor_scale()
        } else {
            1.0
        };
        let spawn = Spawn {
            font_size: ((self.font_size as f32) * scale).round().max(1.0) as i32,
            scrollback: self.scrollback.min(MAX_SCROLLBACK),
            shell: self.shell.to_string(),
            cwd: resolve_cwd(&self.working_directory.to_string()),
            login: self.login_shell,
        };
        let theme = theme::resolve(self.run_in_editor, self.scheme_pref());
        match State::new(&spawn, theme, size, parent_canvas) {
            Ok(state) => self.state = Some(state),
            Err(e) => godot_error!("[godotty] failed to start terminal: {e}"),
        }
        if self.run_in_editor
            && let Some(mut settings) = EditorInterface::singleton().get_editor_settings()
        {
            settings.connect_flags(
                "settings_changed",
                &self.to_gd().callable("on_editor_settings_changed"),
                ConnectFlags::DEFERRED,
            );
        }
        self.base_mut().grab_focus();
        self.base_mut().queue_redraw();
    }

    fn process(&mut self, delta: f64) {
        let control_size = self.base().get_size();
        let visible = self.base().is_visible_in_tree();
        let Some(st) = self.state.as_mut() else {
            return;
        };
        st.writer.flush();

        let mut dirty = st.needs_paint;
        if control_size.x != st.geo.width || control_size.y != st.geo.height {
            if st.pending_size == Some(control_size) {
                st.settle += delta;
                if st.settle >= RESIZE_SETTLE_SECS {
                    st.pending_size = None;
                    if st.refit(control_size) {
                        dirty = true;
                    }
                }
            } else {
                st.pending_size = Some(control_size);
                st.settle = 0.0;
            }
        }
        // Drain to EOF, not to child exit; final output may still be in flight.
        if !st.eof {
            match st.pty.drain(|chunk| st.vt.vt_write(chunk)) {
                Drained::Data => dirty = true,
                Drained::Eof => st.eof = true,
                Drained::Empty => {}
            }
        }
        let mut exit = None;
        if !st.exited {
            exit = st.pty.exit_status().map(|status| {
                st.exited = true;
                dirty = true;
                status.exit_code()
            });
        }
        let bell = st.bell.take();
        let title = st.poll_title(delta);

        let mut bg_changed = false;
        st.since_repaint += delta;
        if dirty {
            // Hidden terminals keep draining but skip the re-record;
            // needs_paint stays sticky for the first visible frame.
            if visible && st.since_repaint >= REPAINT_MIN_SECS {
                st.since_repaint = 0.0;
                st.needs_paint = false;
                match st.repaint(control_size) {
                    Ok(changed) => bg_changed = changed,
                    Err(e) => {
                        st.needs_paint = true;
                        godot_error!("[godotty] repaint failed: {e}");
                    }
                }
            } else {
                st.needs_paint = true;
            }
        }

        // Deferred so handlers may call back into this Terminal.
        if let Some(code) = exit {
            self.base_mut().call_deferred(
                "emit_signal",
                &["exited".to_variant(), i64::from(code).to_variant()],
            );
        }
        if let Some(title) = title {
            self.base_mut().call_deferred(
                "emit_signal",
                &["title_changed".to_variant(), title.to_variant()],
            );
        }
        if bell {
            self.base_mut()
                .call_deferred("emit_signal", &["bell".to_variant()]);
        }
        if bg_changed {
            self.base_mut().queue_redraw();
        }
    }

    fn gui_input(&mut self, event: Gd<InputEvent>) {
        if self.state.is_none() {
            return;
        }

        if let Ok(key) = event.clone().try_cast::<InputEventKey>() {
            self.handle_key(&key);
        } else if let Ok(btn) = event.clone().try_cast::<InputEventMouseButton>() {
            self.handle_mouse_button(&btn);
        } else if let Ok(motion) = event.try_cast::<InputEventMouseMotion>() {
            self.handle_mouse_motion(&motion);
        }
    }

    /// Re-emit the title after reparenting (a dock move) or a theme change:
    /// both rebuild tabs, which restart from the node name. THEME_CHANGED
    /// also arrives after a new editor theme applies.
    fn on_notification(&mut self, what: ControlNotification) {
        if (what == ControlNotification::ENTER_TREE || what == ControlNotification::THEME_CHANGED)
            && let Some(st) = self.state.as_mut()
        {
            st.title.clear();
            st.title_timer = TITLE_POLL_SECS;
        }
        if what == ControlNotification::THEME_CHANGED {
            self.refresh_theme();
        }
    }

    /// Background only; the grid lives on a retained canvas item.
    fn draw(&mut self) {
        let size = self.base().get_size();
        let Some(bg) = self.state.as_ref().map(|st| st.bg) else {
            return;
        };
        self.base_mut()
            .draw_rect(Rect2::new(Vector2::ZERO, size), bg);
    }
}

impl Terminal {
    /// Node property in games; the editor setting for editor terminals.
    fn scheme_pref(&self) -> i32 {
        if !self.run_in_editor {
            return self.color_scheme;
        }
        EditorInterface::singleton()
            .get_editor_settings()
            .map(|s| s.get_setting(crate::plugin::COLOR_SCHEME_SETTING))
            .and_then(|v| v.try_to::<i64>().ok())
            .map(|v| v as i32)
            .unwrap_or(theme::SCHEME_AUTO)
    }

    fn handle_key(&mut self, key: &Gd<InputEventKey>) {
        // Left unconsumed so the editor's shortcut handling sees it.
        if is_editor_passthrough(key) {
            return;
        }

        if key.is_pressed() {
            if is_chord(key, GKey::C) {
                if let Some(st) = self.state.as_ref() {
                    st.copy_selection(false);
                }
                self.base_mut().accept_event();
                return;
            }
            if is_chord(key, GKey::V) {
                if let Some(st) = self.state.as_mut().filter(|st| !st.exited) {
                    st.paste_clipboard();
                    st.needs_paint = true;
                }
                self.base_mut().accept_event();
                return;
            }
        }

        if let Some(st) = self.state.as_mut().filter(|st| !st.exited) {
            let bytes = st.input.encode_key(&st.vt, key);
            if !bytes.is_empty() {
                st.writer.write(bytes);
                let _ = st.vt.set_selection(None);
                st.vt.scroll_viewport(ScrollViewport::Bottom);
                st.needs_paint = true;
            }
            self.base_mut().accept_event();
        }
    }

    fn handle_mouse_button(&mut self, btn: &Gd<InputEventMouseButton>) {
        let index = btn.get_button_index();
        let pressed = btn.is_pressed();
        if index == MouseButton::LEFT && pressed {
            self.base_mut().grab_focus();
        }
        let pos = btn.get_position();
        let mods = event_mods(btn.clone().upcast());

        let mut handled = false;
        if let Some(st) = self.state.as_mut() {
            let geo = st.geo.mouse();
            // Shift bypasses tracking so text stays selectable in apps.
            // Exited terminals keep selection and scrollback, nothing else.
            let tracking = !st.exited
                && st.vt.is_mouse_tracking().unwrap_or(false)
                && !mods.contains(VtMods::SHIFT);

            let wheel_up = index == MouseButton::WHEEL_UP;
            if wheel_up || index == MouseButton::WHEEL_DOWN {
                if pressed {
                    if tracking {
                        let bytes = st.input.encode_scroll(&st.vt, btn, wheel_up, geo);
                        st.writer.write(bytes);
                    } else {
                        let delta = if wheel_up {
                            -SCROLL_LINES
                        } else {
                            SCROLL_LINES
                        };
                        st.vt.scroll_viewport(ScrollViewport::Delta(delta));
                    }
                    handled = true;
                }
            } else if index == MouseButton::LEFT {
                // A selection in progress always completes on release, even
                // if Shift was let go first (which would re-enable tracking).
                if !pressed && st.selecting {
                    st.input.selection_release(&st.vt, pos.x, pos.y, geo);
                    st.selecting = false;
                    st.copy_selection(true);
                } else if tracking {
                    let bytes = st.input.encode_button(
                        &st.vt,
                        mouse::Button::Left,
                        pressed,
                        mods,
                        (pos.x, pos.y),
                        geo,
                    );
                    st.writer.write(bytes);
                } else if pressed {
                    st.input.selection_press(&st.vt, pos.x, pos.y, geo);
                    st.selecting = true;
                }
                handled = true;
            } else if index == MouseButton::MIDDLE && tracking {
                let bytes = st.input.encode_button(
                    &st.vt,
                    mouse::Button::Middle,
                    pressed,
                    mods,
                    (pos.x, pos.y),
                    geo,
                );
                st.writer.write(bytes);
                handled = true;
            } else if index == MouseButton::RIGHT && tracking {
                let bytes = st.input.encode_button(
                    &st.vt,
                    mouse::Button::Right,
                    pressed,
                    mods,
                    (pos.x, pos.y),
                    geo,
                );
                st.writer.write(bytes);
                handled = true;
            }
            if handled {
                st.needs_paint = true;
            }
        }
        if handled {
            self.base_mut().accept_event();
        }
    }

    fn handle_mouse_motion(&mut self, motion: &Gd<InputEventMouseMotion>) {
        let pos = motion.get_position();
        let mods = event_mods(motion.clone().upcast());
        let mask = motion.get_button_mask().ord();

        if let Some(st) = self.state.as_mut() {
            let geo = st.geo.mouse();
            if st.selecting {
                st.input
                    .selection_drag(&st.vt, pos.x, pos.y, mods.contains(VtMods::ALT), geo);
                st.needs_paint = true;
            } else if !st.exited && st.vt.is_mouse_tracking().unwrap_or(false) {
                let held = if mask & MouseButtonMask::LEFT.ord() != 0 {
                    Some(mouse::Button::Left)
                } else if mask & MouseButtonMask::RIGHT.ord() != 0 {
                    Some(mouse::Button::Right)
                } else if mask & MouseButtonMask::MIDDLE.ord() != 0 {
                    Some(mouse::Button::Middle)
                } else {
                    None
                };
                let bytes = st
                    .input
                    .encode_motion(&st.vt, held, mods, (pos.x, pos.y), geo);
                st.writer.write(bytes);
            }
        }
    }
}

/// Grid geometry derived from the font metrics and control size.
#[derive(Clone, Copy)]
struct Geometry {
    cell_w: f32,
    cell_h: f32,
    ascent: f32,
    cols: u16,
    rows: u16,
    width: f32,
    height: f32,
}

impl Geometry {
    fn fit(&mut self, size: Vector2) {
        self.width = size.x;
        self.height = size.y;
        self.cols = (((size.x - 2.0 * PAD) / self.cell_w).floor()).max(1.0) as u16;
        self.rows = (((size.y - 2.0 * PAD) / self.cell_h).floor()).max(1.0) as u16;
    }

    fn shared(&self) -> SharedGeo {
        SharedGeo {
            cols: self.cols,
            rows: self.rows,
            cell_w: self.cell_w as u32,
            cell_h: self.cell_h as u32,
        }
    }

    fn mouse(&self) -> MouseGeometry {
        MouseGeometry {
            screen_width: self.width as u32,
            screen_height: self.height as u32,
            cell_width: self.cell_w as u32,
            cell_height: self.cell_h as u32,
            padding: PAD as u32,
        }
    }
}

#[derive(Clone, Copy)]
struct SharedGeo {
    cols: u16,
    rows: u16,
    cell_w: u32,
    cell_h: u32,
}

/// Node configuration resolved at spawn time.
struct Spawn {
    font_size: i32,
    scrollback: u32,
    shell: String,
    cwd: PathBuf,
    login: bool,
}

fn resolve_cwd(configured: &str) -> PathBuf {
    let globalize =
        |p: &str| PathBuf::from(ProjectSettings::singleton().globalize_path(p).to_string());
    if !configured.is_empty() {
        let path = globalize(configured);
        if path.is_dir() {
            return path;
        }
        godot_warn!("[godotty] working_directory {configured} does not exist, using default");
    }
    let project = globalize("res://");
    if project.is_dir() {
        return project;
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

struct State {
    vt: Vt<'static, 'static>,
    pty: Pty,
    writer: Writer,
    input: Input,
    render_state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    fonts: Rc<Fonts>,
    font_size: i32,
    geo: Geometry,
    shared_geo: Rc<Cell<SharedGeo>>,
    bell: Rc<Cell<bool>>,
    theme: Theme,
    scheme: Rc<Cell<ColorScheme>>,
    exited: bool,
    eof: bool,
    selecting: bool,
    title: String,
    title_timer: f64,
    canvas: Rid,
    bg: Color,
    needs_paint: bool,
    since_repaint: f64,
    pending_size: Option<Vector2>,
    settle: f64,
}

impl Drop for State {
    fn drop(&mut self) {
        RenderingServer::singleton().free_rid(self.canvas);
    }
}

impl State {
    fn new(spawn: &Spawn, theme: Theme, size: Vector2, parent_canvas: Rid) -> Result<Self, String> {
        let fonts = Fonts::shared()?;
        let font_size = spawn.font_size;

        // Integral metrics; the mouse encoder and gestures take integer px.
        let primary = fonts.primary();
        let cell_w = primary.get_char_size('M' as u32, font_size).x.ceil();
        let cell_h = primary.get_height_ex().font_size(font_size).done().ceil();
        let ascent = primary.get_ascent_ex().font_size(font_size).done().round();
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return Err("font produced zero cell size".into());
        }

        let mut geo = Geometry {
            cell_w,
            cell_h,
            ascent,
            cols: 0,
            rows: 0,
            width: 0.0,
            height: 0.0,
        };
        geo.fit(size);

        let pty = Pty::spawn(crate::pty::Options {
            cols: geo.cols,
            rows: geo.rows,
            cell_w: cell_w as u16,
            cell_h: cell_h as u16,
            shell: (!spawn.shell.is_empty()).then_some(spawn.shell.as_str()),
            cwd: &spawn.cwd,
            login: spawn.login,
        })
        .map_err(|e| format!("pty: {e}"))?;
        let writer = pty.writer();

        let shared_geo = Rc::new(Cell::new(geo.shared()));
        let bell = Rc::new(Cell::new(false));

        let mut vt = Vt::new(TerminalOptions {
            cols: geo.cols,
            rows: geo.rows,
            max_scrollback: spawn.scrollback as usize,
        })
        .map_err(|e| format!("terminal: {e}"))?;
        vt.resize(geo.cols, geo.rows, cell_w as u32, cell_h as u32)
            .map_err(|e| format!("terminal resize: {e}"))?;

        vt.set_default_bg_color(Some(theme.bg))
            .and_then(|vt| vt.set_default_fg_color(Some(theme.fg)))
            .and_then(|vt| vt.set_default_cursor_color(Some(theme.cursor)))
            .and_then(|vt| vt.set_default_color_palette(Some(theme.palette)))
            .map_err(|e| format!("theme: {e}"))?;
        let scheme = Rc::new(Cell::new(theme.scheme));
        let sc = scheme.clone();
        vt.on_color_scheme(move |_t| Some(sc.get()))
            .map_err(|e| format!("effect: {e}"))?;

        let w = writer.clone();
        vt.on_pty_write(move |_t, data| w.write(data))
            .map_err(|e| format!("effect: {e}"))?;
        let sg = shared_geo.clone();
        vt.on_size(move |_t| {
            let g = sg.get();
            Some(SizeReportSize {
                rows: g.rows,
                columns: g.cols,
                cell_width: g.cell_w,
                cell_height: g.cell_h,
            })
        })
        .map_err(|e| format!("effect: {e}"))?;
        vt.on_device_attributes(|_t| {
            Some(DeviceAttributes {
                primary: PrimaryDeviceAttributes::new(
                    ConformanceLevel::VT220,
                    &[
                        DeviceAttributeFeature::COLUMNS_132,
                        DeviceAttributeFeature::SELECTIVE_ERASE,
                        DeviceAttributeFeature::ANSI_COLOR,
                    ],
                ),
                secondary: SecondaryDeviceAttributes {
                    device_type: DeviceType::VT220,
                    firmware_version: 1,
                    rom_cartridge: 0,
                },
                tertiary: Default::default(),
            })
        })
        .map_err(|e| format!("effect: {e}"))?;
        vt.on_xtversion(|_t| Some("godotty"))
            .map_err(|e| format!("effect: {e}"))?;
        let ev = bell.clone();
        vt.on_bell(move |_t| ev.set(true))
            .map_err(|e| format!("effect: {e}"))?;

        Ok(Self {
            vt,
            pty,
            writer,
            input: Input::new().map_err(|e| format!("input: {e}"))?,
            render_state: RenderState::new().map_err(|e| format!("render: {e}"))?,
            rows: RowIterator::new().map_err(|e| format!("render: {e}"))?,
            cells: CellIterator::new().map_err(|e| format!("render: {e}"))?,
            fonts,
            font_size,
            geo,
            shared_geo,
            bell,
            exited: false,
            eof: false,
            selecting: false,
            title: String::new(),
            title_timer: TITLE_POLL_SECS,
            canvas: {
                let mut rs = RenderingServer::singleton();
                let canvas = rs.canvas_item_create();
                rs.canvas_item_set_parent(canvas, parent_canvas);
                canvas
            },
            bg: color(theme.bg),
            theme,
            scheme,
            needs_paint: true,
            since_repaint: REPAINT_MIN_SECS,
            pending_size: None,
            settle: 0.0,
        })
    }

    /// Swap default colors in place; explicit cell colors stay untouched.
    fn apply_theme(&mut self, theme: Theme) -> Result<(), String> {
        self.vt
            .set_default_bg_color(Some(theme.bg))
            .and_then(|vt| vt.set_default_fg_color(Some(theme.fg)))
            .and_then(|vt| vt.set_default_cursor_color(Some(theme.cursor)))
            .and_then(|vt| vt.set_default_color_palette(Some(theme.palette)))
            .map_err(|e| format!("theme: {e}"))?;
        if theme.scheme != self.theme.scheme {
            self.scheme.set(theme.scheme);
            // Unsolicited report for apps that enabled mode 2031.
            if self.vt.mode(Mode::COLOR_SCHEME_REPORT).unwrap_or(false) {
                self.writer.write(match theme.scheme {
                    ColorScheme::Dark => b"\x1b[?997;1n",
                    ColorScheme::Light => b"\x1b[?997;2n",
                });
            }
        }
        self.theme = theme;
        self.needs_paint = true;
        Ok(())
    }

    /// Polled title: explicit OSC title wins, else foreground process name.
    fn poll_title(&mut self, delta: f64) -> Option<String> {
        self.title_timer += delta;
        if self.title_timer < TITLE_POLL_SECS {
            return None;
        }
        self.title_timer = 0.0;

        let title = match self.vt.title() {
            Ok(t) if !t.is_empty() => t.to_string(),
            _ => self.pty.foreground_process_name().unwrap_or_default(),
        };
        if title.is_empty() || title == self.title {
            return None;
        }
        self.title = title.clone();
        Some(title)
    }

    /// Copy the active selection to the system (or primary) clipboard.
    fn copy_selection(&self, primary: bool) {
        let Ok(Some(bytes)) = self
            .vt
            .format_selection_alloc(None, FormatOptions::new().with_trim(true))
        else {
            return;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let mut ds = DisplayServer::singleton();
        if primary {
            // Godot warns on every call where no primary clipboard exists.
            if ds.has_feature(DisplayFeature::CLIPBOARD_PRIMARY) {
                ds.clipboard_set_primary(text);
            }
        } else {
            ds.clipboard_set(text);
        }
    }

    /// Paste, encoded through libghostty (strips controls, bracketed paste).
    fn paste_clipboard(&mut self) {
        let text = DisplayServer::singleton().clipboard_get().to_string();
        if text.is_empty() {
            return;
        }

        let bracketed = self.vt.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
        let mut data = text.into_bytes();
        let mut buf = vec![0u8; data.len() + 16];
        let written = match paste::encode(&mut data, bracketed, &mut buf) {
            Ok(n) => n,
            Err(VtError::OutOfSpace { required }) => {
                buf.resize(required, 0);
                match paste::encode(&mut data, bracketed, &mut buf) {
                    Ok(n) => n,
                    Err(_) => return,
                }
            }
            Err(_) => return,
        };
        self.writer.write(&buf[..written]);
        let _ = self.vt.set_selection(None);
        self.vt.scroll_viewport(ScrollViewport::Bottom);
    }

    /// Reflow and SIGWINCH only when the cell grid actually changed.
    fn refit(&mut self, size: Vector2) -> bool {
        let (cols, rows) = (self.geo.cols, self.geo.rows);
        self.geo.fit(size);
        if self.geo.cols == cols && self.geo.rows == rows {
            return false;
        }
        if let Err(e) = self.vt.resize(
            self.geo.cols,
            self.geo.rows,
            self.geo.cell_w as u32,
            self.geo.cell_h as u32,
        ) {
            godot_error!("[godotty] terminal resize failed: {e}");
        }
        self.pty.resize(
            self.geo.cols,
            self.geo.rows,
            self.geo.cell_w as u16,
            self.geo.cell_h as u16,
        );
        self.shared_geo.set(self.geo.shared());
        true
    }

    /// Re-record the grid; returns whether the background color changed.
    fn repaint(&mut self, size: Vector2) -> libghostty_vt::error::Result<bool> {
        let canvas = self.canvas;
        let mut rs = RenderingServer::singleton();
        rs.canvas_item_clear(canvas);
        let snapshot = self.render_state.update(&self.vt)?;
        let colors = snapshot.colors()?;

        let bg = color(colors.background);
        let bg_changed = bg != self.bg;
        self.bg = bg;

        let cursor_vp = if !self.exited && snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?.map(|mut vp| {
                // A cursor on a wide char's spacer tail belongs to its head.
                vp.x = vp.x.saturating_sub(vp.at_wide_tail as u16);
                vp
            })
        } else {
            None
        };
        // The cursor cell's glyphs, redrawn over the cursor block.
        let mut cursor_cell = None;

        let mut chars = ['\0'; 8];
        let mut row_it = self.rows.update(&snapshot)?;
        let mut y = PAD;
        let mut row_i: u16 = 0;
        while let Some(row) = row_it.next() {
            let row_selected = row.selection()?.is_some();
            let mut cell_it = self.cells.update(row)?;
            let mut x = PAD;
            let mut col: u16 = 0;
            while let Some(cell) = cell_it.next() {
                let selected = row_selected && cell.is_selected()?;
                let n = cell.graphemes_len()?;
                let cell_rect = Rect2::new(
                    Vector2::new(x, y),
                    Vector2::new(self.geo.cell_w, self.geo.cell_h),
                );

                if n == 0 {
                    let bg = if selected {
                        Some(colors.foreground)
                    } else {
                        cell.bg_color()?
                    };
                    if let Some(bg) = bg {
                        rs.canvas_item_add_rect(canvas, cell_rect, color(bg));
                    }
                    x += self.geo.cell_w;
                    col += 1;
                    continue;
                }

                let mut fg = cell.fg_color()?.unwrap_or(colors.foreground);
                let mut bg = cell.bg_color()?;
                let mut bold = false;
                let mut italic = false;
                let mut underline = false;
                let mut faint = false;

                if cell.has_styling()? {
                    let style = cell.style()?;
                    bold = style.bold;
                    italic = style.italic;
                    faint = style.faint;
                    underline = !matches!(style.underline, Underline::None);
                    if style.inverse {
                        let old_fg = fg;
                        fg = bg.unwrap_or(colors.background);
                        bg = Some(old_fg);
                    }
                }
                if selected {
                    let old_fg = fg;
                    fg = bg.unwrap_or(colors.background);
                    bg = Some(old_fg);
                }

                if let Some(bg) = bg {
                    rs.canvas_item_add_rect(canvas, cell_rect, color(bg));
                }

                let mut fg = color(fg);
                if faint {
                    fg.a = 0.6;
                }

                let count = n.min(chars.len());
                cell.graphemes_buf(&mut chars[..count])?;
                let style = Fonts::style_index(bold, italic);
                let baseline = Vector2::new(x, y + self.geo.ascent);
                let wide = || matches!(cell.raw_cell().and_then(|c| c.wide()), Ok(CellWide::Wide));
                if cursor_vp
                    .as_ref()
                    .is_some_and(|vp| vp.x == col && vp.y == row_i)
                {
                    cursor_cell = Some((chars, count, style, baseline, wide()));
                }
                for ch in &chars[..count] {
                    let cp = *ch as u32;
                    if let Some(font) = self.fonts.resolve(cp, style, wide) {
                        font.draw_char_ex(canvas, baseline, cp, self.font_size)
                            .modulate(fg)
                            .done();
                    }
                }

                if underline {
                    let line = Rect2::new(
                        Vector2::new(x, y + self.geo.ascent + 1.0),
                        Vector2::new(self.geo.cell_w, 1.0),
                    );
                    rs.canvas_item_add_rect(canvas, line, fg);
                }

                x += self.geo.cell_w;
                col += 1;
            }
            y += self.geo.cell_h;
            row_i += 1;
        }

        if let Some(vp) = cursor_vp {
            let cursor = colors.cursor.unwrap_or(colors.foreground);
            let wide_cursor = matches!(&cursor_cell, Some((.., true)));
            let rect = Rect2::new(
                Vector2::new(
                    PAD + f32::from(vp.x) * self.geo.cell_w,
                    PAD + vp.y as f32 * self.geo.cell_h,
                ),
                Vector2::new(
                    self.geo.cell_w * if wide_cursor { 2.0 } else { 1.0 },
                    self.geo.cell_h,
                ),
            );
            rs.canvas_item_add_rect(canvas, rect, color(cursor));
            if let Some((chars, count, style, baseline, wide)) = cursor_cell {
                // Godot leaves color glyphs untinted, matching ghostty.
                let text = color(colors.background);
                for ch in &chars[..count] {
                    let cp = *ch as u32;
                    if let Some(font) = self.fonts.resolve(cp, style, || wide) {
                        font.draw_char_ex(canvas, baseline, cp, self.font_size)
                            .modulate(text)
                            .done();
                    }
                }
            }
        }

        if self.exited {
            rs.canvas_item_add_rect(
                canvas,
                Rect2::new(Vector2::ZERO, size),
                Color::from_rgba(0.0, 0.0, 0.0, 0.55),
            );
            let msg = "[process exited]";
            let primary = self.fonts.primary();
            let width = primary
                .get_string_size_ex(msg)
                .font_size(self.font_size)
                .done()
                .x;
            primary
                .draw_string_ex(
                    canvas,
                    Vector2::new((size.x - width) / 2.0, size.y / 2.0),
                    msg,
                )
                .font_size(self.font_size)
                .modulate(Color::from_rgba8(255, 255, 255, 255))
                .done();
        }

        Ok(bg_changed)
    }
}

fn color(c: RgbColor) -> Color {
    Color::from_rgba8(c.r, c.g, c.b, 255)
}

/// Copy/paste chord: Cmd on macOS, Ctrl+Shift elsewhere.
fn is_chord(key: &Gd<InputEventKey>, which: GKey) -> bool {
    if key.get_keycode() != which {
        return false;
    }
    if cfg!(target_os = "macos") {
        key.is_meta_pressed()
    } else {
        key.is_ctrl_pressed() && key.is_shift_pressed()
    }
}

/// Chords that belong to the editor even while a terminal is focused,
/// from the editor setting registered by the plugin.
fn is_editor_passthrough(key: &Gd<InputEventKey>) -> bool {
    if !(key.is_ctrl_pressed() || key.is_alt_pressed() || key.is_meta_pressed()) {
        return false;
    }
    if !Engine::singleton().is_editor_hint() {
        return false;
    }
    let Some(settings) = EditorInterface::singleton().get_editor_settings() else {
        return false;
    };
    // The toggle shortcut is always passed through, no separate listing.
    if crate::plugin::toggle_shortcut_matches(&key.clone().upcast::<InputEvent>()) {
        return true;
    }
    let chords = settings
        .get_setting(crate::plugin::PASSTHROUGH_SETTING)
        .try_to::<GString>()
        .unwrap_or_default();
    let combo = key.get_keycode_with_modifiers();
    let os = Os::singleton();
    chords
        .to_string()
        .split(',')
        .map(str::trim)
        .filter(|chord| !chord.is_empty())
        .any(|chord| os.find_keycode_from_string(chord) == combo)
}
