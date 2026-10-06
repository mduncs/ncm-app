//! An owned full-screen tmux client, isolated from the source session's current window.
use super::{validate_id, Tmux};
use anyhow::{ensure, Result};
use std::{
    process::Child,
    time::{Duration, Instant},
};

pub struct View {
    tmux: Tmux,
    session: String,
    child: Option<Child>,
    created: bool,
}
impl Tmux {
    /// No server mutation until the user explicitly opens the selected window.
    pub fn view(&self) -> View {
        View {
            tmux: self.clone(),
            session: format!("_ncm_view_{}", std::process::id()),
            child: None,
            created: false,
        }
    }
}
impl View {
    /// The caller has already released terminal modes and paused its I/O workers.
    pub fn attach(
        &mut self,
        source: &str,
        window: &str,
        pane: &str,
        stopped: impl Fn() -> bool,
    ) -> Result<()> {
        validate_id(window, '@')?;
        validate_id(pane, '%')?;
        self.close();
        self.tmux.run_action(&[
            "new-session",
            "-d",
            "-t",
            &format!("={source}"),
            "-s",
            &self.session,
        ])?;
        self.created = true;
        let result = self.attach_owned(window, pane, stopped);
        self.close();
        result
    }
    fn attach_owned(&mut self, window: &str, pane: &str, stopped: impl Fn() -> bool) -> Result<()> {
        let target = self.session.clone();
        self.tmux.run_action(&[
            "set-option",
            "-t",
            &target,
            "status",
            "on",
            ";",
            "set-option",
            "-t",
            &target,
            "status-right",
            " ncm · C-] back ",
            ";",
            "set-option",
            "-t",
            &target,
            "status-right-length",
            "40",
            ";",
            "bind-key",
            "-n",
            "C-]",
            "if",
            "-F",
            "#{m:_ncm_view_*,#{session_name}}",
            "detach-client",
            "send-keys C-]",
            ";",
            "select-window",
            "-t",
            &format!("{}:{window}", self.session),
            ";",
            "select-pane",
            "-t",
            pane,
        ])?;
        // A second command in the attach queue enables destruction only once attached.
        self.tmux.record_spawn();
        self.child = Some(
            self.tmux
                .command()
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .args([
                    "attach-session",
                    "-t",
                    &target,
                    ";",
                    "set-option",
                    "-t",
                    &target,
                    "destroy-unattached",
                    "on",
                ])
                .spawn()?,
        );
        loop {
            if stopped() {
                return Ok(());
            }
            if let Some(status) = self.child.as_mut().unwrap().try_wait()? {
                ensure!(status.success(), "tmux attach exited with {status}");
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    pub fn close(&mut self) {
        if self.created {
            let _ = self
                .tmux
                .run_action(&["kill-session", "-t", &format!("={}", self.session)]);
            self.created = false;
        }
        if let Some(mut child) = self.child.take() {
            // Give the detached client a chance to restore its terminal before forcing a reap.
            let deadline = Instant::now() + Duration::from_millis(150);
            while child.try_wait().is_ok_and(|s| s.is_none()) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for View {
    fn drop(&mut self) {
        self.close();
    }
}
