use crate::{DEFAULT_FOREGROUND, PAGE_BACKGROUND};
use anyhow::Result;
use ncm_model::{Cell, CellStyle, Grid, Rgb};

/// Port of deck.py shrink: contain-fit, 2×4 dots, any ink and majority colors per output cell.
/// Source rows are twice as tall as columns. Input defaults should already carry pane tints.
pub fn braille_thumbnail(grid: &Grid, columns: usize, rows: usize) -> Result<Grid> {
    let mut out = Grid::new(columns, rows, Cell::default())?;
    if grid.width() == 0 || grid.height() == 0 || columns == 0 || rows == 0 {
        return Ok(out);
    }
    let scale =
        (2.0 * columns as f64 / grid.width() as f64).min(2.0 * rows as f64 / grid.height() as f64);
    let used_w = ((grid.width() as f64 * scale / 2.0).ceil() as usize).min(columns);
    let used_h = ((grid.height() as f64 * scale / 2.0).ceil() as usize).min(rows);
    let ox = (columns - used_w) / 2;
    let oy = (rows - used_h) / 2;
    const DOTS: [[u32; 2]; 4] = [[1, 8], [2, 16], [4, 32], [64, 128]];
    for cy in 0..used_h {
        for cx in 0..used_w {
            let mut bits = 0;
            let mut fgs = Vec::new();
            let mut bgs = Vec::new();
            let mut samples = 0;
            for (dy, dots) in DOTS.iter().enumerate() {
                for (dx, bit) in dots.iter().enumerate() {
                    let (ya, yb) = span(cy * 4 + dy, grid.height(), 2.0 * scale);
                    let (xa, xb) = span(cx * 2 + dx, grid.width(), scale);
                    for y in ya..yb {
                        for x in xa..xb {
                            let cell = grid.get(x, y).expect("sample bounds checked");
                            samples += 1;
                            let mut fg = cell.style.fg.unwrap_or(DEFAULT_FOREGROUND);
                            let mut bg = cell.style.bg;
                            if cell.style.reverse {
                                let old_fg = fg;
                                fg = bg.unwrap_or(PAGE_BACKGROUND);
                                bg = Some(old_fg);
                            }
                            if cell.style.dim {
                                fg = Rgb::new(fg.r / 2, fg.g / 2, fg.b / 2);
                            }
                            if let Some(bg) = bg {
                                add(&mut bgs, bg);
                            }
                            if cell.width > 0 && !cell.text.trim().is_empty() {
                                bits |= bit;
                                add(&mut fgs, fg);
                            }
                        }
                    }
                }
            }
            let fg = majority(&fgs).map(|(c, _)| c).unwrap_or(DEFAULT_FOREGROUND);
            let bg = majority(&bgs)
                .filter(|(_, n)| 2 * n >= samples)
                .map(|(c, _)| c);
            *out.get_mut(ox + cx, oy + cy)
                .expect("output bounds checked") = Cell {
                text: if bits == 0 {
                    " ".into()
                } else {
                    char::from_u32(0x2800 + bits)
                        .expect("braille codepoint")
                        .into()
                },
                width: 1,
                style: CellStyle {
                    fg: Some(fg),
                    bg,
                    ..CellStyle::default()
                },
            };
        }
    }
    Ok(out)
}
fn span(i: usize, len: usize, per: f64) -> (usize, usize) {
    let a = (i as f64 / per) as usize;
    (
        a.min(len),
        len.min((a + 1).max(((i + 1) as f64 / per) as usize)),
    )
}
fn add(colors: &mut Vec<(Rgb, usize)>, color: Rgb) {
    if let Some((_, n)) = colors.iter_mut().find(|(c, _)| *c == color) {
        *n += 1;
    } else {
        colors.push((color, 1));
    }
}
fn majority(colors: &[(Rgb, usize)]) -> Option<(Rgb, usize)> {
    colors
        .iter()
        .copied()
        .reduce(|a, b| if b.1 > a.1 { b } else { a })
}
