//! Read-only diagnostics. `cargo run -p ncm-talk --example dump -- %23 [--counts-only]`.
use anyhow::{bail, Context, Result};
use ncm_model::TalkEvent;
use ncm_talk::{adapter_for, now, SessionDiscovery, TalkBlock, TalkView, TranscriptFollower};
use std::{
    io::{self, Write},
    process::Command,
    time::Instant,
};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let Some(target) = args.next() else {
        bail!("usage: dump <tmux-pane-id|session-id|--pid PID> [--counts-only]");
    };
    let discovery = SessionDiscovery::default();
    let session = if target == "--pid" {
        let pid = args.next().context("--pid needs a process id")?.parse()?;
        discovery.discover(pid)?
    } else if let Some(number) = target.strip_prefix('%') {
        if !number.bytes().all(|b| b.is_ascii_digit()) || number.is_empty() {
            bail!("invalid pane id");
        }
        let output = Command::new("tmux")
            .args(["display-message", "-p", "-t", &target, "#{pane_pid}"])
            .output()?;
        if !output.status.success() {
            bail!("tmux: {}", String::from_utf8_lossy(&output.stderr).trim());
        }
        let pid = String::from_utf8(output.stdout)?.trim().parse()?;
        discovery.discover(pid)?
    } else {
        discovery.session(&target)?
    };
    let counts_only = match args.next().as_deref() {
        None => false,
        Some("--counts-only") => true,
        Some(_) => bail!("unknown option"),
    };
    if args.next().is_some() {
        bail!("too many arguments");
    }
    let mut out = io::BufWriter::new(io::stdout().lock());
    let Some(session) = session else {
        writeln!(
            out,
            "discovery: none (unsupported, unavailable, or ambiguous)"
        )?;
        return Ok(());
    };
    writeln!(
        out,
        "discovery: {} {} {}",
        session.harness.name,
        session.session_id,
        session.transcript_path.display()
    )?;
    let mut adapter = adapter_for(&session).context("no adapter for harness")?;
    let mut follower = TranscriptFollower::new(&session);
    let start = Instant::now();
    let mut events = Vec::new();
    let mut counts = [0usize; 4];
    let mut records = 0usize;
    loop {
        let batch = follower.poll_adapter(&mut *adapter)?;
        if batch.reset {
            events.clear();
            counts = [0; 4];
            records = 0;
        }
        records += batch.records_read;
        for event in &batch.events {
            counts[class_index(event)] += 1;
        }
        if !counts_only {
            events.extend(batch.events);
        }
        if batch.lines_read < follower.max_records {
            break;
        }
    }
    writeln!(
        out,
        "counts: you={} replies={} reports={} report_replies={} malformed={}",
        counts[0], counts[1], counts[2], counts[3], follower.malformed_lines
    )?;
    writeln!(
        out,
        "load: bytes={} records={} elapsed_ms={}",
        follower.offset(),
        records,
        start.elapsed().as_millis()
    )?;
    if counts_only {
        return Ok(());
    }
    let view = TalkView::from_events(&events, now());
    writeln!(out, "Talk:")?;
    for block in &view.blocks {
        match block {
            TalkBlock::Message(event) => writeln!(
                out,
                "{} · {:?}\n{}\n",
                class_name(event),
                event.timestamp(),
                event.body()
            )?,
            TalkBlock::Reports { reports, replies } => writeln!(
                out,
                "·· {reports} reports and {replies} replies to them (Reports tab) ··"
            )?,
        }
    }
    writeln!(out, "Reports by agent:")?;
    for summary in &view.by_agent {
        writeln!(
            out,
            "{} ×{} · {:?}\n{}",
            summary.agent,
            summary.count,
            summary.timestamp.map(|time| time.age_at(now())),
            summary
                .latest_body
                .lines()
                .take(2)
                .collect::<Vec<_>>()
                .join("\n")
        )?;
    }
    writeln!(out, "Reports timeline:")?;
    for event in &view.reports {
        writeln!(
            out,
            "{} · {:?}\n{}",
            class_name(event),
            event.timestamp(),
            event.body()
        )?;
    }
    let facts = adapter.facts();
    writeln!(
        out,
        "Facts:\nnow: {}\nmodel: {}\ncontext_tokens: {:?}\nsubagents: {} ({} busy), {} hidden",
        facts.now.text,
        facts.model.as_deref().unwrap_or("unknown"),
        facts.context_tokens,
        facts.subagents.len(),
        facts.busy_count(),
        facts.hidden_count
    )?;
    for agent in facts.subagents {
        writeln!(
            out,
            "{} {:?} {:?}",
            agent.name, agent.state, agent.running_time
        )?;
    }
    writeln!(out, "unanswered: {:?}", view.unanswered)?;
    Ok(())
}

fn class_index(event: &TalkEvent) -> usize {
    match event {
        TalkEvent::You { .. } => 0,
        TalkEvent::Reply { .. } => 1,
        TalkEvent::Report { .. } => 2,
        TalkEvent::ReplyToReport { .. } => 3,
    }
}
fn class_name(event: &TalkEvent) -> &'static str {
    ["you", "reply", "report", "reply-to-report"][class_index(event)]
}
