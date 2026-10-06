//! Persistent read-only, size-neutral tmux control queries and explicit actions.
//! Constructing a handle performs no I/O; only action methods mutate the targeted server.

use anyhow::{bail, Context, Result};
use ncm_model::{
    AgentState, BorderStyles, CellStyle, Cursor, Harness, PaneId, PaneInfo, Rgb, TabStyle,
    Timestamp, WindowFlags, WindowInfo,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    hash::{Hash, Hasher},
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

mod control;
mod groups;
mod peek;
pub use peek::{ViewerClient, ViewerState};
mod view;
pub use view::View;
mod scheduler;
use groups::{apply_groups, read_groups};
pub use scheduler::{
    CaptureHashes, CapturePlan, CaptureScheduler, ScheduledPoll, POLL_INTERVAL, REFRESH_INTERVAL,
};

/// Lowercase environment word from label plus raw pane title, matching wallcore.py.
pub fn env_word(label: &str, title: &str) -> &'static str {
    let text = format!("{label} {title}").to_lowercase();
    if text.contains("powershell") || text.contains("windows") {
        "ps"
    } else if text.contains("ubuntu") || text.contains("wsl") {
        "wsl"
    } else {
        ""
    }
}

/// Read-only captured ANSI lines, keyed by stable pane id; includes blank terminal rows.
pub type Captures = HashMap<PaneId, Vec<String>>;
/// A live server or an isolated named server. All subprocess arguments bypass shell interpolation.
#[derive(Debug, Clone)]
pub struct Tmux {
    server: Option<String>,
    socket: Option<PathBuf>,
    tmpdir: Option<PathBuf>,
    session: Option<String>,
    cache: Arc<Mutex<QueryCache>>,
    control: Arc<Mutex<Option<control::Control>>>,
}

#[derive(Debug, Default)]
struct QueryCache {
    spawns: u64,
    borders: HashMap<String, BorderStyles>,
}

// The delimiter and expanded pane/style formats come from reference/deck.py.
// Extend the reference's fields with stable session/window identity and cheap signatures.
const FIELDS: &[&str] = &[
    "session_name",
    "window_id",
    "window_index",
    "window_name",
    "@agent_state",
    "window_active",
    "window_flags",
    "window_activity",
    "window_width",
    "window_height",
    "@dashboard_group",
    "?#{@status_style_window},#{E:@status_style_window},#{window-status-style}",
    "E:@status_marker_window",
    "pane_id",
    "pane_index",
    "pane_left",
    "pane_top",
    "pane_width",
    "pane_height",
    "pane_active",
    "pane_pid",
    "pane_current_path",
    "?#{@pane_label},#{@pane_label},#{pane_title}",
    "cursor_x",
    "cursor_y",
    "window-style",
    "@pane_harness",
    "@pane_harness_color",
    "history_size",
    "pane_current_command",
    "@dashboard_key",
    "pane_title",
    "?#{@status_marker_window},1,0",
    "pane-border-style",
    "pane-active-border-style",
];

/// Non-interactive SSH shells often lack the user's tool directories in PATH.
fn tmux_executable() -> OsString {
    use std::os::unix::fs::PermissionsExt;
    let homes = [std::env::var_os("HOME").map(PathBuf::from), account_home()];
    let candidates = homes
        .into_iter()
        .flatten()
        .map(|home| home.join(".local/bin/tmux"))
        .chain([PathBuf::from("/opt/homebrew/bin/tmux")]);
    for path in candidates {
        if std::fs::metadata(&path)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            return path.into_os_string();
        }
    }
    "tmux".into()
}

/// The account's home from the password database, which survives an overridden HOME.
fn account_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf: Vec<libc::c_char> = vec![0; 4096];
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut found,
        )
    };
    if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

impl Tmux {
    /// Target the user's server, honoring the inherited `TMUX` environment.
    pub fn live() -> Self {
        Self {
            server: None,
            socket: None,
            tmpdir: None,
            session: None,
            cache: Arc::default(),
            control: Arc::default(),
        }
    }

