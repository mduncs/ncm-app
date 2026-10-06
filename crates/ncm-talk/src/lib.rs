//! Harness session discovery, incremental transcript transport, and pure Talk projections.
//! Live sources are read-only; adapters retain compact turn ancestry and presentation facts.

mod adapters;
mod discovery;
mod follower;
mod processes;
mod projection;
mod screen;
mod time;

pub use adapters::{adapter_for, ClaudeAdapter, CodexAdapter};
pub use discovery::{harness_for_command, ProcessInfo, ProcessTree, SessionDiscovery};
pub use follower::{
    TranscriptBatch, TranscriptEvents, TranscriptFollower, UnansweredDecision, UnansweredRecord,
};
pub use screen::{merge_facts, screen_facts};
pub use time::{now, parse_timestamp};

use anyhow::Result;
use ncm_model::{Facts, SessionRef, TalkEvent, Timestamp};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

/// Stateful harness adapter. Claude classification must honor origin/turnOrigin/parentUuid,
/// queued commands, legacy human records, and skip sidechains/tool results. Codex has its own
/// record schema. Keep ancestry between batches; a queued human prompt redirects later text.
pub trait TranscriptAdapter {
    /// Consume one record; non-message records may update facts without producing events.
    fn consume(&mut self, record: &Value) -> Result<Vec<TalkEvent>>;
    /// Text/tool activity can answer pending humans even without a presentation event.
    fn answers_pending(&self, _record: &Value, events: &[TalkEvent]) -> bool {
        events.iter().any(|event| {
            matches!(
                event,
                TalkEvent::Reply { .. } | TalkEvent::ReplyToReport { .. }
            )
        })
    }
    /// Latest model/context/now/subagent facts, with screen parsing used only as fallback.
    fn facts(&self) -> Facts;
    /// Configure process lifetime and original subagent paths for live Claude facts.
    fn set_live_context(&mut self, _session: &SessionRef, _started: Option<Timestamp>) {}
    /// Clear cached ancestry/facts after a follower reset or session switch.
    fn reset(&mut self);
}

/// A Talk left-column entry; report runs collapse and user replies remain unabridged data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TalkBlock {
    /// Human message or reply to a human. View wrapping/truncation (~9 lines) is app presentation.
    Message(TalkEvent),
    /// Quiet summary directing the user to Reports; trailing report runs also appear.
    Reports {
        /// Number of agent reports.
        reports: usize,
        /// Number of assistant replies to reports.
        replies: usize,
    },
}

/// One right-column report summary; render at most two lines of `latest_body`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportSummary {
    /// Reporting agent's name.
    pub agent: String,
    /// Report count across the followed session.
    pub count: usize,
    /// Timestamp of its latest report, when known.
    pub timestamp: Option<Timestamp>,
    /// Unabridged latest body.
    pub latest_body: String,
}

/// Human message without subsequent assistant text/tool activity. Unknown timestamps still mean unanswered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unanswered {
    /// Oldest unanswered user's timestamp.
    pub timestamp: Option<Timestamp>,
    /// Elapsed time, or absent when timestamp is missing.
    pub age: Option<Duration>,
    /// Retained for presentation compatibility; assistant replies now clear attention.
    pub replies_to_reports: usize,
}

impl Unanswered {
    /// Card pink badge appears only after strictly more than twenty seconds; Talk shows a banner
    /// immediately even when the age is unknown. Assistant text/tool activity clears attention.
    pub fn show_card_badge(&self) -> bool {
        self.age.is_some_and(|age| age > Duration::from_secs(20))
    }
}

/// Immutable projection of chronological events for the Talk/Reports tabs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TalkView {
    /// Talk left column: human/reply messages with collapsed report runs.
    pub blocks: Vec<TalkBlock>,
    /// Every report/reply-to-report in original file order.
    pub reports: Vec<TalkEvent>,
    /// One summary per reporting agent, newest first; unknown times sort last.
    pub by_agent: Vec<ReportSummary>,
    /// Oldest human message without later assistant activity.
    pub unanswered: Option<Unanswered>,
}

