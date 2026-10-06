use crate::{
    resolve_font, GlyphAtlas, Overlays, PaneTint, ResolvedFont, RgbaImage, ThumbnailSize,
    DEFAULT_FOREGROUND, HAIRLINE, PAGE_BACKGROUND,
};
use anyhow::{bail, Context, Result};
use ncm_model::{AgentState, Grid, Rgb};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

/// Work performed by the most recent render, for scheduling/benchmarking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub full_render: bool,
    pub changed_rows: usize,
    pub rasterized_rows: usize,
}
struct Card {
    rows: Vec<u64>,
    dimensions: (usize, usize),
    tints: Vec<PaneTint>,
    size: ThumbnailSize,
    fit: bool,
    state: AgentState,
    edge_color: Option<Rgb>,
    image: RgbaImage,
}
/// Reuses one bounded glyph atlas and an explicitly removable cache per stable card id.
#[derive(Default)]
pub struct ThumbnailRenderer {
    atlas: Option<GlyphAtlas>,
    font: Option<ResolvedFont>,
    cards: HashMap<String, Card>,
    stats: RenderStats,
}
impl std::fmt::Debug for ThumbnailRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThumbnailRenderer")
            .field("font", &self.font)
            .field("cards", &self.cards.len())
            .field("stats", &self.stats)
            .finish()
    }
}
impl ThumbnailRenderer {
    /// Load a caller-supplied font (also useful off macOS). Resolution is otherwise lazy.
    pub fn with_font(bytes: &[u8]) -> Result<Self> {
        Ok(Self {
            atlas: Some(GlyphAtlas::new(bytes, 8192)?),
            ..Self::default()
        })
    }
    pub fn resolved_font(&self) -> Option<&ResolvedFont> {
        self.font.as_ref()
    }
    pub fn last_stats(&self) -> RenderStats {
        self.stats
    }
    pub fn remove_card(&mut self, id: &str) {
        self.cards.remove(id);
    }
    pub fn clear_cards(&mut self) {
        self.cards.clear();
    }
    /// Existing owned-image API. Use one renderer per card, or `render_card` for multiple cards.
    pub fn render(
        &mut self,
        grid: &Grid,
        tints: &[PaneTint],
        overlays: &Overlays,
        size: ThumbnailSize,
    ) -> Result<RgbaImage> {
        Ok(self.render_card("", grid, tints, overlays, size)?.clone())
    }
    /// Zero-copy cached output. Only changed source rows are painted; size/tint geometry invalidates.
    /// Text overlay changes do no image work. Caller controls the 2 Hz schedule and removes old cards.
    pub fn render_card(
        &mut self,
        id: &str,
        grid: &Grid,
        tints: &[PaneTint],
        overlays: &Overlays,
        size: ThumbnailSize,
    ) -> Result<&RgbaImage> {
        self.render_inner(id, grid, tints, overlays, size, false)
    }
    pub fn render_preview(
        &mut self,
        id: &str,
        grid: &Grid,
        tints: &[PaneTint],
        overlays: &Overlays,
        size: ThumbnailSize,
    ) -> Result<&RgbaImage> {
        self.render_inner(id, grid, tints, overlays, size, true)
    }
    fn render_inner(
        &mut self,
        id: &str,
        grid: &Grid,
        tints: &[PaneTint],
        overlays: &Overlays,
        size: ThumbnailSize,
        fit: bool,
    ) -> Result<&RgbaImage> {
        if size.width <= 8
            || size.height <= 8
            || grid.width() == 0
            || grid.height() == 0
            || !size.cell_width.is_finite()
            || !size.cell_height.is_finite()
            || size.cell_width <= 0.0
            || size.cell_height <= 0.0
        {
            bail!(
                "thumbnail needs nonempty cells, finite positive cell geometry and >8px dimensions"
            );
        }
        if self.atlas.is_none() {
            let font = resolve_font()?;
            self.atlas = Some(GlyphAtlas::new(
                &std::fs::read(&font.path).context("read thumbnail font")?,
                8192,
            )?);
            self.font = Some(font);
        }
        let full = self.cards.get(id).is_none_or(|c| {
            c.fit != fit
                || c.size != size
                || c.tints != tints
                || c.dimensions != (grid.width(), grid.height())
        });
        if full {
            let bytes = (size.width as usize)
                .checked_mul(size.height as usize)
                .and_then(|n| n.checked_mul(4))
                .context("thumbnail dimensions overflow")?;
            let mut pixels = Vec::new();
            pixels
                .try_reserve_exact(bytes)
                .context("allocate thumbnail")?;
            pixels.resize(bytes, 0);
            for p in pixels.chunks_exact_mut(4) {
                p.copy_from_slice(&PAGE_BACKGROUND.rgba(255));
            }
            self.cards.insert(
                id.into(),
                Card {
                    rows: vec![0; grid.height()],
                    dimensions: (grid.width(), grid.height()),
                    tints: tints.to_vec(),
                    size,
                    fit,
                    state: overlays.state,
                    edge_color: overlays.edge_color,
                    image: RgbaImage {
                        width: size.width,
                        height: size.height,
                        pixels,
                    },
                },
            );
        }
        let card = self.cards.get_mut(id).context("card cache insertion")?;
        let atlas = self.atlas.as_mut().context("font atlas initialization")?;
        let mapping = Mapping::new(grid, size, fit);
        let state_changed = card.state != overlays.state || card.edge_color != overlays.edge_color;
        let thickness = if overlays.state == AgentState::None && overlays.edge_color.is_none() {
            1
        } else {
            2
        };
        let old_thickness = if card.state == AgentState::None && card.edge_color.is_none() {
            1
        } else {
            2
        };
        // Color changes and a thicker border overwrite the same narrow frame.
        // Only shrinking it needs source rows repainted to restore covered pixels.
        let restore_border = state_changed && thickness < old_thickness;
        let mut stats = RenderStats {
            full_render: full,
            ..RenderStats::default()
        };
        for y in 0..grid.height() {
            let fingerprint = row_hash(grid.row(y).context("source row")?);
            let changed = full || card.rows[y] != fingerprint;
            if !changed && !restore_border {
                continue;
            }
            stats.changed_rows += usize::from(changed);
            let (top, bottom) = mapping.row_band(y);
            if top < bottom {
                draw_row(&mut card.image, grid, tints, y, &mapping, atlas)?;
                stats.rasterized_rows += 1;
            }
            card.rows[y] = fingerprint;
        }
        if full || stats.rasterized_rows > 0 || state_changed {
            // Repaint the narrow frame after row updates, retaining a 4px page inset.
            frame(
                &mut card.image,
                4,
                thickness,
                overlays
                    .edge_color
                    .or_else(|| overlays.state.color())
                    .unwrap_or(HAIRLINE),
            );
        }
        card.state = overlays.state;
        card.edge_color = overlays.edge_color;
        self.stats = stats;
        Ok(&card.image)
    }
}
struct Mapping {
    cw: f32,
    ch: f32,
    ox: f32,
    oy: f32,
    width: u32,
    height: u32,
    font_px: f32,
}
impl Mapping {
    fn new(grid: &Grid, size: ThumbnailSize, fit: bool) -> Self {
        let iw = (size.width - 8) as f32;
        let ih = (size.height - 8) as f32;
        let sx = iw / (grid.width() as f32 * size.cell_width);
        let sy = ih / (grid.height() as f32 * size.cell_height);
        let scale = if fit { sx.min(sy) } else { sx.max(sy) };
        let cw = size.cell_width * scale;
        let ch = size.cell_height * scale;
        Self {
            cw,
            ch,
            ox: 4.0 + (iw - grid.width() as f32 * cw) / 2.0,
            oy: 4.0 + ih - grid.height() as f32 * ch,
            width: size.width,
            height: size.height,
            font_px: (ch * 13.0 / 16.0 * 64.0).round().max(1.0) / 64.0,
        }
    }
    fn row_band(&self, y: usize) -> (i32, i32) {
        (
            boundary(self.oy + y as f32 * self.ch).clamp(4, self.height as i32 - 4),
            boundary(self.oy + (y + 1) as f32 * self.ch).clamp(4, self.height as i32 - 4),
        )
    }
}
fn boundary(v: f32) -> i32 {
    (v - 0.5).ceil() as i32
}
fn colors(cell: &ncm_model::Cell, tint: Rgb) -> (Rgb, Rgb) {
    let (mut fg, mut bg) = (
        cell.style.fg.unwrap_or(DEFAULT_FOREGROUND),
        cell.style.bg.unwrap_or(tint),
    );
    if cell.style.reverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.style.dim {
        fg = Rgb::new(fg.r / 2, fg.g / 2, fg.b / 2);
    }
    (fg, bg)
}
fn draw_row(
    image: &mut RgbaImage,
    grid: &Grid,
    tints: &[PaneTint],
    y: usize,
    m: &Mapping,
    atlas: &mut GlyphAtlas,
) -> Result<()> {
    let (top, bottom) = m.row_band(y);
    let row = grid.row(y).context("thumbnail row")?;
    let tint_at = |x: usize| {
        tints
            .iter()
            .find(|t| x >= t.x && y >= t.y && x - t.x < t.width && y - t.y < t.height)
            .map(|t| t.color)
            .unwrap_or(PAGE_BACKGROUND)
    };
    for (x, cell) in row.iter().enumerate() {
        let left = boundary(m.ox + x as f32 * m.cw).clamp(4, m.width as i32 - 4);
        let right = boundary(m.ox + (x + 1) as f32 * m.cw).clamp(4, m.width as i32 - 4);
        let (_, bg) = colors(cell, tint_at(x));
        for py in top..bottom {
            for px in left..right {
                pixel(image, px, py, bg);
            }
        }
    }
    let line = atlas
        .font
        .horizontal_line_metrics(m.font_px)
        .context("font horizontal metrics")?;
    let baseline =
        m.oy + y as f32 * m.ch + (m.ch - (line.ascent - line.descent)) / 2.0 + line.ascent;
    for (x, cell) in row.iter().enumerate() {
        if cell.width == 0 || cell.text.trim().is_empty() {
            continue;
        }
        let left = m.ox + x as f32 * m.cw;
        if left + cell.width as f32 * m.cw < 4.0 || left >= (m.width - 4) as f32 {
            continue;
        }
        let (fg, _) = colors(cell, tint_at(x));
        let mut pen = left;
        for character in cell.text.chars() {
            let glyph =
                atlas.glyph_styled(character, m.font_px, cell.style.bold, cell.style.italic)?;
            let gx = pen + glyph.metrics.xmin as f32;
            pen += glyph.metrics.advance_width;
            let gy = baseline - glyph.metrics.ymin as f32 - glyph.metrics.height as f32;
            // Fractional positioning spreads each coverage sample into neighboring pixels;
            // the glyph itself is only ever rasterized once at the thumbnail font size.
            let ix = gx.floor() as i32;
            let iy = gy.floor() as i32;
            let fx = gx - gx.floor();
            let fy = gy - gy.floor();
            for by in 0..glyph.metrics.height {
                for bx in 0..glyph.metrics.width {
                    let alpha = glyph.alpha[by * glyph.metrics.width + bx] as f32;
                    if alpha == 0.0 {
                        continue;
                    }
                    for (dx, dy, weight) in [
                        (0, 0, (1.0 - fx) * (1.0 - fy)),
                        (1, 0, fx * (1.0 - fy)),
                        (0, 1, (1.0 - fx) * fy),
                        (1, 1, fx * fy),
                    ] {
                        let px = ix + bx as i32 + dx;
                        let py = iy + by as i32 + dy;
                        if px >= 4 && px < m.width as i32 - 4 && py >= top && py < bottom {
                            blend(image, px, py, fg, (alpha * weight).round() as u8);
                        }
                    }
                }
            }
        }
        if cell.style.underline {
            let py = (baseline + 1.0).floor() as i32;
            if py >= top && py < bottom {
                for px in boundary(left).max(4)
                    ..boundary(left + cell.width as f32 * m.cw).min(m.width as i32 - 4)
                {
                    blend(image, px, py, fg, 180);
                }
            }
        }
    }
    Ok(())
}
fn pixel(image: &mut RgbaImage, x: i32, y: i32, color: Rgb) {
    let i = (y as usize * image.width as usize + x as usize) * 4;
    image.pixels[i..i + 4].copy_from_slice(&color.rgba(255));
}
fn blend(image: &mut RgbaImage, x: i32, y: i32, color: Rgb, alpha: u8) {
    let i = (y as usize * image.width as usize + x as usize) * 4;
    for (offset, c) in [color.r, color.g, color.b].into_iter().enumerate() {
        image.pixels[i + offset] = ((c as u32 * alpha as u32
            + image.pixels[i + offset] as u32 * (255 - alpha as u32)
            + 127)
            / 255) as u8;
    }
}
fn frame(image: &mut RgbaImage, inset: u32, thickness: u32, color: Rgb) {
    for d in inset..inset + thickness {
        if d * 2 >= image.width || d * 2 >= image.height {
            break;
        }
        let right = image.width - 1 - d;
        let bottom = image.height - 1 - d;
        for x in d..=right {
            pixel(image, x as i32, d as i32, color);
            pixel(image, x as i32, bottom as i32, color);
        }
        for y in d..=bottom {
            pixel(image, d as i32, y as i32, color);
            pixel(image, right as i32, y as i32, color);
        }
    }
}

fn row_hash(row: &[ncm_model::Cell]) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for cell in row {
        cell.text.hash(&mut hash);
        cell.width.hash(&mut hash);
        cell.style.hash(&mut hash);
    }
    hash.finish()
}