    /// Target `tmux -L name`, explicitly ignoring any inherited `TMUX` server.
    /// This does not start a server; see [`testing::PrivateServer`] for owned test servers.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            server: Some(name.into()),
            socket: None,
            tmpdir: None,
            session: None,
            cache: Arc::default(),
            control: Arc::default(),
        }
    }

    /// Target an explicit socket, ignoring the viewer's server/session/pane environment.
    pub fn socket(path: impl Into<PathBuf>) -> Self {
        Self {
            server: None,
            socket: Some(path.into()),
            tmpdir: None,
            session: None,
            cache: Arc::default(),
            control: Arc::default(),
        }
    }

    /// Use a particular socket parent for a named private server (`TMUX_TMPDIR`).
    pub fn with_tmpdir(mut self, path: impl Into<PathBuf>) -> Self {
        self.tmpdir = Some(path.into());
        self.cache = Arc::default();
        self.control = Arc::default();
        self
    }

    fn command(&self) -> Command {
        let mut command = Command::new(tmux_executable());
        if let Some(name) = &self.server {
            command
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .args(["-L", name]);
        }
        if let Some(path) = &self.socket {
            command
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .arg("-S")
                .arg(path);
        }
        if let Some(path) = &self.tmpdir {
            command.env("TMUX_TMPDIR", path);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Result<String> {
        if !control::read_only(args) {
            return self.run_action(args);
        }
        let mut connection = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if connection
            .as_ref()
            .is_some_and(control::Control::disconnected)
        {
            connection.take();
        }
        if connection.is_none() {
            *connection = Some(control::Control::start(self)?);
        }
        let result = connection.as_mut().unwrap().execute(args);
        // Never retry actions implicitly. On EOF/write failure the next query
        // reconnects; a framed command error leaves a healthy stream usable.
        if connection
            .as_ref()
            .is_some_and(control::Control::disconnected)
        {
            connection.take();
        }
        result
    }

    fn control_events(&self) -> Result<control::Events> {
        let mut connection = self.control.lock().unwrap_or_else(|e| e.into_inner());
        if connection
            .as_ref()
            .is_some_and(control::Control::disconnected)
        {
            connection.take();
        }
        if connection.is_none() {
            *connection = Some(control::Control::start(self)?);
        }
        Ok(connection.as_ref().unwrap().take_events())
    }

    fn record_spawn(&self) {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).spawns += 1;
        if let Some(path) = std::env::var_os("NCM_TMUX_QUERY_LOG") {
            use std::io::Write;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let time = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                let _ = writeln!(file, "{time:.6} spawn");
            }
        }
    }

    // Explicit actions use ordinary command clients. The observer accepts only
    // read-only queries, so neither keys nor structural actions reach its stdin.
    fn run_action(&self, args: &[&str]) -> Result<String> {
        self.record_spawn();
        let output = self.command().args(args).output().context("tmux action")?;
        if !output.status.success() {
            bail!(
                "tmux action: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn run_single(&self, args: &[&str]) -> Result<String> {
        let escaped: Vec<_> = args
            .iter()
            .map(|arg| {
                arg.strip_suffix(';')
                    .map_or_else(|| (*arg).to_owned(), |prefix| format!("{prefix}\\;"))
            })
            .collect();
        self.run_action(&escaped.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// Whether this handle targets the server containing the viewer's own pane.
    pub fn is_viewer_server(&self) -> bool {
        if self.server.is_some() {
            return false;
        }
        let Some(socket) = &self.socket else {
            return true;
        };
        let inherited = std::env::var("TMUX").unwrap_or_default();
        let viewer = inherited.rsplitn(3, ',').last().unwrap_or_default();
        matches!((std::fs::canonicalize(socket), std::fs::canonicalize(viewer)), (Ok(a), Ok(b)) if a == b)
    }

    /// Explicit session override (e.g. the app's `--session NAME`).
    pub fn with_session(mut self, session: impl Into<String>) -> Self {
        self.session = Some(session.into());
        self.cache = Arc::default();
        self.control = Arc::default();
        self
    }

    /// Session of the invoking client, or `main` outside tmux. Private named servers use
    /// their current session. Only read-only format queries occur on the size-neutral client.
    pub fn current_session(&self) -> Result<String> {
        if let Some(session) = &self.session {
            return Ok(session.clone());
        }
        if self.socket.is_some() || (self.server.is_none() && std::env::var_os("TMUX").is_none()) {
            return Ok("main".into());
        }
        // TMUX's final field is the invoking session id. Target it explicitly, so a
        // more recently active client in another (possibly linked) session cannot win.
        if self.server.is_none() && self.socket.is_none() {
            if let Some(id) = inherited_session_id() {
                let session = self.run(&[
                    "display-message",
                    "-p",
                    "-t",
                    &format!("${id}"),
                    "#{session_name}",
                ])?;
                if !session.trim().is_empty() {
                    return Ok(session.trim().into());
                }
            }
        }
        let session = self.run(&["display-message", "-p", "#{client_session}"])?;
        if !session.trim().is_empty() {
            return Ok(session.trim().into());
        }
        let mut args = vec!["display-message", "-p"];
        let own = if self.server.is_none() && self.socket.is_none() {
            std::env::var("TMUX_PANE").ok()
        } else {
            None
        };
        if let Some(pane) = &own {
            validate_id(pane, '%')?;
            args.extend(["-t", pane]);
        }
        args.push("#{session_name}");
        let session = self.run(&args)?;
        if session.trim().is_empty() {
            bail!("tmux did not identify the invoking session");
        }
        Ok(session.trim().into())
    }

    /// Snapshot the chosen session in status-section/index order, deduplicated by window id.
    /// On the live server the window containing `TMUX_PANE` is excluded.
    pub fn snapshot(&self) -> Result<Vec<WindowInfo>> {
        Ok(self.poll()?.windows)
    }

    /// Metadata and signatures in one `list-panes -s` query after resolving the session.
    /// This never captures panes. Use [`CaptureScheduler`] for bounded content refreshes.
    pub fn poll(&self) -> Result<Poll> {
        let target = self.poll_target()?;
        let format = snapshot_format();
        let mut args = vec!["list-panes", "-s"];
        if !target.is_empty() {
            args.extend(["-t", &target]);
        }
        args.extend(["-F", &format]);
        let text = self.run(&args)?;
        self.cache_borders(&text);
        self.filter_poll(parse_snapshot(&text)?)
    }

    fn poll_target(&self) -> Result<String> {
        let target = if let Some(session) = &self.session {
            format!("={session}")
        } else if self.server.is_none() && self.socket.is_none() {
            if let Some(id) = inherited_session_id() {
                format!("${id}")
            } else {
                format!("={}", self.current_session()?)
            }
        } else if self.server.is_some() {
            // An untargeted named server uses tmux's current session for both list
            // and format loops, without separate session-resolution subprocesses.
            String::new()
        } else {
            format!("={}", self.current_session()?)
        };
        Ok(target)
    }

    fn filter_poll(&self, mut poll: Poll) -> Result<Poll> {
        if self.server.is_none() && self.socket.is_none() {
            if let Ok(own) = std::env::var("TMUX_PANE") {
                poll.windows
                    .retain(|w| !w.panes.iter().any(|p| p.id == own));
            }
        }
        let mut seen = HashSet::new();
        poll.windows.retain(|w| seen.insert(w.id.clone()));
        let panes: HashSet<_> = poll
            .windows
            .iter()
            .flat_map(|w| &w.panes)
            .map(|p| &p.id)
            .collect();
        poll.signatures.retain(|id, _| panes.contains(id));
        Ok(poll)
    }

    fn cache_borders(&self, text: &str) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.borders.clear();
        for row in text.lines() {
            let fields: Vec<_> = row.split('\x1f').collect();
            if fields.len() == FIELDS.len() {
                cache
                    .borders
                    .insert(fields[1].into(), border_styles(fields[33], fields[34]));
            }
        }
    }

    /// Resolve an environment word using both the label and raw title (even when label overrides it).
    pub fn pane_env_word(&self, pane: &str) -> Result<&'static str> {
        validate_id(pane, '%')?;
        let text = self.run(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{@pane_label} #{pane_title}",
        ])?;
        Ok(env_word("", &text))
    }

    /// Read both inherited pane-border styles for a stable window, including default fallback.
    /// Pass the result to `ncm_term::compose_window_with_styles` after metadata/layout changes.
    pub fn pane_border_styles(&self, window: &str) -> Result<BorderStyles> {
        validate_id(window, '@')?;
        if let Some(styles) = self
            .cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .borders
            .get(window)
        {
            return Ok(*styles);
        }
        let text = self.run(&[
            "display-message",
            "-p",
            "-t",
            window,
            "#{pane-border-style}\x1f#{pane-active-border-style}",
        ])?;
        let (normal, active) = text
            .trim()
            .split_once('\x1f')
            .context("tmux border style fields")?;
        Ok(border_styles(normal, active))
    }

    /// Batch visible ANSI rows over the shared control connection.
    /// Empty input performs no I/O; all targets are validated before starting tmux.
    pub fn capture(&self, panes: &[PaneId]) -> Result<Captures> {
        if panes.is_empty() {
            return Ok(Captures::new());
        }
        for pane in panes {
            validate_id(pane, '%')?;
        }
        let marker = random_marker();
        let mut args = Vec::new();
        for pane in panes {
            args.extend([
                "capture-pane",
                "-p",
                "-e",
                "-N",
                "-t",
                pane.as_str(),
                ";",
                "display-message",
                "-p",
                marker.as_str(),
                ";",
            ]);
        }
        args.pop();
        let output = self.run(&args)?;
        let mut remaining = output.as_str();
        let mut captures = Captures::new();
        for pane in panes {
            let (text, rest) = split_marker(remaining, &marker).context("missing tmux capture")?;
            captures.insert(pane.clone(), text.lines().map(str::to_owned).collect());
            remaining = rest;
        }
        if !remaining.is_empty() {
            bail!("tmux capture separator mismatch");
        }

        Ok(captures)
    }

    /// Select a window and pane ONLY in response to an explicit user action.
    /// tmux accepts stable pane ids as window targets.
    pub fn select(&self, pane: &str) -> Result<()> {
        validate_id(pane, '%')?;
        let window = self.run(&["display-message", "-p", "-t", pane, "#{window_id}"])?;
        self.select_window_pane(window.trim(), pane)
    }

    /// Select a stable window and pane in the chosen session, including linked windows.
    /// Caller invokes this only on an explicit user action.
    pub fn select_window_pane(&self, window: &str, pane: &str) -> Result<()> {
        validate_id(window, '@')?;
        validate_id(pane, '%')?;
        let session = self.current_session()?;
        let target = format!("={session}:{window}");
        self.run(&[
            "select-window",
            "-t",
            &target,
            ";",
            "select-pane",
            "-t",
            pane,
        ])?;
        Ok(())
    }

    /// Forward literal text (`-l`) after the user enters typing mode; never interprets text as keys.
    pub fn send_text(&self, pane: &str, text: &str) -> Result<()> {
        validate_id(pane, '%')?;
        self.run_single(&["send-keys", "-t", pane, "-l", "--", text])?;
        Ok(())
    }

    /// Forward one named tmux key (e.g. `Enter`, `C-c`) on explicit typing input.
    /// The app consumes Ctrl-] before calling this method.
    pub fn send_key(&self, pane: &str, key: &str) -> Result<()> {
        validate_id(pane, '%')?;
        self.run_single(&["send-keys", "-t", pane, "--", key])?;
        Ok(())
    }

    /// Rename a stable window target on an explicit user action.
    pub fn rename_window(&self, window: &str, name: &str) -> Result<()> {
        validate_id(window, '@')?;
        self.run_single(&["rename-window", "-t", window, "--", name])?;
        Ok(())
    }

    /// Close a stable window target. The caller MUST have obtained the user's y/n confirmation.
    pub fn kill_window(&self, window: &str) -> Result<()> {
        validate_id(window, '@')?;
        self.run_single(&["kill-window", "-t", window])?;
        Ok(())
    }

    /// Create a detached window on an explicit user action; never changes selection implicitly.
    /// `command` is intentionally a tmux shell command, not escaped literal input.
    pub fn new_window(&self, session: &str, name: &str, command: &str) -> Result<String> {
        let mut args = vec![
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{window_id}",
            "-t",
            session,
            "-n",
            name,
        ];
        // An empty command means the configured default shell. Passing an empty
        // argument instead runs an empty shell command and immediately loses the window.
        if !command.is_empty() {
            args.push(command);
        }
        self.run_single(&args).map(|id| id.trim().to_owned())
    }

    /// Open a tmux menu only on the user's right click. Commands are explicit tmux command strings.
    /// Requires a client on the targeted server; private detached test servers have no menu client.
    pub fn display_menu(&self, window: &str, title: &str, items: &[MenuItem]) -> Result<()> {
        validate_id(window, '@')?;
        let client = self.peek_client()?;
        let mut args = vec!["display-menu", "-c", &client, "-t", window, "-T", title];
        for item in items {
            args.extend([
                item.label.as_str(),
                item.key.as_str(),
                item.command.as_str(),
            ]);
        }
        self.run_single(&args)?;
        Ok(())
    }
}

