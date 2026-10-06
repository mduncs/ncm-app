#!/usr/bin/env python3
"""Owned pane workloads and ncm supervisor. No network or actual agent sessions."""
import datetime
import fcntl
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import termios
import time


def main():
    if sys.argv[1] == "--terminal":
        # Popen creates our session; acquiring the slave makes this a real login tty.
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)
        os.execvpe(sys.argv[2], sys.argv[2:], os.environ)
    out = Path(sys.argv[1])
    pid = os.getpid()
    (out / "pids" / str(pid)).write_text(" ".join(sys.argv))
    stopping = False
    child = None

    def stop(*_):
        nonlocal stopping
        stopping = True
        if child is not None and child.poll() is None:
            child.terminate()

    for sig in (signal.SIGHUP, signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, stop)
    try:
        if sys.argv[2] == "--own":
            child = subprocess.Popen(sys.argv[3:])
            (out / "pids" / str(child.pid)).write_text("ncm " + " ".join(sys.argv[3:]))
            while not stopping and child.poll() is None:
                time.sleep(.05)
            if child.poll() is None:
                child.terminate()
                try:
                    child.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    child.kill()
            child.wait()
            return
        label, interval = sys.argv[2], float(sys.argv[3])
        sid = "realuse-" + label
        projects = out / "claude" / "projects" / "fixture"
        projects.mkdir(parents=True, exist_ok=True)
        transcript = projects / (sid + ".jsonl")
        now = datetime.datetime.now(datetime.timezone.utc).isoformat()
        records = []
        for i in range(100):
            role = "user" if i % 2 == 0 else "assistant"
            body = f"HISTORY-{i:03d} {label}: " + ("reviewing realistic terminal interactions and output; " * 5)
            records.append({"type": role, "uuid": f"{label}-{i}", "timestamp": now,
                            "origin": {"kind": "human"},
                            "message": {"role": role, "content": body}})
        transcript.write_text("".join(json.dumps(r) + "\n" for r in records))
        meta = out / "claude" / "sessions" / f"{pid}.json"
        socket = os.environ["TMUX"].rsplit(",", 2)[0]
        window = subprocess.check_output(["tmux", "-S", socket, "display-message", "-p", "-t",
                                          os.environ["TMUX_PANE"], "#{window_id}"]).decode().strip()
        meta.write_text(json.dumps({"pid": pid, "sessionId": sid,
                                   "tmux": f"realuse:{window}.{os.environ['TMUX_PANE']}"}))
        print("\033[2J\033[HCLIPBOARD_PROBE selection text", flush=True)
        for i in range(65):
            print(f"{label} initial line {i:03d}: read source / run tool / review result", flush=True)
        # Keep a predictable copy target at the top of the displayed pane.
        print("\033[HCLIPBOARD_PROBE selection text\033[K", end="", flush=True)
        tick = 0
        last = time.monotonic()
        quiet = False
        rang = False
        while not stopping:
            if not rang and label in ("agent-03", "agent-07") and (out / "alert").exists():
                print("\a", end="", flush=True)
                rang = True
            is_quiet = (out / "quiet").exists()
            if is_quiet and not quiet:
                print(f"\033[3;1H{label} quiet, output stopped\033[K", end="", flush=True)
            quiet = is_quiet
            if interval and not quiet and time.monotonic() - last >= interval:
                tick += 1
                print(f"\033[3;1H{label} tool update {tick:04d}: analysing files\033[K", end="", flush=True)
                last = time.monotonic()
            if select.select([sys.stdin], [], [], .05)[0]:
                data = os.read(0, 1024)
                if data:
                    print(f"\033[5;1H{label} received {data.hex()}\033[K", end="", flush=True)
    finally:
        if child is not None and child.poll() is None:
            child.kill()
            child.wait()


if __name__ == "__main__":
    main()
