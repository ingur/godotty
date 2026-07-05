//! A terminal palette generated from a few Godot theme colors: ANSI hues
//! keep their identity, lightness is placed relative to the real background.

use godot::classes::{Control, DisplayServer, EditorInterface};
use godot::prelude::*;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::ColorScheme;

pub const SCHEME_AUTO: i32 = 0;
pub const SCHEME_DARK: i32 = 1;
pub const SCHEME_LIGHT: i32 = 2;

#[derive(Clone, PartialEq)]
pub struct Theme {
    pub scheme: ColorScheme,
    pub bg: RgbColor,
    pub fg: RgbColor,
    pub cursor: RgbColor,
    pub palette: [RgbColor; 256],
}

pub fn resolve(run_in_editor: bool, pref: i32) -> Theme {
    if run_in_editor
        && let Some(control) = EditorInterface::singleton().get_base_control()
        && let Some(theme) = editor_theme(&control, pref)
    {
        return theme;
    }
    let dark = match pref {
        SCHEME_DARK => true,
        SCHEME_LIGHT => false,
        _ => {
            let ds = DisplayServer::singleton();
            !ds.is_dark_mode_supported() || ds.is_dark_mode()
        }
    };
    let (bg, fg) = neutral_surface(dark);
    generate(bg, fg, None, HUES)
}

fn editor_theme(control: &Gd<Control>, pref: i32) -> Option<Theme> {
    let color = |name: &str| -> Option<RgbColor> {
        control
            .has_theme_color_ex(name)
            .theme_type("Editor")
            .done()
            .then(|| {
                let c = control.get_theme_color_ex(name).theme_type("Editor").done();
                RgbColor {
                    r: (c.r.clamp(0.0, 1.0) * 255.0).round() as u8,
                    g: (c.g.clamp(0.0, 1.0) * 255.0).round() as u8,
                    b: (c.b.clamp(0.0, 1.0) * 255.0).round() as u8,
                }
            })
    };
    let mut bg = darken(color("base_color")?, RECESS);
    let mut fg = color("font_color")?;
    let accent = color("accent_color")?;

    // A forced scheme opposite the editor can't blend; take a neutral surface.
    let editor_dark = oklab_of(bg).l < 0.5;
    match pref {
        SCHEME_DARK if !editor_dark => (bg, fg) = neutral_surface(true),
        SCHEME_LIGHT if editor_dark => (bg, fg) = neutral_surface(false),
        _ => {}
    }

    let mut hues = HUES;
    for (slot, name) in [
        (0, "error_color"),
        (1, "success_color"),
        (2, "warning_color"),
    ] {
        if let Some(c) = color(name) {
            let lab = oklab_of(c);
            if (lab.a * lab.a + lab.b * lab.b).sqrt() > 0.03 {
                hues[slot] = lab.b.atan2(lab.a).to_degrees();
            }
        }
    }
    Some(generate(bg, fg, Some(accent), hues))
}

/// OKLCH hue angles of the sRGB primaries/secondaries each ANSI slot
/// descends from: red, green, yellow, blue, magenta, cyan.
const HUES: [f32; 6] = [29.2, 142.5, 110.0, 264.1, 328.4, 194.8];
/// Per-family contrast and chroma targets: measured medians of popular
/// modern themes (Catppuccin, Tokyo Night, One Dark, Nord, Kanagawa...)
/// and their light variants. Order red, green, yellow, blue, magenta, cyan.
/// Slightly magic looking stuff, but it looks pretty good so we'll roll with it.
const DARK_CONTRAST: [f32; 6] = [4.7, 7.0, 8.5, 6.2, 5.6, 8.0];
const DARK_CHROMA: [f32; 6] = [0.16, 0.11, 0.10, 0.10, 0.10, 0.09];
const LIGHT_CONTRAST: [f32; 6] = [4.5, 3.5, 3.0, 4.0, 3.8, 3.3];
const LIGHT_CHROMA: [f32; 6] = [0.21, 0.16, 0.14, 0.17, 0.21, 0.10];
/// Readability floor: WCAG 3:1 for UI components. Trust the process
const FLOOR_CONTRAST: f32 = 3.0;
/// Brights sit half a distinguishable lightness step beyond normals.
const STEP: f32 = 0.05;
/// Recess below the surrounding editor surface.
const RECESS: f32 = 0.03;

fn neutral_surface(dark: bool) -> (RgbColor, RgbColor) {
    if dark {
        (rgb(0x16, 0x18, 0x1d), rgb(0xd8, 0xda, 0xde))
    } else {
        (rgb(0xfa, 0xfa, 0xf8), rgb(0x20, 0x22, 0x26))
    }
}

