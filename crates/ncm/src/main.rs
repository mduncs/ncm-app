//! Full-screen tmux mirror. Actions occur only in response to explicit input.
mod app;
mod collector;
mod demo;
mod input;
mod input_stream;
mod layout;
mod metrics;
mod output;
mod panic_log;
mod pictures;
mod talk_source;
mod terminal;
mod view;

use anyhow::{bail, Context, Result};
use ncm_tmux::Tmux;
use std::path::PathBuf;
#[derive(Default)]
pub struct Options {
    demo: bool,
    read_only: bool,
    stats: bool,
    log: Option<PathBuf>,
    socket: Option<PathBuf>,
    session: Option<String>,
}
impl Options {
    fn tmux(&self) -> Tmux {
        let tmux = self.socket.as_ref().map_or_else(Tmux::live, Tmux::socket);
        self.session
            .as_ref()
            .map_or(tmux.clone(), |s| tmux.with_session(s))
    }
}
fn main() -> Result<()> {
    panic_log::install();
    let result = std::panic::catch_unwind(run);
    if let Some(failure) = panic_log::failure() {
        bail!("{}: {}", failure.dock(), failure.message);
    }
    match result {
        Ok(result) => result,
        Err(_) => bail!("ncm panicked, see ~/.local/state/ncm/panic.log"),
    }
}
fn run() -> Result<()> {
    #[cfg(not(test))]
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    #[cfg(test)]
    let args = if std::env::var_os("NCM_TEST_LIVE").is_some() {
        vec!["--read-only".to_owned()]
    } else {
        vec!["--demo".to_owned(), "--read-only".to_owned()]
    };
    let mut options = Options {
        socket: std::env::var_os("NCM_TMUX_SOCKET").map(PathBuf::from),
        ..Default::default()
    };
    let mut list = false;
    let mut diagnose_talk = false;
    let mut diagnose_panes = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--version" | "-V" => {
                println!("ncm {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                println!("ncm — live tmux window grid and floating Quick Look\n\n  --session NAME  Mirror this session (current client, or main)\n  --socket PATH   Mirror another server (also NCM_TMUX_SOCKET)\n  --read-only     Disable tmux actions (safe monitoring/measurement)\n  --stats         CPU %, RSS, image KB/s, captures/s in dock\n  --log FILE      Write timing traces and per-second measurements\n  --demo          Synthetic windows; no tmux actions or data\n  --list          List tmux windows (read-only)\n  --diagnose-talk  Print pane/session/adapter and unanswered metadata without bodies\n  --pane ID        Limit --diagnose-talk to this pane (repeatable)\n  --version       Print version\n  --help          Print help\n\nArrows move · Space Quick Look · Tab next alert · Enter full screen tmux · q quit\nQuick Look: 1 Screen · 2 Talk · 3 Reports · [ ] pane · i full screen tmux · Ctrl-] back\nOutside tmux: direct kitty pictures (one startup probe); detach or Ctrl-] returns\nInside tmux: text/braille; Enter selects the window\nGrid: , rename · x close (y/n) · n new · Esc never quits");
                return Ok(());
            }
            "--demo" => options.demo = true,
            "--read-only" => options.read_only = true,
            "--stats" => options.stats = true,
            "--list" => list = true,
            "--diagnose-talk" => diagnose_talk = true,
            "--session" | "--socket" | "--log" | "--pane" => {
                let flag = &args[i];
                i += 1;
                let value = args
                    .get(i)
                    .with_context(|| format!("{flag} needs a value"))?;
                match flag.as_str() {
                    "--session" => options.session = Some(value.clone()),
                    "--socket" => options.socket = Some(value.into()),
                    "--pane" => diagnose_panes.push(value.clone()),
                    _ => options.log = Some(value.into()),
                }
            }
            _ => bail!("unknown argument: {}; use ncm --help", args[i]),
        }
        i += 1;
    }
    if !diagnose_talk && !diagnose_panes.is_empty() {
        bail!("--pane requires --diagnose-talk");
    }
    if diagnose_talk {
        let discovery = ncm_talk::SessionDiscovery::default();
        let tree = ncm_talk::ProcessTree::cached()?;
        for w in options.tmux().snapshot()? {
            for p in w.panes {
                if !diagnose_panes.is_empty() && !diagnose_panes.contains(&p.id) {
                    continue;
                }
                let session = discovery.discover_pane_with_tree(&p, &w.session, &tree)?;
                let processes = tree
                    .descendants(p.pid)
                    .iter()
                    .map(|p| format!("{}:{}", p.pid, p.command))
                    .collect::<Vec<_>>()
                    .join(",");
                println!("pane={} session={} root_pid={} processes={} harness={} sessionId={} transcript={} adapter={}",
                    p.id, w.session, p.pid, processes,
                    session.as_ref().map_or("none", |s| s.harness.name.as_str()),
                    session.as_ref().map_or("none", |s| s.session_id.as_str()),
                    session.as_ref().map_or_else(|| "none".into(), |s| s.transcript_path.display().to_string()),
                    if session.as_ref().and_then(ncm_talk::adapter_for).is_some() { "attached" } else { "none" });
                if let Some((session, mut adapter)) = session
                    .as_ref()
                    .and_then(|s| ncm_talk::adapter_for(s).map(|a| (s, a)))
                {
                    let mut follower = ncm_talk::TranscriptFollower::new(session);
                    loop {
                        let batch = follower.poll_adapter_bounded(&mut *adapter, 0, 0, 0)?;
                        if batch.lines_read < follower.max_records {
                            break;
                        }
                    }
                    let decision = follower.unanswered_decision();
                    if let Some(oldest) = &decision.oldest {
                        println!("pane={} unanswered_count={} oldest_timestamp={} oldest_uuid={} record_type={} origin_kind={} reason={:?}",
                            p.id, decision.count,
                            oldest.timestamp_text.clone().or_else(|| oldest.timestamp.map(|t| t.0.to_string())).unwrap_or_else(|| "unknown".into()),
                            oldest.uuid.as_deref().unwrap_or("unknown"), oldest.record_type,
                            oldest.origin_kind.as_deref().unwrap_or("null"), oldest.reason);
                    } else {
                        println!("pane={} unanswered_count=0 oldest=none", p.id);
                    }
                } else {
                    println!("pane={} unanswered_count=unknown oldest=none", p.id);
                }
            }
        }
        return Ok(());
    }
    if list {
        for w in options.tmux().snapshot()? {
            println!(
                "{}:{} {} {}{} ({}x{}, {} panes)",
                w.session,
                w.index,
                w.id,
                w.tab_style.marker,
                w.name,
                w.width,
                w.height,
                w.panes.len()
            );
        }
        return Ok(());
    }
    let result = app::App::run(options);
    if terminal::STOP.load(std::sync::atomic::Ordering::Relaxed) {
        Ok(())
    } else {
        result
    }
}
