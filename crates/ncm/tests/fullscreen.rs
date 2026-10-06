use anyhow::{Context, Result};
use ncm_tmux::{testing::PrivateServer, Tmux};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    thread,
    time::{Duration, Instant},
};
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}
fn owned_ncm() -> String {
    let parent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/run_owned.py");
    format!(
        "python3 {} {}",
        quote(&parent.to_string_lossy()),
        quote(env!("CARGO_BIN_EXE_ncm"))
    )
}
fn child_pid(parent: u32) -> Result<String> {
    let output = Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .context("child pid")?
        .to_owned())
}
fn pane_for(tmux: &Tmux, window: &str) -> Result<ncm_model::PaneInfo> {
    Ok(tmux
        .snapshot()?
        .into_iter()
        .find(|w| w.id == window)
        .context("viewer window exists")?
        .panes[0]
        .clone())
}
fn wait_text(tmux: &Tmux, pane: &str, needle: &str) -> Result<Vec<String>> {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let rows = tmux.capture(&[pane.into()])?.remove(pane).unwrap();
        if rows.iter().any(|r| r.contains(needle)) {
            return Ok(rows);
        }
        anyhow::ensure!(
            Instant::now() < until,
            "no {needle:?} in captured frame:\n{}",
            rows.join("\n")
        );
        thread::sleep(Duration::from_millis(50));
    }
}
fn private_command(server: &PrivateServer, args: &[&str]) -> Result<()> {
    let status = Command::new("tmux")
        .arg("-S")
        .arg(server.socket_path()?)
        .args(args)
        .env_remove("TMUX")
        .status()?;
    anyhow::ensure!(status.success(), "private tmux command failed");
    Ok(())
}
/// Own only the viewer launched under this private pane's shell. Drop runs before
/// the private server, including when an assertion fails or capture returns an error.
struct ViewerProcess(libc::pid_t);
impl ViewerProcess {
    fn start(shell: u32) -> Result<Self> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let owner = child_pid(shell)
                .ok()
                .and_then(|pid| pid.parse::<u32>().ok());
            let output = Command::new("pgrep")
                .args(["-P", &owner.unwrap_or(shell).to_string(), "-x", "ncm"])
                .output()?;
            if let Some(pid) = String::from_utf8_lossy(&output.stdout).lines().next() {
                return Ok(Self(pid.parse()?));
            }
            anyhow::ensure!(Instant::now() < deadline, "private ncm child did not start");
            thread::sleep(Duration::from_millis(20));
        }
    }
    fn alive(&self) -> bool {
        // SAFETY: signal zero tests only our recorded child, without sending a signal.
        unsafe { libc::kill(self.0, 0) == 0 }
    }
}
impl Drop for ViewerProcess {
    fn drop(&mut self) {
        for signal in [libc::SIGTERM, libc::SIGKILL] {
            if !self.alive() {
                return;
            }
            // SAFETY: this PID belongs to this test's private viewer.
            unsafe { libc::kill(self.0, signal) };
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.alive() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(20));
            }
        }
        assert!(!self.alive(), "private viewer was not reaped");
    }
}
#[test]
fn demo_tabs_escape_quit_and_idle_cpu_in_private_server() -> Result<()> {
    let server = PrivateServer::start(160, 50, "sleep 60")?;
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ncm-verification");
    fs::create_dir_all(&output)?;
    let app = server.new_window(
        "demo-viewer",
        160,
        50,
        &format!(
            "saved_modes=$(stty -g); {} --demo; if [ \"$(stty -g)\" = \"$saved_modes\" ]; then printf '\\nRESTORED'; else printf '\\nBAD-MODES'; fi; sleep 60",
            owned_ncm()
        ),
    )?;
    let pane = pane_for(server.tmux(), &app)?;
    let child = ViewerProcess::start(pane.pid)?;
    let rows = wait_text(server.tmux(), &pane.id, "need you")?;
    fs::write(output.join("demo-grid.txt"), rows.join("\n"))?;
    // Sample after startup so font/probe work does not count as idle. Keep the sample brief.
    thread::sleep(Duration::from_secs(2));
    let before = server.tmux().capture(std::slice::from_ref(&pane.id))?;
    let metrics = Command::new("ps")
        .args(["-o", "pid=,%cpu=,rss=,time=", "-p", &child.0.to_string()])
        .output()?;
    fs::write(output.join("demo-idle.txt"), &metrics.stdout)?;
    thread::sleep(Duration::from_secs(1));
    assert_eq!(
        before,
        server.tmux().capture(std::slice::from_ref(&pane.id))?,
        "idle demo frame must stay byte-for-byte stable"
    );
    server.tmux().send_key(&pane.id, "Space")?;
    let rows = wait_text(server.tmux(), &pane.id, "Screen")?;
    fs::write(output.join("demo-screen.txt"), rows.join("\n"))?;
    server.tmux().send_text(&pane.id, "2")?;
    let rows = wait_text(server.tmux(), &pane.id, "Your last message has no reply")?;
    assert!(rows
        .iter()
        .any(|r| r.contains("Your last message has no reply")));
    fs::write(output.join("demo-talk.txt"), rows.join("\n"))?;
    server.tmux().send_text(&pane.id, "3")?;
    wait_text(server.tmux(), &pane.id, "layout-review")?;
    server.tmux().send_key(&pane.id, "Escape")?;
    wait_text(server.tmux(), &pane.id, "need you")?;
    // Escape backs out and never terminates the viewer.
    server.tmux().send_key(&pane.id, "Escape")?;
    assert!(pane_for(server.tmux(), &app).is_ok());
    server.tmux().send_text(&pane.id, "q")?;
    let restored = wait_text(server.tmux(), &pane.id, "RESTORED")?;
    fs::write(output.join("demo-restored.txt"), restored.join("\n"))?;
    Ok(())
}
#[test]
fn legacy_enter_selects_without_a_client_and_sigterm_restores_terminal() -> Result<()> {
    let server = PrivateServer::start(100, 32, "cat")?;
    let original = server.tmux().snapshot()?[0].clone();
    let target = server.new_window("target", 100, 32, "cat")?;
    let app = server.new_window(
        "live-viewer",
        100,
        32,
        &format!(
            "saved_modes=$(stty -g); {} --session ncm-test; if [ \"$(stty -g)\" = \"$saved_modes\" ]; then printf '\\nTERM-RESTORED'; else printf '\\nBAD-MODES'; fi; sleep 60",
            owned_ncm()
        ),
    )?;
    let pane = pane_for(server.tmux(), &app)?;
    let child = ViewerProcess::start(pane.pid)?;
    wait_text(server.tmux(), &pane.id, "need you")?;
    assert_eq!(
        server
            .tmux()
            .snapshot()?
            .iter()
            .find(|w| w.active)
            .unwrap()
            .id,
        original.id
    );
    server.tmux().send_key(&pane.id, "Right")?;
    server.tmux().send_key(&pane.id, "Enter")?;
    thread::sleep(Duration::from_millis(300));
    server.tmux().send_text(&pane.id, "i")?;
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        server
            .tmux()
            .snapshot()?
            .iter()
            .find(|w| w.active)
            .unwrap()
            .id,
        target
    );
    // Legacy use selects the target and never forwards terminal typing.
    assert!(
        !server.tmux().capture(&[original.panes[0].id.clone()])?[&original.panes[0].id]
            .iter()
            .any(|l| l.contains("ncm-private-input"))
    );
    server.tmux().send_key(&pane.id, "Escape")?;
    wait_text(server.tmux(), &pane.id, "need you")?;
    assert!(Command::new("kill")
        .args(["-TERM", &child.0.to_string()])
        .status()?
        .success());
    let restored = wait_text(server.tmux(), &pane.id, "TERM-RESTORED")?;
    assert!(restored.iter().any(|r| r.contains("TERM-RESTORED")));
    // No extra client and no live-server action: all handles are private -S/-L targets.
    private_command(&server, &["display-message", "-p", "#{session_name}"])?;
    Ok(())
}

