//! Shared, serializable data contracts. Coordinates and sizes are terminal cells unless noted.
//! No tmux commands, terminal I/O, rendering, or transcript discovery belongs here.

use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

/// tmux's stable pane identifier, including its `%` prefix (for example `%12`).
pub type PaneId = String;
/// tmux's stable window identifier, including its `@` prefix. Indices are not identities.
pub type WindowId = String;

/// An opaque, unpremultiplied sRGB color with eight bits per channel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Construct an sRGB color.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Return RGBA bytes suitable for a raw kitty image; alpha is supplied by the caller.
    pub const fn rgba(self, alpha: u8) -> [u8; 4] {
        [self.r, self.g, self.b, alpha]
    }
}

/// SGR attributes. `None` colors mean terminal defaults, not black or transparent.
/// Renderers resolve default background to the pane tint before applying reverse video.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CellStyle {
    /// Explicit foreground, or the terminal default.
    pub fg: Option<Rgb>,
    /// Explicit background, or the pane/terminal default.
    pub bg: Option<Rgb>,
    /// Bold weight.
    pub bold: bool,
    /// Reduced intensity.
    pub dim: bool,
    /// Italic face.
    pub italic: bool,
    /// Underlined glyphs.
    pub underline: bool,
    /// Swap resolved foreground and background when drawing.
    pub reverse: bool,
}

/// One physical grid slot. A leading cell holds a base character plus combining marks.
/// `width` is 1 or 2 on a leader; a wide glyph's next slot is empty text with width 0.
/// Blank cells contain a space and width 1. Width 0 is never an independently drawn glyph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
    /// Unicode text, including any combining marks.
    pub text: String,
    /// Display width (0 for a continuation, 1 or 2 for a leading cell).
    pub width: u8,
    /// SGR attributes, retained even on blank and continuation cells.
    pub style: CellStyle,
}

impl Cell {
    /// A space with the given style.
    pub fn blank(style: CellStyle) -> Self {
        Self {
            text: " ".into(),
            width: 1,
            style,
        }
    }

    /// The trailing slot of a double-width glyph.
    pub fn continuation(style: CellStyle) -> Self {
        Self {
            text: String::new(),
            width: 0,
            style,
        }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank(CellStyle::default())
    }
}

/// Invalid grid dimensions or storage, including malformed fixture data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GridError {
    /// Width multiplied by height overflowed `usize`.
    #[error("grid dimensions overflow")]
    Dimensions,
    /// Storage does not contain exactly width × height slots.
    #[error("grid needs {expected} cells, got {actual}")]
    CellCount {
        /// Required physical slot count.
        expected: usize,
        /// Supplied physical slot count.
        actual: usize,
    },
}

/// Rectangular, row-major physical slots, including wide-glyph continuations.
/// Dimensions are immutable; construction and fixture deserialization check the slot count.
/// Clones share immutable rows; mutation copies only its row. The flat/JSON APIs stay row-major.
#[derive(Debug, Serialize, Deserialize)]
#[serde(try_from = "GridData", into = "GridData")]
pub struct Grid {
    width: usize,
    height: usize,
    rows: Vec<Arc<Vec<Cell>>>,
    flat: OnceLock<Vec<Cell>>,
}

#[derive(Serialize, Deserialize)]
struct GridData {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
}

impl Clone for Grid {
    fn clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            rows: self.rows.clone(),
            flat: OnceLock::new(),
        }
    }
}
impl PartialEq for Grid {
    fn eq(&self, other: &Self) -> bool {
        self.width == other.width && self.height == other.height && self.rows == other.rows
    }
}
impl Eq for Grid {}
impl From<Grid> for GridData {
    fn from(grid: Grid) -> Self {
        Self {
            width: grid.width,
            height: grid.height,
            cells: grid
                .rows
                .iter()
                .flat_map(|row| row.iter().cloned())
                .collect(),
        }
    }
}

impl TryFrom<GridData> for Grid {
    type Error = GridError;
    fn try_from(data: GridData) -> Result<Self, Self::Error> {
        Self::from_cells(data.width, data.height, data.cells)
    }
}

impl Grid {
    /// Construct a filled grid; zero-sized grids are valid.
    pub fn new(width: usize, height: usize, fill: Cell) -> Result<Self, GridError> {
        width.checked_mul(height).ok_or(GridError::Dimensions)?;
        let rows = if width == 0 || height == 0 {
            Vec::new()
        } else {
            let row = Arc::new(vec![fill; width]);
            vec![row; height]
        };
        Ok(Self {
            width,
            height,
            rows,
            flat: OnceLock::new(),
        })
    }

