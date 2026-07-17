use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use godot::classes::{Font, FontFile, Os, SystemFont};
use godot::global::Error as GdError;
use godot::prelude::*;

// Font indices into Fonts::fonts. 0..4 are the JetBrains Mono styles.
// Here, I'm basically just trying to match what ghostty does.
const NOTO_MONO: i16 = 4;
const JULIA: i16 = 5;
const COLOR: i16 = 6;
const SYSTEM: i16 = 7;

/// Explicit per-glyph font resolution. Godot's draw_char is a blind
/// first-match-wins over one flat list with no presentation logic, so we
/// order the fallbacks ourselves per codepoint, keyed on the cell width
/// that libghostty already computed.
pub struct Fonts {
    fonts: Vec<Gd<Font>>,
    /// System fonts discovered per codepoint, appended behind `fonts`.
    extra: RefCell<Vec<Gd<Font>>>,
    tried_paths: RefCell<HashSet<String>>,
    primary_cache: RefCell<HashMap<(u32, u8), bool>>,
    fallback_cache: RefCell<HashMap<(u32, bool), i16>>,
}

impl Fonts {
    /// Shared across terminals (the embedded data is large), dropped with
    /// the last one. A Weak cache avoids leaking font RIDs at exit.
    pub fn shared() -> Result<Rc<Self>, String> {
        thread_local! {
            static FONTS: RefCell<std::rc::Weak<Fonts>> =
                const { RefCell::new(std::rc::Weak::new()) };
        }
        FONTS.with(|cell| {
            let mut weak = cell.borrow_mut();
            if let Some(fonts) = weak.upgrade() {
                return Ok(fonts);
            }
            let fonts = Rc::new(Self::load()?);
            *weak = Rc::downgrade(&fonts);
            Ok(fonts)
        })
    }

    /// Fonts are embedded so the extension works in any project without
    /// assets; system fonts fill scripts we cannot bundle (e.g. CJK).
    fn load() -> Result<Self, String> {
        let embed = |name: &str, bytes: &[u8]| -> Result<Gd<Font>, String> {
            let mut font = FontFile::new_gd();
            font.set_data(&PackedByteArray::from(bytes));
            if font.get_data().is_empty() {
                return Err(format!("font {name} failed to parse"));
            }
            Ok(font.upcast())
        };
        let system = |family: &str| -> Gd<Font> {
            let mut font = SystemFont::new_gd();
            font.set_font_names(&PackedStringArray::from([GString::from(family)]));
            font.upcast()
        };
        let fonts = vec![
            embed(
                "Regular",
                include_bytes!("../fonts/JetBrainsMonoNerdFont-Regular.ttf"),
            )?,
            embed(
                "Bold",
                include_bytes!("../fonts/JetBrainsMonoNerdFont-Bold.ttf"),
            )?,
            embed(
                "Italic",
                include_bytes!("../fonts/JetBrainsMonoNerdFont-Italic.ttf"),
            )?,
            embed(
                "BoldItalic",
                include_bytes!("../fonts/JetBrainsMonoNerdFont-BoldItalic.ttf"),
            )?,
            embed(
                "NotoEmoji",
                include_bytes!("../fonts/NotoEmoji-Regular.ttf"),
            )?,
            embed(
                "JuliaMono",
                include_bytes!("../fonts/JuliaMono-Regular.ttf"),
            )?,
            embed(
                "NotoColorEmoji",
                include_bytes!("../fonts/NotoColorEmoji.ttf"),
            )?,
            system("monospace"),
            system("sans-serif"),
        ];
        Ok(Self {
            fonts,
            extra: RefCell::new(Vec::new()),
            tried_paths: RefCell::new(HashSet::new()),
            primary_cache: RefCell::new(HashMap::new()),
            fallback_cache: RefCell::new(HashMap::new()),
        })
    }

    pub fn primary(&self) -> &Gd<Font> {
        &self.fonts[0]
    }

    pub fn style_font(&self, style: u8) -> &Gd<Font> {
        &self.fonts[style as usize]
    }

    pub fn style_index(bold: bool, italic: bool) -> u8 {
        match (bold, italic) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        }
    }

    /// The font to draw a codepoint with, or None if nothing covers it.
    /// `wide` is evaluated only when the primary font lacks the glyph.
    pub fn resolve(&self, cp: u32, style: u8, wide: impl FnOnce() -> bool) -> Option<Gd<Font>> {
        let primary = *self
            .primary_cache
            .borrow_mut()
            .entry((cp, style))
            .or_insert_with(|| self.fonts[style as usize].has_char(cp));
        if primary {
            return Some(self.fonts[style as usize].clone());
        }
        let wide = wide();
        let idx = *self
            .fallback_cache
            .borrow_mut()
            .entry((cp, wide))
            .or_insert_with(|| self.compute_fallback(cp, wide));
        match usize::try_from(idx) {
            Err(_) => None,
            Ok(i) if i < self.fonts.len() => Some(self.fonts[i].clone()),
            Ok(i) => Some(self.extra.borrow()[i - self.fonts.len()].clone()),
        }
    }

    /// Emoji cells prefer color; text cells prefer monochrome, taking Noto's
    /// mono emoji first (matching ghostty for media/clock symbols) then
    /// JuliaMono for the rest; system fonts cover uncovered scripts, with
    /// remaining scripts discovered from the OS per codepoint.
    fn compute_fallback(&self, cp: u32, wide: bool) -> i16 {
        let head: [i16; 3] = if wide {
            [COLOR, NOTO_MONO, JULIA]
        } else {
            [NOTO_MONO, JULIA, COLOR]
        };
        let order = head.into_iter().chain(SYSTEM..self.fonts.len() as i16);
        for i in order {
            if self.fonts[i as usize].has_char(cp) {
                return i;
            }
        }
        for (i, font) in self.extra.borrow().iter().enumerate() {
            if font.has_char(cp) {
                return (self.fonts.len() + i) as i16;
            }
        }
        self.discover(cp)
    }

    /// Ask the OS for fonts covering `cp` (draw_char never shapes, so
    /// Godot's own system fallback cannot kick in) and load the first
    /// suitable one. None found leaves the cell blank, like ghostty.
    fn discover(&self, cp: u32) -> i16 {
        let Some(ch) = char::from_u32(cp) else {
            return -1;
        };
        let paths =
            Os::singleton().get_system_font_path_for_text("monospace", ch.to_string().as_str());
        for path in paths.as_slice() {
            let key = path.to_string();
            if !self.tried_paths.borrow_mut().insert(key) {
                continue;
            }
            let mut font = FontFile::new_gd();
            if font.load_dynamic_font(path) != GdError::OK {
                continue;
            }
            // Keep every loaded font: the extra scan serves codepoints this
            // one covers even when the requested one is missing from it.
            let covers = font.has_char(cp);
            let mut extra = self.extra.borrow_mut();
            extra.push(font.upcast());
            if covers {
                return (self.fonts.len() + extra.len() - 1) as i16;
            }
        }
        -1
    }
}