fn generate(bg: RgbColor, fg: RgbColor, accent: Option<RgbColor>, hues: [f32; 6]) -> Theme {
    let dark = oklab_of(bg).l < 0.5;

    let mut p = [RgbColor::default(); 256];
    // ANSI neutral convention.
    p[0] = gray(0.30);
    p[8] = gray(0.45);
    p[7] = gray(0.80);
    p[15] = gray(0.93);
    let (contrast, chroma) = if dark {
        (&DARK_CONTRAST, &DARK_CHROMA)
    } else {
        (&LIGHT_CONTRAST, &LIGHT_CHROMA)
    };
    for (i, h) in hues.iter().enumerate() {
        let (nl, normal) = slot(*h, bg, contrast[i], chroma[i], 0.05, 0.95);
        let (lo, hi) = if dark {
            ((nl + STEP).min(0.95), 0.95)
        } else {
            (0.05, (nl - STEP).max(0.05))
        };
        let (_, bright) = slot(*h, bg, contrast[i], chroma[i], lo, hi);
        p[1 + i] = normal;
        p[9 + i] = bright;
    }
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    for i in 0..216 {
        p[16 + i] = RgbColor {
            r: STEPS[i / 36],
            g: STEPS[(i / 6) % 6],
            b: STEPS[i % 6],
        };
    }
    for i in 0..24 {
        let v = (8 + 10 * i) as u8;
        p[232 + i] = RgbColor { r: v, g: v, b: v };
    }

    let cursor = accent
        .map(|a| {
            if wcag(a, bg) >= FLOOR_CONTRAST {
                return a;
            }
            let lab = oklab_of(a);
            if (lab.a * lab.a + lab.b * lab.b).sqrt() <= 0.03 {
                return fg;
            }
            // Pooled cluster medians; the cursor has no ANSI family.
            let (t, c) = if dark { (6.5, 0.11) } else { (3.4, 0.16) };
            let h = lab.b.atan2(lab.a).to_degrees();
            slot(h, bg, t, c, 0.05, 0.95).1
        })
        .unwrap_or(fg);
    Theme {
        scheme: if dark {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        bg,
        fg,
        cursor,
        palette: p,
    }
}

fn darken(c: RgbColor, dl: f32) -> RgbColor {
    let lab = oklab_of(c);
    let chroma = (lab.a * lab.a + lab.b * lab.b).sqrt();
    lch(
        (lab.l - dl).max(0.0),
        chroma,
        lab.b.atan2(lab.a).to_degrees(),
    )
}

/// WCAG relative luminance (ITU-R BT.709 weights).
fn luminance(c: RgbColor) -> f32 {
    0.2126 * srgb_to_lin(c.r as f32 / 255.0)
        + 0.7152 * srgb_to_lin(c.g as f32 / 255.0)
        + 0.0722 * srgb_to_lin(c.b as f32 / 255.0)
}

fn wcag(a: RgbColor, b: RgbColor) -> f32 {
    let (ya, yb) = (luminance(a) + 0.05, luminance(b) + 0.05);
    if ya > yb { ya / yb } else { yb / ya }
}

/// The in-band point whose contrast is nearest the family target at the
/// family chroma, subject to the readability floor; best-contrast fallback
/// where the gamut cannot clear the floor at all.
fn slot(
    h_deg: f32,
    bg: RgbColor,
    target: f32,
    chroma: f32,
    lmin: f32,
    lmax: f32,
) -> (f32, RgbColor) {
    let mut best: Option<(f32, f32, RgbColor)> = None;
    // Negative sentinel: the first in-band candidate always seeds the
    // fallback, so an empty result cannot degenerate to black.
    let mut fallback = (-1.0, 0.5, RgbColor::default());
    for i in 0..=72 {
        // f32 accumulation overshoots the last grid point past lmax.
        let l = (0.05 + i as f32 * 0.0125).min(0.95);
        if l < lmin || l > lmax {
            continue;
        }
        let (rgb, _) = lch_c(l, chroma, h_deg);
        let r = wcag(rgb, bg);
        if r >= FLOOR_CONTRAST {
            let d = (r - target).abs();
            if best.is_none_or(|(bd, _, _)| d < bd) {
                best = Some((d, l, rgb));
            }
        } else if r > fallback.0 {
            fallback = (r, l, rgb);
        }
    }
    best.map(|(_, l, rgb)| (l, rgb))
        .unwrap_or((fallback.1, fallback.2))
}

// OKLab (Björn Ottosson): perceptual lightness, uniform across hues.

#[derive(Clone, Copy)]
struct Lab {
    l: f32,
    a: f32,
    b: f32,
}

fn srgb_to_lin(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn lin_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

#[allow(clippy::excessive_precision)] // reference matrix, kept verbatim
fn oklab_of(c: RgbColor) -> Lab {
    let r = srgb_to_lin(c.r as f32 / 255.0);
    let g = srgb_to_lin(c.g as f32 / 255.0);
    let b = srgb_to_lin(c.b as f32 / 255.0);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    Lab {
        l: 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        a: 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        b: 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    }
}

/// `None` when out of the sRGB gamut.
#[allow(clippy::excessive_precision)]
fn lab_to_rgb(lab: Lab) -> Option<RgbColor> {
    let l = (lab.l + 0.3963377774 * lab.a + 0.2158037573 * lab.b).powi(3);
    let m = (lab.l - 0.1055613458 * lab.a - 0.0638541728 * lab.b).powi(3);
    let s = (lab.l - 0.0894841775 * lab.a - 1.2914855480 * lab.b).powi(3);
    let r = 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s;
    let g = -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s;
    let b = -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s;
    const EPS: f32 = 0.001;
    for c in [r, g, b] {
        if !(-EPS..=1.0 + EPS).contains(&c) {
            return None;
        }
    }
    let enc = |c: f32| (lin_to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u8;
    Some(RgbColor {
        r: enc(r),
        g: enc(g),
        b: enc(b),
    })
}

/// Nearest in-gamut color: chroma shrinks until it fits. Returns the
/// achieved chroma.
fn lch_c(l: f32, c: f32, h_deg: f32) -> (RgbColor, f32) {
    let h = h_deg.to_radians();
    let mut c = c;
    for _ in 0..12 {
        if let Some(rgb) = lab_to_rgb(Lab {
            l,
            a: c * h.cos(),
            b: c * h.sin(),
        }) {
            return (rgb, c);
        }
        c *= 0.8;
    }
    (
        lab_to_rgb(Lab { l, a: 0.0, b: 0.0 }).unwrap_or_default(),
        0.0,
    )
}

fn lch(l: f32, c: f32, h_deg: f32) -> RgbColor {
    lch_c(l, c, h_deg).0
}

fn gray(l: f32) -> RgbColor {
    lch(l, 0.0, 0.0)
}

fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}
