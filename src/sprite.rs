//! Glyphs drawn as cell-perfect geometry instead of font glyphs: fills,
//! box lines, braille, prompt separators. Fonts do not know the cell box
//! and leave seams; tables and shapes mirror ghostty's sprite font.
//! Drawing happens in pixels relative to the cell origin.

use godot::classes::RenderingServer;
use godot::prelude::*;
use libghostty_vt::style::Underline;

/// Draw `cp` procedurally into `cell`; false means it is not a sprite
/// codepoint and the caller renders it as a font glyph instead. `thick`
/// is the base stroke width (the font's underline thickness).
pub fn draw(canvas: Rid, cell: Rect2, fg: Color, thick: f32, cp: u32) -> bool {
    let (w, h) = (cell.size.x, cell.size.y);
    match cp {
        0x2500..=0x257f => draw_box(canvas, cell, fg, thick, cp),
        0x2580..=0x259f | 0x1fb00..=0x1fb3b => draw_block(canvas, cell, fg, cp),
        // ◢◣◤◥: solid corner triangles.
        0x25e2 => polygon(canvas, cell, &[(w, 0.0), (w, h), (0.0, h)], fg),
        0x25e3 => polygon(canvas, cell, &[(0.0, 0.0), (w, h), (0.0, h)], fg),
        0x25e4 => polygon(canvas, cell, &[(0.0, 0.0), (w, 0.0), (0.0, h)], fg),
        0x25e5 => polygon(canvas, cell, &[(0.0, 0.0), (w, 0.0), (w, h)], fg),
        0x2800..=0x28ff => draw_braille(canvas, cell, fg, cp),
        0xe0b0..=0xe0bf => draw_powerline(canvas, cell, fg, thick, cp),
        _ => return false,
    }
    true
}

/// Text underline in one of the SGR 4:x styles, cell-perfect so neighbors
/// tile. `y` is the top of the single underline in cell pixels. Shapes
/// mirror ghostty's sprite font, clamped inside the cell.
pub fn underline(canvas: Rid, cell: Rect2, color: Color, thick: f32, y: f32, style: Underline) {
    let (w, h) = (cell.size.x, cell.size.y);
    match style {
        Underline::None => {}
        Underline::Double => {
            // A line above and below the single position, gap in between.
            let yy = y.min(h - 2.0 * thick).max(thick);
            rect(canvas, cell, 0.0, yy - thick, w, yy, color);
            rect(canvas, cell, 0.0, yy + thick, w, yy + 2.0 * thick, color);
        }
        Underline::Dotted => {
            // Square dots sized like ghostty's, spaced to match their size.
            let d = (1.414 * thick).round().max(1.0);
            let count = (w / (2.0 * d))
                .ceil()
                .min((w / (1.5 * d)).floor())
                .min((w / (d + 1.0)).floor())
                .max(1.0);
            let yy = y.min(h - d);
            let slot = w / count;
            for i in 0..count as usize {
                let x = (slot * (i as f32 + 0.5) - d / 2.0).round();
                rect(canvas, cell, x, yy, x + d, yy + d, color);
            }
        }
        Underline::Dashed => {
            // Thirds: dash, gap, dash. The last dash clips at the cell edge.
            let dash_w = (w / 3.0).floor() + 1.0;
            rect(canvas, cell, 0.0, y, dash_w, y + thick, color);
            rect(canvas, cell, 2.0 * dash_w, y, w.min(3.0 * dash_w), y + thick, color);
        }
        Underline::Curly => {
            // One wave cycle per cell so neighbors connect, peak at the
            // center, curvature 0.4 like ghostty.
            let a = w / std::f32::consts::PI;
            let top = y.min(h - a - thick).max(0.0);
            let bottom = top + a;
            let (c, r) = (0.5 * w, 0.4);
            let mut points = Vec::with_capacity(17);
            let up = ((0.0, bottom), (c * r, bottom), (c - c * r, top), (c, top));
            let down = ((c, top), (c + c * r, top), (w - c * r, bottom), (w, bottom));
            for i in 0..=8 {
                points.push(cubic(up.0, up.1, up.2, up.3, i as f32 / 8.0));
            }
            for i in 1..=8 {
                points.push(cubic(down.0, down.1, down.2, down.3, i as f32 / 8.0));
            }
            stroke(canvas, cell, &points, color, thick);
        }
        // Single, and any future style, as the plain line.
        _ => rect(canvas, cell, 0.0, y, w, y + thick, color),
    }
}

