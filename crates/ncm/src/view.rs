use crate::{
    input::{Prompt, State, Tab},
    layout::{Layout, Rect},
};
use ncm_model::*;
use ncm_talk::{TalkBlock, TalkView};
use std::collections::HashMap;
use unicode_width::UnicodeWidthChar;
pub const BACK: Rgb = Rgb::new(10, 11, 14);
const INK: Rgb = Rgb::new(226, 230, 238);
const QUIET: Rgb = Rgb::new(150, 156, 168);
const PINK: Rgb = Rgb::new(255, 120, 170);
const PURPLE: Rgb = Rgb::new(186, 162, 245);
fn style(fg: Rgb, bg: Rgb, bold: bool) -> CellStyle {
    CellStyle {
        fg: Some(fg),
        bg: Some(bg),
        bold,
        ..Default::default()
    }
}
pub fn age(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3600)
    }
}
/// Write untrusted labels as printable text; clipped wide glyphs never leave half a character.
pub fn text(g: &mut Grid, x: usize, y: usize, width: usize, s: &str, st: CellStyle) -> usize {
    let mut used = 0;
    for c in s.chars().filter(|c| !c.is_control()) {
        let w = c.width().unwrap_or(0);
        if w == 0 {
            if used > 0 {
                let leader = if g.get(x + used - 1, y).is_some_and(|cell| cell.width == 0) {
                    used.saturating_sub(2)
                } else {
                    used - 1
                };
                if let Some(cell) = g.get_mut(x + leader, y) {
                    cell.text.push(c);
                }
            }
            continue;
        }
        if used + w > width {
            break;
        }
        if let Some(cell) = g.get_mut(x + used, y) {
            *cell = Cell {
                text: c.to_string(),
                width: w as u8,
                style: st,
            };
        }
        if w == 2 {
            if let Some(cell) = g.get_mut(x + used + 1, y) {
                *cell = Cell::continuation(st);
            }
        }
        used += w;
    }
    used
}
fn fill(g: &mut Grid, r: Rect, color: Rgb) {
    for y in r.y..r.y + r.height {
        for x in r.x..r.x + r.width {
            if let Some(cell) = g.get_mut(x, y) {
                *cell = Cell::blank(style(INK, color, false));
            }
        }
    }
}
pub fn blit(dst: &mut Grid, src: &Grid, r: Rect, top: usize) {
    for y in 0..r.height {
        for x in 0..r.width {
            if let Some(cell) = src.get(x, y + top) {
                let cell = if (cell.width == 2 && x + 1 == r.width) || (cell.width == 0 && x == 0) {
                    Cell::blank(cell.style)
                } else {
                    cell.clone()
                };
                if let Some(slot) = dst.get_mut(r.x + x, r.y + y) {
                    *slot = cell;
                }
            }
        }
    }
}
fn title(g: &mut Grid, r: Rect, w: &WindowInfo) -> usize {
    text(
        g,
        r.x,
        r.y,
        r.width,
        &format!(" {}{} {} ", w.index, w.tab_style.marker, w.name),
        style(
            w.tab_style.fg.unwrap_or(INK),
            w.tab_style.bg.unwrap_or(Rgb::new(52, 56, 64)),
            w.tab_style.bold,
        ),
    )
}
pub fn words(w: &WindowInfo) -> Vec<(String, Rgb)> {
    let mut words = Vec::new();
    for p in &w.panes {
        if let Some(h) = &p.harness {
            if !words.iter().any(|(n, _)| n == &h.name) {
                words.push((h.name.to_lowercase(), h.color));
            }
        }
        let label = p.title.to_lowercase();
        let env = if label.contains("powershell") || label.contains("pwsh") {
            Some(("powershell", Rgb::new(110, 170, 240)))
        } else if label.contains("wsl") || label.contains("ubuntu") {
            Some(("wsl", Rgb::new(240, 130, 90)))
        } else {
            None
        };
        if let Some((name, color)) = env {
            if !words.iter().any(|(n, _)| n == name) {
                words.push((name.into(), color));
            }
        }
    }
    words
}
#[derive(Clone)]
pub struct CardData {
    pub facts: Facts,
    pub talk: std::sync::Arc<TalkView>,
}
pub fn window_edge(w: &WindowInfo) -> Option<Rgb> {
    if w.agent_state == AgentState::Needs {
        w.agent_state.color()
    } else if w.flags.bell {
        Some(Rgb::new(215, 38, 61))
    } else if w.agent_state != AgentState::None {
        w.agent_state.color()
    } else if w.flags.activity {
        Some(Rgb::new(255, 241, 118))
    } else if w.flags.silence {
        Some(QUIET)
    } else {
        None
    }
}
pub fn card_border(g: &mut Grid, r: Rect, w: &WindowInfo, selected: bool) {
    if r.width == 0 || r.height == 0 {
        return;
    }
    let color = if selected {
        Rgb::new(240, 243, 248)
    } else {
        window_edge(w).unwrap_or(Rgb::new(40, 43, 51))
    };
    for y in r.y..r.y + r.height {
        text(g, r.x, y, 1, "│", style(color, BACK, false));
        text(g, r.x + r.width - 1, y, 1, "│", style(color, BACK, false));
    }
}
pub fn card_overlay(g: &mut Grid, r: Rect, w: &WindowInfo, data: &CardData, now: Timestamp) {
    if r.height == 0 || r.width == 0 {
        return;
    }
    // Only overlay rows enter this card-sized scratch grid. Even a future label bug cannot
    // address the neighboring card's cells, including wide-character continuation cells.
    let height = r.height.min(3);
    let rows: Vec<_> = (0..height)
        .map(|y| {
            if y == 0 {
                r.y
            } else {
                r.y + r.height - height + y
            }
        })
        .collect();
    let mut local = frame(r.width, height);
    for (y, source_y) in rows.iter().enumerate() {
        for x in 0..r.width {
            if let (Some(source), Some(target)) = (g.get(r.x + x, *source_y), local.get_mut(x, y)) {
                *target = source.clone();
            }
        }
    }
    card_overlay_rows(
        &mut local,
        Rect {
            x: 0,
            y: 0,
            width: r.width,
            height,
        },
        w,
        data,
        now,
    );
    for (y, target_y) in rows.iter().enumerate() {
        blit(
            g,
            &local,
            Rect {
                x: r.x,
                y: *target_y,
                width: r.width,
                height: 1,
            },
            y,
        );
    }
}