/// One menu entry; the caller controls command strings and the explicit action policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Menu label.
    pub label: String,
    /// tmux menu shortcut.
    pub key: String,
    /// tmux command executed when selected.
    pub command: String,
}

/// Cheap metadata signature, not a content hash. Layout changes must also invalidate rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSignature {
    /// Window activity time; shares second precision with tmux.
    pub activity: Option<Timestamp>,
    /// Number of retained history rows.
    pub history_size: u64,
    /// Pane cursor location.
    pub cursor: Cursor,
    /// Pane dimensions.
    pub size: (usize, usize),
}

/// One metadata poll result, allowing snapshot and change detection to share the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poll {
    /// Sorted windows and panes.
    pub windows: Vec<WindowInfo>,
    /// Signatures keyed by stable pane identity (linked windows share entries).
    pub signatures: HashMap<PaneId, PaneSignature>,
}

/// Stateful signature comparison. Removed panes are evicted; first sightings count as changed.
#[derive(Debug, Clone, Default)]
pub struct ChangeDetector {
    previous: HashMap<PaneId, PaneSignature>,
}

impl ChangeDetector {
    /// Return sorted changed ids plus an optionally forced selected pane; never performs I/O.
    /// Pass `selected` only when its higher-frequency refresh is due.
    pub fn changed(&mut self, poll: &Poll, selected: Option<&str>) -> Vec<PaneId> {
        let mut changed: HashSet<_> = poll
            .signatures
            .iter()
            .filter(|(id, signature)| self.previous.get(*id) != Some(*signature))
            .map(|(id, _)| id.clone())
            .collect();
        if let Some(id) = selected {
            if poll.signatures.contains_key(id) {
                changed.insert(id.to_owned());
            }
        }
        self.previous.clone_from(&poll.signatures);
        let mut ids: Vec<_> = changed.into_iter().collect();
        ids.sort();
        ids
    }
}

fn snapshot_format() -> String {
    FIELDS
        .iter()
        .map(|field| format!("#{{{field}}}"))
        .collect::<Vec<_>>()
        .join("\x1f")
}

fn border_styles(normal: &str, active: &str) -> BorderStyles {
    let convert = |text: &str| {
        let style = parse_style(text);
        CellStyle {
            fg: style.fg.or(Some(Rgb::new(60, 64, 74))),
            bg: style.bg,
            bold: style.bold,
            ..CellStyle::default()
        }
    };
    BorderStyles {
        normal: convert(normal),
        active: convert(active),
    }
}