/// Quadrant bits tl=1, tr=2, bl=4, br=8, indexed from U+2596 (▖▗▘▙▚▛▜▝▞▟).
const QUADS: [u8; 10] = [4, 8, 1, 13, 9, 7, 11, 2, 6, 14];

/// Solid mosaic fills: Block Elements (U+2580..=U+259F) and the sextants
/// of Symbols for Legacy Computing (U+1FB00..=U+1FB3B). All pure rects,
/// boundaries rounded to whole pixels so they tile exactly.
fn draw_block(canvas: Rid, cell: Rect2, fg: Color, cp: u32) {
    let mut alpha = 1.0;
    let mut parts = [[0.0_f32; 4]; 6];
    let mut n = 1;
    match cp {
        // Upper half and eighth.
        0x2580 => parts[0] = [0.0, 0.0, 1.0, 0.5],
        0x2594 => parts[0] = [0.0, 0.0, 1.0, 0.125],
        // Lower blocks rising in eighths up to the full block.
        0x2581..=0x2588 => parts[0] = [0.0, 1.0 - (cp - 0x2580) as f32 / 8.0, 1.0, 1.0],
        // Left blocks shrinking in eighths.
        0x2589..=0x258f => parts[0] = [0.0, 0.0, (0x2590 - cp) as f32 / 8.0, 1.0],
        // Right half and eighth.
        0x2590 => parts[0] = [0.5, 0.0, 1.0, 1.0],
        0x2595 => parts[0] = [0.875, 0.0, 1.0, 1.0],
        // Shades: uniform alpha fills of 0x40/0x80/0xC0.
        0x2591..=0x2593 => {
            alpha = ((cp - 0x2590) * 64) as f32 / 255.0;
            parts[0] = [0.0, 0.0, 1.0, 1.0];
        }
        0x2596..=0x259f => {
            let quads = QUADS[(cp - 0x2596) as usize];
            n = 0;
            for (bit, x, y) in [(1, 0.0, 0.0), (2, 0.5, 0.0), (4, 0.0, 0.5), (8, 0.5, 0.5)] {
                if quads & bit != 0 {
                    parts[n] = [x, y, x + 0.5, y + 0.5];
                    n += 1;
                }
            }
        }
        // Sextants: a 2x3 fill grid in the codepoint bits. The encoding
        // skips the patterns that exist elsewhere (empty, ▌, ▐, █).
        0x1fb00..=0x1fb3b => {
            let idx = cp - 0x1fb00;
            let bits = idx + idx / 0x14 + 1;
            n = 0;
            for row in 0..3 {
                for col in 0..2 {
                    if bits & (1 << (row * 2 + col)) != 0 {
                        let (x, y) = (col as f32 * 0.5, row as f32 / 3.0);
                        parts[n] = [x, y, x + 0.5, y + 1.0 / 3.0];
                        n += 1;
                    }
                }
            }
        }
        _ => return,
    }

    let color = Color {
        a: fg.a * alpha,
        ..fg
    };
    for [x0, y0, x1, y1] in &parts[..n] {
        let px = |f: f32, size: f32| (f * size).round();
        rect(
            canvas,
            cell,
            px(*x0, cell.size.x),
            px(*y0, cell.size.y),
            px(*x1, cell.size.x),
            px(*y1, cell.size.y),
            color,
        );
    }
}

const NONE: u8 = 0;
const LIGHT: u8 = 1;
const HEAVY: u8 = 2;
const DOUBLE: u8 = 3;

