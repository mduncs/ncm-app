use anyhow::Result;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ncm_model::{CellStyle, Grid};
use std::{
    fmt::Write as _,
    io::{self, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

pub static STOP: AtomicBool = AtomicBool::new(false);
// ST closes an interrupted APC/DCS packet before terminal mode commands.
const RESTORE: &[u8] =
    b"\x1b\\\x1b[?2026l\x1b[0m\x1b[?1000l\x1b[?1006l\x1b[?2004l\x1b[?7h\x1b[?25h\x1b[?1049l";
const ENTER: &[u8] = b"\x1b[?1049h\x1b[2J\x1b[?25l\x1b[?7l\x1b[?1000h\x1b[?1006h\x1b[?2004h";
extern "C" fn stop(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}
/// Owns terminal modes and prior signal handlers. No terminal I/O in signal context.
pub struct Terminal {
    handlers: Vec<(libc::c_int, libc::sigaction)>,
    output_flags: libc::c_int,
    original_modes: libc::termios,
}
impl Terminal {
    pub fn enter() -> Result<Self> {
        STOP.store(false, Ordering::Relaxed);
        // Keep an independent copy: tmux may exit while this guard is suspended.
        let original_modes = unsafe {
            let mut modes = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut modes) != 0 {
                return Err(io::Error::last_os_error().into());
            }
            modes
        };
        enable_raw_mode()?;
        // SAFETY: fcntl on our terminal descriptor; restore the original flags on drop.
        let output_flags = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFL) };
        if output_flags >= 0 {
            unsafe {
                libc::fcntl(
                    libc::STDOUT_FILENO,
                    libc::F_SETFL,
                    output_flags | libc::O_NONBLOCK,
                );
            }
        }
        let mut guard = Self {
            handlers: Vec::new(),
            output_flags,
            original_modes,
        };
        // SAFETY: initialized sigaction structures and valid pointers; handlers only set an atomic.
        unsafe {
            for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
                let mut old: libc::sigaction = std::mem::zeroed();
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = stop as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, &mut old) == 0 {
                    guard.handlers.push((signal, old));
                }
            }
        }
        TtyWriter.write_all(ENTER)?;
        TtyWriter.flush()?;
        Ok(guard)
    }
    /// Call only after the input and output workers have acknowledged suspension.
    pub fn suspend(&mut self) {
        restore(&self.original_modes);
        if self.output_flags >= 0 {
            unsafe {
                libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, self.output_flags);
            }
        }
    }
    pub fn resume(&mut self) -> Result<()> {
        enable_raw_mode()?;
        if self.output_flags >= 0 {
            unsafe {
                libc::fcntl(
                    libc::STDOUT_FILENO,
                    libc::F_SETFL,
                    self.output_flags | libc::O_NONBLOCK,
                );
            }
        }
        TtyWriter.write_all(ENTER)?;
        Ok(())
    }
}
fn restore(original_modes: &libc::termios) {
    let _ = disable_raw_mode();
    // SAFETY: a termios returned by tcgetattr on this same descriptor at entry.
    unsafe {
        libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, original_modes);
    }
    emergency_write(RESTORE);
}
fn emergency_write(mut bytes: &[u8]) {
    // An input-triggered exit can interrupt a large frame with a full tty queue.
    // Give the small restore sequence a bounded chance to drain, bypassing locks and STOP.
    let end = Instant::now() + Duration::from_millis(25);
    while !bytes.is_empty() {
        // SAFETY: fixed valid byte slice and initialized pollfd; neither operation can block.
        let n = unsafe { libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len()) };
        if n > 0 {
            bytes = &bytes[n as usize..];
        } else {
            if Instant::now() >= end {
                break;
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                break;
            }
            let mut fd = libc::pollfd {
                fd: libc::STDOUT_FILENO,
                events: libc::POLLOUT,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut fd, 1, 1);
            }
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        // All ordinary writers observe STOP before this owner leaves the alternate screen.
        STOP.store(true, Ordering::Relaxed);
        if let Some(failure) = crate::panic_log::failure().filter(|f| f.worker) {
            let (width, height) = crossterm::terminal::size().unwrap_or((80, 24));
            let line: String = failure
                .dock()
                .chars()
                .filter(|c| !c.is_control())
                .take(width as usize)
                .collect();
            emergency_write(
                format!(
                    "\x1b\\\x1b[?2026l\x1b[0m\x1b[{};1H\x1b[2K{line}",
                    height.max(1)
                )
                .as_bytes(),
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        restore(&self.original_modes);
        if self.output_flags >= 0 {
            unsafe {
                libc::fcntl(libc::STDOUT_FILENO, libc::F_SETFL, self.output_flags);
            }
        }
        for (signal, old) in &self.handlers {
            // SAFETY: restoring a previously returned signal action.
            unsafe {
                libc::sigaction(*signal, old, std::ptr::null_mut());
            }
        }
    }
}
/// Bounded waits, including when the tty stops draining. Never retry a shutdown as EINTR.
pub struct TtyWriter;
static OUTPUT_LOCK: Mutex<()> = Mutex::new(());
impl TtyWriter {
    /// Legacy tmux mode never writes graphics, including probes and cleanup.
    pub fn write_graphics(&mut self, bytes: &[u8]) -> io::Result<bool> {
        if std::env::var_os("TMUX").is_some() {
            return Ok(false);
        }
        self.write_all(bytes)?;
        Ok(true)
    }
}
impl Write for TtyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _guard = OUTPUT_LOCK.lock().unwrap();
        write_tty(bytes)
    }
    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        // A re-probe must never land inside an image packet or continuation.
        let _guard = OUTPUT_LOCK.lock().unwrap();
        while !bytes.is_empty() {
            let n = write_tty(bytes)?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[n..];
        }
        Ok(())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn write_tty(bytes: &[u8]) -> io::Result<usize> {
    loop {
        if STOP.load(Ordering::Relaxed) || !tty_alive() {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "terminal closed"));
        }
        // SAFETY: byte slice valid for its length; stdout is nonblocking while Terminal lives.
        let n = unsafe { libc::write(libc::STDOUT_FILENO, bytes.as_ptr().cast(), bytes.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let error = io::Error::last_os_error();
        if matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            let mut fd = libc::pollfd {
                fd: libc::STDOUT_FILENO,
                events: libc::POLLOUT,
                revents: 0,
            };
            unsafe {
                libc::poll(&mut fd, 1, 5);
            }
            continue;
        }
        STOP.store(true, Ordering::Relaxed);
        return Err(error);
    }
}
/// Crossterm can keep reporting readiness at EOF. Detect terminal hangup independently.
pub fn tty_alive() -> bool {
    let mut fd = libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd, zero timeout, no consumption of terminal input.
    let result = unsafe { libc::poll(&mut fd, 1, 0) };
    let alive = result >= 0 && fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0;
    if !alive && (result >= 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted) {
        STOP.store(true, Ordering::Relaxed);
    }
    alive
}
#[derive(Debug)]
pub struct Capabilities {
    pub graphics: bool,
    pub confirmed: bool,
    pub pending: bool,
    pub pixels: (u16, u16),
    pub tmux: bool,
    pub reason: String,
}
impl Capabilities {
    pub fn pending() -> Self {
        let tmux = std::env::var_os("TMUX").is_some();
        Self {
            graphics: false,
            confirmed: false,
            pending: !tmux,
            pixels: cell_pixels().unwrap_or((8, 16)),
            tmux,
            reason: if tmux {
                "braille: inside tmux"
            } else {
                "graphics: probing"
            }
            .into(),
        }
    }
    /// The regular input decoder owns the reply; send exactly once at startup.
    pub fn send_probe() -> Result<Self> {
        let caps = Self::pending();
        if !caps.tmux {
            let mut query = ncm_render::KittyEncoder::default().query(255);
            if cell_pixels().is_none() {
                query.extend_from_slice(b"\x1b[16t");
            }
            TtyWriter.write_graphics(&query)?;
        }
        Ok(caps)
    }
}

