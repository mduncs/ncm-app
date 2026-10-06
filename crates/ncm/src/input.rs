use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ncm_model::{AgentState, WindowInfo};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Screen,
    Talk,
    Reports,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Prompt {
    #[default]
    None,
    Close(String),
    Rename(String, String),
    Menu(usize),
}
#[derive(Clone, Debug, Default)]
pub struct State {
    pub selected: usize,
    pub quick: bool,
    pub tab: Tab,
    pub pane: Option<String>,
    pub scroll: usize,
    pub new_entries: bool,
    transcript: Option<std::sync::Arc<ncm_talk::TalkView>>,
    transcript_rows: usize,
    pub prompt: Prompt,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Select,
    Use,
    Close(String),
    Rename(String, String),
    New,
}
impl State {
    // Zero distance from the bottom is the pinned state. Never retain an absolute
    // opening anchor: lazy projections and transcript catch-up can keep growing.
    pub fn pin_bottom(&mut self) {
        self.scroll = 0;
        self.new_entries = false;
        self.transcript = None;
        self.transcript_rows = 0;
    }
    pub fn scroll_up(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_add(rows);
    }
    pub fn scroll_down(&mut self, rows: usize) {
        self.scroll = self.scroll.saturating_sub(rows);
        if self.scroll == 0 {
            self.new_entries = false;
        }
    }
    pub fn sync_transcript(
        &mut self,
        view: &std::sync::Arc<ncm_talk::TalkView>,
        rows: usize,
        height: usize,
    ) {
        if self.scroll > 0 {
            if let Some(previous) = &self.transcript {
                let changed = match self.tab {
                    Tab::Talk => {
                        previous.blocks.len() != view.blocks.len()
                            || previous.reports.len() != view.reports.len()
                            || previous.blocks.last() != view.blocks.last()
                            || previous.reports.last() != view.reports.last()
                    }
                    Tab::Reports => {
                        previous.reports.len() != view.reports.len()
                            || previous.reports.last() != view.reports.last()
                    }
                    Tab::Screen => false,
                };
                self.new_entries |= changed;
                // Preserve the reading position instead of moving with an appended tail.
                self.scroll = self
                    .scroll
                    .saturating_add(rows.saturating_sub(self.transcript_rows));
            }
            self.scroll = self.scroll.min(rows.saturating_sub(height));
        }
        if self.scroll == 0 {
            self.new_entries = false;
        }
        self.transcript = Some(view.clone());
        self.transcript_rows = rows;
    }
    pub fn target<'a>(&self, windows: &'a [WindowInfo]) -> Option<&'a ncm_model::PaneInfo> {
        let w = windows.get(self.selected)?;
        self.pane
            .as_ref()
            .and_then(|id| w.panes.iter().find(|p| &p.id == id))
            .or_else(|| w.active_pane())
    }
    pub fn select(&mut self, selected: usize) {
        if self.selected != selected {
            self.selected = selected;
            self.pane = None;
            self.pin_bottom();
        }
    }
    pub fn menu_action(&mut self, item: usize, windows: &[WindowInfo]) -> Action {
        self.prompt = Prompt::None;
        let Some(window) = windows.get(self.selected) else {
            return Action::None;
        };
        match item {
            0 => Action::Use,
            1 => {
                self.prompt = Prompt::Rename(window.id.clone(), String::new());
                Action::None
            }
            2 => {
                self.prompt = Prompt::Close(window.id.clone());
                Action::None
            }
            _ => Action::None,
        }
    }
    pub fn key(&mut self, key: KeyEvent, windows: &[WindowInfo], columns: usize) -> Action {
        match &mut self.prompt {
            Prompt::Close(id) => {
                let id = id.clone();
                self.prompt = Prompt::None;
                return if key.code == KeyCode::Char('y') {
                    Action::Close(id)
                } else {
                    Action::None
                };
            }
            Prompt::Rename(id, text) => {
                return match key.code {
                    KeyCode::Esc => {
                        self.prompt = Prompt::None;
                        Action::None
                    }
                    KeyCode::Enter => {
                        let action = Action::Rename(id.clone(), text.clone());
                        self.prompt = Prompt::None;
                        action
                    }
                    KeyCode::Backspace => {
                        text.pop();
                        Action::None
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.push(c);
                        Action::None
                    }
                    _ => Action::None,
                };
            }
            Prompt::Menu(item) => {
                let chosen = match key.code {
                    KeyCode::Esc => {
                        self.prompt = Prompt::None;
                        return Action::None;
                    }
                    KeyCode::Up => {
                        *item = item.saturating_sub(1);
                        None
                    }
                    KeyCode::Down => {
                        *item = (*item + 1).min(2);
                        None
                    }
                    KeyCode::Enter => Some(*item),
                    KeyCode::Char('r') => Some(1),
                    KeyCode::Char('x') => Some(2),
                    _ => None,
                };
                return chosen.map_or(Action::None, |item| self.menu_action(item, windows));
            }
            Prompt::None => {}
        }
        let n = windows.len();
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc => self.quick = false,
            KeyCode::Char(' ') if n > 0 => {
                self.quick = !self.quick;
                self.pin_bottom();
            }
            KeyCode::Char('i') if n > 0 => {
                self.quick = true;

                self.tab = Tab::Screen;
                self.pin_bottom();
                return Action::Use;
            }
            KeyCode::Enter if n > 0 => return Action::Select,
            KeyCode::Left => self.select(self.selected.saturating_sub(1)),
            KeyCode::Right if n > 0 => self.select((self.selected + 1).min(n - 1)),
            KeyCode::Up if self.quick => self.scroll_up(1),
            KeyCode::PageUp if self.quick => self.scroll_up(10),
            KeyCode::PageDown if self.quick => self.scroll_down(10),
            KeyCode::End if self.quick => self.pin_bottom(),
            KeyCode::Down if self.quick => self.scroll_down(1),
            KeyCode::Up => self.select(self.selected.saturating_sub(columns.max(1))),
            KeyCode::Down if n > 0 && self.selected / columns.max(1) < (n - 1) / columns.max(1) => {
                self.select((self.selected + columns.max(1)).min(n - 1))
            }
            KeyCode::Tab if n > 0 => {
                let mut alerts = (0..n)
                    .filter(|&i| alert_rank(&windows[i]) < 4)
                    .collect::<Vec<_>>();
                alerts.sort_by_key(|&i| (alert_rank(&windows[i]), i));
                if !alerts.is_empty() {
                    let next = alerts
                        .iter()
                        .position(|&i| i == self.selected)
                        .map(|i| (i + 1) % alerts.len())
                        .unwrap_or(0);
                    self.select(alerts[next]);
                }
            }
            KeyCode::Char('1') if self.quick => {
                self.tab = Tab::Screen;
                self.pin_bottom();
            }
            KeyCode::Char('2') if self.quick => {
                self.tab = Tab::Talk;
                self.pin_bottom();
            }
            KeyCode::Char('3') if self.quick => {
                self.tab = Tab::Reports;
                self.pin_bottom();
            }
            KeyCode::Char(c @ ('[' | ']')) if self.quick && n > 0 => {
                let w = &windows[self.selected];
                if !w.panes.is_empty() {
                    let i = self
                        .target(windows)
                        .and_then(|p| w.panes.iter().position(|s| s.id == p.id))
                        .unwrap_or(0);
                    self.pane = Some(
                        w.panes[(i + w.panes.len() + if c == ']' { 1 } else { w.panes.len() - 1 })
                            % w.panes.len()]
                        .id
                        .clone(),
                    );
                    self.pin_bottom();
                }
            }
            KeyCode::Char('x') if !self.quick && n > 0 => {
                self.prompt = Prompt::Close(windows[self.selected].id.clone())
            }
            KeyCode::Char(',') if !self.quick && n > 0 => {
                self.prompt = Prompt::Rename(windows[self.selected].id.clone(), String::new())
            }
            KeyCode::Char('n') if !self.quick => return Action::New,
            _ => {}
        }
        Action::None
    }
}
fn alert_rank(w: &WindowInfo) -> usize {
    if w.agent_state == AgentState::Needs {
        0
    } else if w.flags.bell {
        1
    } else if w.agent_state == AgentState::Done {
        2
    } else if w.flags.activity {
        3
    } else {
        4
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;
    fn k(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
    #[test]
    fn opening_switching_tabs_and_panes_repins() {
        let windows = demo::windows();
        let mut state = State::default();
        state.key(k(KeyCode::Char(' ')), &windows, 3);
        for key in [
            KeyCode::Char('2'),
            KeyCode::Char('3'),
            KeyCode::Char(']'),
            KeyCode::Char('['),
            KeyCode::Right,
        ] {
            state.key(k(KeyCode::Up), &windows, 3);
            assert!(state.scroll > 0);
            state.new_entries = true;
            state.key(k(key), &windows, 3);
            assert_eq!(state.scroll, 0);
            assert!(!state.new_entries);
        }
        state.key(k(KeyCode::Up), &windows, 3);
        state.key(k(KeyCode::Down), &windows, 3);
        assert_eq!(state.scroll, 0);
        state.key(k(KeyCode::Esc), &windows, 3);
        state.scroll_up(3);
        state.key(k(KeyCode::Char(' ')), &windows, 3);
        assert_eq!(state.scroll, 0);
    }
    #[test]
    fn escape_closes_and_i_requests_real_client() {
        let w = demo::windows();
        let mut s = State::default();
        assert_eq!(s.key(k(KeyCode::Char('i')), &w, 3), Action::Use);
        assert!(s.quick);
        assert_eq!(s.key(k(KeyCode::Esc), &w, 3), Action::None);
        assert!(!s.quick);
    }
    #[test]
    fn menu_routes_selection_and_keeps_close_confirmation() {
        let windows = demo::windows();
        let mut state = State::default();
        state.select(1);
        state.prompt = Prompt::Menu(0);
        state.key(k(KeyCode::Down), &windows, 3);
        state.key(k(KeyCode::Enter), &windows, 3);
        assert_eq!(
            state.prompt,
            Prompt::Rename(windows[1].id.clone(), String::new())
        );
        state.key(k(KeyCode::Esc), &windows, 3);
        state.prompt = Prompt::Menu(0);
        assert_eq!(state.key(k(KeyCode::Enter), &windows, 3), Action::Use);
        assert_eq!(state.prompt, Prompt::None);
        state.prompt = Prompt::Menu(0);
        assert_eq!(state.key(k(KeyCode::Char('x')), &windows, 3), Action::None);
        assert_eq!(state.prompt, Prompt::Close(windows[1].id.clone()));
        assert_eq!(state.key(k(KeyCode::Char('n')), &windows, 3), Action::None);
        state.prompt = Prompt::Menu(2);
        state.key(k(KeyCode::Esc), &windows, 3);
        assert_eq!(state.prompt, Prompt::None);
    }
    #[test]
    fn down_in_an_incomplete_row_selects_its_nearest_card() {
        let mut w = demo::windows();
        w.truncate(5);
        let mut s = State::default();
        s.select(2);
        s.key(k(KeyCode::Down), &w, 3);
        assert_eq!(s.selected, 4);
        s.key(k(KeyCode::Up), &w, 3);
        assert_eq!(s.selected, 1);
    }
    #[test]
    fn close_requires_y_and_alerts_follow_priority() {
        let w = demo::windows();
        let mut s = State::default();
        assert_eq!(s.key(k(KeyCode::Char('x')), &w, 3), Action::None);
        assert_eq!(s.key(k(KeyCode::Char('n')), &w, 3), Action::None);
        s.key(k(KeyCode::Char('x')), &w, 3);
        assert_eq!(
            s.key(k(KeyCode::Char('y')), &w, 3),
            Action::Close(w[0].id.clone())
        );
        s.select(4);
        s.key(k(KeyCode::Tab), &w, 3);
        assert_eq!(s.selected, 0);
        s.quick = true;
        s.key(k(KeyCode::Char('2')), &w, 3);
        assert_eq!(s.tab, Tab::Talk);
    }
}

/// Own raw input; terminal replies and user events share one decoder.
pub struct Reader {
    receiver: std::sync::mpsc::Receiver<Input>,
    pending: Option<Input>,
    paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    gate: std::sync::Arc<std::sync::Mutex<()>>,
}
pub enum Input {
    Event(crossterm::event::Event),
    GraphicsReply,
    Pixels((u16, u16)),
}
impl Reader {
    pub fn start() -> anyhow::Result<Self> {
        let (tx, receiver) = std::sync::mpsc::sync_channel(256);
        let paused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let input_paused = paused.clone();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(()));
        let input_gate = gate.clone();
        std::thread::Builder::new()
            .name("ncm-input".into())
            .spawn(move || {
                #[cfg(test)]
                crate::panic_log::inject("ncm-input");
                use std::sync::atomic::Ordering;
                let mut decoder = crate::input_stream::Decoder::default();
                let mut size = crossterm::terminal::size().ok();
                while !crate::terminal::STOP.load(Ordering::Relaxed) && crate::terminal::tty_alive()
                {
                    let guard = input_gate.lock().unwrap();
                    if input_paused.load(Ordering::Acquire) {
                        decoder = crate::input_stream::Decoder::default();
                        drop(guard);
                        std::thread::sleep(std::time::Duration::from_millis(25));
                        continue;
                    }
                    let mut fd = libc::pollfd {
                        fd: libc::STDIN_FILENO,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    // SAFETY: one initialized descriptor; read is bounded by the supplied buffer.
                    let ready = unsafe { libc::poll(&mut fd, 1, 25) };
                    let events = if ready > 0 && fd.revents & libc::POLLIN != 0 {
                        let mut bytes = [0u8; 4096];
                        let n = unsafe {
                            libc::read(libc::STDIN_FILENO, bytes.as_mut_ptr().cast(), bytes.len())
                        };
                        if n <= 0 {
                            break;
                        }
                        decoder.feed(&bytes[..n as usize])
                    } else {
                        decoder.idle()
                    };
                    drop(guard);
                    for event in events {
                        if input_paused.load(Ordering::Acquire) {
                            break;
                        }
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    let current = crossterm::terminal::size().ok();
                    if current != size {
                        size = current;
                        if let Some((w, h)) = current {
                            if tx
                                .send(Input::Event(crossterm::event::Event::Resize(w, h)))
                                .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            receiver,
            pending: None,
            paused,
            gate,
        })
    }
    pub fn pause(&mut self, paused: bool) {
        self.paused
            .store(paused, std::sync::atomic::Ordering::Release);
        let _guard = self.gate.lock().unwrap();
        self.pending = None;
        while self.receiver.try_recv().is_ok() {}
    }
    pub fn next(&mut self) -> Option<Input> {
        self.pending
            .take()
            .or_else(|| self.receiver.try_recv().ok())
    }
    pub fn wait(&mut self, timeout: std::time::Duration) {
        if self.pending.is_none() {
            match self.receiver.recv_timeout(timeout) {
                Ok(event) => self.pending = Some(event),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    crate::terminal::STOP.store(true, std::sync::atomic::Ordering::Relaxed)
                }
                Err(_) => {}
            }
        }
    }
}