/// Per-codepoint spec: 2 bits per arm, up | right<<2 | down<<4 | left<<6.
/// Zeroes are the specials handled before the table (dashes, arcs,
/// diagonals). Derived from the upstream table and verified entry by
/// entry; re-derive from there instead of hand-editing.
#[rustfmt::skip]
const LINES: [u8; 128] = [
    68, 136, 17, 34, 0, 0, 0, 0,
    0, 0, 0, 0, 20, 24, 36, 40,
    80, 144, 96, 160, 5, 9, 6, 10,
    65, 129, 66, 130, 21, 25, 22, 37,
    38, 26, 41, 42, 81, 145, 82, 97,
    98, 146, 161, 162, 84, 148, 88, 152,
    100, 164, 104, 168, 69, 133, 73, 137,
    70, 134, 74, 138, 85, 149, 89, 153,
    86, 101, 102, 150, 90, 165, 105, 154,
    169, 166, 106, 170, 0, 0, 0, 0,
    204, 51, 28, 52, 60, 208, 112, 240,
    13, 7, 15, 193, 67, 195, 29, 55,
    63, 209, 115, 243, 220, 116, 252, 205,
    71, 207, 221, 119, 255, 0, 0, 0,
    0, 0, 0, 0, 64, 1, 4, 16,
    128, 2, 8, 32, 72, 33, 132, 18,
];

/// Box Drawing (U+2500..=U+257F): lines from each cell edge to the
/// center, plus dashed, arc and diagonal specials.
fn draw_box(canvas: Rid, cell: Rect2, fg: Color, thick: f32, cp: u32) {
    let light = (thick.round() as i32).max(1);
    let heavy = light * 2;
    let spaced = light.max(4);
    match cp {
        0x2504 => dash(canvas, cell, fg, 3, light, spaced, false),
        0x2505 => dash(canvas, cell, fg, 3, heavy, spaced, false),
        0x2506 => dash(canvas, cell, fg, 3, light, spaced, true),
        0x2507 => dash(canvas, cell, fg, 3, heavy, spaced, true),
        0x2508 => dash(canvas, cell, fg, 4, light, spaced, false),
        0x2509 => dash(canvas, cell, fg, 4, heavy, spaced, false),
        0x250a => dash(canvas, cell, fg, 4, light, spaced, true),
        0x250b => dash(canvas, cell, fg, 4, heavy, spaced, true),
        0x254c => dash(canvas, cell, fg, 2, light, light, false),
        0x254d => dash(canvas, cell, fg, 2, heavy, heavy, false),
        0x254e => dash(canvas, cell, fg, 2, light, heavy, true),
        0x254f => dash(canvas, cell, fg, 2, heavy, heavy, true),
        // ╭╮╯╰: the sign pair picks which two edges the elbow connects.
        0x256d => arc(canvas, cell, fg, light, (1.0, 1.0)),
        0x256e => arc(canvas, cell, fg, light, (-1.0, 1.0)),
        0x256f => arc(canvas, cell, fg, light, (-1.0, -1.0)),
        0x2570 => arc(canvas, cell, fg, light, (1.0, -1.0)),
        0x2571 => diagonal(canvas, cell, fg, light, true),
        0x2572 => diagonal(canvas, cell, fg, light, false),
        0x2573 => {
            diagonal(canvas, cell, fg, light, true);
            diagonal(canvas, cell, fg, light, false);
        }
        _ => lines_char(canvas, cell, fg, light, LINES[(cp - 0x2500) as usize]),
    }
}

