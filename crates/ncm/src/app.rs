use crate::{
    demo,
    input::{Action, Input, Prompt, State},
    layout::{Layout, Rect},
    terminal::{Capabilities, Terminal, STOP},
    view::{self, Cards},
};
use anyhow::Result;
use crossterm::{
    event::{Event, KeyEventKind, MouseButton, MouseEventKind},
    terminal,
};
use ncm_model::*;
use ncm_render::{braille_thumbnail, KittyEncoder, ThumbnailSize, TintResolver};
use ncm_term::{compose_window, LineParser};
use ncm_tmux::Tmux;
use std::{
    collections::HashMap,
    io::{self, IsTerminal},
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const IMAGE_INTERVAL: Duration = Duration::from_millis(1000);
const TRAFFIC: f64 = 500_000.0;
const BURST: f64 = 100_000.0;
struct Card {
    base: Grid,
    dirty: bool,
    last: Option<Instant>,
    image_ids: Vec<u32>,
    tile_rows: usize,
    edge: Option<Rgb>,
    pending: Option<Vec<u8>>,
    pending_offset: usize,
    redraw_after_pending: bool,
}
pub struct App {
    demo: bool,
    session: String,
    tmux: Tmux,
    state: State,
    windows: Vec<WindowInfo>,
    client: ncm_tmux::View,
    using: bool,
    read_only: bool,
    visible: bool,
    probe_deadline: Option<Instant>,
    metrics: crate::metrics::Metrics,
    image_cleanup: Vec<u8>,
    first_frame: bool,
    next_image: usize,
    upload_window: Option<WindowId>,
    captures: HashMap<PaneId, Vec<String>>,
    panes: HashMap<PaneId, Grid>,
    composites: HashMap<WindowId, Arc<Grid>>,
    data: Cards,
    cards: HashMap<WindowId, Card>,
    size: (usize, usize),
    layout: Layout,
    caps: Capabilities,
    pictures: crate::pictures::Pictures,
    picture_busy: Option<String>,
    generation: u64,
    layout_epoch: u64,
    quick_card: Option<Card>,
    quick_target: Option<String>,
    quick_grid: Option<Arc<Grid>>,
    output: crate::output::Output,
    pending_input: bool,
    loaded: bool,

    error: String,
    dirty: bool,
    budget: f64,
    budget_at: Instant,
    startup_until: Option<Instant>,
}
impl App {
    pub fn run(options: crate::Options) -> Result<()> {
        let mut metrics = crate::metrics::Metrics::new(options.stats, options.log.as_deref())?;
        let tmux = options.tmux();
        let session = if options.demo {
            "demo".into()
        } else {
            tmux.current_session()?
        };
        let tmux = tmux.with_session(&session);
        let demo_mode = options.demo;
        anyhow::ensure!(
            io::stdin().is_terminal() && io::stdout().is_terminal(),
            "ncm needs a terminal; use --list for read-only output"
        );
        let mut terminal_guard = Terminal::enter()?;
        let caps = Capabilities::pending();
        metrics.trace("capabilities", &format!("graphics={}", caps.graphics));
        let size = terminal::size()?;
        let mut app = Self {
            demo: demo_mode,
            session,
            client: tmux.view(),
            tmux,
            state: State::default(),
            windows: Vec::new(),
            using: false,
            read_only: options.read_only,
            visible: true,
            probe_deadline: None,
            metrics,
            image_cleanup: Vec::new(),
            first_frame: true,
            next_image: 0,
            upload_window: None,
            captures: HashMap::new(),
            panes: HashMap::new(),
            composites: HashMap::new(),
            data: HashMap::new(),
            cards: HashMap::new(),
            size: (size.0.into(), size.1.into()),
            layout: Layout::default(),
            caps,
            pictures: crate::pictures::Pictures::start(options.log.as_deref())?,
            picture_busy: None,
            generation: 0,
            layout_epoch: 0,
            quick_card: None,
            quick_target: None,
            quick_grid: None,
            output: crate::output::Output::start(options.log.as_deref())?,
            pending_input: false,
            loaded: demo_mode,
            error: String::new(),
            dirty: true,
            budget: 600_000.0,
            budget_at: Instant::now(),
            startup_until: None,
        };
        if demo_mode {
            app.windows = demo::windows();
            app.captures = demo::captures(&app.windows);
            let tints = TintResolver::new(&app.windows);
            for w in &app.windows {
                for p in &w.panes {
                    app.panes.insert(
                        p.id.clone(),
                        LineParser {
                            tint: Some(tints.resolve(p)),
                        }
                        .grid(&app.captures[&p.id], p.width, p.height)?,
                    );
                }
            }
            for (i, w) in app.windows.iter().enumerate() {
                app.data.insert(
                    w.id.clone(),
                    view::CardData {
                        facts: demo::facts(),
                        talk: Arc::new(ncm_talk::TalkView::from_events(
                            &demo::events(i),
                            demo::NOW,
                        )),
                    },
                );
                app.composites
                    .insert(w.id.clone(), Arc::new(compose_window(w, &app.panes)?));
            }
            app.panes.clear();
            app.captures.clear();
        }
        app.relayout()?;
        app.draw()?;
        let collector = if app.demo {
            None
        } else {
            Some(crate::collector::Collector::start(
                app.tmux.clone(),
                options.log.as_deref(),
                app.visible,
            )?)
        };
        let mut reader = crate::input::Reader::start()?;
        if !app.caps.tmux {
            app.caps = Capabilities::send_probe()?;
            app.probe_deadline = Some(Instant::now() + PROBE_TIMEOUT);
            app.metrics.trace("probe_sent", "attempt=1");
        }
        let mut stats = Instant::now() + Duration::from_secs(1);
        let viewer_tmux = if !app.demo && app.tmux.is_viewer_server() {
            app.tmux.clone()
        } else {
            Tmux::live()
        };
        let viewer_pane = app
            .caps
            .tmux
            .then(ncm_tmux::own_pane)
            .flatten()
            .and_then(|p| p.into_string().ok());
        let mut viewer_at = Instant::now();
        #[cfg(test)]
        crate::panic_log::inject("main");
        while !STOP.load(Ordering::Relaxed) {
            if !crate::terminal::tty_alive() {
                break;
            }
            let now = Instant::now();
            if let Some(collector) = &collector {
                if let Ok(snapshot) = collector.results.try_recv() {
                    app.accept(snapshot)?;
                }
                collector.set_state(&app.state, app.using);
            }
            if now >= viewer_at {
                if let Some(pane) = &viewer_pane {
                    match viewer_tmux.viewer_state_queued(pane) {
                        Ok(viewer) => app.visible = viewer.visible,
                        Err(error) => app.metrics.trace("viewer_error", &error.to_string()),
                    }
                }
                viewer_at = Instant::now() + Duration::from_secs(1);
            }
            if app.probe_deadline.is_some_and(|deadline| now >= deadline) {
                app.probe_deadline = None;
                app.caps.pending = false;
                app.caps.reason = "braille: no graphics reply".into();
                app.metrics.trace("probe_timeout", "attempt=1");
                app.relayout()?;
            }
            if let Some(collector) = &collector {
                collector.set_visible(app.visible);
            }
            if now >= stats {
                app.metrics.sample();
                if app.metrics.show {
                    app.dirty = true;
                }
                stats = Instant::now() + Duration::from_secs(1);
            }
            app.output.pause(!app.visible, app.using);
            if app.using {
                // ncm owns this terminal directly. Hand both descriptors to the real client.
                reader.pause(true);
                app.output.suspend();
                if let Some(collector) = &collector {
                    collector.set_visible(false);
                    collector.set_state(&app.state, true);
                }
                terminal_guard.suspend();
                let result = if let Some(pane) = app.state.target(&app.windows) {
                    app.client.attach(
                        &app.session,
                        &app.windows[app.state.selected].id,
                        &pane.id,
                        || STOP.load(Ordering::Relaxed) || !crate::terminal::tty_alive(),
                    )
                } else {
                    Ok(())
                };
                app.using = false;
                if STOP.load(Ordering::Relaxed) {
                    break;
                }
                terminal_guard.resume()?;
                reader.pause(false);
                app.visible = true;
                app.output.pause(false, false);
                if let Some(collector) = &collector {
                    collector.set_visible(true);
                    collector.set_state(&app.state, false);
                }
                if let Err(error) = result {
                    app.set_error(error.to_string());
                }
                let size = terminal::size()?;
                app.size = (size.0.into(), size.1.into());
                app.reset_uploads();
                app.relayout()?;
                app.metrics.trace("use_closed", "");
            }
            // Drain queued keys before drawing: held arrows coalesce into one frame.
            let mut input = false;
            let mut quit = false;
            let drain_start = Instant::now();
            while !STOP.load(Ordering::Relaxed) {
                let Some(event) = reader.next() else {
                    break;
                };
                let event = match event {
                    Input::Event(event) => event,
                    Input::GraphicsReply => {
                        app.confirm_graphics()?;
                        continue;
                    }
                    Input::Pixels(pixels) => {
                        if app.caps.pixels != pixels {
                            app.caps.pixels = pixels;
                            app.relayout()?;
                        }
                        continue;
                    }
                };
                input = true;
                let action = match event {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        app.metrics.trace("input", &format!("key={:?}", key.code));
                        app.error.clear();
                        app.dirty = true;
                        if app.read_only
                            && matches!(
                                key.code,
                                crossterm::event::KeyCode::Char('i' | ',' | 'x' | 'n')
                            )
                        {
                            app.set_error("read-only: tmux actions disabled".into());
                            Action::None
                        } else {
                            app.state.key(key, &app.windows, app.layout.columns)
                        }
                    }
                    Event::Paste(text) => {
                        if let Prompt::Rename(_, name) = &mut app.state.prompt {
                            name.extend(text.chars().filter(|c| !c.is_control()));
                            app.dirty = true;
                        }
                        Action::None
                    }
                    Event::Mouse(mouse)
                        if matches!(app.state.prompt, Prompt::None | Prompt::Menu(_)) =>
                    {
                        app.mouse(mouse)?;
                        Action::None
                    }
                    Event::Resize(w, h) => {
                        app.size = (w.into(), h.into());
                        app.relayout()?;
                        Action::None
                    }
                    _ => Action::None,
                };
                if app.action(action)? {
                    quit = true;
                    break;
                }
                app.mark_selection();
                if app.using || drain_start.elapsed() >= Duration::from_millis(12) {
                    break;
                }
            }
            if quit || STOP.load(Ordering::Relaxed) {
                break;
            }
            if app.using {
                // Preserve the initiating input until the first ncm frame after
                // native detach; it cannot be painted while tmux owns the tty.
                app.pending_input |= input;
                continue;
            }
            if (app.visible || app.dirty || input)
                && (input
                    || app.dirty
                    || app.cards.values().any(|c| {
                        c.dirty
                            && (c.pending.is_some()
                                || c.last.is_none_or(|t| t.elapsed() >= IMAGE_INTERVAL))
                    })
                    || app
                        .quick_card
                        .as_ref()
                        .is_some_and(|c| c.pending.is_some() || c.dirty)
                    || app.picture_busy.is_some())
            {
                app.pending_input |= input;
                app.draw()?;
            }
            let picture_work = app.visible
                && app.caps.graphics
                && (app.picture_busy.is_some()
                    || app.cards.values().any(|c| c.pending.is_some())
                    || app
                        .quick_card
                        .as_ref()
                        .is_some_and(|c| c.pending.is_some() || c.dirty));
            // Keys still wake the receiver immediately. Prepared uploads need a short
            // timer only while bytes are available; avoid 200 redraws/s while refilling.
            reader.wait(Duration::from_millis(if !picture_work {
                100
            } else if app.picture_busy.is_none() && app.budget < 4096. {
                20
            } else {
                5
            }));
        }
        let encoder = KittyEncoder::default();
        let mut cleanup = Vec::new();
        for card in app.cards.values().chain(app.quick_card.iter()) {
            for id in &card.image_ids {
                cleanup.extend(encoder.delete(*id)?);
            }
        }
        app.output.finish(&cleanup);
        Ok(())
    }
    fn now(&self) -> Timestamp {
        if self.demo {
            demo::NOW
        } else {
            Timestamp(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
            )
        }
    }
    fn set_error(&mut self, error: String) {
        if self.error != error {
            self.error = error;
            self.dirty = true;
        }
    }
    fn accept(&mut self, snapshot: crate::collector::Snapshot) -> Result<()> {
        self.loaded = true;
        let selected = self.windows.get(self.state.selected).map(|w| w.id.clone());
        let layout_changed = self
            .windows
            .iter()
            .map(|w| (&w.id, w.width, w.height))
            .collect::<Vec<_>>()
            != snapshot
                .windows
                .iter()
                .map(|w| (&w.id, w.width, w.height))
                .collect::<Vec<_>>();
        self.windows = snapshot.windows;
        if let Some(id) = selected {
            self.state.selected = self.windows.iter().position(|w| w.id == id).unwrap_or(0);
        }
        self.state.selected = self
            .state
            .selected
            .min(self.windows.len().saturating_sub(1));
        self.composites = snapshot.composites;
        self.data = snapshot.data;
        self.metrics.captures += snapshot.captures;
        self.metrics.changed += snapshot.changed.len();
        for id in snapshot.changed {
            if let Some(card) = self.cards.get_mut(&id) {
                card.dirty = true;
                if card.pending.is_some() || self.picture_busy.as_ref() == Some(&id) {
                    card.redraw_after_pending = true;
                } else {
                    card.pending = None;
                }
            }
        }
        if let Some(error) = snapshot.error {
            self.set_error(error);
        }
        if layout_changed {
            self.relayout()?;
        }
        self.dirty |= snapshot.dirty;
        self.mark_selection();
        Ok(())
    }
    fn confirm_graphics(&mut self) -> Result<()> {
        if self.caps.tmux {
            return Ok(());
        }
        if self.caps.confirmed && !self.caps.pending {
            return Ok(());
        }
        self.probe_deadline = None;
        self.caps.confirmed = true;
        self.caps.graphics = true;
        self.caps.pending = false;
        self.caps.reason.clear();
        self.metrics
            .trace("capabilities", "graphics=true pending=false reason=");
        self.reset_uploads();
        self.relayout()
    }
    fn reset_uploads(&mut self) {
        self.generation += 1;
        self.quick_grid = None;
        if let Some(card) = &mut self.quick_card {
            card.dirty = true;
            card.pending = None;
            card.pending_offset = 0;
            card.last = None;
        }
        self.upload_window = None;
        for card in self.cards.values_mut() {
            card.dirty = true;
            card.pending = None;
            card.pending_offset = 0;
            card.redraw_after_pending = false;
            card.last = None;
        }
        self.output.invalidate();
        self.dirty = true;
    }
    fn relayout(&mut self) -> Result<()> {
        self.layout_epoch += 1;
        self.layout = Layout::new(self.size.0, self.size.1, &self.windows, self.caps.pixels);
        if self.caps.graphics
            && self.layout.cards.iter().any(|r| {
                r.width * usize::from(self.caps.pixels.0) <= 8
                    || r.height * usize::from(self.caps.pixels.1) <= 8
            })
        {
            self.caps.graphics = false;
            self.caps.reason = "braille: terminal too small for image insets".into();
        }
        let old_encoder = KittyEncoder::default();
        if !self.caps.tmux {
            for card in self.cards.values() {
                for id in &card.image_ids {
                    self.image_cleanup.extend(old_encoder.delete(*id)?);
                }
            }
        }
        self.upload_window = None;
        self.generation += 1;
        self.quick_card = None;
        self.quick_target = None;
        self.cards.clear();
        let mut next_id = 1;
        let mut tile_rows = 4;
        while tile_rows < 64
            && self
                .layout
                .cards
                .iter()
                .map(|r| r.height.div_ceil(tile_rows))
                .sum::<usize>()
                > 199
        {
            tile_rows *= 2;
        }
        let encoder = KittyEncoder::default();
        for (w, r) in self.windows.iter().zip(&self.layout.cards) {
            if r.width == 0 || r.height == 0 {
                continue;
            }
            let mut base = view::frame(r.width, r.height);
            let mut ids = Vec::new();
            if self.caps.graphics {
                for top in (0..r.height).step_by(tile_rows) {
                    let h = (r.height - top).min(tile_rows);
                    if next_id > 199 {
                        self.caps.graphics = false;
                        self.caps.reason = "braille: more than 254 image placements".into();
                        break;
                    }
                    match encoder.placeholders(next_id, r.width, h) {
                        Ok(grid) => {
                            view::blit(
                                &mut base,
                                &grid,
                                Rect {
                                    x: 0,
                                    y: top,
                                    width: r.width,
                                    height: h,
                                },
                                0,
                            );
                            ids.push(next_id);
                            next_id += 1;
                        }
                        Err(error) => {
                            self.caps.graphics = false;
                            self.caps.reason = format!("braille: {error}");
                            break;
                        }
                    }
                }
            }
            self.cards.insert(
                w.id.clone(),
                Card {
                    base,
                    dirty: true,
                    last: None,
                    image_ids: ids,
                    tile_rows,
                    edge: view::window_edge(w),
                    pending: None,
                    pending_offset: 0,
                    redraw_after_pending: false,
                },
            );
        }
        self.dirty = true;
        Ok(())
    }
    fn mark_selection(&mut self) {
        for w in &self.windows {
            if let Some(card) = self.cards.get_mut(&w.id) {
                if card.edge != view::window_edge(w) {
                    card.dirty = true;
                    if card.pending.is_some() {
                        card.redraw_after_pending = true;
                    } else {
                        card.pending = None;
                    }
                    card.edge = view::window_edge(w);
                }
            }
        }
    }
    fn update_pictures(&mut self) -> Result<()> {
        if let Ok(picture) = self.pictures.results.try_recv() {
            self.picture_busy = None;
            if picture.generation == self.generation {
                let bytes = picture.bytes?;
                let card = if picture.key == "__quick" {
                    self.quick_card.as_mut()
                } else {
                    self.cards.get_mut(&picture.key)
                };
                if let Some(card) = card {
                    card.pending = Some(bytes);
                }
            }
        }
        if self.state.quick
            && self.quick_target.as_ref().is_some_and(|id| {
                self.windows
                    .get(self.state.selected)
                    .is_none_or(|w| &w.id != id)
            })
        {
            self.retire_quick();
        }
        if self.state.quick
            && self.quick_target.is_none()
            && self
                .quick_card
                .as_ref()
                .is_some_and(|c| c.pending.is_some())
        {
            return Ok(());
        }
        if self.state.quick {
            if let Some(w) = self.windows.get(self.state.selected) {
                let panel = Layout::panel(
                    self.size.0,
                    self.size
                        .1
                        .saturating_sub(Layout::dock_rows(self.caps.pixels.1).saturating_sub(1)),
                    (0, 0),
                    true,
                );
                let cells = (
                    panel.width.saturating_sub(2),
                    panel.height.saturating_sub(2),
                );
                if cells.0 > 1 && cells.1 > 1 {
                    if self.quick_target.as_ref() != Some(&w.id) || self.quick_card.is_none() {
                        // New target/placement must retransmit even if its pixels match an old peek.
                        self.generation += 1;
                        self.quick_target = Some(w.id.clone());
                        self.quick_grid = None;
                        let mut base = view::frame(cells.0, cells.1);
                        let tile_rows = if cells.1 <= 55 { 1 } else { 4 };
                        let ids = if self.caps.graphics {
                            (0..cells.1.div_ceil(tile_rows))
                                .map(|i| 200 + i as u32)
                                .collect::<Vec<_>>()
                        } else {
                            Vec::new()
                        };
                        if self.caps.graphics && ids.last().is_some_and(|id| *id <= 254) {
                            let encoder = KittyEncoder::default();
                            for (part, &id) in ids.iter().enumerate() {
                                let h = (cells.1 - part * tile_rows).min(tile_rows);
                                let grid = encoder.placeholders(id, cells.0, h)?;
                                view::blit(
                                    &mut base,
                                    &grid,
                                    Rect {
                                        x: 0,
                                        y: part * tile_rows,
                                        width: cells.0,
                                        height: h,
                                    },
                                    0,
                                );
                            }
                        }
                        self.quick_card = Some(Card {
                            base,
                            dirty: true,
                            last: None,
                            image_ids: ids,
                            tile_rows,
                            edge: None,
                            pending: None,
                            pending_offset: 0,
                            redraw_after_pending: false,
                        });
                    }
                    if let Some(grid) = self.composites.get(&w.id) {
                        if self
                            .quick_grid
                            .as_ref()
                            .is_none_or(|old| !Arc::ptr_eq(old, grid))
                        {
                            self.quick_grid = Some(grid.clone());
                            let card = self.quick_card.as_mut().unwrap();
                            card.dirty = true;
                            if card.pending.is_some()
                                || self.picture_busy.as_deref() == Some("__quick")
                            {
                                card.redraw_after_pending = true;
                            }
                            if !self.caps.graphics && !self.caps.pending {
                                card.base = braille_thumbnail(grid, cells.0, cells.1)?;
                                card.dirty = false;
                            }
                        }
                    }
                }
            }
        } else if self.quick_target.is_some() {
            self.retire_quick();
        } else if self
            .quick_card
            .as_ref()
            .is_some_and(|c| c.pending.is_none())
        {
            self.quick_card = None;
        }
        if !self.visible || !self.caps.graphics || self.picture_busy.is_some() {
            return Ok(());
        }
        let quick_ready = self.state.quick
            && self.state.tab == crate::input::Tab::Screen
            && self
                .quick_card
                .as_ref()
                .is_some_and(|c| c.dirty && c.pending.is_none());
        let key = if quick_ready {
            Some("__quick".to_owned())
        } else {
            (0..self.windows.len())
                .map(|offset| (self.next_image + offset) % self.windows.len())
                .find_map(|i| {
                    let w = &self.windows[i];
                    self.cards
                        .get(&w.id)
                        .filter(|c| {
                            c.dirty
                                && c.pending.is_none()
                                && c.last.is_none_or(|t| t.elapsed() >= IMAGE_INTERVAL)
                        })
                        .map(|_| w.id.clone())
                })
        };
        if let Some(key) = key {
            let quick = key == "__quick";
            let w = if quick {
                &self.windows[self.state.selected]
            } else {
                self.windows.iter().find(|w| w.id == key).unwrap()
            };
            let card = if quick {
                self.quick_card.as_mut().unwrap()
            } else {
                self.cards.get_mut(&key).unwrap()
            };
            if let Some(grid) = self.composites.get(&w.id) {
                let cells = (card.base.width(), card.base.height());
                self.pictures.requests.try_send(crate::pictures::Request {
                    key: key.clone(),
                    generation: self.generation,
                    layout_epoch: self.layout_epoch,
                    grid: grid.clone(),
                    window: w.clone(),
                    size: ThumbnailSize {
                        width: cells.0 as u32 * u32::from(self.caps.pixels.0),
                        height: cells.1 as u32 * u32::from(self.caps.pixels.1),
                        cell_width: f32::from(self.caps.pixels.0),
                        cell_height: f32::from(self.caps.pixels.1),
                    },
                    cells,
                    ids: card.image_ids.clone(),
                    tile_rows: card.tile_rows,
                    quick,
                })?;
                card.redraw_after_pending = false;
                self.picture_busy = Some(key);
            }
        }
        Ok(())
    }
    fn retire_quick(&mut self) {
        // Complete only an already-started strip; discard unsent strips on close.
        // Continuations have no image id and cannot interleave with grid uploads.
        if let Some(card) = &mut self.quick_card {
            if let Some(bytes) = &mut card.pending {
                let end = continuation_end(bytes, card.pending_offset);
                bytes.truncate(end);
                if end == card.pending_offset {
                    card.pending = None;
                }
            }
            card.dirty = false;
            card.redraw_after_pending = false;
        }
        self.quick_target = None;
        self.quick_grid = None;
        self.generation += 1;
    }
    fn draw(&mut self) -> Result<()> {
        let now = Instant::now();
        let rate = if self.state.quick {
            1_400_000.0
        } else {
            TRAFFIC
        };
        let refill_from = self
            .budget_at
            .max(self.startup_until.unwrap_or(self.budget_at));
        let capacity =
            if self.first_frame || self.startup_until.is_some_and(|deadline| now < deadline) {
                600_000.0
            } else {
                BURST
            };
        self.budget = (self.budget
            + now.saturating_duration_since(refill_from).as_secs_f64() * rate)
            .min(capacity);
        self.budget_at = now;
        let started = Instant::now();
        let mut quick_bytes = 0;
        let upload = self.visible && !self.caps.tmux && self.output.can_upload();
        let mut images = if upload {
            std::mem::take(&mut self.image_cleanup)
        } else {
            Vec::new()
        };
        self.update_pictures()?;
        let start_image = self
            .upload_window
            .as_ref()
            .and_then(|id| self.windows.iter().position(|w| &w.id == id))
            .unwrap_or(self.next_image);
        for offset in 0..self.windows.len() {
            let i = (start_image + offset) % self.windows.len();
            let w = &self.windows[i];
            let Some(r) = self.layout.cards.get(i) else {
                continue;
            };
            let Some(card) = self.cards.get_mut(&w.id) else {
                continue;
            };
            if !card.dirty
                || (card.pending.is_none()
                    && card.last.is_some_and(|t| t.elapsed() < IMAGE_INTERVAL))
            {
                continue;
            }
            let Some(grid) = self.composites.get(&w.id) else {
                continue;
            };
            if self.caps.graphics {
                if self
                    .quick_card
                    .as_ref()
                    .is_some_and(|c| c.pending.is_some())
                    && self.upload_window.is_none()
                {
                    continue;
                }
                if !upload {
                    continue;
                }
                let Some(encoded) = card.pending.take() else {
                    continue;
                };
                let end = upload_end(
                    &encoded,
                    card.pending_offset,
                    (self.budget as usize).min(16_384usize.saturating_sub(images.len())),
                );
                let bytes = end - card.pending_offset;
                self.budget -= bytes as f64;
                self.metrics.image_bytes += bytes;
                images.extend_from_slice(&encoded[card.pending_offset..end]);
                if end < encoded.len() {
                    let continuing = end < encoded.len() && !starts_image(&encoded, end);
                    card.pending_offset = end;
                    card.pending = Some(encoded);
                    if end > 0 {
                        // Kitty continuation chunks have no image id: finish this upload
                        // before starting another, even when the traffic budget spans ticks.
                        self.upload_window = continuing.then(|| w.id.clone());
                        break;
                    }
                    continue;
                }
                card.pending_offset = 0;
                self.upload_window = None;
            } else if !self.caps.pending {
                card.base = braille_thumbnail(grid, r.width, r.height)?;
                self.dirty = true;
            }
            card.dirty = card.redraw_after_pending;
            card.redraw_after_pending = false;
            card.last = Some(now);
            self.next_image = (i + 1) % self.windows.len();
        }
        if upload && self.upload_window.is_none() {
            if let Some(card) = &mut self.quick_card {
                if let Some(encoded) = card.pending.take() {
                    let end = upload_end(
                        &encoded,
                        card.pending_offset,
                        (self.budget as usize).min(16_384usize.saturating_sub(images.len())),
                    );
                    let bytes = end - card.pending_offset;
                    self.budget -= bytes as f64;
                    self.metrics.image_bytes += bytes;
                    images.extend_from_slice(&encoded[card.pending_offset..end]);
                    quick_bytes += bytes;
                    if end < encoded.len() {
                        card.pending_offset = end;
                        card.pending = Some(encoded);
                    } else {
                        card.pending_offset = 0;
                        card.last = Some(now);
                        card.dirty = card.redraw_after_pending;
                        card.redraw_after_pending = false;
                    }
                }
            }
        }
        if !self.dirty && !self.pending_input {
            self.metrics.writes += usize::from(!images.is_empty());
            self.output
                .submit(None, images, quick_bytes, false, &self.state);
            return Ok(());
        }
        let mut frame = view::frame(self.size.0, self.size.1);
        let timestamp = self.now();
        for (i, (w, r)) in self.windows.iter().zip(&self.layout.cards).enumerate() {
            if let Some(card) = self.cards.get(&w.id) {
                view::blit(&mut frame, &card.base, *r, 0);
            }
            let empty = view::empty_data();
            let data = self.data.get(&w.id).unwrap_or(&empty);
            if !self.caps.graphics {
                view::card_border(&mut frame, *r, w, i == self.state.selected);
            }
            view::card_overlay(&mut frame, *r, w, data, timestamp);
            if i == self.state.selected {
                view::selection(&mut frame, *r);
            }
        }
        if self.windows.is_empty() {
            view::text(
                &mut frame,
                0,
                0,
                self.size.0,
                if self.loaded {
                    "No windows in this session"
                } else {
                    "Loading windows…"
                },
                CellStyle {
                    fg: Some(Rgb::new(150, 156, 168)),
                    bg: Some(view::BACK),
                    ..Default::default()
                },
            );
        }
        let _ = if self.state.quick {
            self.windows.get(self.state.selected).and_then(|w| {
                let empty = view::empty_data();
                view::quick(
                    &mut frame,
                    w,
                    &mut self.state,
                    self.quick_card.as_ref().map(|c| &c.base),
                    self.data.get(&w.id).unwrap_or(&empty),
                    timestamp,
                    Layout::dock_rows(self.caps.pixels.1),
                )
            })
        } else {
            None
        };
        view::dock(
            &mut frame,
            &self.windows,
            &self.state,
            &if self.metrics.show {
                if self.caps.reason.is_empty() {
                    self.metrics.segment.clone()
                } else {
                    format!("{} · {}", self.caps.reason, self.metrics.segment)
                }
            } else {
                self.caps.reason.clone()
            },
            if self.loaded {
                &self.error
            } else {
                "Loading windows…"
            },
            Layout::dock_rows(self.caps.pixels.1),
        );
        if let Prompt::Menu(item) = self.state.prompt {
            view::menu(&mut frame, item);
        }
        let image_bytes = images.len();
        self.output.submit(
            Some(frame),
            images,
            quick_bytes,
            std::mem::take(&mut self.pending_input),
            &self.state,
        );
        self.metrics.writes += 1;
        self.metrics.trace(
            "prepared",
            &format!(
                "duration_ms={:.3} images={}",
                started.elapsed().as_secs_f64() * 1000.0,
                image_bytes
            ),
        );
        if self.first_frame && image_bytes > 0 {
            // The initial 600KB allowance consumes a whole second of traffic. Hold
            // replenishment until one second after that flush, then use the small bucket.
            self.startup_until = Some(Instant::now() + Duration::from_secs(1));
        }
        self.first_frame = false;
        self.dirty = false;
        Ok(())
    }
    fn action(&mut self, action: Action) -> Result<bool> {
        if action == Action::Quit {
            return Ok(true);
        }
        if self.read_only && !matches!(action, Action::None) {
            self.set_error("read-only: tmux actions disabled".into());
            return Ok(false);
        }
        if self.demo {
            if !matches!(action, Action::None) {
                self.set_error("demo: tmux action preview only".into());
            }
            return Ok(false);
        }
        let pane = self.state.target(&self.windows).map(|p| p.id.clone());
        let result = match action {
            Action::Use | Action::Select => {
                if let Some(pane) = pane {
                    if self.caps.tmux {
                        let window = &self.windows[self.state.selected];
                        self.tmux
                            .select_window_pane(&window.id, &pane)
                            .map(|_| false)
                    } else {
                        self.state.quick = true;
                        self.state.tab = crate::input::Tab::Screen;
                        self.state.pin_bottom();
                        self.using = true;
                        self.visible = false;
                        self.metrics.trace("use_open", "");
                        Ok(false)
                    }
                } else {
                    Ok(false)
                }
            }
            Action::Close(id) => self.tmux.kill_window(&id).map(|_| false),
            Action::Rename(id, name) => self.tmux.rename_window(&id, &name).map(|_| false),
            Action::New => self
                .tmux
                .new_window(&self.session, "new", "")
                .map(|_| false),
            _ => Ok(false),
        };
        match result {
            Ok(quit) => Ok(quit),
            Err(error) => {
                self.set_error(error.to_string());
                Ok(false)
            }
        }
    }
    fn mouse(&mut self, mouse: crossterm::event::MouseEvent) -> Result<()> {
        let x = usize::from(mouse.column);
        let y = usize::from(mouse.row);
        if matches!(self.state.prompt, Prompt::Menu(_)) {
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                let rect = view::menu_rect(self.size.0, self.size.1);
                let action = if rect.contains(x, y) && (rect.y + 1..rect.y + 4).contains(&y) {
                    self.state.menu_action(y - rect.y - 1, &self.windows)
                } else {
                    self.state.prompt = Prompt::None;
                    Action::None
                };
                self.dirty = true;
                self.action(action)?;
            }
            return Ok(());
        }
        if self.state.quick {
            let Some(w) = self.windows.get(self.state.selected) else {
                return Ok(());
            };
            let panel = Layout::panel(
                self.size.0,
                self.size
                    .1
                    .saturating_sub(Layout::dock_rows(self.caps.pixels.1).saturating_sub(1)),
                (w.width, w.height),
                self.state.tab == crate::input::Tab::Screen,
            );
            if panel.contains(x, y) {
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    self.action(Action::Use)?;
                }
                if mouse.kind == MouseEventKind::ScrollUp {
                    self.state.scroll_up(3);
                    self.dirty = true;
                }
                if mouse.kind == MouseEventKind::ScrollDown {
                    self.state.scroll_down(3);
                    self.dirty = true;
                }
                return Ok(());
            }
        }
        let closing = self.state.quick && mouse.kind == MouseEventKind::Down(MouseButton::Left);
        if closing {
            self.state.quick = false;
            self.dirty = true;
            return Ok(());
        }
        if let Some(i) = self.layout.cards.iter().position(|r| r.contains(x, y)) {
            if matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
            ) {
                self.state.select(i);
                self.dirty = true;
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    self.state.quick = true;
                    self.state.pin_bottom();
                } else if !self.demo && !self.read_only {
                    self.state.quick = false;
                    self.state.prompt = Prompt::Menu(0);
                }
            }
        }
        Ok(())
    }
}