#[test]
fn read_only_remote_viewer_blocks_actions_and_use_mode() -> Result<()> {
    let source = PrivateServer::start(100, 32, "cat")?;
    let before = source.tmux().snapshot()?;
    let viewer = PrivateServer::start(100, 32, "sleep 60")?;
    let app = viewer.new_window(
        "read-only",
        100,
        32,
        &format!(
            "{} --read-only --socket {} --session ncm-test; printf '\\nREADONLY-EXITED'; sleep 60",
            owned_ncm(),
            quote(&source.socket_path()?.to_string_lossy())
        ),
    )?;
    let pane = pane_for(viewer.tmux(), &app)?;
    let _child = ViewerProcess::start(pane.pid)?;
    wait_text(viewer.tmux(), &pane.id, "need you")?;
    viewer.tmux().send_text(&pane.id, "i")?;
    wait_text(viewer.tmux(), &pane.id, "read-only")?;
    for key in ["Enter", "n", "x"] {
        viewer.tmux().send_key(&pane.id, key)?;
    }
    thread::sleep(Duration::from_millis(300));
    assert_eq!(source.tmux().snapshot()?, before);
    viewer.tmux().send_text(&pane.id, "q")?;
    wait_text(viewer.tmux(), &pane.id, "READONLY-EXITED")?;
    Ok(())
}