/// One codepoint of intersecting lines; each arm runs from its edge to a
/// stop chosen so joints meet flush, doubles leaving a gap for arms that
/// pass through. Arms overlap at the center, which double-composites
/// faint (translucent) colors there; accepted as niche.
fn lines_char(canvas: Rid, cell: Rect2, fg: Color, light: i32, spec: u8) {
    let (w, h) = (cell.size.x as i32, cell.size.y as i32);
    let heavy = light * 2;
    let (up, right) = (spec & 3, (spec >> 2) & 3);
    let (down, left) = ((spec >> 4) & 3, (spec >> 6) & 3);

    // Floors keep the strips inside the cell when strokes outgrow it.
    let h_light_top = ((h - light) / 2).max(0);
    let h_light_bottom = h_light_top + light;
    let h_heavy_top = ((h - heavy) / 2).max(0);
    let h_heavy_bottom = h_heavy_top + heavy;
    let h_double_top = (h_light_top - light).max(0);
    let h_double_bottom = h_light_bottom + light;
    let v_light_left = ((w - light) / 2).max(0);
    let v_light_right = v_light_left + light;
    let v_heavy_left = ((w - heavy) / 2).max(0);
    let v_heavy_right = v_heavy_left + heavy;
    let v_double_left = (v_light_left - light).max(0);
    let v_double_right = v_light_right + light;

    // Where an arm stops: past the crossing stroke for heavy neighbors,
    // at the double outer edge, at the light far side, or short of the
    // center when a lone opposing arm passes through.
    let stop = |a: u8, b: u8, this: u8, other: u8, near: i32, mid: i32, far: i32, dbl: i32| {
        if a == HEAVY || b == HEAVY {
            far
        } else if a != b || this == other {
            if a == DOUBLE || b == DOUBLE { dbl } else { mid }
        } else if a == NONE && b == NONE {
            mid
        } else {
            near
        }
    };
    #[rustfmt::skip]
    let up_bottom = stop(left, right, down, up,
        h_light_top, h_light_bottom, h_heavy_bottom, h_double_bottom);
    #[rustfmt::skip]
    let down_top = stop(left, right, up, down,
        h_light_bottom, h_light_top, h_heavy_top, h_double_top);
    #[rustfmt::skip]
    let left_right = stop(up, down, left, right,
        v_light_left, v_light_right, v_heavy_right, v_double_right);
    #[rustfmt::skip]
    let right_left = stop(up, down, right, left,
        v_light_right, v_light_left, v_heavy_left, v_double_left);

    let arm = |x0: i32, y0: i32, x1: i32, y1: i32| {
        rect(canvas, cell, x0 as f32, y0 as f32, x1 as f32, y1 as f32, fg);
    };

    match up {
        LIGHT => arm(v_light_left, 0, v_light_right, up_bottom),
        HEAVY => arm(v_heavy_left, 0, v_heavy_right, up_bottom),
        DOUBLE => {
            let lb = if left == DOUBLE {
                h_light_top
            } else {
                up_bottom
            };
            let rb = if right == DOUBLE {
                h_light_top
            } else {
                up_bottom
            };
            arm(v_double_left, 0, v_light_left, lb);
            arm(v_light_right, 0, v_double_right, rb);
        }
        _ => {}
    }
    match right {
        LIGHT => arm(right_left, h_light_top, w, h_light_bottom),
        HEAVY => arm(right_left, h_heavy_top, w, h_heavy_bottom),
        DOUBLE => {
            let tl = if up == DOUBLE {
                v_light_right
            } else {
                right_left
            };
            let bl = if down == DOUBLE {
                v_light_right
            } else {
                right_left
            };
            arm(tl, h_double_top, w, h_light_top);
            arm(bl, h_light_bottom, w, h_double_bottom);
        }
        _ => {}
    }
    match down {
        LIGHT => arm(v_light_left, down_top, v_light_right, h),
        HEAVY => arm(v_heavy_left, down_top, v_heavy_right, h),
        DOUBLE => {
            let lt = if left == DOUBLE {
                h_light_bottom
            } else {
                down_top
            };
            let rt = if right == DOUBLE {
                h_light_bottom
            } else {
                down_top
            };
            arm(v_double_left, lt, v_light_left, h);
            arm(v_light_right, rt, v_double_right, h);
        }
        _ => {}
    }
    match left {
        LIGHT => arm(0, h_light_top, left_right, h_light_bottom),
        HEAVY => arm(0, h_heavy_top, left_right, h_heavy_bottom),
        DOUBLE => {
            let tr = if up == DOUBLE {
                v_light_left
            } else {
                left_right
            };
            let br = if down == DOUBLE {
                v_light_left
            } else {
                left_right
            };
            arm(0, h_double_top, tr, h_light_top);
            arm(0, h_light_bottom, br, h_double_bottom);
        }
        _ => {}
    }
}

