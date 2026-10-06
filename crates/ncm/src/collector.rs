use crate::{
    app::{direct_grid, direct_pane},
    demo,
    input::State,
    layout::Rect,
    metrics::Metrics,
    view,
    view::{CardData, Cards},
};
use anyhow::Result;
use ncm_model::*;
use ncm_render::TintResolver;
use ncm_talk::{SessionDiscovery, TalkView};
use ncm_term::{compose_window_with_styles, LineParser};
use ncm_tmux::{CaptureHashes, CaptureScheduler, Tmux};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
        Arc, Condvar, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub struct Snapshot {
    pub windows: Vec<WindowInfo>,
    pub composites: HashMap<WindowId, Arc<Grid>>,
    pub data: Cards,
    pub changed: HashSet<WindowId>,
    pub dirty: bool,
    pub error: Option<String>,
    pub captures: usize,
}
pub struct Collector {
    state: Arc<Mutex<State>>,
    wake: Arc<Condvar>,
    pub results: Receiver<Snapshot>,
    stop: Arc<AtomicBool>,
    pub visible: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Collector {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Collector {
    pub fn set_state(&self, state: &State, paused: bool) {
        let mut current = self.state.lock().unwrap();
        let pause_changed = self.paused.swap(paused, Ordering::Relaxed) != paused;
        let changed = current.quick != state.quick
            || (state.quick && (current.selected != state.selected || current.pane != state.pane))
            || pause_changed;
        *current = state.clone();
        if changed {
            self.wake.notify_one();
        }
    }
    pub fn set_visible(&self, visible: bool) {
        if self.visible.swap(visible, Ordering::Relaxed) != visible {
            self.wake.notify_one();
        }
    }
    pub fn start(
        tmux: Tmux,
        log: Option<&std::path::Path>,
        initially_visible: bool,
    ) -> Result<Self> {
        let state = Arc::new(Mutex::new(State::default()));
        let wake = Arc::new(Condvar::new());
        let waker = wake.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, results) = mpsc::sync_channel(1);
        let metrics = Metrics::new(false, log)?;
        let visible = Arc::new(AtomicBool::new(initially_visible));
        let visibility = visible.clone();
        let paused = Arc::new(AtomicBool::new(false));
        let pause = paused.clone();
        let selection = state.clone();
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("ncm-collector".into())
            .spawn(move || {
                #[cfg(test)]
                crate::panic_log::inject("ncm-collector");
                let mut backend = Backend::new(tmux, metrics);

                let mut facts_at = Instant::now();
                let mut initialized = false;
                let mut hidden = false;
                while !stopped.load(Ordering::Relaxed)
                    && !crate::terminal::STOP.load(Ordering::Relaxed)
                {
                    let tick = Instant::now();
                    let shown = visibility.load(Ordering::Relaxed);
                    if initialized && (!shown || pause.load(Ordering::Relaxed)) {
                        if !hidden {
                            backend.tmux.suspend_queries();
                            backend.metrics.trace("collector_hidden", "");
                        }
                        hidden = true;
                        let waiting = selection.lock().unwrap();
                        let _waiting = waker.wait_timeout(waiting, Duration::from_secs(1));
                        continue;
                    }
                    if hidden {
                        backend.scheduler = CaptureScheduler::default();
                        facts_at = tick;
                        backend.dirty = true;
                        backend.metrics.trace("collector_resumed", "");
                        hidden = false;
                    }
                    initialized = true;
                    let selected = selection.lock().unwrap().clone();
                    if selected.quick
                        && (!backend.state.quick
                            || selected.selected != backend.state.selected
                            || selected.pane != backend.state.pane)
                    {
                        // A newly opened/changed Quick Look needs its lazy Talk projection now.
                        facts_at = tick;
                    }
                    backend.state = selected;
                    let collecting = shown && !pause.load(Ordering::Relaxed);
                    let error = backend.poll(collecting).err().map(|e| {
                        backend.metrics.trace("poll_error", &format!("{e:#}"));
                        e.to_string()
                    });
                    let snapshot = Snapshot {
                        windows: backend.windows.clone(),
                        composites: backend.composites.clone(),
                        data: backend.data.clone(),
                        changed: backend.changed.clone(),
                        dirty: backend.dirty,
                        error,
                        captures: backend.metrics.captures,
                    };
                    match tx.try_send(snapshot) {
                        Ok(()) => {
                            backend.changed.clear();
                            backend.dirty = false;
                            backend.metrics.captures = 0;
                        }
                        Err(mpsc::TrySendError::Disconnected(_)) => break,
                        Err(_) => {}
                    }
                    // Publish screen captures before transcript catch-up can delay them.
                    if tick >= facts_at && collecting {
                        backend.facts();
                        facts_at = Instant::now()
                            + if backend.talk_source.catching_up() {
                                Duration::from_millis(250)
                            } else {
                                Duration::from_secs(1)
                            };
                    }
                    let next = selection.lock().unwrap();
                    if next.quick != backend.state.quick
                        || (next.quick
                            && (next.selected != backend.state.selected
                                || next.pane != backend.state.pane))
                        || pause.load(Ordering::Relaxed)
                    {
                        continue;
                    }
                    let remaining = (tick + backend.scheduler.poll_interval())
                        .saturating_duration_since(Instant::now());
                    let _waiting = waker.wait_timeout(next, remaining);
                }
            })?;
        Ok(Self {
            state,
            wake,
            results,
            stop,
            visible,
            paused,
            thread: Some(thread),
        })
    }
}
struct Backend {
    tmux: Tmux,
    state: State,
    demo: bool,
    windows: Vec<WindowInfo>,
    scheduler: CaptureScheduler,
    hashes: CaptureHashes,
    borders: HashMap<WindowId, BorderStyles>,
    captures: HashMap<PaneId, Vec<String>>,
    panes: HashMap<PaneId, Grid>,
    pane_tints: HashMap<PaneId, Rgb>,
    composites: HashMap<WindowId, Arc<Grid>>,
    data: Cards,
    discovery: SessionDiscovery,
    talk_source: crate::talk_source::TalkSource,
    metrics: Metrics,
    changed: HashSet<WindowId>,
    dirty: bool,
}
impl Backend {
    fn new(tmux: Tmux, metrics: Metrics) -> Self {
        Self {
            tmux,
            state: State::default(),
            demo: false,
            windows: vec![],
            scheduler: CaptureScheduler::default(),
            hashes: CaptureHashes::default(),
            borders: HashMap::new(),
            captures: HashMap::new(),
            panes: HashMap::new(),
            pane_tints: HashMap::new(),
            composites: HashMap::new(),
            data: HashMap::new(),
            discovery: SessionDiscovery::default(),
            talk_source: Default::default(),
            metrics,
            changed: HashSet::new(),
            dirty: true,
        }
    }
    fn now(&self) -> Timestamp {
        Timestamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as i64,
        )
    }
    fn poll(&mut self, collecting: bool) -> Result<()> {
        let started = Instant::now();
        let target = self
            .state
            .target(&self.windows)
            .filter(|_| self.state.quick)
            .map(|p| p.id.as_str());
        let Some(scheduled) = self.scheduler.tick(&self.tmux, target, started)? else {
            return Ok(());
        };
        self.hashes.retain(&scheduled.poll);
        let windows = scheduled.poll.windows;
        let selected_id = self.windows.get(self.state.selected).map(|w| w.id.clone());
        let plan = scheduled.plan;
        let visible = windows
            .iter()
            .flat_map(|w| w.panes.iter().map(|p| p.id.clone()))
            .collect::<HashSet<_>>();
        let mut ids = plan.panes;
        let tints = TintResolver::new(&windows);
        let metadata_changed = self.windows != windows;
        let mut force_parse = HashSet::new();
        for p in windows.iter().flat_map(|w| &w.panes) {
            let tint = tints.resolve(p);
            if self.pane_tints.insert(p.id.clone(), tint) != Some(tint) {
                ids.push(p.id.clone());
                self.panes.remove(&p.id);
                force_parse.insert(p.id.clone());
            }
        }
        for w in &windows {
            let old = self.windows.iter().find(|old| old.id == w.id);
            if old.is_none_or(|old| {
                old.width != w.width || old.height != w.height || old.panes != w.panes
            }) {
                // Dimension/tint changes require reparsing even when bytes hash the same.
                for p in &w.panes {
                    if old
                        .and_then(|w| w.panes.iter().find(|old| old.id == p.id))
                        .is_none_or(|old| {
                            old.width != p.width || old.height != p.height || old.tint != p.tint
                        })
                    {
                        ids.push(p.id.clone());
                        self.panes.remove(&p.id);
                        force_parse.insert(p.id.clone());
                    }
                }
            }
        }
        ids.sort();
        ids.dedup();
        if !collecting {
            ids.clear();
        }
        let captures = match self.tmux.capture(&ids) {
            Ok(captures) => captures,
            Err(error) => {
                self.scheduler.retry(&ids, Instant::now());
                return Err(error);
            }
        };
        self.metrics.captures += captures.len();
        let content_changed: HashSet<_> = self.hashes.changed(&captures).into_iter().collect();
        let mut changed = HashSet::new();
        for w in &windows {
            for p in &w.panes {
                if let Some(lines) = captures.get(&p.id) {
                    let direct = direct_pane(w).is_some();
                    if content_changed.contains(&p.id)
                        || force_parse.contains(&p.id)
                        || !self.composites.contains_key(&w.id)
                    {
                        let tint = tints.resolve(p);
                        let incremental = !force_parse.contains(&p.id)
                            && !plan.geometry_changed.contains(&w.id)
                            && self.captures.contains_key(&p.id)
                            && self.composites.contains_key(&w.id);
                        if incremental {
                            update_capture_rows(
                                Arc::make_mut(self.composites.get_mut(&w.id).unwrap()),
                                w,
                                p,
                                lines,
                                &self.captures[&p.id],
                                tint,
                            );
                        } else if direct {
                            self.panes.remove(&p.id);
                            self.composites.insert(
                                w.id.clone(),
                                Arc::new(direct_grid(
                                    w,
                                    p,
                                    lines,
                                    tint,
                                    &self.borders.get(&w.id).copied().unwrap_or_default(),
                                )?),
                            );
                        } else {
                            let parsed =
                                LineParser { tint: Some(tint) }.grid(lines, p.width, p.height)?;
                            if plan.geometry_changed.contains(&w.id)
                                || !self.composites.contains_key(&w.id)
                            {
                                self.panes.insert(p.id.clone(), parsed);
                            } else if let Some(composite) = self.composites.get_mut(&w.id) {
                                update_composite_pane(Arc::make_mut(composite), w, p, &parsed);
                            }
                        }
                        self.captures.insert(p.id.clone(), lines.clone());
                        changed.insert(w.id.clone());
                    }
                }
            }
        }
        self.panes.retain(|id, _| visible.contains(id));
        self.pane_tints.retain(|id, _| visible.contains(id));
        self.captures.retain(|id, _| visible.contains(id));
        self.talk_source.retain(&visible);
        for w in &windows {
            let geometry = self
                .windows
                .iter()
                .find(|old| old.id == w.id)
                .is_none_or(|old| {
                    old.width != w.width
                        || old.height != w.height
                        || old
                            .panes
                            .iter()
                            .map(|p| (&p.id, p.x, p.y, p.width, p.height))
                            .collect::<Vec<_>>()
                            != w.panes
                                .iter()
                                .map(|p| (&p.id, p.x, p.y, p.width, p.height))
                                .collect::<Vec<_>>()
                });
            if !self.borders.contains_key(&w.id) && w.panes.len() > 1 {
                self.borders
                    .insert(w.id.clone(), self.tmux.pane_border_styles(&w.id)?);
            }
            if changed.contains(&w.id) || geometry || plan.geometry_changed.contains(&w.id) {
                if direct_pane(w).is_none()
                    && (geometry
                        || plan.geometry_changed.contains(&w.id)
                        || !self.composites.contains_key(&w.id))
                {
                    // Pane grids exist only while rebuilding geometry; steady updates blit
                    // into the shared composite and clone only while another worker holds it.
                    let mut panes = HashMap::new();
                    for p in &w.panes {
                        if let Some(grid) = self.panes.remove(&p.id) {
                            panes.insert(p.id.clone(), grid);
                        } else if let Some(lines) = self.captures.get(&p.id) {
                            panes.insert(
                                p.id.clone(),
                                LineParser {
                                    tint: Some(tints.resolve(p)),
                                }
                                .grid(lines, p.width, p.height)?,
                            );
                        }
                    }
                    let styles = self.borders.get(&w.id).cloned().unwrap_or_default();
                    self.composites.insert(
                        w.id.clone(),
                        Arc::new(compose_window_with_styles(w, &panes, &styles)?),
                    );
                }
                self.changed.insert(w.id.clone());
                self.dirty = true;
            }
        }
        self.panes.clear();
        self.windows = windows;
        if let Some(id) = selected_id {
            self.state.selected = self.windows.iter().position(|w| w.id == id).unwrap_or(
                self.state
                    .selected
                    .min(self.windows.len().saturating_sub(1)),
            );
        } else {
            self.state.selected = self
                .state
                .selected
                .min(self.windows.len().saturating_sub(1));
        }
        let window_ids = self
            .windows
            .iter()
            .map(|w| w.id.clone())
            .collect::<HashSet<_>>();
        self.metrics.changed += changed.len();
        self.borders.retain(|id, _| window_ids.contains(id));
        self.composites.retain(|id, _| window_ids.contains(id));
        self.data.retain(|id, _| window_ids.contains(id));
        self.dirty |= metadata_changed;
        self.metrics.trace(
            "poll",
            &format!(
                "duration_ms={:.3} captures={} changed={}",
                started.elapsed().as_secs_f64() * 1000.0,
                captures.len(),
                changed.len()
            ),
        );
        Ok(())
    }
    fn facts(&mut self) {
        self.facts_with_tree(None);
    }
    fn facts_with_tree(&mut self, tree: Option<Arc<ncm_talk::ProcessTree>>) {
        let started = Instant::now();
        let now = self.now();
        for (i, w) in self.windows.iter_mut().enumerate() {
            let target = self
                .state
                .pane
                .as_ref()
                .filter(|_| self.state.selected == i)
                .and_then(|id| w.panes.iter().find(|p| &p.id == id))
                .or_else(|| w.active_pane())
                .map(|p| p.id.clone());
            let mut facts = Facts::default();
            let mut talk = Arc::default();
            let mut agents = Vec::new();
            let mut hidden = 0;
            for p in &mut w.panes {
                if self.demo {
                    if target.as_ref() == Some(&p.id) {
                        facts = demo::facts();
                        talk = Arc::new(TalkView::from_events(&demo::events(i), now));
                    }
                } else {
                    let screen = self.captures.get(&p.id).map(Vec::as_slice);
                    let pane_view = self.talk_source.poll(
                        &self.discovery,
                        &w.session,
                        p,
                        screen,
                        now,
                        crate::talk_source::PollMode {
                            project: self.state.quick
                                && self.state.selected == i
                                && target.as_ref() == Some(&p.id),
                            tree: tree.clone(),
                        },
                    );
                    p.harness = pane_view.harness;
                    agents.extend(pane_view.facts.subagents.clone());
                    hidden += pane_view.facts.hidden_count;
                    if target.as_ref() == Some(&p.id) {
                        facts = pane_view.facts;
                        talk = pane_view.talk;
                    }
                }
            }
            if !self.demo {
                facts.subagents = agents;
                facts.hidden_count = hidden;
            }
            if self
                .data
                .get(&w.id)
                .is_none_or(|old| old.facts != facts || !Arc::ptr_eq(&old.talk, &talk))
            {
                self.dirty = true;
            }
            self.data.insert(w.id.clone(), CardData { facts, talk });
        }
        let work = std::mem::take(&mut self.talk_source.work);
        self.metrics.trace(
            "talk_work",
            &format!("discovery_ms={:.3} follow_ms={:.3} facts_ms={:.3} projection_ms={:.3} records={} bytes={} resets={} projections={}",
                work.discovery.as_secs_f64()*1000., work.follow.as_secs_f64()*1000., work.facts.as_secs_f64()*1000., work.projection.as_secs_f64()*1000.,
                work.records, work.bytes, work.resets, work.projections),
        );
        self.metrics.trace(
            "facts",
            &format!(
                "duration_ms={:.3} followed={} catching_up={}",
                started.elapsed().as_secs_f64() * 1000.0,
                self.talk_source.followed_count(),
                self.talk_source.catching_up()
            ),
        );
        if self.windows.iter().any(|w| w.activity.is_some()) {
            self.dirty = true;
        }
    }
}

