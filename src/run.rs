//! Text drawn as shaped runs instead of single glyphs: consecutive ASCII
//! cells with one style and color, shaped together so the font's ligature
//! substitutions apply. Each glyph is anchored to its own cell because the
//! cell grid uses the ceiled advance and pen-advanced drawing drifts off it.

use std::collections::HashMap;

use godot::classes::text_server::Direction;
use godot::classes::{Font, TextServerManager};
use godot::prelude::*;

/// A run being collected while walking a row, at cell-left `x`.
pub struct Run {
    pub text: String,
    pub style: u8,
    pub fg: Color,
    pub x: f32,
}

struct Glyph {
    cell: i64,
    index: i64,
    font_rid: Rid,
    font_size: i64,
    offset: Vector2,
}

struct Entry {
    glyphs: Vec<Glyph>,
    used: u64,
}

/// Shaped runs keyed on content per style font, kept exactly as large as
/// the last repaint. Scrolled rows keep the same content, so they hit.
pub struct RunCache {
    entries: [HashMap<String, Entry>; 4],
    generation: u64,
}

impl RunCache {
    pub fn new() -> Self {
        Self {
            entries: Default::default(),
            generation: 0,
        }
    }

    /// Draw one run with its first baseline at `(run.x, baseline)`. A
    /// single cell skips shaping, alone it has no ligature context.
    pub fn draw(
        &mut self,
        canvas: Rid,
        font: &Gd<Font>,
        run: Run,
        baseline: f32,
        cell_w: f32,
        font_size: i32,
    ) {
        let origin = Vector2::new(run.x, baseline);
        if run.text.len() == 1 {
            font.draw_char_ex(canvas, origin, run.text.as_bytes()[0] as u32, font_size)
                .modulate(run.fg)
                .done();
            return;
        }
        let Some(ts) = TextServerManager::singleton().get_primary_interface() else {
            return;
        };
        let generation = self.generation;
        let entry = self.entries[run.style as usize]
            .entry(run.text)
            .or_insert_with_key(|text| Entry {
                glyphs: shape(text, font, font_size),
                used: generation,
            });
        entry.used = generation;
        for glyph in &entry.glyphs {
            ts.font_draw_glyph_ex(
                glyph.font_rid,
                canvas,
                glyph.font_size,
                origin + Vector2::new(glyph.cell as f32 * cell_w, 0.0) + glyph.offset,
                glyph.index,
            )
            .color(run.fg)
            .done();
        }
    }

    /// Drop runs not drawn since the last call.
    pub fn end_frame(&mut self) {
        let generation = self.generation;
        for map in &mut self.entries {
            map.retain(|_, entry| entry.used == generation);
        }
        self.generation += 1;
    }
}

/// Shape through the TextServer and keep each glyph with the cell it
/// starts in, from its cluster. ASCII runs are one char per cell.
fn shape(text: &str, font: &Gd<Font>, font_size: i32) -> Vec<Glyph> {
    let Some(mut ts) = TextServerManager::singleton().get_primary_interface() else {
        return Vec::new();
    };
    let rid = ts.create_shaped_text_ex().direction(Direction::LTR).done();
    ts.shaped_text_add_string(rid, text, &font.get_rids(), font_size as i64);
    let glyphs = ts
        .shaped_text_get_glyphs(rid)
        .iter_shared()
        .map(|g| {
            let get = |key: &str| g.get(key).unwrap_or_default();
            Glyph {
                cell: get("start").try_to().unwrap_or(0),
                index: get("index").try_to().unwrap_or(0),
                font_rid: get("font_rid").try_to().unwrap_or(Rid::Invalid),
                font_size: get("font_size").try_to().unwrap_or(font_size as i64),
                offset: get("offset").try_to().unwrap_or(Vector2::ZERO),
            }
        })
        .collect();
    ts.free_rid(rid);
    glyphs
}