fn card_overlay_rows(g: &mut Grid, r: Rect, w: &WindowInfo, data: &CardData, now: Timestamp) {
    if r.height == 0 || r.width == 0 {
        return;
    }
    // Clear the gutter on every overlay row before narrowing the text rectangle.
    for y in 0..r.height {
        fill(
            g,
            Rect {
                x: r.x + r.width - 1,
                y: r.y + y,
                width: 1,
                height: 1,
            },
            BACK,
        );
    }
    let r = Rect {
        width: r.width.saturating_sub(1),
        ..r
    };
    if r.width == 0 {
        return;
    }
    let strip = match w.group.as_deref() {
        Some("pinned") => ncm_render::oklch(0.17, 0.02, 250.0),
        Some("build") => ncm_render::oklch(0.17, 0.02, 150.0),
        Some("explore") => ncm_render::oklch(0.17, 0.02, 300.0),
        Some("personal") => ncm_render::oklch(0.17, 0.02, 70.0),
        _ => BACK,
    };
    fill(g, Rect { height: 1, ..r }, strip);
    let age_text = w
        .activity
        .map(|a| format!("■ {}", age(a.age_at(now).as_secs())))
        .unwrap_or_else(|| "■ —".into());
    let age_width = age_text.chars().count().min(r.width);
    let age_color = w
        .tab_style
        .bg
        .filter(|c| u16::from(c.r) + u16::from(c.g) + u16::from(c.b) > 200)
        .unwrap_or(QUIET);
    let room = r.width.saturating_sub(age_width + 1);
    let mut x = title(g, Rect { width: room, ..r }, w);
    for (word, color) in words(w) {
        x += text(
            g,
            r.x + x,
            r.y,
            room.saturating_sub(x),
            &format!(" {word}"),
            style(color, strip, false),
        );
    }
    if let Some(u) = data.talk.unanswered.as_ref().filter(|u| {
        u.timestamp
            .is_some_and(|t| t.age_at(now) > std::time::Duration::from_secs(20))
    }) {
        text(
            g,
            r.x + x,
            r.y,
            room.saturating_sub(x),
            &format!(
                " unanswered {} ",
                age(u.timestamp.unwrap().age_at(now).as_secs())
            ),
            style(Rgb::new(255, 255, 255), Rgb::new(255, 46, 136), true),
        );
    }
    text(
        g,
        r.x + r.width - age_width,
        r.y,
        age_width,
        &age_text,
        style(age_color, strip, false),
    );
    if r.height >= 2 {
        let y = r.y + r.height - 2;
        fill(
            g,
            Rect {
                x: r.x,
                y,
                width: r.width,
                height: 1,
            },
            BACK,
        );
        let f = &data.facts;
        let mut used;
        let color = match f.now.kind {
            NowKind::Tool => Rgb::new(229, 192, 123),
            NowKind::Spinner => Rgb::new(130, 205, 245),
            NowKind::Text => INK,
        };
        if let Some(tool) = f.now.tool.as_ref().filter(|_| f.now.kind == NowKind::Tool) {
            used = text(g, r.x, y, r.width, tool, style(color, BACK, true));
            text(
                g,
                r.x + used,
                y,
                r.width.saturating_sub(used),
                f.now.text.strip_prefix(tool).unwrap_or(&f.now.text),
                style(INK, BACK, false),
            );
        } else {
            text(g, r.x, y, r.width, &f.now.text, style(color, BACK, false));
        }
        if w.panes.iter().any(|p| p.harness.is_some()) {
            let y = r.y + r.height - 1;
            fill(
                g,
                Rect {
                    x: r.x,
                    y,
                    width: r.width,
                    height: 1,
                },
                BACK,
            );
            if !f.subagents.is_empty() || f.hidden_count > 0 {
                used = text(
                    g,
                    r.x,
                    y,
                    r.width,
                    &format!("⤷ {}", f.subagents.len() + f.hidden_count),
                    style(PURPLE, BACK, true),
                );
                let mut agents = f.subagents.iter().collect::<Vec<_>>();
                agents.sort_by_key(|a| a.state == SubagentState::Idle);
                for a in agents {
                    let busy = a.state == SubagentState::Busy;
                    let label = format!(
                        "  {}{}",
                        a.name,
                        if busy {
                            a.running_time
                                .map(|d| format!(" {}", age(d.as_secs())))
                                .unwrap_or_default()
                        } else {
                            String::new()
                        }
                    );
                    used += text(
                        g,
                        r.x + used,
                        y,
                        r.width.saturating_sub(used),
                        &label,
                        style(if busy { INK } else { QUIET }, BACK, busy),
                    );
                }
                if f.hidden_count > 0 {
                    text(
                        g,
                        r.x + used,
                        y,
                        r.width.saturating_sub(used),
                        &format!(" +{}", f.hidden_count),
                        style(QUIET, BACK, false),
                    );
                }
            }
        }
    }
}
pub fn dock(
    g: &mut Grid,
    windows: &[WindowInfo],
    state: &State,
    reason: &str,
    error: &str,
    dock_rows: usize,
) {
    if g.height() == 0 {
        return;
    }
    let y = g.height() - 1;
    let width = g.width();
    let dock_y = g.height().saturating_sub(dock_rows);
    let dock_height = dock_rows.min(g.height());
    fill(
        g,
        Rect {
            x: 0,
            y: dock_y,
            width,
            height: dock_height,
        },
        Rgb::new(20, 21, 25),
    );
    let prompt = match &state.prompt {
        Prompt::Close(_) => Some("Close selected window? y/n".to_owned()),
        Prompt::Rename(_, name) => Some(format!("Rename: {name} (Enter save · Esc cancel)")),
        Prompt::None | Prompt::Menu(_) => None,
    };
    if let Some(prompt) = prompt {
        text(g, 0, y, width, &prompt, style(PINK, BACK, true));
        return;
    }
    if !error.is_empty() {
        text(g, 0, y, width, error, style(PINK, BACK, false));
        return;
    }
    let mut x = 0;
    for (st, label) in [
        (AgentState::Needs, "need you"),
        (AgentState::Working, "working"),
        (AgentState::Done, "done"),
    ] {
        let count = windows.iter().filter(|w| w.agent_state == st).count();
        x += text(
            g,
            x,
            y,
            width.saturating_sub(x),
            &format!("■ {count} {label}  "),
            style(st.color().unwrap(), Rgb::new(20, 21, 25), false),
        );
    }
    let hint = if reason.is_empty() {
        "←↑↓→ move · space preview · tab alert · enter go · q quit"
    } else {
        reason
    };
    let len = hint.chars().count();
    let start = width.saturating_sub(len).max(x);
    text(
        g,
        start,
        y,
        width.saturating_sub(start),
        hint,
        style(QUIET, Rgb::new(20, 21, 25), false),
    );
}
#[derive(Clone)]
struct Line {
    text: String,
    color: Rgb,
    bold: bool,
    inline: Vec<InlineStyle>,
}
fn line(s: impl Into<String>, color: Rgb, bold: bool) -> Line {
    Line {
        text: s.into(),
        color,
        bold,
        inline: Vec::new(),
    }
}
#[derive(Clone, Copy, Default)]
struct InlineStyle {
    bold: bool,
    italic: bool,
    code: bool,
}