fn update_composite_pane(
    composite: &mut Grid,
    window: &WindowInfo,
    pane: &PaneInfo,
    parsed: &Grid,
) {
    if window.flags.zoomed() && !pane.active {
        return;
    }
    view::blit(
        composite,
        parsed,
        Rect {
            x: pane.x,
            y: pane.y,
            width: pane.width.min(window.width.saturating_sub(pane.x)),
            height: pane.height.min(window.height.saturating_sub(pane.y)),
        },
        0,
    );
}

// tmux -e resets SGR independently per line. Reuse the composite's unchanged
// rows instead of allocating/parsing a whole pane for a changing cursor line.
fn update_capture_rows(
    composite: &mut Grid,
    window: &WindowInfo,
    pane: &PaneInfo,
    lines: &[String],
    previous: &[String],
    tint: Rgb,
) {
    if window.flags.zoomed() && !pane.active {
        return;
    }
    let parser = LineParser { tint: Some(tint) };
    for y in 0..pane.height.min(window.height.saturating_sub(pane.y)) {
        if lines.get(y) == previous.get(y) {
            continue;
        }
        for (x, cell) in parser
            .parse(lines.get(y).map_or("", String::as_str), pane.width)
            .into_iter()
            .take(window.width.saturating_sub(pane.x))
            .enumerate()
        {
            if let Some(slot) = composite.get_mut(pane.x + x, pane.y + y) {
                *slot = cell;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ncm_term::compose_window_with_styles;
    #[test]
    fn quick_look_selected_pane_discovers_adapter_and_gets_talk_and_facts() -> Result<()> {
        let pid = std::process::id();
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../target/collector-talk-{pid}"));
        std::fs::create_dir_all(dir.join("claude/sessions"))?;
        std::fs::create_dir_all(dir.join("claude/projects/project"))?;
        // Pane ids are numeric; select an inactive pane in a two-pane Quick Look window.
        let mut window = demo::windows().remove(0);
        window.session = "main".into();
        let mut screen_pane = window.panes[0].clone();
        screen_pane.id = "%41".into();
        screen_pane.pid = 2;
        screen_pane.active = true;
        screen_pane.harness = None;
        let mut talk_pane = screen_pane.clone();
        talk_pane.id = "%40".into();
        talk_pane.pid = 1;
        talk_pane.active = false;
        window.panes = vec![screen_pane, talk_pane];
        std::fs::write(
            dir.join(format!("claude/sessions/{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"collector\",\"tmux\":\"main:@7.%40\"}}"),
        )?;
        std::fs::write(
            dir.join(format!("claude/sessions/{pid}.hash.key")),
            "ignore",
        )?;
        std::fs::write(dir.join("claude/projects/project/collector.jsonl"), concat!(
            "{\"type\":\"user\",\"uuid\":\"u\",\"origin\":{\"kind\":\"human\"},\"message\":{\"content\":\"ask\"}}\n",
            "{\"type\":\"assistant\",\"parentUuid\":\"u\",\"message\":{\"model\":\"collector-model\",\"usage\":{\"input_tokens\":42},\"content\":[{\"type\":\"text\",\"text\":\"reply\"}]}}\n"
        ))?;
        let tree = Arc::new(ncm_talk::ProcessTree::parse(&format!(
            "1 0 zsh\n2 0 zsh\n{pid} 1 2.1.289\n"
        )));
        let mut backend = Backend::new(Tmux::live(), Metrics::new(false, None)?);
        backend.discovery = SessionDiscovery {
            claude_root: dir.join("claude"),
            codex_root: dir.join("codex"),
        };
        backend.state.quick = true;
        backend.state.pane = Some("%40".into());
        backend.windows = vec![window];
        backend.facts_with_tree(Some(tree));
        let data = &backend.data[&backend.windows[0].id];
        assert_eq!(backend.talk_source.followed_count(), 1);
        assert_eq!(data.talk.blocks.len(), 2);
        assert_eq!(data.facts.model.as_deref(), Some("collector-model"));
        assert_eq!(data.facts.context_tokens, Some(42));
        assert_eq!(
            backend.windows[0].panes[1].harness.as_ref().unwrap().name,
            "claude"
        );
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }
    #[test]
    fn collector_uses_quiet_backoff_and_only_forces_quick_look() -> Result<()> {
        let server = ncm_tmux::testing::PrivateServer::start(30, 8, "sleep 60")?;
        let mut backend = Backend::new(server.tmux().clone(), Metrics::new(false, None)?);
        for _ in 0..11 {
            backend.poll(true)?;
            assert!(backend.panes.is_empty(), "pane grids must remain transient");
            std::thread::sleep(Duration::from_millis(255));
        }
        assert_eq!(backend.scheduler.poll_interval(), Duration::from_secs(1));
        assert!(
            backend.metrics.captures <= 2,
            "grid selection forced extra captures"
        );
        let captures = backend.metrics.captures;
        backend.state.quick = true;
        backend.poll(true)?;
        assert_eq!(backend.metrics.captures, captures + 1);
        Ok(())
    }

    #[test]
    fn incremental_split_updates_match_full_composition_and_preserve_neighbors() -> Result<()> {
        let mut window = demo::windows().remove(0);
        window.width = 20;
        window.height = 6;
        let mut left = window.panes[0].clone();
        left.x = 0;
        left.y = 0;
        left.width = 9;
        left.height = 6;
        left.active = true;
        let mut right = left.clone();
        right.id = "%other".into();
        right.x = 10;
        right.width = 10;
        right.active = false;
        window.panes = vec![left.clone(), right.clone()];
        let styles = BorderStyles::default();
        let lines = vec!["old".into(); 6];
        let parser = LineParser {
            tint: Some(Rgb::new(10, 20, 30)),
        };
        let mut panes = HashMap::from([
            (
                left.id.clone(),
                parser.grid(&lines, left.width, left.height)?,
            ),
            (
                right.id.clone(),
                parser.grid(&vec!["neighbor".into(); 6], right.width, right.height)?,
            ),
        ]);
        let mut composite = compose_window_with_styles(&window, &panes, &styles)?;
        let updated = parser.grid(
            &vec!["\x1b[31mupdated 界界".into(); 6],
            left.width,
            left.height,
        )?;
        update_composite_pane(&mut composite, &window, &left, &updated);
        panes.insert(left.id.clone(), updated);
        assert_eq!(
            composite,
            compose_window_with_styles(&window, &panes, &styles)?
        );
        let next = vec!["\x1b[32monly first row 界\x1b[0m".into()];
        update_capture_rows(
            &mut composite,
            &window,
            &left,
            &next,
            &vec!["\x1b[31mupdated 界界".into(); 6],
            parser.tint.unwrap(),
        );
        panes.insert(
            left.id.clone(),
            parser.grid(&next, left.width, left.height)?,
        );
        assert_eq!(
            composite,
            compose_window_with_styles(&window, &panes, &styles)?
        );
        window.flags = WindowFlags {
            raw: "Z".into(),
            ..WindowFlags::default()
        };
        let before = composite.clone();
        update_composite_pane(&mut composite, &window, &right, &panes[&right.id]);
        assert_eq!(composite, before);
        Ok(())
    }
}