// A whole-window pane can serve directly as the composite. Keep the compositor's
// bottom separator without allocating and cloning a second Grid.
pub(crate) fn direct_pane(window: &WindowInfo) -> Option<&PaneInfo> {
    (window.panes.len() == 1)
        .then(|| &window.panes[0])
        .filter(|p| {
            p.x == 0
                && p.y == 0
                && p.width == window.width
                && p.height > 0
                && (p.height == window.height || p.height + 1 == window.height)
        })
}
pub(crate) fn direct_grid(
    window: &WindowInfo,
    pane: &PaneInfo,
    lines: &[String],
    tint: Rgb,
    styles: &BorderStyles,
) -> Result<Grid> {
    let mut grid = LineParser { tint: Some(tint) }.grid(lines, window.width, window.height)?;
    for y in pane.height..window.height {
        for x in 0..window.width {
            *grid.get_mut(x, y).expect("padding bounds") = Cell {
                text: "─".into(),
                width: 1,
                style: if pane.active {
                    styles.active
                } else {
                    styles.normal
                },
            };
        }
    }
    Ok(grid)
}

// Encoded payloads are base64; scan escape bytes rather than comparing a prefix at
// every byte. Each candidate is still checked for its complete APC/DCS framing.
fn packet_starts(bytes: &[u8], start: usize) -> impl Iterator<Item = usize> + '_ {
    let prefix: &[u8] = b"\x1b_G";
    memchr::memchr_iter(b'\x1b', &bytes[start..])
        .filter(move |&i| i > 0 && bytes[start + i..].starts_with(prefix))
        .map(move |i| start + i)
}
fn starts_image(bytes: &[u8], start: usize) -> bool {
    let tail = &bytes[start..];
    let prefix_len = 3;
    let header_end = tail
        .iter()
        .skip(prefix_len)
        .position(|b| *b == b';')
        .map_or(tail.len(), |n| prefix_len + n);
    tail[..header_end].windows(3).any(|s| s == b"a=T")
}
// Never split an escape packet. Only look ahead as far as this tick's byte budget.
fn packet_end(bytes: &[u8], start: usize, budget: usize) -> usize {
    let limit = start.saturating_add(budget).min(bytes.len());
    if limit == bytes.len() {
        return limit;
    }
    packet_starts(bytes, start)
        .take_while(|&i| i <= limit)
        .last()
        .unwrap_or(start)
}
fn upload_end(bytes: &[u8], start: usize, budget: usize) -> usize {
    let candidate = packet_end(bytes, start, budget);
    // A strip beyond the eligible chunk cannot affect this upload. In particular, a
    // zero budget must not scan the remainder of a large pending image every tick.
    strip_end(&bytes[..candidate], start)
}
// Finish a started strip before switching images; Quick Look can preempt between strips.
fn strip_end(bytes: &[u8], start: usize) -> usize {
    packet_starts(bytes, start)
        .find(|&i| starts_image(bytes, i))
        .unwrap_or(bytes.len())
}
fn continuation_end(bytes: &[u8], start: usize) -> usize {
    if start >= bytes.len() || starts_image(bytes, start) {
        start
    } else {
        strip_end(bytes, start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_single_pane_matches_compositor_including_default_padding() -> Result<()> {
        let mut window = crate::demo::windows().remove(0);
        window.panes.truncate(1);
        let pane = window.panes[0].clone();
        window.width = pane.width;
        window.height = pane.height + 1;
        let tint = Rgb::new(1, 2, 3);
        let lines = vec!["\x1b[31mred 界\x1b[0m".into(), "last".into()];
        let panes = HashMap::from([(
            pane.id.clone(),
            LineParser { tint: Some(tint) }.grid(&lines, pane.width, pane.height)?,
        )]);
        assert_eq!(
            direct_grid(&window, &pane, &lines, tint, &BorderStyles::default())?,
            compose_window(&window, &panes)?
        );
        Ok(())
    }

    #[test]
    fn uploads_resume_only_at_complete_protocol_chunk_boundaries() -> Result<()> {
        let image = ncm_render::RgbaImage {
            width: 80,
            height: 80,
            pixels: (0..80 * 80 * 4)
                .map(|i| ((i * 97 + i / 13) % 256) as u8)
                .collect(),
        };
        {
            let encoder = KittyEncoder { chunk_size: 64 };
            let bytes = encoder.encode(&image, 1)?;
            let mut reconstructed = Vec::new();
            let mut start = 0;
            while start < bytes.len() {
                let end = packet_end(&bytes, start, 300);
                assert!(end > start);
                assert!(bytes[start..end].ends_with(b"\x1b\\"));
                reconstructed.extend_from_slice(&bytes[start..end]);
                start = end;
            }
            assert_eq!(reconstructed, bytes);
        }
        Ok(())
    }
    #[test]
    fn switching_images_finishes_continuations_but_discards_unsent_strips() -> Result<()> {
        let image = ncm_render::RgbaImage {
            width: 80,
            height: 4,
            pixels: (0..80 * 4 * 4)
                .map(|i| ((i * 97 + i / 13) % 256) as u8)
                .collect(),
        };
        {
            let encoder = KittyEncoder { chunk_size: 64 };
            let first = encoder.encode_placement(&image, 200, 80, 1)?;
            let second = encoder.encode_placement(&image, 201, 80, 1)?;
            let bytes = [first.as_slice(), second.as_slice()].concat();
            assert_eq!(strip_end(&bytes, 0), first.len());
            assert_eq!(continuation_end(&bytes, 0), 0);
            let chunk = packet_end(&bytes, 0, 160);
            assert!(chunk > 0 && chunk < first.len());
            assert_eq!(continuation_end(&bytes, chunk), first.len());
            assert_eq!(continuation_end(&bytes, first.len()), first.len());
            for start in [0, chunk, first.len()] {
                for budget in [0, 1, 63, 160, first.len(), bytes.len()] {
                    assert_eq!(
                        upload_end(&bytes, start, budget),
                        packet_end(&bytes, start, budget).min(strip_end(&bytes, start)),
                        "packet/strip boundaries must survive bounded lookahead"
                    );
                }
            }
        }
        Ok(())
    }
}