type RichText = Vec<(char, InlineStyle)>;

fn plain(raw: &str) -> RichText {
    raw.chars()
        .filter(|ch| !ch.is_control())
        .map(|ch| (ch, InlineStyle::default()))
        .collect()
}

// Parse paired inline markers before measuring/wrapping, so styles survive line breaks.
// Code is opaque; underscores inside identifiers and unmatched markers remain literal.
fn inline(raw: &str, inherited: InlineStyle) -> RichText {
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < raw.len() {
        let rest = &raw[offset..];
        let ch = rest.chars().next().unwrap();
        if ch == '\\' {
            if let Some(next) = rest[1..].chars().next().filter(|c| "*_`\\".contains(*c)) {
                out.push((next, inherited));
                offset += 1 + next.len_utf8();
                continue;
            }
        }
        let marker = if ch == '`' {
            &rest[..rest.bytes().take_while(|b| *b == b'`').count()]
        } else if rest.starts_with("**") {
            "**"
        } else if ch == '*' || ch == '_' {
            &rest[..1]
        } else {
            ""
        };
        let word_underscore = ch == '_'
            && raw[..offset]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric);
        if !marker.is_empty() && !word_underscore {
            let tail = &rest[marker.len()..];
            let closing = tail.match_indices(marker).find(|(end, _)| {
                *end > 0
                    && !tail[..*end].ends_with('\\')
                    && (ch == '`'
                        || (!tail.starts_with(char::is_whitespace)
                            && !tail[..*end].ends_with(char::is_whitespace)))
                    && (ch != '_'
                        || !tail[*end + marker.len()..].starts_with(char::is_alphanumeric))
            });
            if let Some((end, _)) = closing {
                let mut st = inherited;
                if ch == '`' {
                    st.code = true;
                    out.extend(plain(&tail[..end]).into_iter().map(|(ch, _)| (ch, st)));
                } else {
                    if marker == "**" {
                        st.bold = true;
                    } else {
                        st.italic = true;
                    }
                    out.extend(inline(&tail[..end], st));
                }
                offset += marker.len() * 2 + end;
                continue;
            }
        }
        if !ch.is_control() {
            out.push((ch, inherited));
        }
        offset += ch.len_utf8();
    }
    out
}

fn rich_width(row: &RichText) -> usize {
    row.iter().map(|(ch, _)| ch.width().unwrap_or(0)).sum()
}

#[cfg(test)]
fn wrap(body: &str, width: usize) -> Vec<String> {
    wrap_rich(body, width)
        .into_iter()
        .map(|row| row.into_iter().map(|(ch, _)| ch).collect())
        .collect()
}