    /// Construct from physical slots, rejecting mismatched dimensions.
    pub fn from_cells(width: usize, height: usize, cells: Vec<Cell>) -> Result<Self, GridError> {
        let expected = width.checked_mul(height).ok_or(GridError::Dimensions)?;
        if cells.len() != expected {
            return Err(GridError::CellCount {
                expected,
                actual: cells.len(),
            });
        }
        let mut cells = cells.into_iter();
        let rows = if width == 0 {
            Vec::new()
        } else {
            (0..height)
                .map(|_| Arc::new(cells.by_ref().take(width).collect()))
                .collect()
        };
        Ok(Self {
            width,
            height,
            rows,
            flat: OnceLock::new(),
        })
    }

    /// Width in physical terminal columns.
    pub fn width(&self) -> usize {
        self.width
    }
    /// Height in terminal rows.
    pub fn height(&self) -> usize {
        self.height
    }
    /// All slots in row-major order.
    pub fn cells(&self) -> &[Cell] {
        self.flat.get_or_init(|| {
            self.rows
                .iter()
                .flat_map(|row| row.iter().cloned())
                .collect()
        })
    }
    /// A physical slot, or `None` outside the grid.
    pub fn get(&self, x: usize, y: usize) -> Option<&Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.rows.get(y)?.get(x)
    }
    /// A mutable physical slot, or `None` outside the grid.
    pub fn get_mut(&mut self, x: usize, y: usize) -> Option<&mut Cell> {
        if x >= self.width || y >= self.height {
            return None;
        }
        self.flat.take();
        Arc::make_mut(self.rows.get_mut(y)?).get_mut(x)
    }
    /// A complete row, or `None` outside the grid (including valid zero-width rows).
    pub fn row(&self, y: usize) -> Option<&[Cell]> {
        if y >= self.height {
            return None;
        }
        if self.width == 0 {
            Some(&[])
        } else {
            self.rows.get(y).map(|row| row.as_slice())
        }
    }
}

/// Wall-clock timestamp in milliseconds since the Unix epoch, shared by transcript adapters.
/// Missing/invalid transcript times are represented by `Option<Timestamp>`, never fabricated ages.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// Nonnegative age; clock skew or a future event yields zero.
    pub fn age_at(self, now: Self) -> Duration {
        Duration::from_millis(now.0.saturating_sub(self.0).max(0) as u64)
    }
}

/// A harness name and its label color; arbitrary names allow future adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Harness {
    /// Lowercase display/adapter name, e.g. `claude` or `codex`.
    pub name: String,
    /// tmux override or built-in harness color.
    pub color: Rgb,
}

/// Persistent agent-alert state. Flags such as bell/activity remain separate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// No harness state available.
    #[default]
    None,
    /// Needs an explicit user response; attention does not decay.
    Needs,
    /// Agent is working.
    Working,
    /// Agent completed work.
    Done,
}

impl AgentState {
    /// Default state edge/flag color from SPEC, with no age-based fading.
    pub fn color(self) -> Option<Rgb> {
        match self {
            Self::None => None,
            Self::Needs => Some(Rgb::new(255, 46, 136)),
            Self::Working => Some(Rgb::new(76, 201, 240)),
            Self::Done => Some(Rgb::new(126, 231, 135)),
        }
    }
}

/// Pane-relative cursor location, zero-based, in terminal cells.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Physical column.
    pub x: usize,
    /// Row.
    pub y: usize,
}

/// A pane's tmux identity, geometry, metadata and appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    /// Stable `%N` identity, used as the command target.
    pub id: PaneId,
    /// Current pane index; may change without changing identity.
    pub index: u32,
    /// Left edge in the containing window.
    pub x: usize,
    /// Top edge in the containing window.
    pub y: usize,
    /// Pane width, excluding separators.
    pub width: usize,
    /// Pane height, excluding separators.
    pub height: usize,
    /// Is this the window's active pane?
    pub active: bool,
    /// Pane root process id, not necessarily the harness process.
    pub pid: u32,
    /// tmux's current working directory.
    pub cwd: PathBuf,
    /// Pane title, using `@pane_label` when present.
    pub title: String,
    /// Pane-relative cursor location.
    pub cursor: Cursor,
    /// Background from per-pane `window-style`; absent means choose a fallback tint.
    pub tint: Option<Rgb>,
    /// User override or detected harness; absent means Screen only unless discovered later.
    pub harness: Option<Harness>,
}

