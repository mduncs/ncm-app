//! Panic evidence and shutdown notification. The hook never touches terminal modes.
use std::{
    backtrace::Backtrace,
    fmt::{self, Write as _},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

const LIMIT: usize = 256 * 1024;
const RECORD_LIMIT: usize = 32 * 1024;
static FAILURE: Mutex<Option<Failure>> = Mutex::new(None);

#[derive(Clone)]
pub struct Failure {
    pub thread: String,
    pub message: String,
    pub worker: bool,
}
impl Failure {
    pub fn dock(&self) -> String {
        format!("panic in {}, see ~/.local/state/ncm/panic.log", self.thread)
    }
}
pub fn failure() -> Option<Failure> {
    FAILURE.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// Bound formatting, including arbitrary panic payloads and symbolized backtraces.
#[derive(Default)]
struct Record(String);
impl fmt::Write for Record {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut end = s.len().min(RECORD_LIMIT.saturating_sub(self.0.len()));
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.0.push_str(&s[..end]);
        Ok(())
    }
}
fn open(path: &std::path::Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
}
fn append(file: &mut File, record: &[u8]) -> io::Result<()> {
    let length = file.metadata()?.len();
    if length.saturating_add(record.len() as u64 + 1) > LIMIT as u64 {
        // Read at most half the cap, even if an existing log is unexpectedly huge.
        file.seek(SeekFrom::Start(length.saturating_sub((LIMIT / 2) as u64)))?;
        let mut tail = Vec::with_capacity(LIMIT / 2);
        file.take((LIMIT / 2) as u64).read_to_end(&mut tail)?;
        // Drop the first partial line (also avoids retaining a partial UTF-8 character).
        let start = tail
            .iter()
            .position(|&b| b == b'\n')
            .map_or(tail.len(), |i| i + 1);
        file.set_len(0)?;
        file.write_all(&tail[start..])?;
    }
    file.write_all(record)?;
    file.write_all(b"\n")
}

pub fn install() {
    let main = std::thread::current().id();
    let path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".local/state/ncm/panic.log"));
    // Resolve paths and open the log before entering raw mode or starting workers.
    let log = Mutex::new(path.as_deref().and_then(|p| open(p).ok()));
    std::panic::set_hook(Box::new(move |info| {
        let current = std::thread::current();
        let worker = current.id() != main;
        let thread = current.name().unwrap_or("unnamed");
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("non-string panic payload");
        let mut short = Record::default();
        let _ = write!(short, "{message}");
        let mut failure = FAILURE.lock().unwrap_or_else(|p| p.into_inner());
        if failure.is_none() {
            *failure = Some(Failure {
                thread: thread.into(),
                message: short.0,
                worker,
            });
        }
        let mut record = Record::default();
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let _ = writeln!(
            record,
            "timestamp_unix={}.{:03} thread={thread}",
            timestamp.as_secs(),
            timestamp.subsec_millis()
        );
        if let Some(location) = info.location() {
            let _ = writeln!(
                record,
                "location={}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            );
        }
        // Reserve space for the backtrace even for a huge panic message.
        let mut end = message.len().min(4096);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let _ = writeln!(
            record,
            "message={}\nbacktrace:\n{}",
            &message[..end],
            Backtrace::force_capture()
        );
        let mut file = log.lock().unwrap_or_else(|p| p.into_inner());
        if file.is_none() {
            *file = path.as_deref().and_then(|p| open(p).ok());
        }
        if let Some(file) = file.as_mut() {
            let _ = append(file, record.0.as_bytes());
        }
        // Publish shutdown only after evidence is written. Keeping FAILURE locked also
        // makes a concurrent signal/normal quit wait for this hook before restoring.
        crate::terminal::STOP.store(true, std::sync::atomic::Ordering::Relaxed);
        drop(failure);
    }));
}

#[cfg(test)]
pub fn inject(thread: &str) {
    if std::env::var("NCM_TEST_PANIC").as_deref() == Ok(thread) {
        std::thread::sleep(std::time::Duration::from_millis(250));
        panic!("injected {thread} panic");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_unicode_record() {
        let mut record = Record::default();
        let _ = write!(record, "{}", "🙂".repeat(RECORD_LIMIT));
        assert!(record.0.len() <= RECORD_LIMIT);
        assert!(record.0.ends_with('🙂'));
    }
    #[test]
    fn rotates_oldest_half_and_keeps_new_evidence() -> io::Result<()> {
        let path = std::env::temp_dir().join(format!("ncm-panic-rotation-{}", std::process::id()));
        let mut file = open(&path)?;
        file.set_len(0)?;
        file.write_all(&vec![b'x'; LIMIT / 2])?;
        file.write_all(b"\nrecent evidence\n")?;
        file.write_all(&vec![b'y'; LIMIT / 2 - 20])?;
        append(&mut file, b"\nnew panic\n")?;
        let text = fs::read_to_string(&path)?;
        assert!(text.len() <= LIMIT);
        assert!(text.contains("recent evidence"));
        assert!(!text.starts_with('x'));
        assert!(text.ends_with("new panic\n\n"));
        fs::remove_file(path)
    }
    // Invoked by panic_check.py in an owned PTY; injection exists only in this test binary.
    #[test]
    #[ignore]
    fn terminal_child() {
        if let Err(error) = crate::main() {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }
}
