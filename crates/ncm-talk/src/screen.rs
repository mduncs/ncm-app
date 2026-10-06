use ncm_model::{Facts, NowKind, NowLine, Subagent, SubagentState};
use std::time::Duration;

/// Fallback port of wallcore.py facts/SPIN/TOOL/SUB/CHROME, with ANSI stripped first.
pub fn screen_facts<S: AsRef<str>>(lines: &[S]) -> Facts {
    let lines: Vec<_> = lines.iter().map(|line| plain(line.as_ref())).collect();
    let mut facts = Facts::default();
    let mut first_sub = None;
    for (index, line) in lines.iter().enumerate() {
        if facts.model.is_none() {
            if let Some(rest) = line.split_once("Model: ").map(|(_, rest)| rest) {
                let mut words = rest.split_whitespace();
                if let (Some(name), Some(version)) = (words.next(), words.next()) {
                    if name.chars().all(|c| c.is_ascii_alphabetic())
                        && version.chars().all(|c| c.is_ascii_digit() || c == '.')
                    {
                        facts.model = Some(format!("{name} {version}"));
                    }
                }
            }
        }
        if facts.context_tokens.is_none() {
            if let Some(rest) = line.split_once("Ctx: ").map(|(_, rest)| rest) {
                if let Some(token) = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.strip_suffix('k'))
                {
                    facts.context_tokens = token
                        .parse::<f64>()
                        .ok()
                        .filter(|n| n.is_finite() && *n >= 0.0)
                        .map(|n| (n * 1000.0) as u64);
                }
            }
        }
        if let Some((glyph, name, detail)) = sub_line(line) {
            if glyph != '⏺' && first_sub.is_none() {
                first_sub = Some(index);
            }
            let idle = detail
                .split('·')
                .next()
                .unwrap_or(detail)
                .trim_end()
                .ends_with("idle");
            facts.subagents.push(Subagent {
                name: name.into(),
                state: if idle {
                    SubagentState::Idle
                } else {
                    SubagentState::Busy
                },
                running_time: if idle { None } else { duration(detail) },
            });
        }
        // The prototype sums both compact hidden-count forms, wherever they occur.
        facts.hidden_count += counts_before(line, " idle agents");
        let mut remaining = line.as_str();
        while let Some((_, rest)) = remaining.split_once("↓ ") {
            if let Some((number, _)) = rest.split_once(" more") {
                if let Ok(n) = number.parse::<usize>() {
                    facts.hidden_count = facts.hidden_count.saturating_add(n);
                }
            }
            remaining = rest;
        }
    }
    for line in &lines {
        let text = line.trim_start();
        if let Some(first) = text.chars().next() {
            let rest = text[first.len_utf8()..].trim_start();
            if !first.is_alphanumeric()
                && first != '_'
                && !first.is_whitespace()
                && text[first.len_utf8()..].starts_with(char::is_whitespace)
            {
                if let Some((word, _)) = rest.split_once("…") {
                    let mut chars = word.chars();
                    if chars.next().is_some_and(|c| c.is_ascii_uppercase())
                        && chars.clone().next().is_some()
                        && chars.all(|c| c.is_ascii_lowercase())
                        && rest
                            .split_once('…')
                            .is_some_and(|(_, suffix)| suffix.trim_start().starts_with('('))
                    {
                        facts.now = NowLine {
                            kind: NowKind::Spinner,
                            text: rest.into(),
                            tool: None,
                        };
                        return facts;
                    }
                }
            }
        }
    }
    for line in lines.iter().rev() {
        let text = line.trim_start();
        let candidates = [
            text,
            text.chars()
                .next()
                .map(|c| &text[c.len_utf8()..])
                .unwrap_or("")
                .trim_start(),
        ];
        for candidate in candidates {
            if let Some((tool, _)) = candidate.split_once('(') {
                if matches!(
                    tool,
                    "Bash"
                        | "Read"
                        | "Edit"
                        | "Write"
                        | "Update"
                        | "Task"
                        | "Agent"
                        | "Grep"
                        | "Glob"
                        | "WebSearch"
                        | "WebFetch"
                        | "Search"
                ) {
                    facts.now = NowLine {
                        kind: NowKind::Tool,
                        text: candidate.into(),
                        tool: Some(tool.into()),
                    };
                    return facts;
                }
            }
        }
    }
    let end = first_sub
        .filter(|index| *index > 0)
        .map(|index| index - 1)
        .unwrap_or(lines.len());
    for line in lines[..end].iter().rev() {
        if !line.trim().is_empty() && !chrome(line) {
            facts.now.text = line.trim().into();
            break;
        }
    }
    facts
}