fn wrap_rich(body: &str, width: usize) -> Vec<RichText> {
    if width == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let raw_lines: Vec<_> = body.lines().collect();
    let mut index = 0;
    let mut fence: Option<(char, usize)> = None;
    while index < raw_lines.len() {
        let raw = raw_lines[index];
        let trimmed = raw.trim_start();
        let fence_marker = trimmed.chars().next().filter(|ch| matches!(ch, '`' | '~'));
        let run = fence_marker.map_or(0, |ch| trimmed.chars().take_while(|c| *c == ch).count());
        let fence_line = run >= 3;
        let literal = fence.is_some() || fence_line;
        if fence_line {
            match fence {
                None => fence = Some((fence_marker.unwrap(), run)),
                Some((ch, length))
                    if Some(ch) == fence_marker
                        && run >= length
                        && trimmed[run..].trim().is_empty() =>
                {
                    fence = None
                }
                _ => {}
            }
        }
        if !literal && index + 1 < raw_lines.len() {
            if let Some(header) = pipe_cells(raw) {
                if pipe_cells(raw_lines[index + 1]).is_some_and(|cells| {
                    cells.len() == header.len()
                        && cells.iter().all(|cell| {
                            let cell = cell.trim_matches(':');
                            cell.len() >= 3 && cell.chars().all(|ch| ch == '-')
                        })
                }) {
                    let mut rows = vec![header];
                    index += 2;
                    while index < raw_lines.len() {
                        let Some(cells) = pipe_cells(raw_lines[index])
                            .filter(|cells| cells.len() == rows[0].len())
                        else {
                            break;
                        };
                        rows.push(cells);
                        index += 1;
                    }
                    out.extend(table_lines(&rows, width));
                    continue;
                }
            }
        }
        let mut row = Vec::new();
        let mut used = 0;
        let chars = if literal {
            plain(raw)
        } else {
            inline(raw, InlineStyle::default())
        };
        for (ch, st) in chars {
            let w = ch.width().unwrap_or(0);
            if used + w > width && !row.is_empty() {
                out.push(std::mem::take(&mut row));
                used = 0;
            }
            if w > width {
                continue;
            }
            row.push((ch, st));
            used += w;
        }
        out.push(row);
        index += 1;
    }
    out
}

fn pipe_cells(raw: &str) -> Option<Vec<String>> {
    let raw = raw.trim();
    if !raw.contains('|') {
        return None;
    }
    let raw = raw.strip_prefix('|').unwrap_or(raw);
    let raw = raw.strip_suffix('|').unwrap_or(raw);
    Some(
        raw.split('|')
            .map(|cell| cell.trim().chars().filter(|ch| !ch.is_control()).collect())
            .collect(),
    )
}

