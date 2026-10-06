use anyhow::Result;
use ncm_model::{
    AgentState, BorderStyles, Cell, CellStyle, Cursor, Grid, PaneInfo, Rgb, TabStyle, WindowFlags,
    WindowInfo,
};
use ncm_term::{changed_rows, compose_window, compose_window_with_styles, LineParser};
use std::collections::HashMap;

fn pane(id: &str, x: usize, y: usize, width: usize, height: usize, active: bool) -> PaneInfo {
    PaneInfo {
        id: id.into(),
        index: 0,
        x,
        y,
        width,
        height,
        active,
        pid: 0,
        cwd: "/synthetic".into(),
        title: String::new(),
        cursor: Cursor::default(),
        tint: Some(Rgb::new(1, 36, 86)),
        harness: None,
    }
}

fn window(panes: Vec<PaneInfo>, width: usize, height: usize) -> WindowInfo {
    WindowInfo {
        session: "synthetic".into(),
        index: 0,
        id: "@0".into(),
        name: "golden".into(),
        active: true,
        flags: WindowFlags::default(),
        activity: None,
        group: None,
        tab_style: TabStyle::default(),
        agent_state: AgentState::None,
        panes,
        width,
        height,
    }
}

fn text(grid: &Grid) -> Vec<String> {
    (0..grid.height())
        .map(|y| {
            grid.row(y)
                .unwrap()
                .iter()
                .map(|c| c.text.as_str())
                .collect()
        })
        .collect()
}

#[test]
fn synthetic_capture_golden_attributes_and_slots() {
    let tint = Rgb::new(1, 36, 86);
    let parser = LineParser { tint: Some(tint) };
    let base = CellStyle {
        bg: Some(tint),
        ..CellStyle::default()
    };
    let rich = CellStyle {
        fg: Some(Rgb::new(10, 20, 30)),
        bg: Some(Rgb::new(255, 0, 0)),
        bold: true,
        dim: true,
        italic: true,
        underline: true,
        reverse: true,
    };
    let actual = parser.parse(
        "\x1b[1;2;3;4;7;38;2;10;20;30;48;5;196m界e\u{301}\x1b[22;23;24;27;39;49m!",
        6,
    );
    assert_eq!(
        actual,
        vec![
            Cell {
                text: "界".into(),
                width: 2,
                style: rich
            },
            Cell::continuation(rich),
            Cell {
                text: "e\u{301}".into(),
                width: 1,
                style: rich
            },
            Cell {
                text: "!".into(),
                width: 1,
                style: base
            },
            Cell::blank(base),
            Cell::blank(base),
        ]
    );
    assert_eq!(
        parser.parse("\x1b[38:2::10:20:30;48:5:196mX", 1)[0].style,
        CellStyle {
            fg: rich.fg,
            bg: rich.bg,
            ..CellStyle::default()
        }
    );
    assert_eq!(parser.parse("\x1b[31;44mA\x1b[mB", 2)[1].style, base);
    assert_eq!(parser.parse("\x1b[4:2mA\x1b[4:0mB", 2)[1].style, base);
    assert_eq!(parser.parse("\x1b[38;2;1;2mX", 1)[0].style, base);
    assert_eq!(
        parser
            .parse("\x1b]0;ignored\x07a\x1b[2Kb", 2)
            .iter()
            .map(|c| c.text.as_str())
            .collect::<String>(),
        "ab"
    );
    assert_eq!(parser.parse("a界", 2)[1], Cell::blank(base));
    assert_eq!(parser.parse("界\u{301}", 2)[0].text, "界\u{301}");
    assert!(parser.parse("x", 0).is_empty());
}

