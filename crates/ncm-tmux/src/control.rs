//! One attached, size-neutral control client. Pane output is discarded at the reader.
use super::Tmux;
use anyhow::{bail, Context, Result};
use std::{
    collections::HashSet,
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, Stdio},
    sync::{mpsc, Arc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

#[derive(Debug, Default)]
pub(crate) struct Events {
    pub dirty: HashSet<String>,
    pub metadata: bool,
    pub disconnected: bool,
    pub reset: bool,
}

#[derive(Debug)]
pub(crate) struct Control {
    child: Child,
    input: ChildStdin,
    replies: mpsc::Receiver<Result<String, String>>,
    events: Arc<Mutex<Events>>,
    reader: Option<JoinHandle<()>>,
}

impl Control {
    pub fn start(tmux: &Tmux) -> Result<Self> {
        let mut command = tmux.command();
        // -N prevents accidentally starting a server. No terminal/size negotiation,
        // session/window selection, option changes, or helper sessions are needed.
        command.args(["-N", "-C", "attach-session", "-f", "ignore-size"]);
        if let Some(session) = &tmux.session {
            command.args(["-t", &format!("={session}")]);
        } else if tmux.server.is_none() && tmux.socket.is_none() {
            if let Some(id) = super::inherited_session_id() {
                command.args(["-t", &format!("${id}")]);
            } else {
                command.args(["-t", "=main"]);
            }
        } else if tmux.socket.is_some() {
            command.args(["-t", "=main"]);
        }
        tmux.record_spawn();
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start tmux control client")?;
        let input = child.stdin.take().context("tmux control stdin")?;
        let output = child.stdout.take().context("tmux control stdout")?;
        let events = Arc::new(Mutex::new(Events {
            metadata: true,
            reset: true,
            ..Events::default()
        }));
        let state = events.clone();
        let (tx, replies) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("ncm-tmux-control".into())
            .spawn(move || {
                let result = read_protocol(BufReader::new(output), &tx, &state);
                state.lock().unwrap_or_else(|e| e.into_inner()).disconnected = true;
                let _ = tx.send(Err(result
                    .err()
                    .map_or_else(|| "tmux control EOF".into(), |e| e.to_string())));
            });
        let reader = match reader {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("start tmux control reader");
            }
        };
        let control = Self {
            child,
            input,
            replies,
            events,
            reader: Some(reader),
        };
        control.reply()?; // attach's initial %begin/%end (flags 0), before user commands
        Ok(control)
    }

    fn reply(&self) -> Result<String> {
        match self.replies.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(error)) => bail!("tmux control: {error}"),
            Err(error) => {
                self.events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .disconnected = true;
                bail!("tmux control reply: {error}")
            }
        }
    }

    pub fn execute(&mut self, args: &[&str]) -> Result<String> {
        // Send one command at a time. Guard ids frame arbitrary capture text; a
        // command error is consumed fully, leaving the connection synchronized.
        if !read_only(args) {
            bail!("tmux observer command is not read-only");
        }
        let mut output = String::new();
        for command in args.split(|arg| *arg == ";").filter(|c| !c.is_empty()) {
            let line = command
                .iter()
                .map(|arg| quote(arg))
                .collect::<Vec<_>>()
                .join(" ");
            if let Err(error) = writeln!(self.input, "{line}").and_then(|_| self.input.flush()) {
                self.events
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .disconnected = true;
                return Err(error).context("write tmux control command");
            }
            output.push_str(&self.reply()?);
        }
        Ok(output)
    }

    pub fn take_events(&self) -> Events {
        let mut state = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let disconnected = state.disconnected;
        let mut events = std::mem::take(&mut *state);
        state.disconnected = disconnected;
        events.disconnected = disconnected;
        events
    }

    #[cfg(test)]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn disconnected(&self) -> bool {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .disconnected
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        // Closing pipes alone is not sufficient: wait for our exact tmux pid and
        // join its reader so neither a client nor its read thread can escape.
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

// Enforce the observer's command boundary in-process. tmux's read-only flag
// rejects unrelated external send-keys when this client is selected as their
// implicit target, and does not prevent structural command mutations anyway.
// No key/selection/action command can reach the observer's stdin.
pub(crate) fn read_only(args: &[&str]) -> bool {
    args.split(|arg| *arg == ";")
        .filter(|c| !c.is_empty())
        .all(|command| match command[0] {
            "list-panes" | "list-clients" | "list-keys" | "show-options" => true,
            "display-message" | "capture-pane" => command.contains(&"-p"),
            _ => false,
        })
}

