//! ANSI capture parsing, physical-slot composition, and row comparison for flicker-free drawing.
//! Default backgrounds resolve to pane tints; default foregrounds remain optional.

use anyhow::Result;
use ncm_model::{BorderStyles, Cell, CellStyle, Grid, PaneId, Rgb, WindowInfo};
use std::collections::HashMap;
use unicode_width::UnicodeWidthChar;

/// Parser configuration. Capture lines are independently reset, as emitted by tmux `-e`.
#[derive(Debug, Clone, Copy, Default)]
pub struct LineParser {
    /// Pane background substituted for default background, including SGR 0/49 and padding.
    pub tint: Option<Rgb>,
}

impl LineParser {
    /// Parse a tmux capture line into exactly `width` physical slots, clipping without splitting
    /// wide glyphs and appending combining marks to their leader. Ignore OSC and non-SGR escapes.
    /// Supports basic SGR attributes, ANSI colors and semicolon/colon extended colors.
    pub fn parse(&self, text: &str, width: usize) -> Vec<Cell> {
        let mut line = ParsedLine {
            cells: Vec::with_capacity(width),
            width,
            style: CellStyle {
                bg: self.tint,
                ..CellStyle::default()
            },
            tint: self.tint,
            clipped: false,
        };
        let mut parser = vte::Parser::new();
        parser.advance(&mut line, text.as_bytes());
        line.cells.resize(width, Cell::blank(line.style));
        line.cells
    }

    /// Parse visible capture lines into a fixed-size pane grid; missing rows become tinted blanks.
    pub fn grid(&self, lines: &[String], width: usize, height: usize) -> Result<Grid> {
        let mut grid = Grid::new(
            width,
            height,
            Cell::blank(CellStyle {
                bg: self.tint,
                ..CellStyle::default()
            }),
        )?;
        for (y, line) in lines.iter().take(height).enumerate() {
            for (x, cell) in self.parse(line, width).into_iter().enumerate() {
                if let Some(slot) = grid.get_mut(x, y) {
                    *slot = cell;
                }
            }
        }
        Ok(grid)
    }
}

struct ParsedLine {
    cells: Vec<Cell>,
    width: usize,
    style: CellStyle,
    tint: Option<Rgb>,
    clipped: bool,
}