/// Both parsed attention flags and the original tmux status flags.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowFlags {
    /// Original flags, e.g. `*`, `!`, `#`, `~`, `-`, `Z`.
    pub raw: String,
    /// Bell needs attention.
    pub bell: bool,
    /// tmux monitored activity.
    pub activity: bool,
    /// tmux monitored silence.
    pub silence: bool,
}

impl WindowFlags {
    /// Whether tmux's original window flags contain the zoom flag.
    pub fn zoomed(&self) -> bool {
        self.raw.contains('Z')
    }
}

/// Server/window pane separator styles, independent of the stable window metadata contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BorderStyles {
    /// Style for separators that do not touch the active pane.
    pub normal: CellStyle,
    /// Style for separators that touch the active pane.
    pub active: CellStyle,
}

impl Default for BorderStyles {
    fn default() -> Self {
        let style = CellStyle {
            fg: Some(Rgb::new(60, 64, 74)),
            ..CellStyle::default()
        };
        Self {
            normal: style,
            active: style,
        }
    }
}

impl PaneInfo {
    /// Environment word inferred from the pane label/title, matching wallcore.py.
    pub fn env_word(&self) -> &'static str {
        let title = self.title.to_lowercase();
        if title.contains("powershell") || title.contains("windows") {
            "ps"
        } else if title.contains("ubuntu") || title.contains("wsl") {
            "wsl"
        } else {
            ""
        }
    }
}

/// Expanded status tab style and marker; absent colors use standard terminal defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabStyle {
    /// Status tab foreground.
    pub fg: Option<Rgb>,
    /// Status tab background.
    pub bg: Option<Rgb>,
    /// Status tab bold weight.
    pub bold: bool,
    /// Expanded `@status_marker_window` or state fallback.
    pub marker: String,
}

/// A whole window. Linked windows can occur once per session with the same stable `id`.
/// Consumers use `(session, id)` for a status-bar entry and `id` for window content caches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Owning session name, not a transcript session id.
    pub session: String,
    /// Current status bar index within the session.
    pub index: u32,
    /// Stable tmux `@N` window identity.
    pub id: WindowId,
    /// tmux window name.
    pub name: String,
    /// Is active within this session?
    pub active: bool,
    /// Bell/activity/silence and original status flags.
    pub flags: WindowFlags,
    /// `window_activity`, converted from epoch seconds to milliseconds; absent if unavailable.
    pub activity: Option<Timestamp>,
    /// `@dashboard_group` or window-groups.json section; `None` means ungrouped.
    pub group: Option<String>,
    /// Real status tab colors and marker.
    pub tab_style: TabStyle,
    /// Agent state is persistent until changed by tmux/user hooks.
    pub agent_state: AgentState,
    /// Every pane, in pane-index order, including inactive panes.
    pub panes: Vec<PaneInfo>,
    /// Whole window width.
    pub width: usize,
    /// Whole window height.
    pub height: usize,
}

impl WindowInfo {
    /// Default pane for Quick Look, facts and Talk; use the first pane if none is marked active.
    pub fn active_pane(&self) -> Option<&PaneInfo> {
        self.panes
            .iter()
            .find(|pane| pane.active)
            .or_else(|| self.panes.first())
    }
}

/// How a now line is emphasized in card subtitles.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NowKind {
    /// Ordinary text.
    #[default]
    Text,
    /// Tool name (amber bold) and arguments.
    Tool,
    /// Spinner/progress text (blue).
    Spinner,
}

/// Latest useful activity, extracted from a transcript first, terminal text as fallback.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NowLine {
    /// Text, tool or spinner presentation.
    pub kind: NowKind,
    /// Complete readable line.
    pub text: String,
    /// Tool name to emphasize; only meaningful for `NowKind::Tool`.
    pub tool: Option<String>,
}

/// A visible subagent's activity state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentState {
    /// Running work.
    Busy,
    /// Available/finished, rendered quietly.
    Idle,
}

/// One visible subagent; unknown runtimes remain absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    /// Harness agent name.
    pub name: String,
    /// Busy or idle.
    pub state: SubagentState,
    /// Elapsed running time, without assuming a transcript timestamp exists.
    pub running_time: Option<Duration>,
}

