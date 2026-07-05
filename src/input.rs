use std::time::Instant;

use godot::classes::{InputEventKey, InputEventMouseButton, InputEventWithModifiers};
use godot::global::Key as GKey;
use godot::obj::Gd;
use libghostty_vt::key::{self, Key};
use libghostty_vt::selection::gesture::{DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent};
use libghostty_vt::terminal::{Point, PointCoordinate};
use libghostty_vt::{Terminal, mouse};

/// Pixel geometry for mapping surface positions to cells.
#[derive(Clone, Copy)]
pub struct MouseGeometry {
    pub screen_width: u32,
    pub screen_height: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    pub padding: u32,
}

impl MouseGeometry {
    fn encoder_size(self) -> mouse::EncoderSize {
        mouse::EncoderSize {
            screen_width: self.screen_width,
            screen_height: self.screen_height,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
            padding_top: self.padding,
            padding_bottom: self.padding,
            padding_left: self.padding,
            padding_right: self.padding,
        }
    }

    fn gesture_geometry(self, term: &Terminal<'static, 'static>) -> Geometry {
        Geometry {
            columns: u32::from(term.cols().unwrap_or(1)),
            cell_width: self.cell_width.max(1),
            padding_left: self.padding,
            screen_height: self.screen_height.max(1),
        }
    }

    fn viewport_point(self, term: &Terminal<'static, 'static>, x: f32, y: f32) -> Point {
        let pad = self.padding as f32;
        let cols = term.cols().unwrap_or(1);
        let rows = term.rows().unwrap_or(1);
        let cx = if x > pad {
            ((x - pad) / self.cell_width.max(1) as f32)
                .clamp(0.0, f32::from(cols.saturating_sub(1))) as u16
        } else {
            0
        };
        let cy = if y > pad {
            ((y - pad) / self.cell_height.max(1) as f32)
                .clamp(0.0, f32::from(rows.saturating_sub(1))) as u32
        } else {
            0
        };
        Point::Viewport(PointCoordinate { x: cx, y: cy })
    }
}

pub struct Input {
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse_encoder: mouse::Encoder<'static>,
    mouse_event: mouse::Event<'static>,
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    release: ReleaseEvent<'static>,
    drag: DragEvent<'static>,
    epoch: Instant,
    buf: Vec<u8>,
}

impl Input {
    pub fn new() -> libghostty_vt::error::Result<Self> {
        let mut press = PressEvent::new()?;
        press.set_repeat_interval(std::time::Duration::from_millis(500))?;
        Ok(Self {
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse_encoder: mouse::Encoder::new()?,
            mouse_event: mouse::Event::new()?,
            gesture: Gesture::new()?,
            press,
            release: ReleaseEvent::new()?,
            drag: DragEvent::new()?,
            epoch: Instant::now(),
            buf: Vec::with_capacity(64),
        })
    }

    /// Key event to pty bytes; encoder options sync from terminal modes.
    pub fn encode_key(
        &mut self,
        term: &Terminal<'static, 'static>,
        ev: &Gd<InputEventKey>,
    ) -> &[u8] {
        self.buf.clear();

        let action = if ev.is_echo() {
            key::Action::Repeat
        } else if ev.is_pressed() {
            key::Action::Press
        } else {
            key::Action::Release
        };

        let mods = event_mods(ev.clone().upcast());
        let (vt_key, unshifted) = map_key(ev.get_keycode()).unwrap_or((Key::Unidentified, '\0'));

        // Control combos carry no text; the encoder derives them itself.
        // Windows reports AltGr as Ctrl+Alt, and those combos do carry text.
        let unicode = ev.get_unicode();
        let altgr =
            cfg!(windows) && mods.contains(key::Mods::CTRL) && mods.contains(key::Mods::ALT);
        let text = match char::from_u32(unicode) {
            Some(ch)
                if action != key::Action::Release
                    && unicode >= 0x20
                    && unicode != 0x7f
                    && (!mods.contains(key::Mods::CTRL) || altgr)
                    && !mods.contains(key::Mods::SUPER) =>
            {
                Some(ch)
            }
            _ => None,
        };

        // Modifiers that already shaped the produced text are consumed.
        let consumed = match text {
            Some(_) if altgr => mods & (key::Mods::SHIFT | key::Mods::CTRL | key::Mods::ALT),
            Some(_) => mods & key::Mods::SHIFT,
            None => key::Mods::empty(),
        };

        let mut utf8 = [0u8; 4];
        self.key_event
            .set_action(action)
            .set_key(vt_key)
            .set_mods(mods)
            .set_consumed_mods(consumed)
            .set_unshifted_codepoint(unshifted)
            .set_utf8(text.map(|ch| &*ch.encode_utf8(&mut utf8)));

        let _ = self
            .key_encoder
            .set_options_from_terminal(term)
            .encode_to_vec(&self.key_event, &mut self.buf);
        &self.buf
    }

