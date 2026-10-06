//! Read-only discovery, incremental adapter transport, and cached conversation projections.
use ncm_model::{Facts, Harness, PaneInfo, SessionRef, TalkEvent, Timestamp};
use ncm_talk::{
    adapter_for, merge_facts, screen_facts, ProcessTree, SessionDiscovery, TalkView,
    TranscriptAdapter, TranscriptFollower,
};
use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};
struct Follow {
    session: SessionRef,
    follower: TranscriptFollower,
    adapter: Box<dyn TranscriptAdapter>,
    events: Vec<TalkEvent>,
    view: Arc<TalkView>,
    catching_up: bool,
    events_dirty: bool,
    card_view: Arc<TalkView>,
    facts: Facts,
    facts_checked: Instant,
    facts_at: Timestamp,
}
#[derive(Default)]
pub struct TalkSource {
    followed: HashMap<String, Follow>,
    empty: Arc<TalkView>,
    screens: HashMap<String, (u64, Facts)>,
    searched: HashMap<String, (Instant, u32, Option<Harness>)>,
    pub work: TalkWork,
}
pub struct PaneView {
    pub harness: Option<Harness>,
    pub facts: Facts,
    pub talk: Arc<TalkView>,
}
#[derive(Default)]
pub struct PollMode {
    pub project: bool,
    pub tree: Option<Arc<ProcessTree>>,
}
#[derive(Default)]
pub struct TalkWork {
    pub discovery: Duration,
    pub follow: Duration,
    pub facts: Duration,
    pub projection: Duration,
    pub records: usize,
    pub bytes: u64,
    pub resets: usize,
    pub projections: usize,
}
impl TalkSource {
    pub fn poll(
        &mut self,
        discovery: &SessionDiscovery,
        session_name: &str,
        pane: &PaneInfo,
        screen: Option<&[String]>,
        now: Timestamp,
        mode: PollMode,
    ) -> PaneView {
        let started = Instant::now();
        if self.searched.get(&pane.id).is_none_or(|(last, pid, _)| {
            *pid != pane.pid || last.elapsed() >= Duration::from_secs(3)
        }) {
            // A failed process scan leaves the previous session intact; a successful empty
            // discovery clears it, preventing a departed harness's transcript from lingering.
            if let Ok(tree) = mode.tree.map_or_else(ProcessTree::cached_shared, Ok) {
                let harness = tree.harness(pane.pid);
                if let Ok((session, started_at)) =
                    discovery.discover_pane_with_context(pane, session_name, &tree)
                {
                    match session {
                        Some(session)
                            if self
                                .followed
                                .get(&pane.id)
                                .is_none_or(|old| old.session != session) =>
                        {
                            if let Some(adapter) = adapter_for(&session) {
                                let mut follower = TranscriptFollower::new(&session);
                                follower.max_records = 1024;
                                self.followed.insert(
                                    pane.id.clone(),
                                    Follow {
                                        session,
                                        follower,
                                        adapter,
                                        events: Vec::new(),
                                        view: Arc::default(),
                                        catching_up: true,
                                        events_dirty: true,
                                        card_view: Arc::default(),
                                        facts: Facts::default(),
                                        facts_checked: Instant::now(),
                                        facts_at: now,
                                    },
                                );
                            }
                        }
                        None => {
                            self.followed.remove(&pane.id);
                        }
                        _ => {}
                    }
                    if let Some(follow) = self.followed.get_mut(&pane.id) {
                        follow.adapter.set_live_context(&follow.session, started_at);
                    }
                }
                self.searched
                    .insert(pane.id.clone(), (Instant::now(), pane.pid, harness));
            }
        }
        self.work.discovery += started.elapsed();
        let started = Instant::now();
        let lines = screen.unwrap_or_default();
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        lines.hash(&mut hash);
        let signature = hash.finish();
        if self
            .screens
            .get(&pane.id)
            .is_none_or(|(old, _)| *old != signature)
        {
            self.screens
                .insert(pane.id.clone(), (signature, screen_facts(lines)));
        }
        let fallback = self.screens[&pane.id].1.clone();
        let discovered = self.searched.get(&pane.id).and_then(|(_, _, h)| h.clone());
        // tmux overrides include the user's chosen color. Discovery fills missing metadata.
        let harness = pane.harness.clone().or(discovered);
        self.work.facts += started.elapsed();
        if let Some(follow) = self.followed.get_mut(&pane.id) {
            let started = Instant::now();
            let offset = follow.follower.offset();
            let batch = follow.follower.poll_adapter_bounded(
                &mut *follow.adapter,
                256,
                256 * 1024,
                16 * 1024,
            );
            self.work.follow += started.elapsed();
            let mut facts_changed = false;
            if let Ok(batch) = batch {
                self.work.records += batch.records_read;
                self.work.bytes += follow.follower.offset().saturating_sub(offset);
                self.work.resets += usize::from(batch.reset);
                follow.catching_up = batch.lines_read >= follow.follower.max_records;
                facts_changed = batch.reset || batch.records_read > 0;
                if batch.reset {
                    follow.events.clear();
                }
                if batch.reset || !batch.events.is_empty() {
                    follow.events_dirty = true;
                    follow.events.extend(batch.events);
                    // Retain bounded original events; construct Talk/Reports only for Quick Look.
                    let mut bytes = 0;
                    let keep = follow
                        .events
                        .iter()
                        .rev()
                        .take(256)
                        .take_while(|e| {
                            bytes += event_body(e).len();
                            bytes <= 256 * 1024
                        })
                        .count();
                    let remove = follow.events.len() - keep;
                    if remove > 0 {
                        follow.events.drain(..remove);
                    }
                }
            }
            let started = Instant::now();
            // New usage/tool records can change facts without emitting a message. Freshness
            // can change without new records, so refresh that cached snapshot every five seconds.
            if facts_changed || follow.facts_checked.elapsed() >= Duration::from_secs(5) {
                follow.facts = follow.adapter.facts();
                follow.facts_at = now;
                follow.facts_checked = Instant::now();
            }
            let mut facts = follow.facts.clone();
            for agent in &mut facts.subagents {
                if let Some(runtime) = &mut agent.running_time {
                    *runtime += follow.facts_at.age_at(now);
                }
            }
            let facts = merge_facts(facts, fallback);
            self.work.facts += started.elapsed();
            let started = Instant::now();
            let unanswered = follow.follower.unanswered_at(now);
            // Card overlays calculate age from the timestamp, so their small projection can
            // be reused until the attention identity or reply count changes.
            let same_attention = match (&follow.card_view.unanswered, &unanswered) {
                (None, None) => true,
                (Some(a), Some(b)) => {
                    a.timestamp == b.timestamp && a.replies_to_reports == b.replies_to_reports
                }
                _ => false,
            };
            if !same_attention {
                follow.events_dirty = true;
                follow.card_view = Arc::new(TalkView {
                    unanswered: unanswered.clone(),
                    ..Default::default()
                });
            }
            if mode.project && follow.events_dirty {
                let mut view = TalkView::from_events(&follow.events, now);
                view.unanswered = unanswered;
                follow.view = Arc::new(view);
                follow.events_dirty = false;
                self.work.projections += 1;
            }
            self.work.projection += started.elapsed();
            return PaneView {
                harness: harness.or_else(|| Some(follow.session.harness.clone())),
                facts,
                talk: if mode.project {
                    follow.view.clone()
                } else {
                    follow.card_view.clone()
                },
            };
        }
        PaneView {
            harness,
            facts: fallback,
            talk: self.empty.clone(),
        }
    }
    pub fn followed_count(&self) -> usize {
        self.followed.len()
    }
    pub fn catching_up(&self) -> bool {
        self.followed.values().any(|f| f.catching_up)
    }
    pub fn retain(&mut self, ids: &HashSet<String>) {
        self.followed.retain(|id, _| ids.contains(id));
        self.searched.retain(|id, _| ids.contains(id));
        self.screens.retain(|id, _| ids.contains(id));
    }
}
fn event_body(event: &TalkEvent) -> &str {
    match event {
        TalkEvent::You { body, .. }
        | TalkEvent::Reply { body, .. }
        | TalkEvent::Report { body, .. }
        | TalkEvent::ReplyToReport { body, .. } => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ncm_model::Rgb;
    #[test]
    fn live_boundary_projects_human_reports_facts_reset_and_harness_override() -> anyhow::Result<()>
    {
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/talk-integration");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}.jsonl", std::process::id()));
        std::fs::write(&path, concat!(
            "{\"type\":\"user\",\"uuid\":\"u\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"origin\":{\"kind\":\"human\"},\"message\":{\"content\":\"help\"}}\n",
            "{\"type\":\"user\",\"uuid\":\"r\",\"origin\":{\"kind\":\"peer\"},\"message\":{\"content\":\"<agent-message from=\\\"worker\\\">ready</agent-message>\"}}\n"
        ))?;
        let mut pane = crate::demo::windows().remove(0).panes.remove(0);
        pane.harness = Some(Harness {
            name: "claude".into(),
            color: Rgb::new(1, 2, 3),
        });
        let session = SessionRef {
            harness: pane.harness.clone().unwrap(),
            session_id: "fixture".into(),
            transcript_path: path.clone(),
        };
        let mut source = TalkSource::default();
        source
            .searched
            .insert(pane.id.clone(), (Instant::now(), pane.pid, None));
        source.followed.insert(
            pane.id.clone(),
            Follow {
                follower: TranscriptFollower::new(&session),
                adapter: adapter_for(&session).unwrap(),
                session,
                events: Vec::new(),
                view: Arc::default(),
                catching_up: true,
                events_dirty: true,
                card_view: Arc::default(),
                facts: Facts::default(),
                facts_checked: Instant::now(),
                facts_at: Timestamp(0),
            },
        );
        let now = Timestamp(1767225630000);
        let lines = vec!["Model: Opus 4.6 · Ctx: 20k".into()];
        let card = source.poll(
            &SessionDiscovery::default(),
            "main",
            &pane,
            Some(&lines),
            now,
            PollMode::default(),
        );
        assert!(card.talk.blocks.is_empty());
        assert!(card.talk.reports.is_empty());
        assert!(card.talk.unanswered.is_some());
        assert_eq!(source.work.projections, 0);
        let caught_up_offset = source.followed[&pane.id].follower.offset();
        assert_eq!(caught_up_offset, std::fs::metadata(&path)?.len());
        let view = source.poll(
            &SessionDiscovery::default(),
            "main",
            &pane,
            Some(&lines),
            now,
            PollMode {
                project: true,
                ..Default::default()
            },
        );
        assert_eq!(view.harness, pane.harness);
        assert_eq!(view.facts.model.as_deref(), Some("Opus 4.6"));
        assert_eq!(view.talk.reports.len(), 1);
        assert!(view.talk.unanswered.as_ref().unwrap().show_card_badge());
        let unchanged = source.poll(
            &SessionDiscovery::default(),
            "main",
            &pane,
            Some(&lines),
            now,
            PollMode {
                project: true,
                ..Default::default()
            },
        );
        assert!(Arc::ptr_eq(&view.talk, &unchanged.talk));
        assert_eq!(source.work.projections, 1);
        assert_eq!(
            source.followed[&pane.id].follower.offset(),
            caught_up_offset
        );
        // Metadata-only appended records update card facts without replaying history or
        // rebuilding the unchanged Talk/Reports projection.
        use std::io::Write;
        writeln!(std::fs::OpenOptions::new().append(true).open(&path)?, "{{\"type\":\"assistant\",\"message\":{{\"model\":\"changed-model\",\"usage\":{{\"input_tokens\":42}}}}}}")?;
        let appended = source.poll(
            &SessionDiscovery::default(),
            "main",
            &pane,
            Some(&lines),
            now,
            PollMode {
                project: true,
                ..Default::default()
            },
        );
        assert_eq!(appended.facts.model.as_deref(), Some("changed-model"));
        assert_eq!(appended.facts.context_tokens, Some(42));
        assert!(Arc::ptr_eq(&view.talk, &appended.talk));
        assert_eq!(source.work.projections, 1);
        // A tool-only answer emits no Talk text, but must refresh both attention views.
        std::fs::OpenOptions::new().append(true).open(&path)?.write_all(
            concat!(r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"fixture-tool","name":"Read","input":{"file_path":"fixture.txt"}}]}}"#, "\n").as_bytes(),
        )?;
        let answered_card = source.poll(
            &SessionDiscovery::default(),
            "main",
            &pane,
            Some(&lines),
            now,
            PollMode::default(),
        );
        assert!(answered_card.talk.unanswered.is_none());
        let answered_view = source.poll(
            &SessionDiscovery::default(),
            "main",
            &pane,
            Some(&lines),
            now,
            PollMode {
                project: true,
                ..Default::default()
            },
        );
        assert!(answered_view.talk.unanswered.is_none());
        assert_eq!(answered_view.talk.blocks, view.talk.blocks);
        assert_eq!(source.work.projections, 2);
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"message\":{\"content\":\"new\"}}\n",
        )?;
        let reset = source.poll(
            &SessionDiscovery::default(),
            "main",
            &pane,
            Some(&lines),
            now,
            PollMode {
                project: true,
                ..Default::default()
            },
        );
        assert!(reset.talk.reports.is_empty());
        std::fs::remove_file(path)?;
        Ok(())
    }
}