/// Dashes sized so the pattern tiles seamlessly cell to cell: one gap per
/// dash, half gaps at the sides horizontally, a full trailing gap
/// vertically; leftover pixels widen dashes, not gaps.
fn dash(canvas: Rid, cell: Rect2, fg: Color, count: i32, thick: i32, gap: i32, vertical: bool) {
    let (w, h) = (cell.size.x as i32, cell.size.y as i32);
    let dim = if vertical { h } else { w };
    let cross = if vertical { w } else { h };
    let mid = (cross - thick) / 2;
    let seg = |from: i32, to: i32| {
        let (a, b) = (from as f32, to as f32);
        let (lo, hi) = (mid as f32, (mid + thick) as f32);
        if vertical {
            rect(canvas, cell, lo, a, hi, b, fg);
        } else {
            rect(canvas, cell, a, lo, b, hi, fg);
        }
    };
    if dim < 2 * count {
        // No room for the pattern; solid line instead.
        return seg(0, dim);
    }
    let gap = gap.min(dim / (2 * count));
    let total_dash = dim - count * gap;
    let dash_len = total_dash / count;
    let mut extra = total_dash % count;

    let mut pos = if vertical { 0 } else { gap / 2 };
    for _ in 0..count {
        let mut end = pos + dash_len;
        if extra > 0 {
            extra -= 1;
            end += 1;
        }
        seg(pos, end);
        pos = end + gap;
    }
}

/// Quarter-turn corner: straight runs from two edges joined by a bezier
/// bend around the center, stroked at light thickness. `dir` is the
/// (x, y) sign of the two edges the arc connects.
fn arc(canvas: Rid, cell: Rect2, fg: Color, light: i32, dir: (f32, f32)) {
    let (w, h) = (cell.size.x, cell.size.y);
    let (sx, sy) = dir;
    let half = light as f32 / 2.0;
    let cx = ((cell.size.x as i32 - light) / 2) as f32 + half;
    let cy = ((cell.size.y as i32 - light) / 2) as f32 + half;
    let r = w.min(h) / 2.0;
    // Fraction from the center for the bend's control points.
    let s = 0.25;

    let mut points = Vec::with_capacity(12);
    points.push((cx, if sy > 0.0 { h } else { 0.0 }));
    let p0 = (cx, cy + sy * r);
    let c1 = (cx, cy + sy * s * r);
    let c2 = (cx + sx * s * r, cy);
    let p1 = (cx + sx * r, cy);
    for i in 0..=8 {
        points.push(cubic(p0, c1, c2, p1, i as f32 / 8.0));
    }
    points.push((if sx > 0.0 { w } else { 0.0 }, cy));
    stroke(canvas, cell, &points, fg, light as f32);
}

/// Corner-to-corner light line, overshooting slightly along the slope so
/// diagonals in adjacent cells connect. `up` slants like ╱.
fn diagonal(canvas: Rid, cell: Rect2, fg: Color, light: i32, up: bool) {
    let (w, h) = (cell.size.x, cell.size.y);
    let sx = (w / h).min(1.0) * 0.5;
    let sy = (h / w).min(1.0) * 0.5;
    let (from, to) = if up {
        ((w + sx, -sy), (-sx, h + sy))
    } else {
        ((-sx, -sy), (w + sx, h + sy))
    };
    stroke(canvas, cell, &[from, to], fg, light as f32);
}

/// Codepoint bit -> (column, row) of the dot it controls.
#[rustfmt::skip]
const DOTS: [(usize, usize); 8] = [
    (0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3),
];