fn quote(arg: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in arg.chars() {
        match ch {
            '\\' | '"' | '$' => {
                quoted.push('\\');
                quoted.push(ch);
            }
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

fn read_protocol(
    mut input: impl BufRead,
    replies: &mpsc::Sender<Result<String, String>>,
    events: &Mutex<Events>,
) -> std::io::Result<()> {
    let mut guard: Option<Vec<u8>> = None;
    let mut block = Vec::new();
    let mut line = Vec::new();
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            return Ok(());
        }
        let length = buffer
            .iter()
            .position(|&b| b == b'\n')
            .map_or(buffer.len(), |n| n + 1);
        let complete = buffer[length - 1] == b'\n';
        // Outside command blocks, only the notification header matters. Even a
        // multi-megabyte %output line occupies at most 256 bytes here, and no
        // output bytes are unescaped, UTF-8 decoded, queued, or retained.
        let keep = if guard.is_some() {
            length
        } else {
            length.min(256_usize.saturating_sub(line.len()))
        };
        line.extend_from_slice(&buffer[..keep]);
        input.consume(length);
        if !complete {
            continue;
        }
        if let Some(id) = &guard {
            let end = line
                .strip_prefix(b"%end ")
                .map(|rest| (rest, false))
                .or_else(|| line.strip_prefix(b"%error ").map(|rest| (rest, true)));
            if let Some((rest, error)) = end.filter(|(rest, _)| *rest == id.as_slice()) {
                let _ = rest;
                let text = String::from_utf8_lossy(&block).into_owned();
                if replies
                    .send(if error { Err(text) } else { Ok(text) })
                    .is_err()
                {
                    return Ok(());
                }
                block.clear();
                guard = None;
            } else {
                block.extend_from_slice(&line);
            }
        } else if let Some(id) = line.strip_prefix(b"%begin ") {
            guard = Some(id.to_vec());
        } else {
            notification(&line, events);
        }
        line.clear();
    }
}

fn notification(line: &[u8], events: &Mutex<Events>) {
    let mut fields = line.split(|b| b.is_ascii_whitespace());
    let kind = fields.next().unwrap_or_default();
    let mut state = events.lock().unwrap_or_else(|e| e.into_inner());
    match kind {
        b"%output" | b"%extended-output" | b"%pause" | b"%pane-mode-changed" => {
            if let Some(id) = fields.next().filter(|id| {
                id.len() > 1 && id.starts_with(b"%") && id[1..].iter().all(u8::is_ascii_digit)
            }) {
                state.dirty.insert(String::from_utf8_lossy(id).into_owned());
            }
        }
        b"%exit" => state.disconnected = true,
        // Includes layouts, renames, pane focus, options, and format subscription
        // changes. Unknown future notifications conservatively refresh metadata.
        _ => state.metadata = true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_keeps_capture_notifications_literal_and_recovers_after_errors() {
        let stream = b"%output %7 ignored\\033bytes\n%begin 1 2 1\n%output %99 literal cell text\n%end 8 9 1\n%end 1 2 1\n%begin 1 3 1\nfailed\n%error 1 3 1\n%layout-change @1 layout\n";
        let (tx, rx) = mpsc::channel();
        let state = Mutex::new(Events::default());
        read_protocol(&stream[..], &tx, &state).unwrap();
        assert_eq!(
            rx.recv().unwrap().unwrap(),
            "%output %99 literal cell text\n%end 8 9 1\n"
        );
        assert_eq!(rx.recv().unwrap().unwrap_err(), "failed\n");
        let events = state.into_inner().unwrap();
        assert_eq!(events.dirty, HashSet::from(["%7".into()]));
        assert!(events.metadata);
    }
    #[test]
    fn large_non_utf8_output_is_discarded_across_buffer_boundaries() {
        let mut bytes = b"%extended-output %12 0 : ".to_vec();
        bytes.resize(1_000_000, 0xff);
        bytes.extend_from_slice(b"\n%begin 1 4 1\nok\n%end 1 4 1\n");
        let (tx, rx) = mpsc::channel();
        let state = Mutex::new(Events::default());
        read_protocol(BufReader::with_capacity(31, bytes.as_slice()), &tx, &state).unwrap();
        assert_eq!(rx.recv().unwrap().unwrap(), "ok\n");
        assert!(state.lock().unwrap().dirty.contains("%12"));
    }
}
