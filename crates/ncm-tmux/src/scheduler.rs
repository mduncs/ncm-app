//! Notification-driven live captures with bounded per-card rates.
use super::*;
use ncm_model::WindowId;

/// Active metadata cadence and maximum Quick Look pane refresh interval.
pub const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Slow safety refresh bound, independent of activity-based captures.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(15);
const ROUND_ROBIN_INTERVAL: Duration = Duration::from_secs(14);

type Geometry = (
    usize,
    usize,
    Vec<(PaneId, usize, usize, usize, usize, bool)>,
);
type Structure = Vec<(String, WindowId, String, u32, Option<String>, Vec<PaneId>)>;

/// Captures due now and cache invalidations. Removed windows also appear in geometry changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturePlan {
    /// Stable ids to pass to `Tmux::capture`, with no duplicates.
    pub panes: Vec<PaneId>,
    /// Windows whose dimensions, pane rectangles, or active pane changed.
    pub geometry_changed: Vec<WindowId>,
    /// A window/pane was added, removed, reordered, renamed, or moved to another section.
    pub structure_changed: bool,
}

/// One due signature poll and its capture plan; the app can reuse its metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledPoll {
    /// Latest metadata (cached between full polls); no second snapshot query is needed.
    pub poll: Poll,
    /// Captures to perform and composites to invalidate.
    pub plan: CapturePlan,
}

/// Pure scheduling state, plus an optional read-only `tick` convenience method.
/// Initial panes are immediate; changed cards are limited to one capture per second.
/// Quick Look windows refresh every 250ms. Safety captures are staggered within 15s.
#[derive(Debug, Clone, Default)]
pub struct CaptureScheduler {
    detector: ChangeDetector,
    last_poll: Option<Instant>,
    cached_poll: Option<Poll>,
    quiet_ticks: usize,
    selection: Option<(PaneId, Instant)>,
    next_capture: HashMap<PaneId, Instant>,
    last_capture: HashMap<PaneId, Instant>,
    pending: HashSet<PaneId>,
    geometry: BTreeMap<WindowId, Geometry>,
    structure: Structure,
}

impl CaptureScheduler {
    /// Whether the next cheap poll is due. Empty/first state is immediately due.
    pub fn poll_due(&self, now: Instant) -> bool {
        self.last_poll
            .is_none_or(|last| now.saturating_duration_since(last) >= self.poll_interval())
    }

    pub fn poll_interval(&self) -> Duration {
        if self.quiet_ticks >= 8 && self.selection.is_none() {
            Duration::from_secs(1)
        } else {
            POLL_INTERVAL
        }
    }

    /// Drain pane notifications and refresh metadata over the shared connection.
    /// Signatures are only a fallback for caller-owned `schedule` snapshots;
    /// live output uses stable pane ids, including writes without cursor movement.
    pub fn tick(
        &mut self,
        tmux: &Tmux,
        selected: Option<&str>,
        now: Instant,
    ) -> Result<Option<ScheduledPoll>> {
        let events = tmux.control_events()?;
        if events.reset {
            self.last_capture.clear();
            self.cached_poll = None;
        }
        self.pending.extend(events.dirty);
        let poll = if events.metadata || self.cached_poll.is_none() || self.poll_due(now) {
            match tmux.poll() {
                Ok(poll) => poll,
                Err(error) => {
                    // Reconnects and metadata failures must not leave stale pane
                    // content treated as current on the next successful snapshot.
                    self.cached_poll = None;
                    self.last_capture.clear();
                    return Err(error);
                }
            }
        } else {
            self.cached_poll.clone().unwrap()
        };
        if events.metadata {
            // A new connection or layout/mode change needs new content. Ordinary
            // output does not invalidate other panes in the same window.
            for (id, signature) in &poll.signatures {
                if self
                    .cached_poll
                    .as_ref()
                    .and_then(|p| p.signatures.get(id))
                    .is_none_or(|old| old.size != signature.size)
                {
                    self.pending.insert(id.clone());
                    self.last_capture.remove(id);
                }
            }
        }
        let plan = self.schedule_inner(&poll, selected, now, true, false);
        self.cached_poll = Some(poll.clone());
        Ok(Some(ScheduledPoll { poll, plan }))
    }

