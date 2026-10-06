//! Visibility checks for the legacy text-only viewer inside tmux.
use super::Tmux;
use anyhow::{ensure, Context, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerClient {
    pub pid: String,
    pub tty: String,
    pub termname: String,
    pub termtype: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerState {
    pub visible: bool,
    pub client: Option<ViewerClient>,
    pub clients: Vec<ViewerClient>,
}
impl Default for ViewerState {
    fn default() -> Self {
        Self {
            visible: true,
            client: None,
            clients: Vec::new(),
        }
    }
}
const VIEWER_FORMAT: &str = "#{session_name}\x1f#{window_id}\x1f#{client_pid}";
const CLIENT_FORMAT: &str = "#{client_session}\x1f#{window_id}\x1f#{client_pid}\x1f#{client_tty}\x1f#{client_termname}\x1f#{client_termtype}\x1f#{client_control_mode}";
fn parse_viewer(value: &str) -> ViewerState {
    let mut lines = value.lines();
    let fields: Vec<_> = lines.next().unwrap_or_default().split('\x1f').collect();
    let session = fields.first().copied().unwrap_or_default();
    let window = fields.get(1).copied().unwrap_or_default();
    let selected = fields.get(2).copied().unwrap_or_default();
    let mut clients = Vec::new();
    for line in lines {
        let row: Vec<_> = line.split('\x1f').collect();
        if !(row.len() == 6 || row.len() == 7)
            || row[0] != session
            || row[1] != window
            || row.get(6) == Some(&"1")
        {
            continue;
        }
        clients.push(ViewerClient {
            pid: row[2].into(),
            tty: row[3].into(),
            termname: row[4].into(),
            termtype: row[5].into(),
        });
    }
    clients.sort_by(|a, b| a.pid.cmp(&b.pid));
    ViewerState {
        visible: !clients.is_empty(),
        client: clients
            .iter()
            .find(|c| c.pid == selected)
            .or_else(|| clients.first())
            .cloned(),
        clients,
    }
}
impl Tmux {
    /// Cheap viewer-only visibility check, safe even when its data source is another server.
    pub fn viewer_state(&self, pane: &str) -> Result<ViewerState> {
        super::validate_id(pane, '%')?;
        let value = self.run(&[
            "display-message",
            "-p",
            "-t",
            pane,
            VIEWER_FORMAT,
            ";",
            "list-clients",
            "-F",
            CLIENT_FORMAT,
        ])?;
        Ok(parse_viewer(&value))
    }
    /// Viewer metadata uses its server's persistent read-only control client.
    pub fn viewer_state_queued(&self, pane: &str) -> Result<ViewerState> {
        self.viewer_state(pane)
    }
    /// The legacy context menu still needs the viewer's real client, never a headless observer.
    pub fn peek_client(&self) -> Result<String> {
        ensure!(
            self.is_viewer_server(),
            "menu unavailable: viewer is on another server"
        );
        let pane = std::env::var("TMUX_PANE").context("menu needs a tmux viewer pane")?;
        self.viewer_state(&pane)?
            .client
            .filter(|client| !client.tty.is_empty())
            .map(|client| client.tty)
            .context("menu needs an attached viewer client")
    }
    /// Disconnect while hidden; resume creates a fresh client and snapshot.
    pub fn suspend_queries(&self) {
        self.control
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn headless_control_client_does_not_make_legacy_window_visible() {
        let header = "main\x1f@1\x1f10\n";
        let observer = "main\x1f@1\x1f10\x1f\x1f\x1f\x1f1\n";
        assert!(!parse_viewer(&format!("{header}{observer}")).visible);
        let terminal = "main\x1f@1\x1f11\x1f/dev/tty\x1fxterm\x1fxterm\x1f0\n";
        let viewer = parse_viewer(&format!("{header}{observer}{terminal}"));
        assert!(viewer.visible);
        assert_eq!(viewer.clients.len(), 1);
        assert_eq!(viewer.client.unwrap().pid, "11");
    }
}