impl TalkView {
    /// Build a pure projection. Input must be in transcript order; timestamps are not used to
    /// reorder turns. Reports alone do not clear attention; any assistant reply does.
    pub fn from_events(events: &[TalkEvent], now: Timestamp) -> Self {
        let mut view = Self::default();
        let mut reports = 0;
        let mut replies = 0;
        let mut summaries = HashMap::<String, ReportSummary>::new();
        for event in events {
            match event {
                TalkEvent::Report {
                    agent,
                    body,
                    timestamp,
                } => {
                    reports += 1;
                    let summary = summaries.entry(agent.clone()).or_insert(ReportSummary {
                        agent: agent.clone(),
                        count: 0,
                        timestamp: None,
                        latest_body: String::new(),
                    });
                    summary.count += 1;
                    summary.timestamp = *timestamp;
                    summary.latest_body.clone_from(body);
                    view.reports.push(event.clone());
                }
                TalkEvent::ReplyToReport { .. } => {
                    replies += 1;
                    view.reports.push(event.clone());
                }
                _ => {
                    if reports + replies > 0 {
                        view.blocks.push(TalkBlock::Reports { reports, replies });
                        reports = 0;
                        replies = 0;
                    }
                    view.blocks.push(TalkBlock::Message(event.clone()));
                }
            }
        }
        if reports + replies > 0 {
            view.blocks.push(TalkBlock::Reports { reports, replies });
        }
        view.by_agent = summaries.into_values().collect();
        view.by_agent.sort_by(|a, b| {
            b.timestamp
                .cmp(&a.timestamp)
                .then_with(|| a.agent.cmp(&b.agent))
        });
        view.unanswered = unanswered(events, now);
        view
    }
}

/// Find the oldest human prompt since the last assistant reply, queued prompts included.
pub fn unanswered(events: &[TalkEvent], now: Timestamp) -> Option<Unanswered> {
    let after_reply = events
        .iter()
        .rposition(|event| {
            matches!(
                event,
                TalkEvent::Reply { .. } | TalkEvent::ReplyToReport { .. }
            )
        })
        .map_or(0, |index| index + 1);
    let last = after_reply
        + events[after_reply..]
            .iter()
            .position(|event| matches!(event, TalkEvent::You { .. }))?;
    let timestamp = events[last].timestamp();
    Some(Unanswered {
        timestamp,
        age: timestamp.map(|time| time.age_at(now)),
        replies_to_reports: events[last + 1..]
            .iter()
            .filter(|event| matches!(event, TalkEvent::ReplyToReport { .. }))
            .count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ncm_model::{Harness, Rgb, SessionRef};
    use std::{
        fs,
        io::Write,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn incremental_follower_handles_partial_lines_and_truncation() -> Result<()> {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/talk-fixtures");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!(
            "{}-{}.jsonl",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<()> {
            fs::write(&path, b"{\"fixture\":1}\n{\"fixture\":")?;
            let session = SessionRef {
                harness: Harness {
                    name: "synthetic".into(),
                    color: Rgb::default(),
                },
                session_id: "fixture".into(),
                transcript_path: path.clone(),
            };
            let mut follower = TranscriptFollower::new(&session);
            let first = follower.poll()?;
            assert_eq!(first.records, vec![serde_json::json!({"fixture":1})]);
            assert!(!first.reset);
            let offset = follower.offset();
            assert!(follower.poll()?.records.is_empty());
            assert_eq!(follower.offset(), offset);
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)?
                .write_all(b"2}\n")?;
            assert_eq!(
                follower.poll()?.records,
                vec![serde_json::json!({"fixture":2})]
            );
            fs::write(&path, b"{}\n")?;
            assert!(follower.poll()?.reset);
            Ok(())
        })();
        fs::remove_file(path)?;
        result
    }

    #[test]
    fn any_assistant_reply_clears_queued_human() {
        let mut events = vec![
            TalkEvent::You {
                timestamp: Some(Timestamp(1000)),
                body: "queued prompt".into(),
            },
            TalkEvent::Report {
                timestamp: Some(Timestamp(2000)),
                agent: "worker".into(),
                body: "done".into(),
            },
            TalkEvent::ReplyToReport {
                timestamp: Some(Timestamp(3000)),
                agent: Some("worker".into()),
                body: "ack".into(),
            },
        ];
        let view = TalkView::from_events(&events, Timestamp(22000));
        assert_eq!(view.blocks.len(), 2);
        assert_eq!(view.reports.len(), 2);
        assert_eq!(view.by_agent[0].count, 1);
        assert!(view.unanswered.is_none());
        events.push(TalkEvent::Reply {
            timestamp: None,
            body: "human answer".into(),
        });
        assert!(unanswered(&events, Timestamp(23000)).is_none());
    }
}