#[test]
fn synthetic_capture_golden_palette() {
    let base = [
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
    for (i, &(r, g, b)) in base.iter().enumerate() {
        let fg = if i < 8 { 30 + i } else { 90 + i - 8 };
        let bg = if i < 8 { 40 + i } else { 100 + i - 8 };
        let cell = &LineParser::default().parse(&format!("\x1b[{fg};{bg}mX"), 1)[0];
        assert_eq!(
            (cell.style.fg, cell.style.bg),
            (Some(Rgb::new(r, g, b)), Some(Rgb::new(r, g, b)))
        );
    }
    for (n, rgb) in [
        (16, (0, 0, 0)),
        (21, (0, 0, 255)),
        (67, (95, 135, 175)),
        (231, (255, 255, 255)),
        (232, (8, 8, 8)),
        (255, (238, 238, 238)),
    ] {
        let cell = &LineParser::default().parse(&format!("\x1b[38;5;{n}mX"), 1)[0];
        assert_eq!(cell.style.fg, Some(Rgb::new(rgb.0, rgb.1, rgb.2)));
    }
}

#[test]
fn three_pane_composite_golden() -> Result<()> {
    let w = window(
        vec![
            pane("%0", 0, 0, 3, 5, true),
            pane("%1", 4, 0, 3, 2, false),
            pane("%2", 4, 3, 3, 2, false),
        ],
        7,
        5,
    );
    let captures: HashMap<_, _> = w
        .panes
        .iter()
        .map(|p| {
            let lines = vec![format!("{}", p.id.chars().last().unwrap()).repeat(p.width); p.height];
            Ok((
                p.id.clone(),
                LineParser { tint: p.tint }.grid(&lines, p.width, p.height)?,
            ))
        })
        .collect::<Result<_>>()?;
    let styles = BorderStyles {
        normal: CellStyle {
            fg: Some(Rgb::new(20, 30, 40)),
            ..CellStyle::default()
        },
        active: CellStyle {
            fg: Some(Rgb::new(50, 60, 70)),
            ..CellStyle::default()
        },
    };
    let grid = compose_window_with_styles(&w, &captures, &styles)?;
    assert_eq!(
        text(&grid),
        ["000│111", "000│111", "000├───", "000│222", "000│222"]
    );
    assert_eq!(grid.get(3, 2).unwrap().style, styles.active);
    assert_eq!(grid.get(5, 2).unwrap().style, styles.normal);
    let fallback = compose_window(&w, &HashMap::new())?;
    assert_eq!(
        fallback.get(3, 2).unwrap().style.fg,
        Some(Rgb::new(60, 64, 74))
    );
    assert_eq!(fallback.get(0, 0).unwrap().style.bg, w.panes[0].tint);
    assert!(changed_rows(Some(&grid), &grid).is_empty());
    let mut changed = grid.clone();
    changed.get_mut(0, 3).unwrap().text = "!".into();
    assert_eq!(changed_rows(Some(&grid), &changed), [3]);
    Ok(())
}

#[test]
fn cross_and_horizontal_border_goldens() -> Result<()> {
    let cross = window(
        vec![
            pane("%0", 0, 0, 2, 2, true),
            pane("%1", 3, 0, 2, 2, false),
            pane("%2", 0, 3, 2, 2, false),
            pane("%3", 3, 3, 2, 2, false),
        ],
        5,
        5,
    );
    assert_eq!(
        text(&compose_window(&cross, &HashMap::new())?),
        ["  │  ", "  │  ", "──┼──", "  │  ", "  │  "]
    );
    let split = window(
        vec![pane("%0", 0, 0, 5, 1, true), pane("%1", 0, 2, 5, 1, false)],
        5,
        3,
    );
    assert_eq!(
        text(&compose_window(&split, &HashMap::new())?),
        ["     ", "─────", "     "]
    );
    Ok(())
}

#[test]
fn zoomed_active_pane_is_not_overwritten_by_hidden_panes() -> Result<()> {
    let mut w = window(
        vec![pane("%0", 0, 0, 3, 2, true), pane("%1", 2, 0, 1, 2, false)],
        3,
        2,
    );
    w.flags.raw = "Z".into();
    let panes = HashMap::from([(
        "%0".into(),
        LineParser::default().grid(&["AAA".into(), "BBB".into()], 3, 2)?,
    )]);
    assert_eq!(text(&compose_window(&w, &panes)?), ["AAA", "BBB"]);
    Ok(())
}