impl vte::Perform for ParsedLine {
    fn print(&mut self, ch: char) {
        if self.clipped {
            return;
        }
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            if let Some(cell) = self.cells.iter_mut().rev().find(|cell| cell.width != 0) {
                cell.text.push(ch);
            }
        } else if self.cells.len() + width <= self.width {
            self.cells.push(Cell {
                text: ch.to_string(),
                width: width as u8,
                style: self.style,
            });
            if width == 2 {
                self.cells.push(Cell::continuation(self.style));
            }
        } else {
            self.clipped = true;
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if action != 'm' || ignore || !intermediates.is_empty() {
            return;
        }
        let params: Vec<_> = params.iter().collect();
        let mut i = 0;
        while i < params.len() {
            let p = params[i][0];
            match p {
                0 => {
                    self.style = CellStyle {
                        bg: self.tint,
                        ..CellStyle::default()
                    }
                }
                1 => self.style.bold = true,
                2 => self.style.dim = true,
                3 => self.style.italic = true,
                4 => self.style.underline = params[i].get(1) != Some(&0),
                7 => self.style.reverse = true,
                22 => {
                    self.style.bold = false;
                    self.style.dim = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                27 => self.style.reverse = false,
                30..=37 => self.style.fg = Some(indexed_color((p - 30) as u8)),
                90..=97 => self.style.fg = Some(indexed_color((p - 82) as u8)),
                40..=47 => self.style.bg = Some(indexed_color((p - 40) as u8)),
                100..=107 => self.style.bg = Some(indexed_color((p - 92) as u8)),
                39 => self.style.fg = None,
                49 => self.style.bg = self.tint,
                38 | 48 => {
                    let mut color = None;
                    if params[i].len() > 1 {
                        let sub = params[i];
                        if sub[1] == 5 && sub.len() == 3 {
                            color = u8::try_from(sub[2]).ok().map(indexed_color);
                        }
                        if sub[1] == 2 && sub.len() >= 5 {
                            color = rgb(&sub[sub.len() - 3..]);
                        }
                    } else if let Some(next) = params.get(i + 1) {
                        let count = match next[0] {
                            5 => 2,
                            2 => 4,
                            _ => 0,
                        };
                        if count > 0 && i + count < params.len() {
                            if next[0] == 5 {
                                color = u8::try_from(params[i + 2][0]).ok().map(indexed_color);
                            } else {
                                color =
                                    rgb(&[params[i + 2][0], params[i + 3][0], params[i + 4][0]]);
                            }
                            i += count;
                        } else if count > 0 {
                            // Incomplete extended colors must not turn their channel values
                            // into unrelated SGR attributes.
                            i = params.len() - 1;
                        }
                    }
                    if let Some(color) = color {
                        if p == 38 {
                            self.style.fg = Some(color);
                        } else {
                            self.style.bg = Some(color);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

fn rgb(channels: &[u16]) -> Option<Rgb> {
    Some(Rgb::new(
        channels[0].try_into().ok()?,
        channels[1].try_into().ok()?,
        channels[2].try_into().ok()?,
    ))
}

fn indexed_color(n: u8) -> Rgb {
    const BASE: [(u8, u8, u8); 16] = [
        (40, 44, 52),
        (224, 108, 117),
        (152, 195, 121),
        (229, 192, 123),
        (97, 175, 239),
        (198, 120, 221),
        (86, 182, 194),
        (220, 223, 228),
        (93, 103, 122),
        (224, 108, 117),
        (152, 195, 121),
        (229, 192, 123),
        (97, 175, 239),
        (198, 120, 221),
        (86, 182, 194),
        (220, 223, 228),
    ];
    if n < 16 {
        let (r, g, b) = BASE[n as usize];
        return Rgb::new(r, g, b);
    }
    if n >= 232 {
        let v = 8 + 10 * (n - 232);
        return Rgb::new(v, v, v);
    }
    let n = n - 16;
    let steps = [0, 95, 135, 175, 215, 255];
    Rgb::new(
        steps[(n / 36) as usize],
        steps[(n / 6 % 6) as usize],
        steps[(n % 6) as usize],
    )
}

/// Composite every pane at its real coordinates, using the neutral border fallback.
/// Use [`compose_window_with_styles`] with `Tmux::pane_border_styles` for server colors.
pub fn compose_window(window: &WindowInfo, panes: &HashMap<PaneId, Grid>) -> Result<Grid> {
    compose_window_with_styles(window, panes, &BorderStyles::default())
}

/// Composite captures with inherited server/window pane-border styles. Gap topology supplies
/// horizontal/vertical box-drawing and junctions. Borders touching the active pane use its style.
/// Missing captures are tinted spaces. Panes are clipped without splitting wide glyphs.
pub fn compose_window_with_styles(
    window: &WindowInfo,
    panes: &HashMap<PaneId, Grid>,
    styles: &BorderStyles,
) -> Result<Grid> {
    let mut grid = Grid::new(window.width, window.height, Cell::default())?;
    let mut occupied = vec![false; window.width * window.height];
    let zoomed = window.flags.zoomed();
    let zoom_pane = window.active_pane().map(|p| p.id.as_str());
    let visible: Vec<_> = window
        .panes
        .iter()
        .filter(|p| !zoomed || Some(p.id.as_str()) == zoom_pane)
        .collect();
    for pane in &visible {
        let width = pane.width.min(window.width.saturating_sub(pane.x));
        let height = pane.height.min(window.height.saturating_sub(pane.y));
        for y in 0..height {
            for x in 0..width {
                let mut cell = panes
                    .get(&pane.id)
                    .and_then(|grid| grid.get(x, y))
                    .cloned()
                    .unwrap_or_else(|| {
                        Cell::blank(CellStyle {
                            bg: pane.tint,
                            ..CellStyle::default()
                        })
                    });
                if cell.width == 2 && x + 1 >= width {
                    cell = Cell::blank(cell.style);
                }
                if let Some(slot) = grid.get_mut(pane.x + x, pane.y + y) {
                    *slot = cell;
                    occupied[(pane.y + y) * window.width + pane.x + x] = true;
                }
            }
        }
    }
    let gap = |x: usize, y: usize| !occupied[y * window.width + x];
    for y in 0..window.height {
        for x in 0..window.width {
            if !gap(x, y) {
                continue;
            }
            // L=1 R=2 U=4 D=8; a border continues to the window's outer edge.
            let mut mask = 0;
            if x > 0 && gap(x - 1, y) {
                mask |= 1;
            }
            if x + 1 < window.width && gap(x + 1, y) {
                mask |= 2;
            }
            if y > 0 && gap(x, y - 1) {
                mask |= 4;
            }
            if y + 1 < window.height && gap(x, y + 1) {
                mask |= 8;
            }
            if mask & 3 != 0 {
                if x == 0 {
                    mask |= 1;
                }
                if x + 1 == window.width {
                    mask |= 2;
                }
            }
            if mask & 12 != 0 {
                if y == 0 {
                    mask |= 4;
                }
                if y + 1 == window.height {
                    mask |= 8;
                }
            }
            if mask == 0 {
                // A single-cell separator has no gap neighbors; pane positions determine it.
                let side = visible.iter().any(|p| {
                    y >= p.y
                        && y < p.y.saturating_add(p.height)
                        && (x.checked_add(1) == Some(p.x) || x == p.x.saturating_add(p.width))
                });
                mask = if side { 12 } else { 3 };
            }
            let text = match mask {
                1..=3 => "─",
                4 | 8 | 12 => "│",
                5 => "┘",
                6 => "└",
                9 => "┐",
                10 => "┌",
                7 => "┴",
                11 => "┬",
                13 => "┤",
                14 => "├",
                15 => "┼",
                _ => " ",
            };
            let active = visible.iter().any(|p| {
                p.active
                    && x.saturating_add(1) >= p.x
                    && x <= p.x.saturating_add(p.width)
                    && y.saturating_add(1) >= p.y
                    && y <= p.y.saturating_add(p.height)
            });
            if let Some(slot) = grid.get_mut(x, y) {
                *slot = Cell {
                    text: text.into(),
                    width: 1,
                    style: if active { styles.active } else { styles.normal },
                };
            }
        }
    }
    Ok(grid)
}

/// Return current row indices needing writes. First frame/width changes redraw all current rows.
/// The app erases removed rows on height shrink and wraps writes in synchronized output, without
/// a full-screen clear. This function itself emits no terminal output.
pub fn changed_rows(previous: Option<&Grid>, current: &Grid) -> Vec<usize> {
    (0..current.height())
        .filter(|&y| {
            previous
                .is_none_or(|old| old.width() != current.width() || old.row(y) != current.row(y))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_preserves_styles_widths_combining_and_tint() {
        let tint = Rgb::new(1, 36, 86);
        let cells = LineParser { tint: Some(tint) }
            .parse("\x1b[1;3;4;7;38;2;10;20;30m界e\u{301}\x1b[0m!", 5);
        assert_eq!(cells.len(), 5);
        assert_eq!(cells[0].width, 2);
        assert_eq!(cells[1].width, 0);
        assert_eq!(cells[2].text, "e\u{301}");
        assert!(
            cells[0].style.bold
                && cells[0].style.italic
                && cells[0].style.underline
                && cells[0].style.reverse
        );
        assert_eq!(cells[0].style.fg, Some(Rgb::new(10, 20, 30)));
        assert_eq!(cells[3].style.bg, Some(tint));
        assert!(!cells[3].style.bold);
        assert_eq!(LineParser::default().parse("x界", 2)[1].text, " ");
        assert_eq!(LineParser::default().parse("x界y", 2)[1].text, " ");
    }
}
