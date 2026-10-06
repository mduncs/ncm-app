use ncm_model::WindowInfo;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}
impl Rect {
    pub fn contains(self, x: usize, y: usize) -> bool {
        x >= self.x && y >= self.y && x - self.x < self.width && y - self.y < self.height
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub columns: usize,
    pub cards: Vec<Rect>,
}
impl Layout {
    pub fn new(width: usize, height: usize, windows: &[WindowInfo], pixels: (u16, u16)) -> Self {
        let available = height.saturating_sub(Self::dock_rows(pixels.1));
        if width == 0 || available == 0 || windows.is_empty() {
            return Self::default();
        }
        let n = windows.len();
        let mut best = (f64::NEG_INFINITY, 1);
        for columns in 1..=n.min(width) {
            let rows = n.div_ceil(columns);
            if rows > available {
                continue;
            }
            // Gaps are inside each image: rectangles themselves have no cell gaps.
            let cw = (width / columns) as f64 * f64::from(pixels.0);
            let ch = (available / rows) as f64 * f64::from(pixels.1);
            let score = windows
                .iter()
                .map(|w| {
                    (cw / (w.width.max(1) as f64 * f64::from(pixels.0)))
                        .min(ch / (w.height.max(1) as f64 * f64::from(pixels.1)))
                })
                .fold(f64::INFINITY, f64::min);
            if score > best.0 {
                best = (score, columns);
            }
        }
        let columns = best.1;
        let rows = n.div_ceil(columns);
        let cards = (0..n)
            .map(|i| {
                let col = i % columns;
                let row = i / columns;
                let x = col * width / columns;
                let y = row * available / rows;
                Rect {
                    x,
                    y,
                    width: (col + 1) * width / columns - x,
                    height: (row + 1) * available / rows - y,
                }
            })
            .collect();
        Self { columns, cards }
    }
    pub fn dock_rows(cell_height: u16) -> usize {
        30usize.div_ceil(usize::from(cell_height.max(1)))
    }
    pub fn panel(width: usize, height: usize, _native: (usize, usize), _screen: bool) -> Rect {
        let available = height.saturating_sub(1);
        // sqrt(.78) on both axes occupies ~78% of the grid, leaving a visible rim.
        let w = (width * 88 / 100).max(3).min(width);
        let h = (available * 88 / 100).max(3).min(available);
        Rect {
            x: (width - w) / 2,
            y: (available - h) / 2,
            width: w,
            height: h,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    #[test]
    fn tiles_every_cell_without_touching_dock() {
        let windows = demo::windows();
        for (w, h) in [(80, 24), (161, 51), (1, 1), (1, 2), (3, 4)] {
            let l = Layout::new(w, h, &windows, (8, 16));
            let mut occupied = vec![false; w * h];
            for r in l.cards {
                assert!(
                    r.x + r.width <= w && r.y + r.height <= h.saturating_sub(Layout::dock_rows(16))
                );
                for y in r.y..r.y + r.height {
                    for x in r.x..r.x + r.width {
                        assert!(!occupied[y * w + x]);
                        occupied[y * w + x] = true;
                    }
                }
            }
        }
        let l = Layout::new(120, 31, &windows, (8, 16));
        assert_eq!(
            l.cards.iter().map(|r| r.width * r.height).sum::<usize>(),
            120 * 29
        );
    }
    #[test]
    fn score_uses_native_aspect_and_panel_is_centered() {
        let mut windows = demo::windows();
        windows.truncate(4);
        for w in &mut windows {
            w.width = 80;
            w.height = 20;
        }
        assert_eq!(Layout::new(160, 41, &windows, (8, 16)).columns, 2);
        assert_eq!(
            Layout::panel(80, 24, (100, 40), true),
            Rect {
                x: 5,
                y: 1,
                width: 70,
                height: 20
            }
        );
    }
}
