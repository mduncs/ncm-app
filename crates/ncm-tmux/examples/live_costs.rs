//! Read-only live-server timings using one size-neutral control client.
use anyhow::Result;
use ncm_tmux::{CaptureHashes, CaptureScheduler, Tmux, POLL_INTERVAL};
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let tmux = std::env::args()
        .nth(1)
        .map_or_else(Tmux::live, |s| Tmux::live().with_session(s));
    let start = Instant::now();
    let windows = tmux.snapshot()?;
    let snapshot_ms = start.elapsed().as_secs_f64() * 1000.0;
    let panes: Vec<_> = windows
        .iter()
        .flat_map(|w| &w.panes)
        .map(|p| p.id.clone())
        .collect();
    let start = Instant::now();
    let captures = tmux.capture(&panes)?;
    let capture_ms = start.elapsed().as_secs_f64() * 1000.0;
    println!(
        "snapshot_ms={snapshot_ms:.3} capture_all_ms={capture_ms:.3} pane_count={}",
        panes.len()
    );
    let mut scheduler = CaptureScheduler::default();
    let mut hashes = CaptureHashes::default();
    hashes.changed(&captures);
    let poll = tmux.poll()?;
    scheduler.schedule(&poll, None, Instant::now());
    let start = Instant::now();
    let mut polls = 0;
    let mut captures = 0;
    let mut changed = 0;
    let mut capture_ms = 0.0;
    // No selected pane: this measures the idle background round robin after initial capture.
    let mut deadline = start + POLL_INTERVAL;
    while deadline <= start + Duration::from_secs(10) {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        if let Some(update) = scheduler.tick(&tmux, None, Instant::now())? {
            polls += 1;
            captures += update.plan.panes.len();
            let capture_start = Instant::now();
            changed += hashes.changed(&tmux.capture(&update.plan.panes)?).len();
            capture_ms += capture_start.elapsed().as_secs_f64() * 1000.0;
        }
        deadline += POLL_INTERVAL;
    }
    println!("idle_seconds={:.3} polls={polls} captures={captures} changed_captures={changed} capture_ms={capture_ms:.3}", start.elapsed().as_secs_f64());
    Ok(())
}