    fn selected_window<'a>(poll: &'a Poll, selected: Option<&str>) -> HashSet<&'a str> {
        poll.windows
            .iter()
            .find(|w| w.panes.iter().any(|p| Some(p.id.as_str()) == selected))
            .map(|w| w.panes.iter().map(|p| p.id.as_str()).collect())
            .unwrap_or_default()
    }

    fn last_window_capture(&self, poll: &Poll, id: &str) -> Option<Instant> {
        poll.windows
            .iter()
            .find(|w| w.panes.iter().any(|p| p.id == id))
            .and_then(|w| {
                w.panes
                    .iter()
                    .filter_map(|p| self.last_capture.get(&p.id).copied())
                    .max()
            })
    }

    /// Schedule from a caller-owned poll, without I/O. Use a monotonic clock.
    /// Calls faster than 250ms still catch signatures, but do not repeatedly capture selection.
    pub fn schedule(&mut self, poll: &Poll, selected: Option<&str>, now: Instant) -> CapturePlan {
        self.schedule_inner(poll, selected, now, true, true)
    }

    fn schedule_inner(
        &mut self,
        poll: &Poll,
        selected: Option<&str>,
        now: Instant,
        acknowledge: bool,
        detect_signatures: bool,
    ) -> CapturePlan {
        let poll_due = self.poll_due(now);
        let interval = self.poll_interval();
        if poll_due {
            // Keep the original cadence instead of accumulating subprocess/sleep jitter.
            self.last_poll = Some(self.last_poll.map_or(now, |last| {
                let remainder =
                    now.saturating_duration_since(last).as_nanos() % interval.as_nanos();
                now - Duration::from_nanos(remainder as u64)
            }));
        }
        self.next_capture
            .retain(|id, _| poll.signatures.contains_key(id));
        self.last_capture
            .retain(|id, _| poll.signatures.contains_key(id));
        let mut ids: Vec<_> = poll.signatures.keys().cloned().collect();
        ids.sort();
        let count = ids.len().max(1) as u32;
        for (index, id) in ids.iter().enumerate() {
            self.next_capture.entry(id.clone()).or_insert_with(|| {
                now + ROUND_ROBIN_INTERVAL.mul_f64((index + 1) as f64 / count as f64)
            });
        }
        self.pending.retain(|id| poll.signatures.contains_key(id));
        let changed = if detect_signatures {
            self.detector.changed(poll, None)
        } else {
            poll.signatures
                .keys()
                .filter(|id| !self.last_capture.contains_key(*id))
                .cloned()
                .collect()
        };
        self.pending.extend(changed.iter().cloned());
        let selected_panes = Self::selected_window(poll, selected);
        let mut captures: HashSet<_> = self
            .pending
            .iter()
            .filter(|id| {
                let cadence = if selected_panes.contains(id.as_str()) {
                    POLL_INTERVAL
                } else {
                    Duration::from_secs(1)
                };
                self.last_window_capture(poll, id)
                    .is_none_or(|last| now.saturating_duration_since(last) >= cadence)
            })
            .cloned()
            .collect();
        if poll_due {
            self.quiet_ticks = if changed.is_empty() {
                self.quiet_ticks.saturating_add(1)
            } else {
                0
            };
        }
        if let Some(id) = selected.filter(|id| poll.signatures.contains_key(*id)) {
            let deadline = match &self.selection {
                Some((old, deadline)) if old == id => *deadline,
                _ => now,
            };
            if now >= deadline {
                captures.extend(selected_panes.iter().map(|id| (*id).to_owned()));
                let remainder =
                    now.saturating_duration_since(deadline).as_nanos() % POLL_INTERVAL.as_nanos();
                self.selection = Some((
                    id.into(),
                    now - Duration::from_nanos(remainder as u64) + POLL_INTERVAL,
                ));
            }
        } else {
            self.selection = None;
        }
        for id in &ids {
            if detect_signatures
                && self.next_capture[id] <= now
                && self.last_window_capture(poll, id).is_none_or(|last| {
                    now.saturating_duration_since(last) >= Duration::from_secs(1)
                })
            {
                captures.insert(id.clone());
            }
        }
        // Keep the initial staggering for the first full capture. Later output changes reset
        // that pane's deadline: they already satisfy its content refresh guarantee.
        for id in captures.iter().filter(|_| acknowledge) {
            self.pending.remove(id);
            if self.last_capture.insert(id.clone(), now).is_some() {
                self.next_capture
                    .insert(id.clone(), now + ROUND_ROBIN_INTERVAL);
            }
        }
        let geometry: BTreeMap<_, _> = poll
            .windows
            .iter()
            .map(|w| {
                (
                    w.id.clone(),
                    (
                        w.width,
                        w.height,
                        w.panes
                            .iter()
                            .map(|p| (p.id.clone(), p.x, p.y, p.width, p.height, p.active))
                            .collect(),
                    ),
                )
            })
            .collect();
        let mut geometry_changed: Vec<_> = geometry
            .iter()
            .filter(|(id, value)| self.geometry.get(*id) != Some(*value))
            .map(|(id, _)| id.clone())
            .collect();
        geometry_changed.extend(
            self.geometry
                .keys()
                .filter(|id| !geometry.contains_key(*id))
                .cloned(),
        );
        geometry_changed.sort();
        let structure: Structure = poll
            .windows
            .iter()
            .map(|w| {
                (
                    w.session.clone(),
                    w.id.clone(),
                    w.name.clone(),
                    w.index,
                    w.group.clone(),
                    w.panes.iter().map(|p| p.id.clone()).collect(),
                )
            })
            .collect();
        let structure_changed = structure != self.structure;
        if !geometry_changed.is_empty() || structure_changed {
            self.quiet_ticks = 0;
        }
        self.geometry = geometry;
        self.structure = structure;
        let mut panes: Vec<_> = captures.into_iter().collect();
        panes.sort();
        CapturePlan {
            panes,
            geometry_changed,
            structure_changed,
        }
    }

    /// Make failed captures due on the next poll. Successful plans need no acknowledgment.
    pub fn retry(&mut self, panes: &[PaneId], now: Instant) {
        for id in panes {
            self.next_capture.insert(id.clone(), now);
            self.last_capture.remove(id);
            self.pending.insert(id.clone());
        }
    }
}

/// Content hashes for skipping parse/render of identical captures, including forced refreshes.
#[derive(Debug, Clone, Default)]
pub struct CaptureHashes {
    hashes: HashMap<PaneId, u64>,
}

impl CaptureHashes {
    /// Return only panes whose ANSI lines changed. Incremental captures retain other hashes.
    pub fn changed(&mut self, captures: &Captures) -> Vec<PaneId> {
        let mut changed = Vec::new();
        for (id, lines) in captures {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            lines.hash(&mut hasher);
            let hash = hasher.finish();
            if self.hashes.insert(id.clone(), hash) != Some(hash) {
                changed.push(id.clone());
            }
        }
        changed.sort();
        changed
    }

    /// Evict removed panes after a structure change.
    pub fn retain(&mut self, poll: &Poll) {
        self.hashes.retain(|id, _| poll.signatures.contains_key(id));
    }
}
