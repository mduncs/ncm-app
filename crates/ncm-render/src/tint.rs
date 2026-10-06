use crate::PaneTint;
use ncm_model::{PaneInfo, Rgb, WindowInfo};
use std::collections::BTreeMap;

pub const PAGE_BACKGROUND: Rgb = Rgb::new(14, 15, 18);
pub const HAIRLINE: Rgb = Rgb::new(40, 43, 51);
pub const DEFAULT_FOREGROUND: Rgb = Rgb::new(220, 223, 228);
pub const POWERSHELL_TINT: Rgb = Rgb::new(1, 36, 86);
pub const WSL_TINT: Rgb = Rgb::new(48, 10, 36);

/// Muted project colors spread across the projects in the current snapshot.
#[derive(Debug, Default)]
pub struct TintResolver {
    projects: BTreeMap<String, Rgb>,
}
impl TintResolver {
    pub fn new(windows: &[WindowInfo]) -> Self {
        let mut projects: BTreeMap<_, _> = windows
            .iter()
            .flat_map(|w| &w.panes)
            .map(|p| (project(p), PAGE_BACKGROUND))
            .collect();
        let count = projects.len().max(1) as f32;
        for (i, color) in projects.values_mut().enumerate() {
            *color = oklch(0.20, 0.03, (25.0 + i as f32 * 360.0 / count) % 360.0);
        }
        Self { projects }
    }
    /// Explicit tmux tint wins, followed by environment, then per-project OKLCH.
    pub fn resolve(&self, pane: &PaneInfo) -> Rgb {
        if let Some(tint) = pane.tint {
            return tint;
        }
        let label = pane.title.to_lowercase();
        if label.contains("powershell") || label.contains("windows") || label.contains("pwsh") {
            return POWERSHELL_TINT;
        }
        if label.contains("wsl") || label.contains("ubuntu") {
            return WSL_TINT;
        }
        self.projects
            .get(&project(pane))
            .copied()
            .unwrap_or_else(|| oklch(0.20, 0.03, 250.0))
    }
    pub fn window_tints(&self, window: &WindowInfo) -> Vec<PaneTint> {
        window
            .panes
            .iter()
            .map(|p| PaneTint {
                pane_id: p.id.clone(),
                x: p.x,
                y: p.y,
                width: p.width,
                height: p.height,
                color: self.resolve(p),
            })
            .collect()
    }
}
fn project(pane: &PaneInfo) -> String {
    let parts: Vec<_> = pane.cwd.iter().map(|p| p.to_string_lossy()).collect();
    if let Some(i) = parts
        .iter()
        .rposition(|p| p == "code" || p == "code-storage")
    {
        if let Some(name) = parts.get(i + 1) {
            if name == "research" {
                if let Some(child) = parts.get(i + 2) {
                    return format!("research/{child}");
                }
            }
            return name.to_string();
        }
    }
    pane.cwd.to_string_lossy().into_owned()
}
/// OKLCH to clamped sRGB, as in wallcore.py.
pub fn oklch(lightness: f32, chroma: f32, hue: f32) -> Rgb {
    let a = chroma * hue.to_radians().cos();
    let b = chroma * hue.to_radians().sin();
    let l = (lightness + 0.39633778 * a + 0.21580376 * b).powi(3);
    let m = (lightness - 0.105561346 * a - 0.06385417 * b).powi(3);
    let s = (lightness - 0.08948418 * a - 1.2914855 * b).powi(3);
    let channel = |v: f32| -> u8 {
        let v = if v <= 0.0031308 {
            12.92 * v
        } else {
            1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055
        };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    Rgb::new(
        channel(4.0767417 * l - 3.3077116 * m + 0.23096994 * s),
        channel(-1.268438 * l + 2.6097574 * m - 0.34131938 * s),
        channel(-0.0041960864 * l - 0.7034186 * m + 1.7076147 * s),
    )
}