fn random_marker() -> String {
    let mut bytes = [0u8; 32];
    // OS entropy, per invocation, without a random-number utility subprocess.
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .expect("OS entropy");
    // OSC framing cannot be emitted by capture-pane -e, which serializes printable
    // cells and SGR styles, not terminal OSC control strings. Also require a full
    // line and a fresh 256-bit nonce, so marker-looking cell text is ordinary text.
    format!(
        "\x1b]ncm-{}\x07",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

fn split_marker<'a>(text: &'a str, marker: &str) -> Option<(&'a str, &'a str)> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.strip_suffix('\n') == Some(marker) {
            return Some((&text[..offset], &text[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn inherited_session_id() -> Option<String> {
    let tmux = std::env::var("TMUX").ok()?;
    let id = tmux.rsplit(',').next()?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.into())
}

fn validate_id(id: &str, prefix: char) -> Result<()> {
    if !id.starts_with(prefix) || id.len() < 2 || !id[1..].bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("invalid tmux target: {id:?}");
    }
    Ok(())
}

fn parse_snapshot(text: &str) -> Result<Poll> {
    parse_snapshot_with_groups(text, &read_groups())
}

fn parse_snapshot_with_groups(text: &str, groups: &serde_json::Value) -> Result<Poll> {
    let mut windows = BTreeMap::<(String, String), WindowInfo>::new();
    let mut signatures = HashMap::new();
    let mut keys = HashMap::new();
    for line in text.lines() {
        let p: Vec<_> = line.split('\x1f').collect();
        if p.len() != FIELDS.len() {
            bail!(
                "tmux snapshot has {} fields, expected {}",
                p.len(),
                FIELDS.len()
            );
        }
        validate_id(p[1], '@')?;
        validate_id(p[13], '%')?;
        let number = |i: usize| -> Result<usize> {
            p[i].parse()
                .with_context(|| format!("invalid tmux {}", FIELDS[i]))
        };
        let state = match p[4] {
            "needs" => AgentState::Needs,
            "working" => AgentState::Working,
            "done" => AgentState::Done,
            _ => AgentState::None,
        };
        let activity = p[7]
            .parse::<i64>()
            .ok()
            .and_then(|s| s.checked_mul(1000))
            .map(Timestamp);
        let mut tab = parse_style(p[11]);
        tab.marker = if p[32] != "1" && p[12].is_empty() {
            match state {
                AgentState::Needs => "?",
                AgentState::Working => "●",
                AgentState::Done => "✓",
                AgentState::None => "",
            }
            .into()
        } else {
            p[12].into()
        };
        let key = (p[0].to_owned(), p[1].to_owned());
        keys.insert(key.clone(), p[30].to_owned());
        if !windows.contains_key(&key) {
            windows.insert(
                key.clone(),
                WindowInfo {
                    session: p[0].into(),
                    id: p[1].into(),
                    index: number(2)?.try_into()?,
                    name: p[3].into(),
                    active: p[5] == "1",
                    flags: WindowFlags {
                        raw: p[6].into(),
                        bell: p[6].contains('!'),
                        activity: p[6].contains('#'),
                        silence: p[6].contains('~'),
                    },
                    activity,
                    group: (!p[10].is_empty()).then(|| p[10].to_owned()),
                    tab_style: tab,
                    agent_state: state,
                    panes: Vec::new(),
                    width: number(8)?,
                    height: number(9)?,
                },
            );
        }
        let pane = PaneInfo {
            id: p[13].into(),
            index: number(14)?.try_into()?,
            x: number(15)?,
            y: number(16)?,
            width: number(17)?,
            height: number(18)?,
            active: p[19] == "1",
            pid: number(20)?.try_into()?,
            cwd: p[21].into(),
            title: p[22].into(),
            cursor: Cursor {
                x: number(23)?,
                y: number(24)?,
            },
            tint: parse_style(p[25]).bg,
            harness: harness_from(p[26], p[27], p[29]),
        };
        signatures.insert(
            pane.id.clone(),
            PaneSignature {
                activity,
                history_size: p[28].parse().unwrap_or(0),
                cursor: pane.cursor,
                size: (pane.width, pane.height),
            },
        );
        if let Some(window) = windows.get_mut(&key) {
            window.panes.push(pane);
        }
    }
    apply_groups(&mut windows, &keys, groups);
    let mut windows: Vec<_> = windows.into_values().collect();
    for window in &mut windows {
        window.panes.sort_by_key(|pane| pane.index);
        let mut seen = HashSet::new();
        window.panes.retain(|pane| seen.insert(pane.id.clone()));
    }
    let group_order = |g: Option<&str>| match g {
        Some("pinned") => 0,
        Some("build") => 1,
        Some("explore") => 2,
        Some("personal") => 3,
        Some(_) => 4,
        None => 5,
    };
    windows.sort_by(|a, b| {
        (group_order(a.group.as_deref()), &a.session, a.index).cmp(&(
            group_order(b.group.as_deref()),
            &b.session,
            b.index,
        ))
    });
    Ok(Poll {
        windows,
        signatures,
    })
}

fn harness_from(explicit: &str, color: &str, command: &str) -> Option<Harness> {
    let lower = command.to_lowercase();
    let names: Vec<_> = lower
        .split_whitespace()
        .take(5)
        .map(|s| s.trim_matches(['\'', '"']).rsplit('/').next().unwrap_or(s))
        .collect();
    let known = [
        ("codex", &["codex"][..]),
        ("claude", &["claude", "claude-code"][..]),
        ("kilo", &["kilo", "kilo-code"][..]),
        ("opencode", &["opencode"][..]),
        ("gemini", &["gemini", "gemini-cli"][..]),
        ("aider", &["aider"][..]),
        ("pi", &["pi", "pi-agent"][..]),
        ("prime", &["prime", "prime-agent"][..]),
        ("fable", &["fable"][..]),
    ];
    let name = if !explicit.trim().is_empty() {
        explicit.trim().to_lowercase()
    } else if lower.contains("@openai/codex") || lower.contains("/codex/") {
        "codex".into()
    } else if lower.contains("pi-coding-agent") {
        "pi".into()
    } else {
        known
            .iter()
            .find(|(_, aliases)| aliases.iter().any(|alias| names.contains(alias)))?
            .0
            .into()
    };
    Some(Harness {
        color: parse_color(color).unwrap_or_else(|| harness_color(&name)),
        name,
    })
}

fn harness_color(name: &str) -> Rgb {
    match name.to_lowercase().as_str() {
        "claude" => Rgb::new(217, 119, 87),
        "codex" => Rgb::new(16, 163, 127),
        _ => Rgb::new(150, 156, 166),
    }
}

fn parse_style(style: &str) -> TabStyle {
    let mut tab = TabStyle::default();
    for item in style.replace("#[", ",").replace(']', ",").split(',') {
        let item = item.trim();
        if let Some(value) = item.strip_prefix("fg=") {
            tab.fg = parse_color(value);
        }
        if let Some(value) = item.strip_prefix("bg=") {
            tab.bg = parse_color(value);
        }
        if item == "default" {
            tab = TabStyle::default();
        }
        if item == "nobold" {
            tab.bold = false;
        }
        if item == "bold" {
            tab.bold = true;
        }
    }
    tab
}

fn parse_color(value: &str) -> Option<Rgb> {
    let value = value.trim().to_lowercase();
    if let Some(hex) = value.strip_prefix('#') {
        if hex.len() == 6 {
            let rgb = u32::from_str_radix(hex, 16).ok()?;
            return Some(Rgb::new((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8));
        }
    }
    let names = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    let n = if let Some(n) = value
        .strip_prefix("colour")
        .or_else(|| value.strip_prefix("color"))
    {
        n.parse::<u8>().ok()?
    } else if let Some(name) = value.strip_prefix("bright") {
        names.iter().position(|v| *v == name)? as u8 + 8
    } else {
        names.iter().position(|v| *v == value)? as u8
    };
    Some(indexed_color(n))
}

fn indexed_color(n: u8) -> Rgb {
    const BASE: [(u8, u8, u8); 16] = [
        (40, 44, 52),
        (224, 108, 117),
        (152, 195, 121),
        (229, 192, 123),
        (97, 175, 239),
        (198, 120, 221),
        (86, 182, 194),
        (220, 223, 228),
        (93, 103, 122),
        (224, 108, 117),
        (152, 195, 121),
        (229, 192, 123),
        (97, 175, 239),
        (198, 120, 221),
        (86, 182, 194),
        (220, 223, 228),
    ];
    if n < 16 {
        let (r, g, b) = BASE[n as usize];
        return Rgb::new(r, g, b);
    }
    if n >= 232 {
        let v = 8 + 10 * (n - 232);
        return Rgb::new(v, v, v);
    }
    let n = n - 16;
    let steps = [0, 95, 135, 175, 215, 255];
    Rgb::new(
        steps[(n / 36) as usize],
        steps[(n / 6 % 6) as usize],
        steps[(n % 6) as usize],
    )
}

/// Owned private-server test support, available to sibling crates' integration tests.
pub mod testing {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// A private tmux server with no user configuration or inherited live-server connection.
    /// Creates sockets under this workspace's target directory and kills only its named server
    /// on drop. Window/pane commands intentionally run through tmux's shell.
    #[derive(Debug)]
    pub struct PrivateServer {
        tmux: Tmux,
        session: String,
    }

    impl PrivateServer {
        /// Start `ncm-test-<pid>-<n>` with one detached window at exactly `width × height` cells.
        /// Use long-running commands (e.g. `sleep 60`) to keep the session alive.
        /// Socket parent is resolved from the crate path, never the caller's cwd or `/tmp`.
        pub fn start(width: usize, height: usize, command: &str) -> Result<Self> {
            if width == 0 || height == 0 {
                bail!("private server size must be nonzero");
            }
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .canonicalize()?;
            let tmpdir = root.join("target");
            fs::create_dir_all(&tmpdir)?;
            let name = format!(
                "ncm-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let tmux = Tmux::named(name)
                .with_tmpdir(tmpdir)
                .with_session("ncm-test");
            let server = Self {
                tmux,
                session: "ncm-test".into(),
            };
            let output = server
                .tmux
                .command()
                .args([
                    "-f",
                    "/dev/null",
                    "new-session",
                    "-d",
                    "-s",
                    &server.session,
                    "-x",
                    &width.to_string(),
                    "-y",
                    &height.to_string(),
                    command,
                ])
                .output()
                .context("start private tmux server")?;
            if !output.status.success() {
                bail!(
                    "private tmux server: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
            Ok(server)
        }

        /// The isolated handle; its actions never target the user's live server.
        pub fn tmux(&self) -> &Tmux {
            &self.tmux
        }

        /// Socket label supplied to `-L`, useful for proving isolation/cleanup.
        pub fn name(&self) -> &str {
            self.tmux.server.as_deref().unwrap_or("")
        }

        /// Absolute socket path, useful for setting `TMUX` on a CLI subprocess in isolation tests.
        pub fn socket_path(&self) -> Result<PathBuf> {
            Ok(self
                .tmux
                .run(&["display-message", "-p", "#{socket_path}"])?
                .trim()
                .into())
        }

        /// Create a detached window with a command and explicit dimensions; returns its `@N` id.
        pub fn new_window(
            &self,
            name: &str,
            width: usize,
            height: usize,
            command: &str,
        ) -> Result<String> {
            if width == 0 || height == 0 {
                bail!("private window size must be nonzero");
            }
            let id = self.tmux.new_window(&self.session, name, command)?;
            self.tmux.run(&[
                "resize-window",
                "-t",
                &id,
                "-x",
                &width.to_string(),
                "-y",
                &height.to_string(),
            ])?;
            Ok(id)
        }

        /// Split a pane, detached, running the supplied command. `horizontal` splits left/right.
        pub fn split_pane(&self, pane: &str, horizontal: bool, command: &str) -> Result<PaneId> {
            validate_id(pane, '%')?;
            self.tmux
                .run(&[
                    "split-window",
                    "-d",
                    if horizontal { "-h" } else { "-v" },
                    "-P",
                    "-F",
                    "#{pane_id}",
                    "-t",
                    pane,
                    command,
                ])
                .map(|id| id.trim().to_owned())
        }
    }

    impl Drop for PrivateServer {
        fn drop(&mut self) {
            let _ = self.tmux.run(&["kill-server"]);
        }
    }
}

// Keep OsString out of the subprocess API: public targets are validated stable ids.
// It is used only to expose the inherited own-pane value without terminal/GUI access.
/// Read the viewer's own pane identity; the app can exclude its entire containing window.
pub fn own_pane() -> Option<OsString> {
    std::env::var_os("TMUX_PANE")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_server_creates_windows_and_panes_and_cleans_up() -> Result<()> {
        let server = testing::PrivateServer::start(80, 24, "sleep 60")?;
        assert!(server
            .name()
            .starts_with(&format!("ncm-test-{}-", std::process::id())));
        let handle = server.tmux().clone();
        let first = handle.snapshot()?;
        assert_eq!(first.len(), 1);
        assert_eq!((first[0].width, first[0].height), (80, 24));
        let second = server.new_window("second", 100, 30, "sleep 60")?;
        let pane = first[0].panes[0].id.clone();
        server.split_pane(&pane, true, "sleep 60")?;
        let poll = handle.poll()?;
        assert_eq!(poll.windows.len(), 2);
        assert_eq!(
            poll.windows
                .iter()
                .find(|w| w.id == second)
                .map(|w| (w.width, w.height)),
            Some((100, 30))
        );
        assert_eq!(
            poll.windows
                .iter()
                .find(|w| w.id == first[0].id)
                .map(|w| w.panes.len()),
            Some(2)
        );
        assert!(handle.capture(&[pane])?.values().next().is_some());
        let mut detector = ChangeDetector::default();
        assert_eq!(detector.changed(&poll, None).len(), 3);
        assert!(detector.changed(&poll, None).is_empty());
        let id = poll.windows[0].panes[0].id.as_str();
        assert_eq!(detector.changed(&poll, Some(id)), vec![id.to_owned()]);
        drop(server);
        assert!(
            handle.snapshot().is_err(),
            "private server must be killed on drop"
        );
        Ok(())
    }
}

#[cfg(test)]
mod metadata_tests {
    use super::*;

    fn row(window: &str, pane: &str, index: u32) -> Vec<String> {
        let mut p = vec![String::new(); FIELDS.len()];
        for (i, value) in [
            (0, "fixture"),
            (1, window),
            (3, "work"),
            (4, "needs"),
            (5, "1"),
            (6, "!#~Z"),
            (7, "123"),
            (8, "80"),
            (9, "24"),
            (11, "#[fg=colour196]#[bg=#012456]#[bold]"),
            (12, "?"),
            (13, pane),
            (14, "0"),
            (15, "0"),
            (16, "0"),
            (17, "80"),
            (18, "24"),
            (19, "1"),
            (20, "42"),
            (21, "/synthetic/project"),
            (22, "PowerShell"),
            (23, "2"),
            (24, "3"),
            (25, "bg=colour17"),
            (28, "12"),
            (29, "node /packages/@openai/codex/bin/codex.js"),
            (32, "1"),
        ] {
            p[i] = value.into();
        }
        p[2] = index.to_string();
        p
    }

    #[test]
    fn snapshot_fields_and_optional_harness_styles() -> Result<()> {
        let p = row("@10", "%11", 7);
        let poll = parse_snapshot_with_groups(&p.join("\x1f"), &serde_json::Value::Null)?;
        let w = &poll.windows[0];
        assert_eq!(
            (&w.session, &w.id, w.index, w.name.as_str()),
            (&"fixture".into(), &"@10".into(), 7, "work")
        );
        assert!(
            w.active && w.flags.bell && w.flags.activity && w.flags.silence && w.flags.zoomed()
        );
        assert_eq!(w.activity, Some(Timestamp(123000)));
        assert_eq!(
            (w.width, w.height, w.agent_state),
            (80, 24, AgentState::Needs)
        );
        assert_eq!(
            w.tab_style,
            TabStyle {
                fg: Some(Rgb::new(255, 0, 0)),
                bg: Some(Rgb::new(1, 36, 86)),
                bold: true,
                marker: "?".into()
            }
        );
        let pane = &w.panes[0];
        assert_eq!(
            (
                pane.index,
                pane.x,
                pane.y,
                pane.width,
                pane.height,
                pane.pid
            ),
            (0, 0, 0, 80, 24, 42)
        );
        assert!(pane.active);
        assert_eq!(pane.cursor, Cursor { x: 2, y: 3 });
        assert_eq!(pane.cwd, PathBuf::from("/synthetic/project"));
        assert_eq!(pane.title, "PowerShell");
        assert_eq!(pane.env_word(), "ps");
        assert_eq!(pane.tint, Some(Rgb::new(0, 0, 95)));
        assert_eq!(pane.harness.as_ref().unwrap().name, "codex");
        assert_eq!(poll.signatures["%11"].history_size, 12);
        let custom = harness_from("CUSTOM", "colour67", "claude").unwrap();
        assert_eq!(custom.name, "custom");
        assert_eq!(custom.color, Rgb::new(95, 135, 175));
        assert!(
            harness_from("", "", "node").is_none(),
            "node alone is not evidence of codex"
        );
        assert_eq!(
            harness_from("", "", "/opt/bin/claude").unwrap().name,
            "claude"
        );
        assert_eq!(env_word("work", "Windows / Ubuntu"), "ps");
        assert_eq!(env_word("Ubuntu", "work"), "wsl");
        assert_eq!(
            parse_style("#[bold,fg=red]#[default]#[bg=colour17,nobold]"),
            TabStyle {
                bg: Some(Rgb::new(0, 0, 95)),
                ..TabStyle::default()
            }
        );
        Ok(())
    }

    #[test]
    fn group_file_uuid_fingerprints_and_section_order() -> Result<()> {
        let records = serde_json::json!({"version":1,"sessions":{"fixture":{"windows":{
            "uuid-a":{"group":"personal","name":"renamed","path":"/different","panes":1,"last_seen":1},
            "old":{"group":"explore","name":"work","path":"/synthetic/project","panes":1,"last_seen":2},
            "new":{"group":"build","name":"work","path":"/synthetic/project","panes":1,"last_seen":3}
        }}}});
        let mut a = row("@0", "%0", 0);
        a[30] = "uuid-a".into();
        let b = row("@1", "%1", 1);
        let mut c = row("@2", "%2", 2);
        c[10] = "pinned".into();
        let poll = parse_snapshot_with_groups(
            &[a.join("\x1f"), b.join("\x1f"), c.join("\x1f")].join("\n"),
            &records,
        )?;
        assert_eq!(
            poll.windows
                .iter()
                .map(|w| (w.id.as_str(), w.group.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("@2", Some("pinned")),
                ("@1", Some("build")),
                ("@0", Some("personal"))
            ]
        );
        // Match name/path even if pane count changed, then unique path after a rename.
        let loose = serde_json::json!({"sessions":{"fixture":{"windows":{
            "uuid":{"group":"explore","name":"work","path":"/synthetic/project","panes":2,"last_seen":1}
        }}}});
        assert_eq!(
            parse_snapshot_with_groups(&row("@0", "%0", 0).join("\x1f"), &loose)?.windows[0]
                .group
                .as_deref(),
            Some("explore")
        );
        let mut renamed = row("@0", "%0", 0);
        renamed[3] = "changed".into();
        assert_eq!(
            parse_snapshot_with_groups(&renamed.join("\x1f"), &loose)?.windows[0]
                .group
                .as_deref(),
            Some("explore")
        );
        renamed[12].clear(); // Explicitly set empty markers must not gain a state fallback.
        assert!(
            parse_snapshot_with_groups(&renamed.join("\x1f"), &loose)?.windows[0]
                .tab_style
                .marker
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn tick_batches_discovery_new_panes_and_cached_reads() -> Result<()> {
        let server = testing::PrivateServer::start(80, 24, "sleep 60")?;
        let mut tmux = server.tmux().clone();
        tmux.session = None;
        let mut scheduler = CaptureScheduler::default();
        let start = Instant::now();
        let count = || tmux.cache.lock().unwrap().spawns;
        let before = count();
        let first = scheduler.tick(&tmux, None, start)?.unwrap();
        assert_eq!(first.plan.panes.len(), 1);
        let captured = tmux.capture(&first.plan.panes)?;
        assert_eq!(captured[&first.plan.panes[0]].len(), 24);
        tmux.pane_border_styles(&first.poll.windows[0].id)?;
        assert!(count() - before <= 1);
        let before = count();
        let second = scheduler.tick(&tmux, None, start + POLL_INTERVAL)?.unwrap();
        assert!(second.plan.panes.is_empty());
        assert!(count() - before <= 1);
        tmux.run(&["split-window", "-d", "-t", "=ncm-test:", "sleep 60"])?;
        let before = count();
        let third = scheduler
            .tick(&tmux, None, start + POLL_INTERVAL * 2)?
            .unwrap();
        assert_eq!(third.poll.windows[0].panes.len(), 2);
        assert!(!third.plan.panes.is_empty());
        tmux.capture(&third.plan.panes)?;
        assert!(count() - before <= 1);
        let selected = third.poll.windows[0].panes[0].id.clone();
        let before = count();
        let quick = scheduler
            .tick(&tmux, Some(&selected), start + POLL_INTERVAL * 3)?
            .unwrap();
        assert_eq!(quick.plan.panes.len(), 2);
        tmux.capture(&quick.plan.panes)?;
        assert!(count() - before <= 1);
        Ok(())
    }

    #[test]
    fn control_errors_reconnect_and_read_only_size_neutrality() -> Result<()> {
        let server = testing::PrivateServer::start(73, 19, "sleep 60")?;
        let tmux = server.tmux();
        let mut scheduler = CaptureScheduler::default();
        let first = scheduler.tick(tmux, None, Instant::now())?.unwrap();
        let pane = first.plan.panes[0].clone();
        assert_eq!(
            (first.poll.windows[0].width, first.poll.windows[0].height),
            (73, 19)
        );
        let flags = tmux.run(&["display-message", "-p", "#{client_flags}"])?;
        assert!(flags.contains("ignore-size"));
        assert!(tmux
            .control
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .execute(&["send-keys", "-t", &pane, "x"])
            .is_err());
        assert!(tmux
            .control
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .execute(&["select-window", "-t", "=ncm-test:"])
            .is_err());
        let initial = tmux.capture(&first.plan.panes)?;
        let before = tmux.cache.lock().unwrap().spawns;
        assert!(tmux
            .run(&["capture-pane", "-p", "-t", "%999999999"])
            .is_err());
        assert_eq!(tmux.capture(std::slice::from_ref(&pane))?, initial);
        assert_eq!(tmux.cache.lock().unwrap().spawns, before);
        let pid = tmux.control.lock().unwrap().as_ref().unwrap().pid();
        // Kill only our attached client, leaving the private server alive.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while !tmux
            .control
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .disconnected()
        {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let resumed = scheduler.tick(tmux, None, Instant::now())?.unwrap();
        assert!(resumed.plan.panes.contains(&pane));
        assert_eq!(tmux.capture(&resumed.plan.panes)?, initial);
        assert_eq!(tmux.cache.lock().unwrap().spawns, before + 1);
        assert_eq!(
            (
                resumed.poll.windows[0].width,
                resumed.poll.windows[0].height
            ),
            (73, 19)
        );
        Ok(())
    }

    #[test]
    fn live_sized_queries_fit_the_tmux_command_message() -> Result<()> {
        let server = testing::PrivateServer::start(30, 8, "sleep 60")?;
        for index in 0..30 {
            server.new_window(&format!("card-{index}"), 30, 8, "sleep 60")?;
        }
        let tmux = server.tmux();
        let mut scheduler = CaptureScheduler::default();
        for _ in 0..9 {
            let frame = scheduler.tick(tmux, None, Instant::now())?.unwrap();
            assert_eq!(frame.poll.signatures.len(), 31);
            tmux.capture(&frame.plan.panes)?;
            std::thread::sleep(POLL_INTERVAL);
        }
        Ok(())
    }

    #[test]
    fn quiet_quick_look_reuses_the_persistent_client() -> Result<()> {
        let server = testing::PrivateServer::start(30, 8, "sleep 60")?;
        let tmux = server.tmux();
        let mut scheduler = CaptureScheduler::default();
        for _ in 0..11 {
            if let Some(frame) = scheduler.tick(tmux, None, Instant::now())? {
                tmux.capture(&frame.plan.panes)?;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
        assert_eq!(scheduler.poll_interval(), Duration::from_secs(1));
        let pane = tmux.snapshot()?[0].panes[0].id.clone();
        let before = tmux.cache.lock().unwrap().spawns;
        for _ in 0..4 {
            let frame = scheduler.tick(tmux, Some(&pane), Instant::now())?.unwrap();
            assert_eq!(frame.plan.panes, [pane.clone()]);
            tmux.capture(&frame.plan.panes)?;
            std::thread::sleep(POLL_INTERVAL);
        }
        assert_eq!(scheduler.poll_interval(), POLL_INTERVAL);
        assert_eq!(tmux.cache.lock().unwrap().spawns - before, 0);
        Ok(())
    }

    #[test]
    fn split_cards_share_a_one_second_capture_limit() -> Result<()> {
        let text = [
            row("@0", "%0", 0).join("\x1f"),
            row("@0", "%1", 0).join("\x1f"),
        ]
        .join("\n");
        let mut poll = parse_snapshot_with_groups(&text, &serde_json::Value::Null)?;
        let mut scheduler = CaptureScheduler::default();
        let start = Instant::now();
        assert_eq!(scheduler.schedule(&poll, None, start).panes.len(), 2);
        poll.signatures.get_mut("%0").unwrap().history_size += 1;
        assert_eq!(
            scheduler
                .schedule(&poll, None, start + Duration::from_secs(1))
                .panes,
            ["%0"]
        );
        poll.signatures.get_mut("%1").unwrap().history_size += 1;
        assert!(scheduler
            .schedule(&poll, None, start + Duration::from_millis(1250))
            .panes
            .is_empty());
        assert_eq!(
            scheduler
                .schedule(&poll, None, start + Duration::from_secs(2))
                .panes,
            ["%1"]
        );
        assert_eq!(
            scheduler
                .schedule(&poll, Some("%0"), start + Duration::from_millis(2250))
                .panes
                .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn throttled_activity_is_pending_until_one_second() -> Result<()> {
        let mut poll =
            parse_snapshot_with_groups(&row("@0", "%0", 0).join("\x1f"), &serde_json::Value::Null)?;
        let mut scheduler = CaptureScheduler::default();
        let start = Instant::now();
        scheduler.schedule(&poll, None, start);
        poll.signatures.get_mut("%0").unwrap().history_size += 1;
        for step in 1..4 {
            assert!(scheduler
                .schedule(&poll, None, start + POLL_INTERVAL * step)
                .panes
                .is_empty());
        }
        assert_eq!(
            scheduler
                .schedule(&poll, None, start + Duration::from_secs(1))
                .panes,
            ["%0"]
        );
        Ok(())
    }

    #[test]
    fn capture_separator_requires_an_entire_control_framed_line() {
        let marker = random_marker();
        let text = format!("ncm-capture-1-0\nnear {marker} match\n{marker}\nremaining\n");
        let (captured, rest) = split_marker(&text, &marker).unwrap();
        assert!(captured.contains("near "));
        assert_eq!(rest, "remaining\n");
        assert_ne!(marker, random_marker());
    }

    #[test]
    fn bounded_refresh_selected_changes_and_geometry() -> Result<()> {
        let rows: Vec<_> = (0..29)
            .map(|i| row(&format!("@{i}"), &format!("%{i}"), i).join("\x1f"))
            .collect();
        let mut poll = parse_snapshot_with_groups(&rows.join("\n"), &serde_json::Value::Null)?;
        let mut scheduler = CaptureScheduler::default();
        let start = Instant::now();
        let first = scheduler.schedule(&poll, Some("%0"), start);
        assert_eq!(first.panes.len(), 29);
        assert_eq!(first.geometry_changed.len(), 29);
        assert!(first.structure_changed);
        let mut last: HashMap<_, _> = poll
            .signatures
            .keys()
            .map(|id| (id.clone(), start))
            .collect();
        let mut total = 0;
        for step in 1..=40 {
            let now = start + POLL_INTERVAL * step;
            let plan = scheduler.schedule(&poll, Some("%0"), now);
            assert!(plan.panes.contains(&"%0".into()));
            assert!(!plan.structure_changed && plan.geometry_changed.is_empty());
            assert!(
                plan.panes.len() < 29,
                "unchanged panes must not all be captured every tick"
            );
            for id in plan.panes {
                last.insert(id, now);
                total += 1;
            }
            assert!(last
                .values()
                .all(|last| now.duration_since(*last) <= REFRESH_INTERVAL));
        }
        assert!(total < 170);
        assert!(!scheduler.poll_due(start + Duration::from_millis(10001)));
        poll.signatures.get_mut("%1").unwrap().cursor.x += 1;
        poll.windows[0].panes[0].x += 1;
        let plan = scheduler.schedule(&poll, None, start + Duration::from_millis(10001));
        assert!(plan.panes.contains(&"%1".into()));
        assert_eq!(plan.geometry_changed, ["@0"]);
        poll.windows.remove(0);
        poll.signatures.remove("%0");
        let plan = scheduler.schedule(&poll, None, start + Duration::from_millis(10250));
        assert!(plan.structure_changed);
        assert_eq!(plan.geometry_changed, ["@0"]);
        let mut hashes = CaptureHashes::default();
        let captures = HashMap::from([("%1".into(), vec!["same".into()])]);
        assert_eq!(hashes.changed(&captures), ["%1"]);
        assert!(hashes.changed(&captures).is_empty());
        Ok(())
    }

    #[test]
    fn poll_cadence_and_selection_do_not_drift_with_sleep_jitter() -> Result<()> {
        let poll =
            parse_snapshot_with_groups(&row("@0", "%0", 0).join("\x1f"), &serde_json::Value::Null)?;
        let start = Instant::now();
        let mut scheduler = CaptureScheduler::default();
        scheduler.schedule(&poll, Some("%0"), start);
        for step in 1..=40 {
            let jitter = Duration::from_millis(if step % 2 == 0 { 1 } else { 2 });
            let now = start + POLL_INTERVAL * step + jitter;
            assert_eq!(
                scheduler.poll_due(now),
                scheduler.poll_interval() == POLL_INTERVAL || step % 4 == 0
            );
            assert_eq!(scheduler.schedule(&poll, Some("%0"), now).panes, ["%0"]);
            assert!(!scheduler.poll_due(now + Duration::from_millis(100)));
        }
        Ok(())
    }

    #[test]
    fn one_idle_pane_is_not_captured_on_every_tick() -> Result<()> {
        let poll =
            parse_snapshot_with_groups(&row("@0", "%0", 0).join("\x1f"), &serde_json::Value::Null)?;
        let start = Instant::now();
        let mut scheduler = CaptureScheduler::default();
        scheduler.schedule(&poll, None, start);
        for step in 1..56 {
            assert!(scheduler
                .schedule(&poll, None, start + POLL_INTERVAL * step)
                .panes
                .is_empty());
        }
        assert_eq!(
            scheduler
                .schedule(&poll, None, start + POLL_INTERVAL * 56)
                .panes,
            ["%0"]
        );
        Ok(())
    }

    #[test]
    fn quiet_poll_backoff_keeps_quick_look_and_spread_refresh() -> Result<()> {
        let rows: Vec<_> = (0..29)
            .map(|i| row(&format!("@{i}"), &format!("%{i}"), i).join("\x1f"))
            .collect();
        let mut poll = parse_snapshot_with_groups(&rows.join("\n"), &serde_json::Value::Null)?;
        let start = Instant::now();
        let mut scheduler = CaptureScheduler::default();
        scheduler.schedule(&poll, None, start);
        let mut last: HashMap<_, _> = poll
            .signatures
            .keys()
            .map(|id| (id.clone(), start))
            .collect();
        for step in 1..=80 {
            let now = start + POLL_INTERVAL * step;
            let plan = scheduler.schedule(&poll, None, now);
            assert!(
                plan.panes.len() < 6,
                "round robin must stay spread after backoff"
            );
            for id in plan.panes {
                last.insert(id, now);
            }
            assert!(last
                .values()
                .all(|last| now.duration_since(*last) <= REFRESH_INTERVAL));
            if step >= 8 {
                assert_eq!(scheduler.poll_interval(), Duration::from_secs(1));
            }
        }
        assert!(!scheduler.poll_due(start + POLL_INTERVAL * 81));
        for step in 81..=84 {
            let now = start + POLL_INTERVAL * step;
            assert!(scheduler
                .schedule(&poll, Some("%0"), now)
                .panes
                .contains(&"%0".into()));
        }
        poll.signatures.get_mut("%1").unwrap().cursor.x += 1;
        let now = start + POLL_INTERVAL * 88;
        assert!(scheduler
            .schedule(&poll, None, now)
            .panes
            .contains(&"%1".into()));
        assert_eq!(scheduler.poll_interval(), POLL_INTERVAL);
        assert!(scheduler.poll_due(now + POLL_INTERVAL));
        Ok(())
    }

    #[test]
    fn private_actions_sessions_and_link_dedup_are_explicit() -> Result<()> {
        let server = testing::PrivateServer::start(80, 24, "sleep 60")?;
        let tmux = server.tmux();
        assert_eq!(tmux.current_session()?, "ncm-test");
        let w = &tmux.snapshot()?[0];
        tmux.run(&[
            "set-option",
            "-p",
            "-t",
            &w.panes[0].id,
            "@pane_label",
            "generic",
        ])?;
        tmux.run(&["select-pane", "-t", &w.panes[0].id, "-T", "Ubuntu"])?;
        assert_eq!(tmux.pane_env_word(&w.panes[0].id)?, "wsl");
        tmux.run(&["new-session", "-d", "-s", "linked", "sleep 60"])?;
        tmux.run(&["link-window", "-s", &w.id, "-t", "linked:8"])?;
        tmux.run(&["link-window", "-s", &w.id, "-t", "linked:9"])?;
        let linked = tmux.clone().with_session("linked").snapshot()?;
        assert_eq!(linked.iter().filter(|win| win.id == w.id).count(), 1);
        assert_eq!(
            linked
                .iter()
                .find(|win| win.id == w.id)
                .unwrap()
                .panes
                .len(),
            1
        );
        tmux.rename_window(&w.id, "renamed ; literal;")?;
        assert_eq!(
            tmux.snapshot()?
                .iter()
                .find(|win| win.id == w.id)
                .unwrap()
                .name,
            "renamed ; literal;"
        );
        let created = tmux.new_window("ncm-test", "new", "sleep 60")?;
        let pane = tmux
            .snapshot()?
            .iter()
            .find(|win| win.id == created)
            .unwrap()
            .panes[0]
            .id
            .clone();
        tmux.select(&pane)?;
        assert!(
            tmux.snapshot()?
                .iter()
                .find(|win| win.id == created)
                .unwrap()
                .active
        );
        tmux.kill_window(&created)?;
        assert!(!tmux.snapshot()?.iter().any(|win| win.id == created));
        assert!(tmux.capture(&["bad".into()]).is_err());
        Ok(())
    }

    #[test]
    fn private_literal_text_and_named_keys() -> Result<()> {
        let server = testing::PrivateServer::start(
            80,
            24,
            "stty -echo; IFS= read -r line; printf 'got<%s>' \"$line\"; sleep 60",
        )?;
        let tmux = server.tmux();
        let pane = tmux.snapshot()?[0].panes[0].id.clone();
        // Wait for the private shell's input setup, then deliberately include tmux syntax.
        std::thread::sleep(Duration::from_millis(100));
        tmux.send_text(&pane, "-hello $HOME ")?;
        tmux.send_text(&pane, ";")?;
        tmux.send_text(&pane, r"\;")?;
        tmux.send_key(&pane, "Enter")?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let capture = tmux.capture(std::slice::from_ref(&pane))?;
            if capture[&pane]
                .iter()
                .any(|l| l.contains(r"got<-hello $HOME ;\;>"))
            {
                break;
            }
            anyhow::ensure!(
                Instant::now() < deadline,
                "literal text/keys were not forwarded: {:?}",
                capture[&pane]
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}