/// Braille (U+2800..=U+28FF): a 2x4 grid of square dots encoded in the
/// codepoint bits. Spare pixels go first to dot size, then margins, then
/// spacing, so dots stay visible at tiny cells.
fn draw_braille(canvas: Rid, cell: Rect2, fg: Color, cp: u32) {
    let (w, h) = (cell.size.x as i32, cell.size.y as i32);
    let mut dot = (w / 4).min(h / 8);
    let mut x_spacing = w / 4;
    let mut y_spacing = h / 8;
    let mut x_margin = x_spacing / 2;
    let mut y_margin = y_spacing / 2;
    let mut x_left = w - 2 * x_margin - x_spacing - 2 * dot;
    let mut y_left = h - 2 * y_margin - 3 * y_spacing - 4 * dot;

    if x_left >= 2 && y_left >= 4 && dot == 0 {
        dot += 1;
        x_left -= 2;
        y_left -= 4;
    }
    if x_left >= 2 && x_margin == 0 {
        x_margin = 1;
        x_left -= 2;
    }
    if y_left >= 2 && y_margin == 0 {
        y_margin = 1;
        y_left -= 2;
    }
    if x_left >= 1 {
        x_spacing += 1;
        x_left -= 1;
    }
    if y_left >= 3 {
        y_spacing += 1;
        y_left -= 3;
    }
    if x_left >= 2 {
        x_margin += 1;
        x_left -= 2;
    }
    if y_left >= 2 {
        y_margin += 1;
        y_left -= 2;
    }
    if x_left >= 2 && y_left >= 4 {
        dot += 1;
    }

    let xs = [x_margin, x_margin + dot + x_spacing];
    let mut ys = [y_margin; 4];
    for i in 1..4 {
        ys[i] = ys[i - 1] + dot + y_spacing;
    }

    for (bit, (col, row)) in DOTS.iter().enumerate() {
        if cp & (1 << bit) != 0 {
            let (x, y) = (xs[*col] as f32, ys[*row] as f32);
            rect(canvas, cell, x, y, x + dot as f32, y + dot as f32, fg);
        }
    }
}

/// Powerline (U+E0B0..=U+E0BF): prompt segment glyphs that must butt
/// flush against the neighboring cell.
fn draw_powerline(canvas: Rid, cell: Rect2, fg: Color, thick: f32, cp: u32) {
    let (w, h) = (cell.size.x, cell.size.y);
    let light = (thick.round() as i32).max(1);
    match cp {
        // Solid and outlined arrows pointing right/left.
        0xe0b0 => polygon(canvas, cell, &[(0.0, 0.0), (w, h / 2.0), (0.0, h)], fg),
        0xe0b1 => chevron(canvas, cell, fg, light, false),
        0xe0b2 => polygon(canvas, cell, &[(w, 0.0), (0.0, h / 2.0), (w, h)], fg),
        0xe0b3 => chevron(canvas, cell, fg, light, true),
        // Solid and outlined half-discs.
        0xe0b4 => half_disc(canvas, cell, fg, light, false, true),
        0xe0b5 => half_disc(canvas, cell, fg, light, false, false),
        0xe0b6 => half_disc(canvas, cell, fg, light, true, true),
        0xe0b7 => half_disc(canvas, cell, fg, light, true, false),
        // Solid and lined bottom/top corner triangles.
        0xe0b8 => polygon(canvas, cell, &[(0.0, 0.0), (w, h), (0.0, h)], fg),
        0xe0b9 | 0xe0bf => diagonal(canvas, cell, fg, light, false),
        0xe0ba => polygon(canvas, cell, &[(w, 0.0), (w, h), (0.0, h)], fg),
        0xe0bb | 0xe0bd => diagonal(canvas, cell, fg, light, true),
        0xe0bc => polygon(canvas, cell, &[(0.0, 0.0), (w, 0.0), (0.0, h)], fg),
        0xe0be => polygon(canvas, cell, &[(0.0, 0.0), (w, 0.0), (w, h)], fg),
        _ => {}
    }
}

fn chevron(canvas: Rid, cell: Rect2, fg: Color, light: i32, flip: bool) {
    let (w, h) = (cell.size.x, cell.size.y);
    let x = |x: f32| if flip { w - x } else { x };
    #[rustfmt::skip]
    let points = [(x(0.0), 0.0), (x(w), h / 2.0), (x(0.0), h)];
    stroke(canvas, cell, &points, fg, light as f32);
}