    pub fn encode_button(
        &mut self,
        term: &Terminal<'static, 'static>,
        button: mouse::Button,
        pressed: bool,
        mods: key::Mods,
        pos: (f32, f32),
        geo: MouseGeometry,
    ) -> &[u8] {
        self.buf.clear();
        self.mouse_event
            .set_mods(mods)
            .set_position(mouse::Position { x: pos.0, y: pos.1 })
            .set_button(Some(button))
            .set_action(if pressed {
                mouse::Action::Press
            } else {
                mouse::Action::Release
            });
        let _ = self
            .mouse_encoder
            .set_options_from_terminal(term)
            .set_size(geo.encoder_size())
            .encode_to_vec(&self.mouse_event, &mut self.buf);
        &self.buf
    }

    pub fn encode_motion(
        &mut self,
        term: &Terminal<'static, 'static>,
        held: Option<mouse::Button>,
        mods: key::Mods,
        pos: (f32, f32),
        geo: MouseGeometry,
    ) -> &[u8] {
        self.buf.clear();
        self.mouse_event
            .set_mods(mods)
            .set_position(mouse::Position { x: pos.0, y: pos.1 })
            .set_button(held)
            .set_action(mouse::Action::Motion);
        let _ = self
            .mouse_encoder
            .set_options_from_terminal(term)
            .set_size(geo.encoder_size())
            .set_any_button_pressed(held.is_some())
            .set_track_last_cell(true)
            .encode_to_vec(&self.mouse_event, &mut self.buf);
        &self.buf
    }

    /// Wheel tick as mouse button 4/5 press+release.
    pub fn encode_scroll(
        &mut self,
        term: &Terminal<'static, 'static>,
        ev: &Gd<InputEventMouseButton>,
        up: bool,
        geo: MouseGeometry,
    ) -> &[u8] {
        self.buf.clear();

        let pos = ev.get_position();
        let button = if up {
            mouse::Button::Four
        } else {
            mouse::Button::Five
        };

        self.mouse_event
            .set_mods(event_mods(ev.clone().upcast()))
            .set_position(mouse::Position { x: pos.x, y: pos.y })
            .set_button(Some(button));
        self.mouse_encoder
            .set_options_from_terminal(term)
            .set_size(geo.encoder_size());

        for action in [mouse::Action::Press, mouse::Action::Release] {
            self.mouse_event.set_action(action);
            let _ = self
                .mouse_encoder
                .encode_to_vec(&self.mouse_event, &mut self.buf);
        }
        &self.buf
    }

    /// Left press; repeated clicks widen to word/line via gesture state.
    pub fn selection_press(
        &mut self,
        term: &Terminal<'static, 'static>,
        x: f32,
        y: f32,
        geo: MouseGeometry,
    ) {
        let point = geo.viewport_point(term, x, y);
        let Ok(grid_ref) = term.grid_ref(point) else {
            return;
        };
        let selection = self
            .press
            .set_repeat_distance(f64::from(geo.cell_width))
            .and_then(|e| e.set_time(self.epoch.elapsed()))
            .and_then(|e| e.set_position(f64::from(x), f64::from(y)))
            .and_then(|e| e.apply(&mut self.gesture, term, grid_ref));
        if let Ok(selection) = selection {
            let _ = term.set_selection(selection.as_ref());
        }
    }

    pub fn selection_drag(
        &mut self,
        term: &Terminal<'static, 'static>,
        x: f32,
        y: f32,
        rectangle: bool,
        geo: MouseGeometry,
    ) {
        let point = geo.viewport_point(term, x, y);
        let Ok(grid_ref) = term.grid_ref(point) else {
            return;
        };
        let selection = self
            .drag
            .set_rectangle(rectangle)
            .and_then(|e| e.set_position(f64::from(x), f64::from(y)))
            .and_then(|e| {
                e.apply(
                    &mut self.gesture,
                    term,
                    grid_ref,
                    geo.gesture_geometry(term),
                )
            });
        if let Ok(selection) = selection {
            let _ = term.set_selection(selection.as_ref());
        }
    }

    pub fn selection_release(
        &mut self,
        term: &Terminal<'static, 'static>,
        x: f32,
        y: f32,
        geo: MouseGeometry,
    ) {
        let point = geo.viewport_point(term, x, y);
        let grid_ref = term.grid_ref(point).ok();
        let _ = self.release.apply(&mut self.gesture, term, grid_ref);
    }
}

pub fn event_mods(ev: Gd<InputEventWithModifiers>) -> key::Mods {
    let mut mods = key::Mods::empty();
    if ev.is_shift_pressed() {
        mods |= key::Mods::SHIFT;
    }
    if ev.is_ctrl_pressed() {
        mods |= key::Mods::CTRL;
    }
    if ev.is_alt_pressed() {
        mods |= key::Mods::ALT;
    }
    if ev.is_meta_pressed() {
        mods |= key::Mods::SUPER;
    }
    mods
}