fn cell_pixels() -> Option<(u16, u16)> {
    // SAFETY: TIOCGWINSZ writes a winsize to a valid initialized pointer.
    let ws = unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) != 0 {
            return None;
        }
        ws
    };
    if ws.ws_col > 0 && ws.ws_row > 0 && ws.ws_xpixel >= ws.ws_col && ws.ws_ypixel >= ws.ws_row {
        Some((ws.ws_xpixel / ws.ws_col, ws.ws_ypixel / ws.ws_row))
    } else {
        None
    }
}
pub fn parse_cell_reply(text: &str) -> Option<(u16, u16)> {
    let tail = text.split("\x1b[6;").nth(1)?.split('t').next()?;
    let (h, w) = tail.split_once(';')?;
    let size = (w.parse().ok()?, h.parse().ok()?);
    (size.0 > 0 && size.1 > 0).then_some(size)
}
#[derive(Default)]
pub struct DiffWriter {
    previous: Option<Grid>,
    cursor: Option<(usize, usize)>,
}
impl DiffWriter {
    /// Generate row/run differences, including wide-cell boundaries, in one synchronized update.
    /// An identical frame and cursor returns no bytes, including no sync or SGR sequences.
    pub fn draw(
        &mut self,
        current: Grid,
        cursor: Option<(usize, usize)>,
        images: &[u8],
        out: &mut impl Write,
    ) -> Result<usize> {
        let mut body = Vec::new();
        body.extend_from_slice(images);
        for y in 0..current.height() {
            let row = current.row(y).unwrap();
            let old = self
                .previous
                .as_ref()
                .filter(|p| p.width() == current.width())
                .and_then(|p| p.row(y));
            if old == Some(row) {
                continue;
            }
            let mut x = 0;
            while x < row.len() {
                if old.is_some_and(|o| o[x] == row[x]) {
                    x += 1;
                    continue;
                }
                let mut start = x;
                if row[x].text.starts_with('\u{10eeee}') {
                    start = 0;
                }
                if start > 0 && (row[start].width == 0 || old.is_some_and(|o| o[start].width == 0))
                {
                    start -= 1;
                }
                x += 1;
                while x < row.len() && old.is_none_or(|o| o[x] != row[x]) {
                    x += 1;
                }
                if x < row.len() && (row[x].width == 0 || old.is_some_and(|o| o[x].width == 0)) {
                    x += 1;
                }
                write!(body, "\x1b[{};{}H", y + 1, start + 1)?;
                let mut style = None;
                let mut cells = row[start..x].iter().peekable();
                while let Some(cell) = cells.next() {
                    if cell.width == 0 {
                        continue;
                    }
                    let placeholder = cell.text.starts_with('\u{10eeee}');
                    let next = (cell.style, placeholder);
                    if style != Some(next) {
                        body.extend(sgr(cell.style, placeholder).as_bytes());
                        style = Some(next);
                    }
                    body.extend_from_slice(cell.text.as_bytes());
                    // Placeholder columns after the anchored first cell are identical.
                    // REP preserves tmux's cell stream while avoiding tens of KB of
                    // repeated UTF-8 on every Quick Look open/close.
                    if cell.text == "\u{10eeee}" {
                        let mut repeats = 0;
                        while cells.peek().is_some_and(|next| *next == cell) {
                            cells.next();
                            repeats += 1;
                        }
                        if repeats > 1 {
                            write!(body, "\x1b[{repeats}b")?;
                        } else if repeats == 1 {
                            body.extend_from_slice(cell.text.as_bytes());
                        }
                    }
                }
            }
        }
        if cursor != self.cursor || (!body.is_empty() && cursor.is_some()) {
            if let Some((x, y)) = cursor {
                write!(body, "\x1b[{};{}H\x1b[?25h", y + 1, x + 1)?;
            } else {
                body.extend(b"\x1b[?25l");
            }
        }
        let count = if body.is_empty() {
            0
        } else {
            let open = b"\x1b[?2026h\x1b[?25l";
            let close = b"\x1b[0m\x1b[?2026l";
            out.write_all(open)?;
            out.write_all(&body)?;
            out.write_all(close)?;
            out.flush()?;
            body.len() + open.len() + close.len()
        };
        self.previous = Some(current);
        self.cursor = cursor;
        Ok(count)
    }
}
fn sgr(s: CellStyle, placeholder: bool) -> String {
    let mut out = "\x1b[0".to_owned();
    for (enabled, code) in [
        (s.bold, 1),
        (s.dim, 2),
        (s.italic, 3),
        (s.underline, 4),
        (s.reverse, 7),
    ] {
        if enabled {
            let _ = write!(out, ";{code}");
        }
    }
    if let Some(c) = s.fg {
        if placeholder && c.r == 0 && c.g == 0 {
            let _ = write!(out, ";38;5;{}", c.b);
        } else {
            let _ = write!(out, ";38;2;{};{};{}", c.r, c.g, c.b);
        }
    }
    if let Some(c) = s.bg {
        let _ = write!(out, ";48;2;{};{};{}", c.r, c.g, c.b);
    }
    out.push('m');
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    use ncm_model::{Cell, Rgb};
    #[test]
    fn idle_writes_nothing_and_changed_run_does_not_clear() -> Result<()> {
        let mut writer = DiffWriter::default();
        let mut out = Vec::new();
        let mut grid = Grid::new(12, 3, Cell::default())?;
        writer.draw(grid.clone(), None, &[], &mut out)?;
        out.clear();
        assert_eq!(writer.draw(grid.clone(), None, &[], &mut out)?, 0);
        assert!(out.is_empty());
        grid.get_mut(6, 1).unwrap().text = "x".into();
        writer.draw(grid, None, &[], &mut out)?;
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\x1b[2;7H"));
        assert!(!text.contains("2J"));
        assert_eq!(text.matches('x').count(), 1);
        Ok(())
    }
    #[test]
    fn placeholder_rep_expands_in_tmux_without_repeating_anchor_diacritics() -> Result<()> {
        let grid = ncm_render::KittyEncoder::default().placeholders(200, 180, 1)?;
        let mut bytes = Vec::new();
        DiffWriter::default().draw(grid, None, &[], &mut bytes)?;
        assert!(bytes.len() < 150, "placeholder row must stay compact");
        let text = String::from_utf8(bytes)?;
        let command = format!("printf '%s' '{}'; sleep 60", text.replace('\'', "'\\''"));
        let server = ncm_tmux::testing::PrivateServer::start(180, 3, &command)?;
        let pane = server.tmux().snapshot()?[0].panes[0].id.clone();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let captured = server.tmux().capture(std::slice::from_ref(&pane))?;
            let first = &captured[&pane][0];
            if first.matches('\u{10eeee}').count() == 180 {
                assert_eq!(first.matches('\u{0305}').count(), 2);
                return Ok(());
            }
            assert!(
                Instant::now() < deadline,
                "tmux did not expand REP: {first:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn wide_glyph_replacement_repairs_leader_and_continuation() -> Result<()> {
        let mut writer = DiffWriter::default();
        let mut out = Vec::new();
        let mut g = Grid::new(4, 1, Cell::default())?;
        *g.get_mut(1, 0).unwrap() = Cell {
            text: "界".into(),
            width: 2,
            style: CellStyle::default(),
        };
        *g.get_mut(2, 0).unwrap() = Cell::continuation(CellStyle::default());
        writer.draw(g.clone(), None, &[], &mut out)?;
        out.clear();
        *g.get_mut(1, 0).unwrap() = Cell::default();
        *g.get_mut(2, 0).unwrap() = Cell::default();
        writer.draw(g, None, &[], &mut out)?;
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("\x1b[1;2H\x1b[0m  "));
        assert!(sgr(
            CellStyle {
                fg: Some(Rgb::new(0, 0, 42)),
                ..Default::default()
            },
            true
        )
        .contains("38;5;42"));
        Ok(())
    }
    #[test]
    fn restore_closes_partial_apc_before_leaving_alt_screen() -> Result<()> {
        let bytes = [
            b"\x1b[?1049h\x1b_Ga=T;partial".as_slice(),
            RESTORE,
            b"\r\nRESTORED",
        ]
        .concat();
        let command = format!("printf '%s' '{}'; sleep 60", String::from_utf8(bytes)?);
        let server = ncm_tmux::testing::PrivateServer::start(40, 3, &command)?;
        let pane = server.tmux().snapshot()?[0].panes[0].id.clone();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let rows = server.tmux().capture(std::slice::from_ref(&pane))?;
            if rows[&pane].iter().any(|row| row.contains("RESTORED")) {
                return Ok(());
            }
            assert!(
                Instant::now() < deadline,
                "restore was swallowed by a partial escape string"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn parses_pixel_reply() {
        assert_eq!(parse_cell_reply("\x1b[6;18;9t"), Some((9, 18)));
        assert_eq!(parse_cell_reply("\x1b[6;0;0t"), None);
    }
}