fn table_lines(rows: &[Vec<String>], width: usize) -> Vec<RichText> {
    let rows: Vec<Vec<RichText>> = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|cell| inline(cell, InlineStyle::default()))
                .collect()
        })
        .collect();
    let columns = rows[0].len();
    let gap = if width >= columns.saturating_mul(3).saturating_sub(2) {
        2
    } else {
        0
    };
    let available = width.saturating_sub(gap * columns.saturating_sub(1));
    let natural: Vec<_> = (0..columns)
        .map(|column| {
            rows.iter()
                .map(|row| rich_width(&row[column]))
                .max()
                .unwrap_or(0)
                .max(1)
        })
        .collect();
    let mut widths = vec![0; columns];
    let mut remaining = available;
    // Share space fairly, returning unused space from short columns to longer ones.
    while remaining > 0 {
        let mut changed = false;
        for (column, used) in widths.iter_mut().enumerate() {
            if remaining > 0 && *used < natural[column] {
                *used += 1;
                remaining -= 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let mut result = Vec::new();
        for (column, (cell, width)) in row.iter().zip(&widths).enumerate() {
            if column > 0 {
                result.extend(plain(&" ".repeat(gap)));
            }
            let clipped = truncate_rich(cell, *width);
            let padding = width.saturating_sub(rich_width(&clipped));
            result.extend(clipped);
            result.extend(plain(&" ".repeat(padding)));
        }
        lines.push(result);
        if index == 0 {
            lines.push(plain(&"─".repeat(
                widths.iter().sum::<usize>() + gap * columns.saturating_sub(1),
            )));
        }
    }
    lines
}

fn truncate_rich(cell: &RichText, width: usize) -> RichText {
    if rich_width(cell) <= width {
        return cell.clone();
    }
    if width == 0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut used = 0;
    for &(ch, st) in cell {
        let size = ch.width().unwrap_or(0);
        if used + size >= width {
            break;
        }
        result.push((ch, st));
        used += size;
    }
    result.push(('…', InlineStyle::default()));
    result
}

#[cfg(test)]
fn truncate_cell(cell: &str, width: usize) -> String {
    truncate_rich(&plain(cell), width)
        .into_iter()
        .map(|(ch, _)| ch)
        .collect()
}

fn rich_line(row: RichText, color: Rgb) -> Line {
    let (chars, inline): (String, Vec<_>) = row.into_iter().unzip();
    Line {
        text: chars,
        color,
        bold: false,
        inline,
    }
}

fn draw_line(g: &mut Grid, x: usize, y: usize, width: usize, l: &Line) {
    let base = style(l.color, Rgb::new(21, 23, 28), l.bold);
    if l.inline.is_empty() {
        text(g, x, y, width, &l.text, base);
        return;
    }
    // Draw contiguous styles together so combining marks remain attached to their base.
    let mut run = String::new();
    let mut current = base;
    let mut used = 0;
    for (ch, inline) in l.text.chars().zip(&l.inline) {
        let mut st = base;
        st.bold |= inline.bold;
        st.italic = inline.italic;
        if inline.code {
            st.dim = true;
            st.fg = Some(PURPLE);
        }
        if st != current && !run.is_empty() {
            used += text(g, x + used, y, width.saturating_sub(used), &run, current);
            run.clear();
        }
        current = st;
        run.push(ch);
    }
    text(g, x + used, y, width.saturating_sub(used), &run, current);
}

fn event_lines(
    event: &TalkEvent,
    width: usize,
    now: Timestamp,
    harness: &str,
    color: Rgb,
    truncate: bool,
) -> Vec<Line> {
    let (who, body, color) = match event {
        TalkEvent::You { body, .. } => ("you".to_owned(), body, PINK),
        TalkEvent::Reply { body, .. } => (harness.to_owned(), body, color),
        TalkEvent::Report { agent, body, .. } => (agent.clone(), body, PURPLE),
        TalkEvent::ReplyToReport { agent, body, .. } => (
            format!("{harness} → {}", agent.as_deref().unwrap_or("report")),
            body,
            PURPLE,
        ),
    };
    let time = event
        .timestamp()
        .map(|t| format!("{} ago", age(t.age_at(now).as_secs())))
        .unwrap_or_else(|| "unknown age".into());
    let mut out = vec![line(format!("{who} · {time}"), color, true)];
    let rows = wrap_rich(body, width.saturating_sub(2));
    let count = rows.len();
    out.extend(
        rows.into_iter()
            .take(if truncate && matches!(event, TalkEvent::Reply { .. }) {
                9
            } else {
                usize::MAX
            })
            .map(|row| {
                let mut prefixed = plain(if matches!(event, TalkEvent::You { .. }) {
                    "▎ "
                } else {
                    "  "
                });
                prefixed.extend(row);
                rich_line(prefixed, INK)
            }),
    );
    if truncate && matches!(event, TalkEvent::Reply { .. }) && count > 9 {
        out.push(line(format!("… {} more lines", count - 9), QUIET, false));
    }
    out.push(line("", QUIET, false));
    out
}
fn draw_lines(g: &mut Grid, r: Rect, lines: &[Line], scroll: usize) {
    let top = lines.len().saturating_sub(r.height).saturating_sub(scroll);
    for (i, l) in lines.iter().skip(top).take(r.height).enumerate() {
        draw_line(g, r.x, r.y + i, r.width, l);
    }
}
pub fn quick(
    g: &mut Grid,
    w: &WindowInfo,
    state: &mut State,
    composite: Option<&Grid>,
    data: &CardData,
    now: Timestamp,
    dock_rows: usize,
) -> Option<(usize, usize)> {
    let r = Layout::panel(
        g.width(),
        g.height().saturating_sub(dock_rows.saturating_sub(1)),
        (w.width, w.height),
        state.tab == Tab::Screen,
    );
    if r.width < 3 || r.height < 3 {
        return None;
    }
    let edge = window_edge(w).unwrap_or(Rgb::new(60, 64, 74));
    fill(g, r, Rgb::new(21, 23, 28));
    for x in r.x..r.x + r.width {
        text(g, x, r.y, 1, "─", style(edge, BACK, false));
        text(g, x, r.y + r.height - 1, 1, "─", style(edge, BACK, false));
    }
    for y in r.y..r.y + r.height {
        text(g, r.x, y, 1, "│", style(edge, BACK, false));
        text(g, r.x + r.width - 1, y, 1, "│", style(edge, BACK, false));
    }
    let head = Rect {
        x: r.x + 1,
        y: r.y,
        width: r.width - 2,
        height: 1,
    };
    fill(g, head, Rgb::new(32, 34, 41));
    let mut x = title(g, head, w);
    let harness = w.panes.iter().find_map(|p| p.harness.as_ref());
    if let Some(h) = harness {
        x += text(
            g,
            head.x + x,
            head.y,
            head.width.saturating_sub(x),
            &format!(" {} ", h.name),
            style(h.color, Rgb::new(32, 34, 41), false),
        );
    }
    for (tab, label) in [
        (Tab::Screen, "Screen"),
        (Tab::Talk, "Talk"),
        (Tab::Reports, "Reports"),
    ] {
        let label = if tab == Tab::Reports {
            format!(" Reports {} ", data.talk.reports.len())
        } else {
            format!(" {label} ")
        };
        x += text(
            g,
            head.x + x,
            head.y,
            head.width.saturating_sub(x),
            &label,
            style(
                if state.tab == tab { INK } else { QUIET },
                if state.tab == tab {
                    Rgb::new(21, 23, 28)
                } else {
                    Rgb::new(32, 34, 41)
                },
                state.tab == tab,
            ),
        );
    }
    let f = &data.facts;
    let mut facts = Vec::new();
    if let Some(model) = &f.model {
        facts.push(model.clone());
    }
    if let Some(ctx) = f.context_tokens {
        facts.push(format!("{:.1}k ctx", ctx as f64 / 1000.0));
    }
    facts.push(format!(
        "{} subagents, {} busy",
        f.subagents.len() + f.hidden_count,
        f.busy_count()
    ));
    if let Some(a) = w.activity {
        facts.push(format!("active {} ago", age(a.age_at(now).as_secs())));
    }
    let facts = facts.join(" · ");
    let start = head.width.saturating_sub(facts.chars().count()).max(x + 1);
    text(
        g,
        head.x + start,
        head.y,
        head.width.saturating_sub(start),
        &facts,
        style(QUIET, Rgb::new(32, 34, 41), false),
    );
    let body = Rect {
        x: r.x + 1,
        y: r.y + 1,
        width: r.width - 2,
        height: r.height - 2,
    };
    if state.tab == Tab::Screen {
        if let Some(src) = composite {
            blit(g, src, body, 0);
        }
        return None;
    }
    let (name, color) = harness
        .map(|h| (h.name.as_str(), h.color))
        .unwrap_or(("agent", QUIET));
    if state.tab == Tab::Reports {
        let lines = data
            .talk
            .reports
            .iter()
            .flat_map(|e| event_lines(e, body.width, now, name, color, false))
            .collect::<Vec<_>>();
        state.sync_transcript(&data.talk, lines.len(), body.height);
        draw_lines(g, body, &lines, state.scroll);
    } else {
        let right_width = if body.width >= 60 { body.width / 3 } else { 0 };
        let left = Rect {
            width: body
                .width
                .saturating_sub(right_width + usize::from(right_width > 0)),
            ..body
        };
        let mut lines = Vec::new();
        for block in &data.talk.blocks {
            match block {
                TalkBlock::Message(e) => {
                    lines.extend(event_lines(e, left.width, now, name, color, true))
                }
                TalkBlock::Reports { reports, replies } => lines.push(line(
                    format!("·· {reports} reports and {replies} replies to them (Reports tab) ··"),
                    QUIET,
                    false,
                )),
            }
        }
        if lines.is_empty() {
            lines.push(line(
                "Talk unavailable: no transcript adapter for this pane",
                QUIET,
                false,
            ));
        }
        let banner = data.talk.unanswered.as_ref();
        let transcript = Rect {
            height: left.height.saturating_sub(usize::from(banner.is_some())),
            ..left
        };
        state.sync_transcript(&data.talk, lines.len(), transcript.height);
        draw_lines(g, transcript, &lines, state.scroll);
        if let Some(u) = banner {
            fill(
                g,
                Rect {
                    y: left.y + left.height - 1,
                    height: 1,
                    ..left
                },
                Rgb::new(82, 22, 47),
            );
            text(
                g,
                left.x,
                left.y + left.height - 1,
                left.width,
                &format!(
                    "Your last message has no reply yet{}",
                    u.timestamp
                        .map(|t| format!(" · {}", age(t.age_at(now).as_secs())))
                        .unwrap_or_default()
                ),
                style(PINK, Rgb::new(82, 22, 47), true),
            );
        }
        if right_width > 0 {
            let right = Rect {
                x: body.x + left.width + 1,
                width: right_width,
                ..body
            };
            let mut lines = vec![
                line("reports, by agent", PURPLE, true),
                line("", QUIET, false),
            ];
            for report in &data.talk.by_agent {
                let age = report
                    .timestamp
                    .map(|t| age(t.age_at(now).as_secs()))
                    .unwrap_or_else(|| "—".into());
                lines.push(line(
                    format!("{}  ×{} · {} ago", report.agent, report.count, age),
                    INK,
                    true,
                ));
                lines.extend(
                    wrap_rich(&report.latest_body, right.width)
                        .into_iter()
                        .take(2)
                        .map(|row| rich_line(row, QUIET)),
                );
                lines.push(line("", QUIET, false));
            }
            // Reports by agent are newest first, so this column starts at its top.
            for (i, l) in lines.iter().take(right.height).enumerate() {
                draw_line(g, right.x, right.y + i, right.width, l);
            }
        }
    }
    if state.new_entries {
        let mut marker = style(QUIET, BACK, false);
        marker.dim = true;
        let marker_width = (r.width - 2).min(5);
        text(
            g,
            r.x + r.width - 2 - marker_width,
            r.y + r.height - 1,
            marker_width,
            "↓ new",
            marker,
        );
    }
    None
}
pub fn frame(width: usize, height: usize) -> Grid {
    Grid::new(width, height, Cell::blank(style(INK, BACK, false)))
        .expect("terminal dimensions fit memory")
}
pub fn empty_data() -> CardData {
    CardData {
        facts: Facts::default(),
        talk: std::sync::Arc::default(),
    }
}
pub type Cards = HashMap<String, CardData>;

/// Selection is terminal cells only: no image cache invalidation or transmission.
pub fn selection(g: &mut Grid, r: Rect) {
    for x in r.x..r.x + r.width {
        if let Some(cell) = g.get_mut(x, r.y) {
            cell.style.bg = Some(Rgb::new(240, 243, 248));
            cell.style.fg = Some(Rgb::new(20, 23, 29));
            cell.style.bold = true;
        }
    }
    for y in r.y + r.height.saturating_sub(2)..r.y + r.height {
        text(
            g,
            r.x,
            y,
            1,
            "▌",
            style(Rgb::new(240, 243, 248), BACK, true),
        );
    }
}

/// The same bounds drive menu painting and mouse hit testing.
pub fn menu_rect(width: usize, height: usize) -> Rect {
    let w = width.min(34);
    let h = height.min(6);
    Rect {
        x: width.saturating_sub(w) / 2,
        y: height.saturating_sub(h) / 2,
        width: w,
        height: h,
    }
}
pub fn menu(g: &mut Grid, selected: usize) {
    let rect = menu_rect(g.width(), g.height());
    fill(g, rect, BACK);
    for (row, label) in [
        "Options",
        "Go to window",
        "Rename",
        "Close (confirm)",
        "Esc cancel",
    ]
    .iter()
    .enumerate()
    {
        if row >= rect.height {
            break;
        }
        let chosen = row == selected + 1;
        text(
            g,
            rect.x,
            rect.y + row,
            rect.width,
            label,
            style(
                if chosen { BACK } else { INK },
                if chosen { INK } else { BACK },
                chosen,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_width::UnicodeWidthStr;
    fn transcript_frame(state: &mut State, events: &[TalkEvent]) -> Grid {
        let window = crate::demo::windows().remove(0);
        let mut data = empty_data();
        data.talk = std::sync::Arc::new(TalkView::from_events(events, Timestamp(0)));
        let mut grid = frame(120, 24);
        quick(&mut grid, &window, state, None, &data, Timestamp(0), 1);
        grid
    }
    fn row_text(grid: &Grid, y: usize) -> String {
        (0..grid.width())
            .map(|x| grid.get(x, y).unwrap().text.as_str())
            .collect()
    }
    fn history() -> Vec<TalkEvent> {
        (0..40)
            .flat_map(|i| {
                [
                    TalkEvent::Reply {
                        timestamp: None,
                        body: format!("message {i}"),
                    },
                    TalkEvent::Report {
                        timestamp: None,
                        agent: "worker".into(),
                        body: format!("report {i}"),
                    },
                ]
            })
            .collect()
    }
    #[test]
    fn opening_and_growing_projection_stays_at_newest_entry() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for tab in [Tab::Talk, Tab::Reports] {
            let mut state = State::default();
            state.key(
                KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE),
                &crate::demo::windows(),
                3,
            );
            state.key(
                KeyEvent::new(
                    KeyCode::Char(if tab == Tab::Talk { '2' } else { '3' }),
                    KeyModifiers::NONE,
                ),
                &crate::demo::windows(),
                3,
            );
            let events = history();
            for end in [0, 10, 30, events.len()] {
                let grid = transcript_frame(&mut state, &events[..end]);
                let window = crate::demo::windows().remove(0);
                let panel = Layout::panel(120, 24, (window.width, window.height), false);
                if end >= 10 {
                    if tab == Tab::Talk {
                        assert!(
                            row_text(&grid, panel.y + panel.height - 2).contains("Reports tab) ··")
                        );
                        assert!((panel.y..panel.y + panel.height)
                            .any(|y| row_text(&grid, y)
                                .contains(&format!("message {}", end / 2 - 1))));
                    } else {
                        let rows = end / 2 * 3; // Report header, body, trailing blank.
                        let last_body = panel.y + 1 + rows.min(panel.height - 2) - 2;
                        assert!(
                            row_text(&grid, last_body).contains(&format!("report {}", end / 2 - 1))
                        );
                    }
                }
                assert_eq!(state.scroll, 0);
                assert!(!state.new_entries);
            }
        }
    }
    #[test]
    fn unpinned_growth_shows_marker_and_down_or_end_repins() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for tab in [Tab::Talk, Tab::Reports] {
            let mut state = State::default();
            state.quick = true;
            state.tab = tab;
            let mut events = history();
            transcript_frame(&mut state, &events);
            state.key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE), &[], 3);
            let before = transcript_frame(&mut state, &events);
            events.push(TalkEvent::Reply {
                timestamp: None,
                body: "latest message".into(),
            });
            events.push(TalkEvent::Report {
                timestamp: None,
                agent: "worker".into(),
                body: "latest report".into(),
            });
            let after = transcript_frame(&mut state, &events);
            let window = crate::demo::windows().remove(0);
            let panel = Layout::panel(120, 24, (window.width, window.height), false);
            assert_eq!(
                row_text(&before, panel.y + 1),
                row_text(&after, panel.y + 1)
            );
            assert!(state.scroll > 10);
            assert!(state.new_entries);
            let marker_y = panel.y + panel.height - 1;
            assert!(row_text(&after, marker_y).contains("↓ new"));
            assert!(
                after
                    .get(panel.x + panel.width - 7, marker_y)
                    .unwrap()
                    .style
                    .dim
            );
            state.scroll_down(usize::MAX);
            let bottom = transcript_frame(&mut state, &events);
            assert_eq!(state.scroll, 0);
            assert!(!row_text(&bottom, marker_y).contains("↓ new"));
            state.scroll_up(3); // Same path used by wheel scrolling.
            state.key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &[], 3);
            events.push(TalkEvent::Report {
                timestamp: None,
                agent: "worker".into(),
                body: "newest report".into(),
            });
            transcript_frame(&mut state, &events);
            assert_eq!(state.scroll, 0);
            assert!(!state.new_entries);
        }
    }

    #[test]
    fn inline_markdown_fixture_renders_styles_in_talk_and_reports() {
        let body = include_str!("../tests/fixtures/talk-inline.md");
        let rows = wrap(body, 60);
        assert_eq!(rows[0], "bold code italic slant plain");
        assert_eq!(
            rows[1],
            "**literal** _code_ snake_case *escaped* unmatched *"
        );
        assert_eq!(rows[2], "wide 界 é and outer nested");
        assert_eq!(rows[4], "Name   Value ");
        assert_eq!(rows[6], "bold   code  ");
        assert_eq!(rows[7], "slant  italic");
        assert_eq!(
            &rows[9..17],
            &[
                "````md",
                "**fenced** `code` _literal_",
                "```",
                "~~~",
                "| **x** | y |",
                "| --- | --- |",
                "````",
                "~~~md",
            ]
        );
        assert_eq!(rows[17], "**tilde** `code` *literal*");
        assert_eq!(rows[19], "after");
        for event in [
            TalkEvent::You {
                timestamp: None,
                body: body.into(),
            },
            TalkEvent::Reply {
                timestamp: None,
                body: body.into(),
            },
            TalkEvent::Report {
                timestamp: None,
                agent: "worker".into(),
                body: body.into(),
            },
            TalkEvent::ReplyToReport {
                timestamp: None,
                agent: Some("worker".into()),
                body: body.into(),
            },
        ] {
            let lines = event_lines(&event, 62, Timestamp(0), "claude", PURPLE, false);
            let mut grid = frame(62, lines.len());
            draw_lines(
                &mut grid,
                Rect {
                    x: 0,
                    y: 0,
                    width: 62,
                    height: lines.len(),
                },
                &lines,
                0,
            );
            let at = |row: usize, word: &str| {
                let offset = rows[row].find(word).unwrap();
                let x = rows[row][..offset].width() + 2;
                grid.get(x, row + 1).unwrap().style
            };
            assert!(at(0, "bold").bold);
            assert!(at(0, "code").dim);
            assert_eq!(at(0, "code").fg, Some(PURPLE));
            assert!(at(0, "italic").italic && at(0, "slant").italic);
            assert!(!at(0, "plain").bold && !at(0, "plain").italic);
            assert!(at(1, "literal").dim && !at(1, "literal").bold);
            assert!(!at(1, "snake").italic && !at(1, "escaped").italic);
            assert!(at(2, "nested").bold && at(2, "nested").italic);
            assert!(at(4, "Name").bold && at(6, "bold").bold);
            assert!(at(6, "code").dim && at(7, "slant").italic);
            assert!(!at(10, "fenced").bold && !at(10, "code").dim);
            assert!(!at(17, "tilde").bold && !at(17, "literal").italic);
            assert!(at(19, "after").bold);
            assert_eq!(grid.get(8, 3).unwrap().width, 0); // wide glyph continuation
            assert_eq!(grid.get(10, 3).unwrap().text, "é");
        }
    }

    #[test]
    fn inline_styles_survive_wrapping_and_table_clipping() {
        let rows = wrap_rich("**abcdef** `ghijkl`", 3);
        assert_eq!(
            rows.iter().map(rich_width).collect::<Vec<_>>(),
            [3, 3, 3, 3, 1]
        );
        assert!(rows[0].iter().chain(&rows[1]).all(|(_, st)| st.bold));
        assert!(rows[3].iter().all(|(_, st)| st.code));
        let table = "| **abcdef** | `界界` |\n| --- | --- |\n| _italic_ | plain |";
        for width in [1, 2, 3, 4, 8, 20] {
            let rows = wrap_rich(table, width);
            assert!(rows.iter().all(|row| rich_width(row) <= width));
        }
        let rows = wrap_rich(table, 8);
        assert!(rows[0].iter().any(|(ch, st)| *ch == 'a' && st.bold));
        assert!(rows[2].iter().any(|(ch, st)| *ch == 'i' && st.italic));
    }

    #[test]
    fn talk_table_fixture_aligns_columns_and_clips_to_display_width() {
        let body = include_str!("../tests/fixtures/talk-table.md");
        let rows = wrap(body, 40);
        assert_eq!(
            &rows[2..6],
            &[
                "      Now    When I reset  How  ",
                "────────────────────────────────",
                "Task  ready  later         retry",
                "File  界界   unchanged     ask  ",
            ]
        );
        for width in [1, 2, 3, 4, 5, 8, 14, 20, 40] {
            let rows = wrap(body, width);
            assert!(rows.iter().all(|row| row.width() <= width));
            assert_eq!(rows.iter().filter(|row| row.contains('─')).count(), 1);
            assert!(rows.iter().all(|row| !row.contains('|')));
            if width == 20 {
                assert!(rows.iter().any(|row| row.contains('…')));
            }
            for event in [
                TalkEvent::You {
                    timestamp: None,
                    body: body.into(),
                },
                TalkEvent::Report {
                    timestamp: None,
                    agent: "worker".into(),
                    body: body.into(),
                },
            ] {
                let lines = event_lines(&event, width + 2, Timestamp(0), "claude", PURPLE, false);
                assert!(lines
                    .iter()
                    .skip(1)
                    .all(|line| line.text.width() <= width + 2));
                assert!(lines.iter().any(|line| line.text.contains('─')));
            }
        }
        assert_eq!(truncate_cell("界界", 3), "界…");
        assert_eq!(truncate_cell("e\u{301}long", 2), "e\u{301}…");
    }

    #[test]
    fn pipe_prose_and_fenced_tables_keep_their_original_text() {
        assert_eq!(wrap("a | b\nx | y", 20), ["a | b", "x | y"]);
        assert_eq!(wrap("| a | b |\n|---|", 20), ["| a | b |", "|---|"]);
        assert_eq!(
            wrap("```\n| a |\n|---|\n```", 20),
            ["```", "| a |", "|---|", "```"]
        );
        assert_eq!(
            wrap("A | B\n--- | ---\none | two", 20),
            ["A    B  ", "────────", "one  two"]
        );
    }

    #[test]
    fn every_overlay_stays_inside_card_with_right_neighbor() {
        let mut window = crate::demo::windows().remove(0);
        window.name = "long 界 label ".repeat(30);
        let mut data = empty_data();
        data.facts.now.text = "long now 界 combining e\u{301} ".repeat(100);
        data.facts.subagents = vec![Subagent {
            name: "long agent 界 ".repeat(40),
            state: SubagentState::Busy,
            running_time: Some(std::time::Duration::from_secs(42)),
        }];
        data.facts.hidden_count = 10;
        data.talk = std::sync::Arc::new(TalkView::from_events(
            &[TalkEvent::You {
                timestamp: Some(Timestamp(0)),
                body: "ask".into(),
            }],
            Timestamp(30000),
        ));
        for height in [1, 2, 3, 8] {
            for width in [1, 2, 7, 30] {
                let mut grid = frame(80, 12);
                for y in 0..grid.height() {
                    for x in 0..grid.width() {
                        grid.get_mut(x, y).unwrap().text = "#".into();
                    }
                }
                let original = grid.clone();
                let card = Rect {
                    x: 3,
                    y: 2,
                    width,
                    height,
                };
                for tool in [None, Some("VeryLongToolName".repeat(10))] {
                    data.facts.now.tool = tool;
                    data.facts.now.kind = if data.facts.now.tool.is_some() {
                        NowKind::Tool
                    } else {
                        NowKind::Text
                    };
                    card_overlay(&mut grid, card, &window, &data, Timestamp(30000));
                    for y in [
                        card.y,
                        card.y + card.height.saturating_sub(2),
                        card.y + card.height - 1,
                    ] {
                        assert_eq!(
                            grid.get(card.x + card.width - 1, y).unwrap().text,
                            " ",
                            "right gutter at width={width}, height={height}"
                        );
                    }
                    for y in 0..grid.height() {
                        for x in 0..grid.width() {
                            if x < card.x
                                || x >= card.x + card.width
                                || y < card.y
                                || y >= card.y + card.height
                            {
                                assert_eq!(
                                    grid.get(x, y),
                                    original.get(x, y),
                                    "overlay leaked at {x},{y}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
