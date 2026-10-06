#!/usr/bin/env python3
"""Create only an owned, local tmux world. No harnesses, networking, or live data."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import secrets
import shlex
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "target/demo"
NAMES = ["otter-ledger", "plum-forge", "velvet-radar", "moss-kettle",
         "amber-orbit", "pebble-vault", "lilac-engine", "waffle-comet",
         "noodle-beacon", "mint-canyon", "coral-picnic", "cloud-teapot"]
STATES = ["working", "working", "needs", "working", "done", "done", "working", ""]
RATES = [1, 1.7, 0, 2.4, 0, 0, 3, 0]
GROUPS = ["pinned", "build", "explore", "personal"]


def safe_paths():
    # A symlinked target or fixture is an original input, never writable demo state.
    for p in [ROOT / "target", OUT]:
        if p.is_symlink():
            raise RuntimeError(f"refusing symlinked output directory: {p}")
    if OUT.exists():
        for base, dirs, files in os.walk(OUT, followlinks=False):
            for name in dirs + files:
                if (Path(base) / name).is_symlink():
                    raise RuntimeError(f"refusing symlink in demo state: {Path(base) / name}")


def environment():
    # Whitelist rather than inheriting real Claude/Codex hooks, XDG roots, or test flags.
    return {"PATH": "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
            "HOME": str(OUT / "home"), "TERM": "xterm-ghostty", "LANG": "en_US.UTF-8",
            "TMPDIR": str(OUT / "tmp"), "TMUX_TMPDIR": str(OUT / "t"),
            "CODEX_HOME": str(OUT / "home/.codex"),
            "XDG_STATE_HOME": str(OUT / "home/.local/state"),
            "NCM_CLAUDE_ROOT": str(OUT / "claude"),
            "TMUX_GROUP_STATE_FILE": str(OUT / "home/.local/state/tmux/window-groups.json")}


def tmux(*args, check=True):
    p = subprocess.run([str(OUT / "bin/tmux"), "-L", "ncm-demo", "-f", "/dev/null", *args],
                       env=environment(), capture_output=True, text=True, timeout=8)
    if check and p.returncode:
        raise RuntimeError(p.stderr.strip())
    return p


def server_alive():
    return (OUT / "bin/tmux").exists() and tmux("list-sessions", check=False).returncode == 0


def check_owner(manifest):
    if tmux("show-options", "-s", "-v", "@ncm_demo_owner").stdout.strip() != manifest["owner"]:
        raise RuntimeError("private server is not owned by this demo; refusing to alter it")


def run_command(manifest, binary="ncm"):
    env = environment()
    # Also clear inherited ncm/test/graphics switches; shell command is copy/paste safe.
    args = ["env", "-u", "TMUX", "-u", "TMUX_PANE", "-u", "NCM_TMUX_SOCKET",
            "-u", "NCM_TEST_LIVE", "-u", "NCM_TEST_PANIC", "-u", "NCM_TMUX_QUERY_LOG"]
    args += [f"{key}={value}" for key, value in env.items() if key != "PATH"]
    return shlex.join(args + [binary, "--socket", manifest["socket"], "--session", "demo"])


def up(binary=None):
    manifest_path = OUT / "manifest.json"
    if manifest_path.exists() and server_alive():
        manifest = json.loads(manifest_path.read_text())
        check_owner(manifest)
        expected = {w["name"] for w in manifest["windows"]}
        actual = set(tmux("list-windows", "-t", "demo", "-F", "#{window_name}").stdout.splitlines())
        if expected != actual:
            raise RuntimeError("demo was modified; run down.sh, then up.sh to reset it")
        print(run_command(manifest))
        return manifest
    if server_alive():
        raise RuntimeError("unowned private server exists; refusing to alter it")
    for directory in ["bin", "pids", "home/.codex/sessions", "home/.local/state/tmux",
                      "claude/sessions", "claude/projects/fixture", "tmp", "t", "screens"]:
        (OUT / directory).mkdir(parents=True, exist_ok=True)
    os.chmod(OUT / "t", 0o700)
    for item in (OUT / "claude/sessions").glob("*.json"):
        item.unlink()
    for item in (OUT / "pids").iterdir():
        item.unlink()
    tmux_bin = shutil.which("tmux")
    if not tmux_bin:
        raise RuntimeError("tmux is required")
    shutil.copy2(tmux_bin, OUT / "bin/tmux")
    # The host's tmux optionally has an @rpath jemalloc next to its original bin/.
    jemalloc = Path(tmux_bin).resolve().parent.parent / "lib/libjemalloc.2.dylib"
    if jemalloc.exists():
        (OUT / "lib").mkdir(exist_ok=True)
        shutil.copy2(jemalloc, OUT / "lib/libjemalloc.2.dylib")
    binary = binary or os.environ.get("NCM_DEMO_BINARY") or shutil.which("ncm")
    if not binary:
        raise RuntimeError("ncm is required (or set NCM_DEMO_BINARY)")
    if Path(binary).resolve() != (OUT / "bin/ncm").resolve():
        shutil.copy2(binary, OUT / "bin/ncm")
    python = sys.executable
    # The realuse supervisor registers PIDs, forwards termination, and waits for its child.
    def pane_command(index, kind):
        return shlex.join([python, str(ROOT / "scripts/realuse/fake_agent.py"), str(OUT),
                           "--own", python, str(ROOT / "scripts/demo/workload.py"),
                           str(OUT), NAMES[index], kind, str(RATES[index] if index < 8 else 1.8)])
    owner = secrets.token_hex(20)
    manifest = {"owner": owner, "windows": [], "server_pid": None}
    manifest_path.write_text(json.dumps(manifest, indent=2))
    try:
        tmux("new-session", "-d", "-s", "demo", "-n", NAMES[0], "-x", "200", "-y", "55",
             "-c", str(OUT / "home"), pane_command(0, "agent"))
        tmux("set-option", "-s", "@ncm_demo_owner", owner)
        manifest["socket"] = tmux("display-message", "-p", "-t", "demo", "#{socket_path}").stdout.strip()
        manifest["server_pid"] = int(tmux("display-message", "-p", "-t", "demo", "#{pid}").stdout)
        manifest_path.write_text(json.dumps(manifest, indent=2))
        for option, value in [("mouse", "on"), ("status", "off"), ("default-shell", "/bin/sh"),
                              ("renumber-windows", "off"), ("base-index", "0")]:
            tmux("set-option", "-g", option, value)
        tmux("set-option", "-gw", "automatic-rename", "off")
        tmux("set-option", "-gw", "allow-rename", "off")
        tmux("set-option", "-gw", "window-size", "manual")
        records = {}
        for index, name in enumerate(NAMES):
            kind = "agent" if index < 8 else ["build", "monitor", "shell", "split"][index - 8]
            cwd = OUT / "home/work" / name
            cwd.mkdir(parents=True, exist_ok=True)
            if index:
                tmux("new-window", "-d", "-t", f"demo:{index}", "-n", name, "-c", str(cwd),
                     pane_command(index, kind))
            window, pane = tmux("display-message", "-p", "-t", f"demo:{index}",
                                "#{window_id}|#{pane_id}").stdout.strip().split("|")
            tmux("set-option", "-w", "-t", window, "@dashboard_key", name)
            tmux("set-option", "-p", "-t", pane, "@pane_label", "~/work/" + name)
            tmux("select-pane", "-t", pane, "-T", "~/work/" + name)
            if index < 8:
                # Badge override is independent of transcript discovery. All fixtures use
                # the realuse Claude metadata protocol; one has a synthetic Codex badge.
                tmux("set-option", "-p", "-t", pane, "@pane_harness", "codex" if index == 6 else "claude")
                tmux("set-option", "-w", "-t", window, "@agent_state", STATES[index])
            if index == 11:
                peer = tmux("split-window", "-h", "-t", pane, "-c", str(cwd), "-P", "-F", "#{pane_id}",
                            pane_command(index, "split-peer")).stdout.strip()
                tmux("set-option", "-p", "-t", peer, "@pane_label", "~/work/" + name)
                tmux("select-pane", "-t", peer, "-T", "~/work/" + name)
            tmux("resize-window", "-t", window, "-x", "200", "-y", "55")
            records[name] = {"group": GROUPS[index // 3], "name": name, "path": str(cwd),
                             "panes": 2 if index == 11 else 1, "last_seen": int(time.time())}
            manifest["windows"].append({"index": index, "name": name, "id": window, "pane": pane,
                                        "kind": kind, "group": GROUPS[index // 3]})
        (OUT / "home/.local/state/tmux/window-groups.json").write_text(json.dumps(
            {"version": 1, "sessions": {"demo": {"windows": records}}}, indent=2))
        tmux("select-window", "-t", "demo:0")
        manifest_path.write_text(json.dumps(manifest, indent=2))
        deadline = time.monotonic() + 8
        while (len(list((OUT / "claude/sessions").glob("*.json"))) != 8
               or len(list((OUT / "pids").iterdir())) != 26):
            if time.monotonic() > deadline:
                raise RuntimeError("not all eight agents and thirteen owned panes became ready")
            time.sleep(.05)
    except BaseException:
        if server_alive():
            check_owner(manifest)
            tmux("kill-server")
        raise
    print(run_command(manifest))
    (OUT / "run.txt").write_text(run_command(manifest) + "\n")
    return manifest


def down():
    manifest_path = OUT / "manifest.json"
    if not manifest_path.exists():
        print("Demo already down; no owned server to remove.")
        return
    manifest = json.loads(manifest_path.read_text())
    if server_alive():
        check_owner(manifest)
        tmux("kill-server")
    # Closing tmux sends SIGHUP to the pane supervisors, which terminate/reap children.
    owned = [int(p.name) for p in (OUT / "pids").iterdir() if p.name.isdecimal()]
    if manifest.get("server_pid"):
        owned.append(manifest["server_pid"])
    deadline = time.monotonic() + 5
    live = []
    while True:
        live = []
        for pid in owned:
            try:
                os.kill(pid, 0)
                live.append(pid)
            except ProcessLookupError:
                pass
        if not live or time.monotonic() > deadline:
            break
        time.sleep(.05)
    if live:
        raise RuntimeError(f"owned processes have not exited: {live}; retained ownership manifest")
    if server_alive():
        raise RuntimeError("owned tmux server survived shutdown")
    socket = Path(manifest.get("socket", OUT / "missing"))
    if socket.is_relative_to(OUT) and socket.exists():
        socket.unlink()
    for directory in [OUT / "pids", OUT / "claude/sessions"]:
        for item in directory.iterdir():
            item.unlink()
    manifest_path.unlink()
    (OUT / "cleanup.json").write_text(json.dumps({"live_owned_pids": live, "server_alive": False,
                                                  "artifacts_retained": True}, indent=2))
    print("Demo stopped; owned processes and private server are gone. Artifacts remain in target/demo/.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["up", "down"])
    parser.add_argument("--binary", help="ncm executable to snapshot for verification")
    args = parser.parse_args()
    safe_paths()
    OUT.mkdir(parents=True, exist_ok=True)
    with (OUT / "lifecycle.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if args.action == "up":
            up(args.binary)
        else:
            down()


if __name__ == "__main__":
    main()