/// Facts shown in overlays and the Quick Look header; missing metadata stays absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Facts {
    /// Current tool/spinner/text line.
    pub now: NowLine,
    /// Model identifier/display name.
    pub model: Option<String>,
    /// Context occupancy in tokens, not an already formatted `k` label.
    pub context_tokens: Option<u64>,
    /// Visible subagents with independent busy/idle state and elapsed time.
    pub subagents: Vec<Subagent>,
    /// Additional subagents whose names/details are unavailable.
    pub hidden_count: usize,
}

impl Facts {
    /// Busy visible agents; hidden agents have unknown activity and are not guessed.
    pub fn busy_count(&self) -> usize {
        self.subagents
            .iter()
            .filter(|agent| agent.state == SubagentState::Busy)
            .count()
    }
}

/// Chronological transcript events, classified by the turn that started the reply.
/// A reply to a report never answers a user message. Queued human messages count as `You`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TalkEvent {
    /// Human text, including typed/queued/suggestion-accepted prompts.
    You {
        /// Original event time, when known.
        timestamp: Option<Timestamp>,
        /// Human message body.
        body: String,
    },
    /// Assistant text responding to a human turn.
    Reply {
        /// Original event time, when known.
        timestamp: Option<Timestamp>,
        /// Assistant message body.
        body: String,
    },
    /// Task-notification or peer message.
    Report {
        /// Original event time, when known.
        timestamp: Option<Timestamp>,
        /// Reporting agent's name.
        agent: String,
        /// Report content/status.
        body: String,
    },
    /// Assistant text responding to a task-notification or peer turn.
    ReplyToReport {
        /// Original event time, when known.
        timestamp: Option<Timestamp>,
        /// Reporting agent when attribution is available.
        agent: Option<String>,
        /// Assistant message body.
        body: String,
    },
}

impl TalkEvent {
    /// Original timestamp, with missing metadata preserved.
    pub fn timestamp(&self) -> Option<Timestamp> {
        match self {
            Self::You { timestamp, .. }
            | Self::Reply { timestamp, .. }
            | Self::Report { timestamp, .. }
            | Self::ReplyToReport { timestamp, .. } => *timestamp,
        }
    }

    /// Readable body for each event kind.
    pub fn body(&self) -> &str {
        match self {
            Self::You { body, .. }
            | Self::Reply { body, .. }
            | Self::Report { body, .. }
            | Self::ReplyToReport { body, .. } => body,
        }
    }
}

/// A harness transcript session discovered from a pane process, without attaching clients/hooks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRef {
    /// Harness/adapter and label color.
    pub harness: Harness,
    /// Harness-specific session identifier (not a tmux session name).
    pub session_id: String,
    /// Original transcript path; followers read this file without copying it into the repo.
    pub transcript_path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_clones_share_rows_and_invalidate_flat_views_after_mutation() {
        let original = Grid::new(6, 3, Cell::default()).unwrap();
        let mut changed = original.clone();
        assert!(Arc::ptr_eq(&original.rows[0], &changed.rows[0]));
        assert_eq!(original.cells(), changed.cells());
        changed.get_mut(2, 1).unwrap().text = "x".into();
        assert!(Arc::ptr_eq(&original.rows[0], &changed.rows[0]));
        assert!(!Arc::ptr_eq(&original.rows[1], &changed.rows[1]));
        assert_ne!(original.get(2, 1), changed.get(2, 1));
        assert_eq!(changed.cells()[8].text, "x");
        assert_eq!(
            changed,
            serde_json::from_str::<Grid>(&serde_json::to_string(&changed).unwrap()).unwrap()
        );
        assert_eq!(
            Grid::new(0, 3, Cell::default()).unwrap().row(2),
            Some(&[][..])
        );
        assert!(Grid::new(usize::MAX, 2, Cell::default()).is_err());
    }

    #[test]
    fn grid_fixtures_validate_dimensions_and_preserve_wide_slots() {
        let grid = Grid::from_cells(
            2,
            1,
            vec![
                Cell {
                    text: "界".into(),
                    width: 2,
                    style: CellStyle::default(),
                },
                Cell::continuation(CellStyle::default()),
            ],
        )
        .unwrap();
        let json = serde_json::to_string(&grid).unwrap();
        assert_eq!(serde_json::from_str::<Grid>(&json).unwrap(), grid);
        assert!(serde_json::from_str::<Grid>(r#"{"width":2,"height":1,"cells":[]}"#).is_err());
        assert!(grid.get(2, 0).is_none());
        assert_eq!(Timestamp(10).age_at(Timestamp(5)), Duration::ZERO);
    }
}
