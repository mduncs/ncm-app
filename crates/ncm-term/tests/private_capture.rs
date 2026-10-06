use anyhow::{ensure, Result};
use ncm_model::Rgb;
use ncm_term::{compose_window_with_styles, LineParser};
use ncm_tmux::testing::PrivateServer;
use std::{
    collections::HashMap,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn three_private_panes_capture_colors_and_compose_real_borders() -> Result<()> {
    let server = PrivateServer::start(
        40,
        12,
        r"sleep 0.2; printf '\033[1;38;2;10;20;30mLEFT\033[0m'; sleep 60",
    )?;
    let first = server.tmux().snapshot()?[0].panes[0].id.clone();
    let right = server.split_pane(
        &first,
        true,
        r"sleep 0.2; printf '\033[38;5;196mTOP\033[0m'; sleep 60",
    )?;
    let bottom = server.split_pane(
        &right,
        false,
        r"sleep 0.2; printf '\033[32mBOTTOM\033[0m'; sleep 60",
    )?;
    // Configuration mutations are ONLY against this test's private socket.
    let socket = server.socket_path()?;
    for args in [
        vec!["set-option", "-w", "pane-border-style", "fg=#123456"],
        vec!["set-option", "-w", "pane-active-border-style", "fg=#abcdef"],
        vec![
            "set-option",
            "-p",
            "-t",
            first.as_str(),
            "window-style",
            "bg=#012456",
        ],
    ] {
        ensure!(Command::new("tmux")
            .env_remove("TMUX")
            .args(["-S", socket.to_str().unwrap()])
            .args(args)
            .status()?
            .success());
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let captures = loop {
        let captures = server
            .tmux()
            .capture(&[first.clone(), right.clone(), bottom.clone()])?;
        if captures[&first].iter().any(|l| l.contains("LEFT"))
            && captures[&right].iter().any(|l| l.contains("TOP"))
            && captures[&bottom].iter().any(|l| l.contains("BOTTOM"))
        {
            break captures;
        }
        ensure!(Instant::now() < deadline, "fixture output did not arrive");
        std::thread::sleep(Duration::from_millis(20));
    };
    let w = &server.tmux().snapshot()?[0];
    assert_eq!(w.panes.len(), 3);
    let mut panes = HashMap::new();
    for p in &w.panes {
        panes.insert(
            p.id.clone(),
            LineParser { tint: p.tint }.grid(&captures[&p.id], p.width, p.height)?,
        );
    }
    let styles = server.tmux().pane_border_styles(&w.id)?;
    assert_eq!(styles.normal.fg, Some(Rgb::new(0x12, 0x34, 0x56)));
    assert_eq!(styles.active.fg, Some(Rgb::new(0xab, 0xcd, 0xef)));
    let grid = compose_window_with_styles(w, &panes, &styles)?;
    let l = w.panes.iter().find(|p| p.id == first).unwrap();
    let r = w.panes.iter().find(|p| p.id == right).unwrap();
    let b = w.panes.iter().find(|p| p.id == bottom).unwrap();
    assert_eq!(grid.get(l.x, l.y).unwrap().text, "L");
    assert!(grid.get(l.x, l.y).unwrap().style.bold);
    assert_eq!(
        grid.get(l.x, l.y).unwrap().style.fg,
        Some(Rgb::new(10, 20, 30))
    );
    assert_eq!(
        grid.get(l.x, l.y).unwrap().style.bg,
        Some(Rgb::new(1, 36, 86))
    );
    assert_eq!(
        grid.get(r.x, r.y).unwrap().style.fg,
        Some(Rgb::new(255, 0, 0))
    );
    assert_eq!(grid.get(b.x, b.y).unwrap().text, "B");
    assert_eq!(
        grid.get(b.x, b.y).unwrap().style.fg,
        Some(Rgb::new(152, 195, 121))
    );
    assert_eq!(grid.get(r.x - 1, 0).unwrap().text, "│");
    assert_eq!(grid.get(r.x - 1, b.y - 1).unwrap().text, "├");
    assert_eq!(grid.get(r.x, b.y - 1).unwrap().text, "─");
    assert_eq!(grid.get(r.x - 1, 0).unwrap().style, styles.active);
    assert_eq!(grid.get(r.x, b.y - 1).unwrap().style, styles.normal);
    Ok(())
}
