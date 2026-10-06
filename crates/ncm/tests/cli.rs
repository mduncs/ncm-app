use anyhow::Result;
use ncm_tmux::testing::PrivateServer;
use std::process::Command;

#[test]
fn version_and_list_work_without_attaching_a_client() -> Result<()> {
    let version = Command::new(env!("CARGO_BIN_EXE_ncm"))
        .arg("--version")
        .output()?;
    assert!(version.status.success());
    assert_eq!(String::from_utf8(version.stdout)?.trim(), "ncm 0.1.0");

    let server = PrivateServer::start(80, 24, "sleep 60")?;
    server.new_window("cli-fixture", 100, 30, "sleep 60")?;
    let output = Command::new(env!("CARGO_BIN_EXE_ncm"))
        .arg("--list")
        .env("TMUX", format!("{},0,0", server.socket_path()?.display()))
        .env_remove("TMUX_PANE")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("cli-fixture (100x30, 1 panes)"));
    // Read-only CLI work must never attach a client or change the active window.
    assert_eq!(
        server
            .tmux()
            .snapshot()?
            .iter()
            .find(|w| w.active)
            .unwrap()
            .index,
        0
    );
    Ok(())
}

#[test]
fn explicit_socket_ignores_inherited_pane_and_session() -> Result<()> {
    let server = PrivateServer::start(80, 24, "sleep 60")?;
    let window = server.tmux().snapshot()?.remove(0);
    for env_socket in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ncm"));
        command
            .arg("--list")
            .arg("--session")
            .arg("ncm-test")
            .env("TMUX_PANE", &window.panes[0].id)
            .env("TMUX", "/missing/viewer-socket,0,77");
        if env_socket {
            command.env("NCM_TMUX_SOCKET", server.socket_path()?);
        } else {
            command
                .arg("--socket")
                .arg(server.socket_path()?)
                .env_remove("NCM_TMUX_SOCKET");
        }
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8(output.stdout)?.contains(&window.id));
    }
    Ok(())
}