/// Godot keycode to libghostty key plus its unshifted codepoint.
fn map_key(key: GKey) -> Option<(Key, char)> {
    Some(match key {
        GKey::A => (Key::A, 'a'),
        GKey::B => (Key::B, 'b'),
        GKey::C => (Key::C, 'c'),
        GKey::D => (Key::D, 'd'),
        GKey::E => (Key::E, 'e'),
        GKey::F => (Key::F, 'f'),
        GKey::G => (Key::G, 'g'),
        GKey::H => (Key::H, 'h'),
        GKey::I => (Key::I, 'i'),
        GKey::J => (Key::J, 'j'),
        GKey::K => (Key::K, 'k'),
        GKey::L => (Key::L, 'l'),
        GKey::M => (Key::M, 'm'),
        GKey::N => (Key::N, 'n'),
        GKey::O => (Key::O, 'o'),
        GKey::P => (Key::P, 'p'),
        GKey::Q => (Key::Q, 'q'),
        GKey::R => (Key::R, 'r'),
        GKey::S => (Key::S, 's'),
        GKey::T => (Key::T, 't'),
        GKey::U => (Key::U, 'u'),
        GKey::V => (Key::V, 'v'),
        GKey::W => (Key::W, 'w'),
        GKey::X => (Key::X, 'x'),
        GKey::Y => (Key::Y, 'y'),
        GKey::Z => (Key::Z, 'z'),
        GKey::KEY_0 => (Key::Digit0, '0'),
        GKey::KEY_1 => (Key::Digit1, '1'),
        GKey::KEY_2 => (Key::Digit2, '2'),
        GKey::KEY_3 => (Key::Digit3, '3'),
        GKey::KEY_4 => (Key::Digit4, '4'),
        GKey::KEY_5 => (Key::Digit5, '5'),
        GKey::KEY_6 => (Key::Digit6, '6'),
        GKey::KEY_7 => (Key::Digit7, '7'),
        GKey::KEY_8 => (Key::Digit8, '8'),
        GKey::KEY_9 => (Key::Digit9, '9'),
        GKey::SPACE => (Key::Space, ' '),
        GKey::ENTER => (Key::Enter, '\0'),
        GKey::KP_ENTER => (Key::NumpadEnter, '\0'),
        GKey::KP_0 => (Key::Numpad0, '0'),
        GKey::KP_1 => (Key::Numpad1, '1'),
        GKey::KP_2 => (Key::Numpad2, '2'),
        GKey::KP_3 => (Key::Numpad3, '3'),
        GKey::KP_4 => (Key::Numpad4, '4'),
        GKey::KP_5 => (Key::Numpad5, '5'),
        GKey::KP_6 => (Key::Numpad6, '6'),
        GKey::KP_7 => (Key::Numpad7, '7'),
        GKey::KP_8 => (Key::Numpad8, '8'),
        GKey::KP_9 => (Key::Numpad9, '9'),
        GKey::KP_PERIOD => (Key::NumpadDecimal, '.'),
        GKey::KP_ADD => (Key::NumpadAdd, '+'),
        GKey::KP_SUBTRACT => (Key::NumpadSubtract, '-'),
        GKey::KP_MULTIPLY => (Key::NumpadMultiply, '*'),
        GKey::KP_DIVIDE => (Key::NumpadDivide, '/'),
        GKey::TAB => (Key::Tab, '\0'),
        GKey::BACKSPACE => (Key::Backspace, '\0'),
        GKey::ESCAPE => (Key::Escape, '\0'),
        GKey::UP => (Key::ArrowUp, '\0'),
        GKey::DOWN => (Key::ArrowDown, '\0'),
        GKey::LEFT => (Key::ArrowLeft, '\0'),
        GKey::RIGHT => (Key::ArrowRight, '\0'),
        GKey::HOME => (Key::Home, '\0'),
        GKey::END => (Key::End, '\0'),
        GKey::PAGEUP => (Key::PageUp, '\0'),
        GKey::PAGEDOWN => (Key::PageDown, '\0'),
        GKey::INSERT => (Key::Insert, '\0'),
        GKey::DELETE => (Key::Delete, '\0'),
        GKey::MINUS => (Key::Minus, '-'),
        GKey::EQUAL => (Key::Equal, '='),
        GKey::BRACKETLEFT => (Key::BracketLeft, '['),
        GKey::BRACKETRIGHT => (Key::BracketRight, ']'),
        GKey::BACKSLASH => (Key::Backslash, '\\'),
        GKey::SEMICOLON => (Key::Semicolon, ';'),
        GKey::APOSTROPHE => (Key::Quote, '\''),
        GKey::COMMA => (Key::Comma, ','),
        GKey::PERIOD => (Key::Period, '.'),
        GKey::SLASH => (Key::Slash, '/'),
        GKey::QUOTELEFT => (Key::Backquote, '`'),
        GKey::F1 => (Key::F1, '\0'),
        GKey::F2 => (Key::F2, '\0'),
        GKey::F3 => (Key::F3, '\0'),
        GKey::F4 => (Key::F4, '\0'),
        GKey::F5 => (Key::F5, '\0'),
        GKey::F6 => (Key::F6, '\0'),
        GKey::F7 => (Key::F7, '\0'),
        GKey::F8 => (Key::F8, '\0'),
        GKey::F9 => (Key::F9, '\0'),
        GKey::F10 => (Key::F10, '\0'),
        GKey::F11 => (Key::F11, '\0'),
        GKey::F12 => (Key::F12, '\0'),
        _ => return None,
    })
}
