//! Cached thumbnail rasterization, font/tint resolution, Kitty transport and braille fallback.
//! Card labels are ordinary terminal text supplied by the app, never image pixels.

use anyhow::{bail, Context, Result};
use ncm_model::{AgentState, Facts, PaneId, Rgb, TabStyle};
use std::collections::HashMap;

mod braille;
mod font;
mod kitty;
mod png;
mod thumbnail;
mod tint;
pub use braille::braille_thumbnail;
pub use font::{resolve_font, ResolvedFont};
pub use kitty::{KittyEncoder, KittyTile, ROW_DIACRITICS};
pub use thumbnail::{RenderStats, ThumbnailRenderer};
pub use tint::{
    oklch, TintResolver, DEFAULT_FOREGROUND, HAIRLINE, PAGE_BACKGROUND, POWERSHELL_TINT, WSL_TINT,
};

/// Raw row-major, unpremultiplied RGBA8 image, exactly width × height × 4 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// RGBA bytes.
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    /// Validate dimensions and byte count before allocating/transmitting.
    pub fn validate(&self) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            bail!("image dimensions must be nonzero");
        }
        let expected = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|n| n.checked_mul(4))
            .context("image dimensions overflow")?;
        if self.pixels.len() != expected {
            bail!(
                "invalid RGBA byte count: expected {expected}, got {}",
                self.pixels.len()
            );
        }
        Ok(())
    }
}

/// Pane tint region in terminal-cell coordinates; applies only to default backgrounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneTint {
    /// Stable pane identity for invalidation.
    pub pane_id: PaneId,
    /// Left edge.
    pub x: usize,
    /// Top edge.
    pub y: usize,
    /// Region width.
    pub width: usize,
    /// Region height.
    pub height: usize,
    /// Resolved tmux/environment/project tint.
    pub color: Rgb,
}

/// Terminal overlay metadata retained for API compatibility. Only state/edge_color affect pixels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overlays {
    /// Window index/marker/name text.
    pub title: String,
    /// Actual tmux status tab appearance.
    pub tab: TabStyle,
    /// Dark section strip color.
    pub section_color: Option<Rgb>,
    /// Lowercase harness and environment words with their own colors.
    pub words: Vec<(String, Rgb)>,
    /// Pink unanswered label (caller applies the >20s threshold).
    pub unanswered: Option<String>,
    /// Formatted age since tmux activity.
    pub age: String,
    /// Now/subagent captions; only show subagents for windows containing a harness.
    pub facts: Facts,
    /// Persistent state edge.
    pub state: AgentState,
    /// Optional tmux attention edge (bell/activity/silence), resolved by the caller.
    pub edge_color: Option<Rgb>,
    /// Retained for compatibility; selection is painted by the terminal UI.
    pub selected: bool,
}

/// Thumbnail target geometry. Native cell aspect ratio determines the centered/bottom cover crop;
/// the atlas rasterizes glyphs at the resulting thumbnail size, without downscaling glyphs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbnailSize {
    /// Target width in pixels.
    pub width: u32,
    /// Target height in pixels.
    pub height: u32,
    /// Native terminal cell width in pixels.
    pub cell_width: f32,
    /// Native terminal cell height in pixels.
    pub cell_height: f32,
}

/// Reusable glyph bitmap at a thumbnail font size, independent of cell color.
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphBitmap {
    /// Fontdue bearing/advance and bitmap dimensions.
    pub metrics: fontdue::Metrics,
    /// Row-major alpha coverage.
    pub alpha: Vec<u8>,
}

/// A bounded glyph cache for one face, keyed by thumbnail size and synthetic style.
/// Layout changes select a new size; glyph bitmaps are never downscaled afterward.
pub struct GlyphAtlas {
    font: fontdue::Font,
    glyphs: HashMap<(char, u32, u8), GlyphBitmap>,
    capacity: usize,
}

impl GlyphAtlas {
    /// Load a resolved Ghostty/SF Mono/Menlo face supplied by the caller; rejects invalid font data.
    pub fn new(font_bytes: &[u8], capacity: usize) -> Result<Self> {
        if capacity == 0 {
            bail!("glyph cache capacity must be nonzero");
        }
        let font = fontdue::Font::from_bytes(font_bytes, fontdue::FontSettings::default())
            .map_err(|error| anyhow::anyhow!("load font: {error}"))?;
        Ok(Self {
            font,
            glyphs: HashMap::new(),
            capacity,
        })
    }

    /// Rasterize once per character/size, then reuse its alpha bitmap. The bounded cache
    /// clears at capacity to prevent persistent growth on arbitrary captured Unicode.
    pub fn glyph(&mut self, character: char, pixels: f32) -> Result<&GlyphBitmap> {
        if !pixels.is_finite() || pixels <= 0.0 {
            bail!("invalid glyph size");
        }
        self.glyph_styled(character, pixels, false, false)
    }

    /// Cached synthetic bold/oblique coverage, without loading extra full font faces.
    pub fn glyph_styled(
        &mut self,
        character: char,
        pixels: f32,
        bold: bool,
        italic: bool,
    ) -> Result<&GlyphBitmap> {
        if !pixels.is_finite() || pixels <= 0.0 {
            bail!("invalid glyph size");
        }
        let key = (
            character,
            pixels.to_bits(),
            u8::from(bold) | (u8::from(italic) << 1),
        );
        if !self.glyphs.contains_key(&key) {
            if self.glyphs.len() >= self.capacity {
                self.glyphs.clear();
            }
            let (mut metrics, mut alpha) = self.font.rasterize(character, pixels);
            if (bold || italic) && metrics.width > 0 && metrics.height > 0 {
                let extra = usize::from(bold)
                    + if italic {
                        (metrics.height as f32 * 0.15).ceil() as usize
                    } else {
                        0
                    };
                let width = metrics.width + extra;
                let mut styled = vec![0; width * metrics.height];
                for y in 0..metrics.height {
                    let shift = if italic {
                        ((metrics.height - 1 - y) as f32 * 0.15).round() as usize
                    } else {
                        0
                    };
                    for x in 0..metrics.width {
                        let a = alpha[y * metrics.width + x];
                        styled[y * width + x + shift] = styled[y * width + x + shift].max(a);
                        if bold {
                            styled[y * width + x + shift + 1] =
                                styled[y * width + x + shift + 1].max((a as f32 * 0.45) as u8);
                        }
                    }
                }
                metrics.width = width;
                alpha = styled;
            }
            self.glyphs.insert(key, GlyphBitmap { metrics, alpha });
        }
        self.glyphs
            .get(&key)
            .context("glyph cache insertion failed")
    }
}
