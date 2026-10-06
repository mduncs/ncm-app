use ncm_model::*;
use std::collections::HashMap;
pub const NOW: Timestamp = Timestamp(1_800_000_000_000);
pub fn windows() -> Vec<WindowInfo> {
    [
        "main",
        "terminal",
        "workbench",
        "research",
        "reports",
        "personal",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, name)| {
        let tint = [
            Rgb::new(48, 24, 28),
            Rgb::new(12, 44, 40),
            Rgb::new(1, 36, 86),
            Rgb::new(39, 30, 53),
            Rgb::new(26, 44, 31),
            Rgb::new(35, 38, 44),
        ][i];
        let panes = (0..if i == 0 { 2 } else { 1 })
            .map(|j| PaneInfo {
                id: format!("%{}", i * 2 + j + 1),
                index: j as u32,
                x: j * 61,
                y: 0,
                width: if i == 0 { 60 } else { 121 },
                height: 32,
                active: j == 0,
                pid: 0,
                cwd: "/demo/project".into(),
                title: if i == 2 {
                    "powershell".into()
                } else {
                    name.into()
                },
                cursor: Cursor { x: 2, y: 29 },
                tint: Some(tint),
                harness: if i == 1 {
                    None
                } else {
                    Some(Harness {
                        name: if i % 2 == 0 {
                            "claude".into()
                        } else {
                            "codex".into()
                        },
                        color: if i % 2 == 0 {
                            Rgb::new(217, 119, 87)
                        } else {
                            Rgb::new(16, 163, 127)
                        },
                    })
                },
            })
            .collect();
        let state = match i {
            0 => AgentState::Needs,
            1 | 3 => AgentState::Working,
            4 => AgentState::Done,
            _ => AgentState::None,
        };
        WindowInfo {
            session: "demo".into(),
            index: i as u32 + 1,
            id: format!("@{}", i + 1),
            name: name.into(),
            active: i == 0,
            flags: WindowFlags::default(),
            activity: None,
            group: Some(if i < 3 { "build" } else { "explore" }.into()),
            tab_style: TabStyle {
                fg: Some(Rgb::new(24, 27, 31)),
                bg: Some(state.color().unwrap_or(Rgb::new(255, 241, 118))),
                bold: true,
                marker: match state {
                    AgentState::Needs => "?",
                    AgentState::Done => "✓",
                    _ => "+",
                }
                .into(),
            },
            agent_state: state,
            panes,
            width: 121,
            height: 32,
        }
    })
    .collect()
}
pub fn captures(windows: &[WindowInfo]) -> HashMap<String, Vec<String>> {
    windows
        .iter()
        .flat_map(|w| {
            w.panes.iter().map(move |p| {
                let mut lines = (0..32)
                    .map(|y| {
                        format!(
                            "\x1b[38;2;130;170;190m{:02}\x1b[0m  {} / {}   processing row {}",
                            y + 1,
                            w.name,
                            p.id,
                            y + 1
                        )
                    })
                    .collect::<Vec<_>>();
                lines[5] =
                    "\x1b[1;38;2;126;231;135m✓ Compilation finished successfully\x1b[0m".into();
                lines[16] =
                    "Real cells: 界 e\u{301} · ANSI colors · all panes at native layout".into();
                lines[28] = "\x1b[38;2;229;192;123mBash\x1b[0m cargo test --workspace".into();
                lines[29] = "Ready. Space Quick Look · 2 Talk · 3 Reports".into();
                lines[30] = String::new();
                lines[31] = String::new();
                (p.id.clone(), lines)
            })
        })
        .collect()
}
pub fn events(window: usize) -> Vec<TalkEvent> {
    let stamp = |seconds: i64| Some(Timestamp(NOW.0 - seconds * 1000));
    let mut events=vec![
        TalkEvent::You{timestamp:stamp(840),body:"Please review the terminal layout and keep the grid visible during Quick Look.".into()},
        TalkEvent::Reply{timestamp:stamp(780),body:"I checked the geometry, cell colors, and pane borders. The cards tile the area above the dock.\n\nThe Screen panel keeps native terminal cells and anchors recent output at the bottom.\n\nI will run the private-server checks next.".into()},
        TalkEvent::Report{timestamp:stamp(360),agent:"layout-review".into(),body:"Verified six cards at 80×24 and 160×50. No dock overlap; every pane stays at its actual coordinates.".into()},
        TalkEvent::ReplyToReport{timestamp:stamp(330),agent:Some("layout-review".into()),body:"Thanks. The remainder columns are now distributed without gaps.".into()},
        TalkEvent::Report{timestamp:stamp(260),agent:"input-check".into(),body:"Escape never quits. Typing forwards literal text and named special keys. Ctrl-] returns to Quick Look.".into()},
        TalkEvent::You{timestamp:stamp(180),body:"What remains before the end-to-end check?".into()},
    ];
    if window != 0 {
        events.push(TalkEvent::Reply{timestamp:stamp(120),body:"The private-server check is complete. The remaining work is integration with the live transcript adapters.".into()});
    }
    events
}
pub fn facts() -> Facts {
    Facts {
        now: NowLine {
            kind: NowKind::Tool,
            text: "Bash cargo test --workspace".into(),
            tool: Some("Bash".into()),
        },
        model: Some("Opus 5.5".into()),
        context_tokens: Some(141_400),
        subagents: vec![
            Subagent {
                name: "layout-review".into(),
                state: SubagentState::Busy,
                running_time: Some(std::time::Duration::from_secs(97)),
            },
            Subagent {
                name: "input-check".into(),
                state: SubagentState::Idle,
                running_time: None,
            },
        ],
        hidden_count: 1,
    }
}