/// Transcripts win field by field; absent transcript metadata is filled from the screen.
pub fn merge_facts(mut transcript: Facts, screen: Facts) -> Facts {
    if transcript.now.text.is_empty() {
        transcript.now = screen.now;
    }
    if transcript.model.is_none() {
        transcript.model = screen.model;
    }
    if transcript.context_tokens.is_none() {
        transcript.context_tokens = screen.context_tokens;
    }
    for live in screen.subagents {
        if let Some(agent) = transcript
            .subagents
            .iter_mut()
            .find(|a| a.name == live.name)
        {
            *agent = live;
        } else {
            transcript.subagents.push(live);
        }
    }
    transcript.hidden_count = transcript.hidden_count.max(screen.hidden_count);
    transcript
}

fn sub_line(line: &str) -> Option<(char, &str, &str)> {
    let text = line.trim_start();
    let glyph = text.chars().next()?;
    if !"○●◉◯◎⏺".contains(glyph) {
        return None;
    }
    let rest = &text[glyph.len_utf8()..];
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let split = rest.find(char::is_whitespace)?;
    let name = &rest[..split];
    let detail = &rest[split..];
    if detail.chars().take_while(|c| c.is_whitespace()).count() < 2 {
        return None;
    }
    Some((glyph, name, detail.trim()))
}

fn duration(text: &str) -> Option<Duration> {
    let mut seconds = 0u64;
    let mut found = false;
    for word in text.split_whitespace() {
        if let Some((unit, factor)) = [('h', 3600), ('m', 60), ('s', 1)]
            .into_iter()
            .find(|(unit, _)| word.ends_with(*unit))
        {
            if let Ok(n) = word.trim_end_matches(unit).parse::<u64>() {
                seconds = seconds.saturating_add(n.saturating_mul(factor));
                found = true;
            }
        }
    }
    found.then(|| Duration::from_secs(seconds))
}

fn counts_before(text: &str, needle: &str) -> usize {
    let mut total = 0usize;
    let mut rest = text;
    while let Some((before, after)) = rest.split_once(needle) {
        let digits: String = before
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        total = total.saturating_add(digits.parse::<usize>().unwrap_or(0));
        rest = after;
    }
    total
}

fn chrome(line: &str) -> bool {
    let text = line.trim_start();
    let path_bar = text
        .rsplit_once('|')
        .is_some_and(|(_, rest)| rest.trim_start().starts_with(['/', '~', '(']));
    path_bar
        || text.contains("Ctx:")
        || text.starts_with("Model:")
        || text.starts_with("Session ID")
        || text.contains("bypass permissions")
        || text.contains("? for shortcuts")
        || text.starts_with(['─', '━', '›', '>'])
        || text.starts_with("current: ")
        || text.starts_with("↓ ")
        || text
            .split_once(" idle agents")
            .is_some_and(|(number, _)| number.parse::<usize>().is_ok())
        || text.chars().next().is_some_and(|c| {
            "○●◉◯◎".contains(c) && text[c.len_utf8()..].starts_with(char::is_whitespace)
        })
}

fn plain(line: &str) -> String {
    let mut result = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.peek() == Some(&'\\')) {
                            if c == '\x1b' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            result.push(if c == '\u{a0}' { ' ' } else { c });
        }
    }
    result.trim_end().into()
}