/// Semicircle bulging right (or left when flipped), circular arcs
/// approximated by the standard two-bezier construction.
fn half_disc(canvas: Rid, cell: Rect2, fg: Color, light: i32, flip: bool, fill: bool) {
    let (w, h) = (cell.size.x, cell.size.y);
    let r = w.min(h / 2.0);
    // Coefficient for approximating a circular arc.
    let c = (std::f32::consts::SQRT_2 - 1.0) * 4.0 / 3.0;
    let x = |x: f32| if flip { w - x } else { x };

    let mut points = Vec::with_capacity(20);
    let mut curve = |p0: (f32, f32), c1: (f32, f32), c2: (f32, f32), p1: (f32, f32)| {
        for i in 0..=8 {
            let (px, py) = cubic(p0, c1, c2, p1, i as f32 / 8.0);
            points.push((x(px), py));
        }
    };
    curve((0.0, 0.0), (r * c, 0.0), (r, r - r * c), (r, r));
    curve((r, h - r), (r, h - r + r * c), (r * c, h), (0.0, h));

    if fill {
        polygon(canvas, cell, &points, fg);
    } else {
        stroke(canvas, cell, &points, fg, light as f32);
    }
}

// Canvas helpers shared by the glyph families above.

/// Axis-aligned rect in cell pixels.
fn rect(canvas: Rid, cell: Rect2, x0: f32, y0: f32, x1: f32, y1: f32, color: Color) {
    RenderingServer::singleton().canvas_item_add_rect(
        canvas,
        Rect2::new(
            Vector2::new(cell.position.x + x0, cell.position.y + y0),
            Vector2::new(x1 - x0, y1 - y0),
        ),
        color,
    );
}

/// Filled polygon in cell pixels. Godot polygons have no antialiasing, so
/// interior edges are feathered with a thin antialiased outline; edges on
/// the cell boundary stay crisp and keep butting flush against neighbors.
fn polygon(canvas: Rid, cell: Rect2, points: &[(f32, f32)], color: Color) {
    let canvas_points: PackedVector2Array = points
        .iter()
        .map(|&(x, y)| Vector2::new(cell.position.x + x, cell.position.y + y))
        .collect();
    RenderingServer::singleton().canvas_item_add_polygon(
        canvas,
        &canvas_points,
        &PackedColorArray::from([color]),
    );

    let (w, h) = (cell.size.x, cell.size.y);
    let on_boundary = |a: (f32, f32), b: (f32, f32)| {
        (a.0 == b.0 && (a.0 <= 0.0 || a.0 >= w)) || (a.1 == b.1 && (a.1 <= 0.0 || a.1 >= h))
    };
    let mut run: Vec<(f32, f32)> = Vec::with_capacity(points.len() + 1);
    for i in 0..points.len() {
        let (a, b) = (points[i], points[(i + 1) % points.len()]);
        if on_boundary(a, b) {
            if run.len() > 1 {
                stroke(canvas, cell, &run, color, 1.0);
            }
            run.clear();
        } else {
            if run.is_empty() {
                run.push(a);
            }
            run.push(b);
        }
    }
    if run.len() > 1 {
        stroke(canvas, cell, &run, color, 1.0);
    }
}

/// Stroked open path in cell pixels, butt caps.
fn stroke(canvas: Rid, cell: Rect2, points: &[(f32, f32)], color: Color, width: f32) {
    let points: PackedVector2Array = points
        .iter()
        .map(|&(x, y)| Vector2::new(cell.position.x + x, cell.position.y + y))
        .collect();
    RenderingServer::singleton()
        .canvas_item_add_polyline_ex(canvas, &points, &PackedColorArray::from([color]))
        .width(width)
        .antialiased(true)
        .done();
}

fn cubic(p0: (f32, f32), c1: (f32, f32), c2: (f32, f32), p1: (f32, f32), t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let f = |a: f32, b: f32, c: f32, d: f32| {
        u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
    };
    (f(p0.0, c1.0, c2.0, p1.0), f(p0.1, c1.1, c2.1, p1.1))
}
