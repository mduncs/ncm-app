//! File-only visual/timing harness. Live tmux access is snapshot/capture only.
use anyhow::{bail, Context, Result};
use ncm_model::{
    AgentState, Cell, CellStyle, Cursor, Grid, PaneInfo, Rgb, WindowFlags, WindowInfo,
};
use ncm_render::{
    Overlays, RgbaImage, ThumbnailRenderer, ThumbnailSize, TintResolver, PAGE_BACKGROUND,
};
use ncm_term::{compose_window, LineParser};
use ncm_tmux::Tmux;
use std::{collections::HashMap, path::Path, time::Instant};

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help") {
        println!("cards [--fixtures-only | --live-only] [--session NAME]\nWrites target/render-out/{{fixture,live}}-grid.png and per-card PNGs; never mutates tmux.");
        return Ok(());
    }
    let session = args
        .iter()
        .position(|a| a == "--session")
        .map(|i| args.get(i + 1).context("--session needs a name"))
        .transpose()?;
    for arg in &args {
        if !["--fixtures-only", "--live-only", "--session"].contains(&arg.as_str())
            && Some(arg) != session
        {
            bail!("unknown argument {arg}");
        }
    }
    std::fs::create_dir_all("target/render-out")?;
    let mut renderer = ThumbnailRenderer::default();
    if !args.iter().any(|a| a == "--live-only") {
        let (windows, grids) = fixtures()?;
        render_set("fixture", &windows, &grids, &mut renderer)?;
    }
    if !args.iter().any(|a| a == "--fixtures-only") {
        renderer.clear_cards();
        let live = Tmux::live();
        let windows = live.snapshot()?;
        let default_session = std::env::var_os("TMUX")
            .and_then(|_| {
                std::process::Command::new("tmux")
                    .args(["display-message", "-p", "#{client_session}"])
                    .output()
                    .ok()
            })
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "main".into());
        let session = session.map(String::as_str).unwrap_or(&default_session);
        let own = std::env::var("TMUX_PANE").ok();
        let windows: Vec<_> = windows
            .into_iter()
            .filter(|w| w.session == session)
            .filter(|w| !w.panes.iter().any(|p| Some(&p.id) == own.as_ref()))
            .collect();
        if windows.is_empty() {
            bail!("no live windows in session {session}");
        }
        let captures = live.capture(
            &windows
                .iter()
                .flat_map(|w| w.panes.iter().map(|p| p.id.clone()))
                .collect::<Vec<_>>(),
        )?;
        let mut grids = Vec::new();
        for window in &windows {
            let mut panes = HashMap::new();
            for pane in &window.panes {
                let lines = captures.get(&pane.id).context("captured pane missing")?;
                panes.insert(
                    pane.id.clone(),
                    LineParser { tint: pane.tint }.grid(lines, pane.width, pane.height)?,
                );
            }
            grids.push(compose_window(window, &panes)?);
        }
        render_set("live", &windows, &grids, &mut renderer)?;
    }
    if let Some(font) = renderer.resolved_font() {
        println!("Font: {} ({})", font.family, font.path.display());
    }
    Ok(())
}
fn render_set(
    name: &str,
    windows: &[WindowInfo],
    grids: &[Grid],
    renderer: &mut ThumbnailRenderer,
) -> Result<()> {
    let n = windows.len();
    let cols = (1..=n)
        .max_by(|&a, &b| score(a, windows).total_cmp(&score(b, windows)))
        .context("empty render set")?;
    let rows = n.div_ceil(cols);
    println!(
        "{name}: {n} cards, {cols}×{rows}, 2560×1440; release={}",
        !cfg!(debug_assertions)
    );
    let tints = TintResolver::new(windows);
    let mut page = RgbaImage {
        width: 2560,
        height: 1440,
        pixels: PAGE_BACKGROUND.rgba(255).repeat(2560 * 1440),
    };
    let mut full_times = Vec::new();
    let mut changed_times = Vec::new();
    for (i, (window, grid)) in windows.iter().zip(grids).enumerate() {
        let x = (i % cols) * 2560 / cols;
        let right = (i % cols + 1) * 2560 / cols;
        let y = (i / cols) * 1410 / rows;
        let bottom = (i / cols + 1) * 1410 / rows;
        let size = ThumbnailSize {
            width: (right - x) as u32,
            height: (bottom - y) as u32,
            cell_width: 8.0,
            cell_height: 16.0,
        };
        let tint = tints.window_tints(window);
        let overlays = Overlays {
            state: window.agent_state,
            selected: i == 0,
            ..Overlays::default()
        };
        let start = Instant::now();
        let image = renderer.render_card(&window.id, grid, &tint, &overlays, size)?;
        let full_ms = start.elapsed().as_secs_f64() * 1000.0;
        full_times.push(full_ms);
        paste(&mut page, image, x, y);
        image.write_png(format!("target/render-out/{name}-card-{i:02}.png"))?;
        let mut changed = grid.clone();
        let mut times = Vec::new();
        for sample in 0..100 {
            let row = changed.height() - 1;
            let x = changed.width() / 2;
            let slot = changed.get_mut(x, row).context("benchmark cell")?;
            // Local synthetic mutation: no send-keys, files or live pane changes.
            *slot = Cell {
                text: if sample % 2 == 0 {
                    "#".into()
                } else {
                    "*".into()
                },
                width: 1,
                style: CellStyle {
                    fg: Some(Rgb::new(229, 192, 123)),
                    ..CellStyle::default()
                },
            };
            let start = Instant::now();
            renderer.render_card(&window.id, &changed, &tint, &overlays, size)?;
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        times.sort_by(f64::total_cmp);
        let p50 = times[49];
        let p95 = times[94];
        changed_times.extend(&times);
        println!("{name} card {i:02} {}: full {full_ms:.3} ms; changed-row p50 {p50:.3} ms, p95 {p95:.3} ms (100 samples)",window.id);
        // The PNG page already owns these pixels; retain no completed benchmark cards.
        renderer.remove_card(&window.id);
    }
    let path = format!("target/render-out/{name}-grid.png");
    page.write_png(Path::new(&path))?;
    full_times.sort_by(f64::total_cmp);
    changed_times.sort_by(f64::total_cmp);
    println!("{name} aggregate: full p50 {:.3} ms, max {:.3} ms; changed-row p50 {:.3} ms, p95 {:.3} ms, max {:.3} ms; {path}",full_times[n/2],full_times[n-1],changed_times[changed_times.len()/2],changed_times[changed_times.len()*95/100],changed_times[changed_times.len()-1]);
    Ok(())
}
fn score(cols: usize, windows: &[WindowInfo]) -> f64 {
    let w = 2560.0 / cols as f64 - 8.0;
    let h = 1410.0 / windows.len().div_ceil(cols) as f64 - 8.0;
    windows
        .iter()
        .map(|win| (w / (win.width as f64 * 8.0)).min(h / (win.height as f64 * 16.0)))
        .fold(f64::INFINITY, f64::min)
}
fn paste(page: &mut RgbaImage, card: &RgbaImage, x: usize, y: usize) {
    for row in 0..card.height as usize {
        let dst = ((y + row) * page.width as usize + x) * 4;
        let src = row * card.width as usize * 4;
        page.pixels[dst..dst + card.width as usize * 4]
            .copy_from_slice(&card.pixels[src..src + card.width as usize * 4]);
    }
}
fn fixtures() -> Result<(Vec<WindowInfo>, Vec<Grid>)> {
    let mut windows = Vec::new();
    let mut grids = Vec::new();
    for i in 0..21 {
        let split = i % 3 == 0;
        let mut panes = Vec::new();
        let mut pane_grids = HashMap::new();
        for p in 0..if split { 2 } else { 1 } {
            let width = if split { 127 } else { 255 };
            let pane = PaneInfo {
                id: format!("%fixture-{i}-{p}"),
                index: p as u32,
                x: p * 128,
                y: 0,
                width,
                height: 60,
                active: p == 0,
                pid: 0,
                cwd: format!("/fixture/code/project-{i}").into(),
                title: match i {
                    5 | 13 => "PowerShell",
                    9 => "WSL Ubuntu",
                    _ => "fixture",
                }
                .into(),
                cursor: Cursor::default(),
                tint: if i % 4 == 0 {
                    Some(ncm_render::oklch(0.285, 0.045, i as f32 * 37.0))
                } else {
                    None
                },
                harness: None,
            };
            let mut lines = Vec::new();
            for row in 0..60 {
                let line = match row%8 {
                    0=>format!("\x1b[38;2;217;119;87m● fixture project {i}, pane {p}: incremental renderer"),
                    1=>format!("\x1b[1;38;2;229;192;123mBash\x1b[0m cargo test --workspace  # synthetic line {row}"),
                    2=>"  per-cell colors • glyph atlas • changed rows • bottom crop".into(),
                    3=>"\x1b[38;2;126;231;135m✓ all checks passed\x1b[0m;  no clients attached".into(),
                    4=>"\x1b[7m reverse \x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m e\u{301} 界".into(),
                    5=>"\x1b[48;2;32;46;62m explicit SGR background \x1b[0m".into(),
                    6=>format!("  {:03} src/render.rs  ................................",row+i),
                    _=>String::new(),
                };
                lines.push(line);
            }
            pane_grids.insert(
                pane.id.clone(),
                LineParser::default().grid(&lines, width, 60)?,
            );
            panes.push(pane);
        }
        let window = WindowInfo {
            session: "fixture".into(),
            index: i as u32,
            id: format!("@fixture-{i}"),
            name: format!("project-{i}"),
            active: i == 0,
            flags: WindowFlags::default(),
            activity: None,
            group: None,
            tab_style: Default::default(),
            agent_state: match i {
                0 => AgentState::Needs,
                1 | 8 => AgentState::Working,
                13 => AgentState::Done,
                _ => AgentState::None,
            },
            panes,
            width: 255,
            height: 60,
        };
        grids.push(compose_window(&window, &pane_grids)?);
        windows.push(window);
    }
    Ok((windows, grids))
}
