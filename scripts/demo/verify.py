#!/usr/bin/env python3
"""Verify through realuse's owned PTY/pyte terminal; leave the demo stopped."""
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import world

ROOT, OUT = world.ROOT, world.OUT
spec = importlib.util.spec_from_file_location("realuse", ROOT / "scripts/realuse/harness.py")
realuse = importlib.util.module_from_spec(spec)
spec.loader.exec_module(realuse)
realuse.OUT, realuse.WIDTH, realuse.HEIGHT = OUT, 200, 55
RESULTS = []
TERM = None


def wait(predicate, timeout=6):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if TERM and TERM.error:
            raise RuntimeError(TERM.error)
        if predicate():
            return True
        time.sleep(.05)
    return bool(predicate())


def save(name):
    time.sleep(.2)
    with TERM.lock:
        raw = TERM.text()
        # Kitty placeholders carry combining marks; show their cells as spaces in the
        # readable dump, retaining the exact screen and VT stream alongside it.
        text = "\n".join("".join(" " if cell.data and ord(cell.data[0]) >= 0xf0000 else cell.data
                                for cell in [TERM.screen.buffer[y][x] for x in range(realuse.WIDTH)]).rstrip()
                         for y in range(realuse.HEIGHT))
        styles = {str(y): {str(x): {"fg": cell.fg, "bg": cell.bg, "bold": cell.bold}
                          for x, cell in row.items() if cell.data.strip()}
                  for y, row in TERM.screen.buffer.items()}
    (OUT / "screens" / f"{name}.txt").write_text(text + "\n")
    (OUT / "screens" / f"{name}-raw.txt").write_text(raw + "\n")
    (OUT / "screens" / f"{name}-styles.json").write_text(json.dumps(styles, indent=2))
    return text


def check(name, passed, detail=""):
    RESULTS.append({"name": name, "pass": bool(passed), "detail": detail})
    print(f"{'PASS' if passed else 'FAIL'} {name}: {detail}", flush=True)
    if not passed:
        if TERM:
            save("failure")
        raise AssertionError(name)


def point(name):
    with TERM.lock:
        for y in range(realuse.HEIGHT):
            row = "".join(TERM.screen.buffer[y][x].data[:1] or " " for x in range(realuse.WIDTH))
            x = row.find(name)
            if x >= 0:
                return x + 3, y + 3
    raise AssertionError("card not visible: " + name)


def click(x, y, button=0):
    TERM.feed(f"\x1b[<{button};{x};{y}M".encode())
    time.sleep(.05)
    TERM.feed(f"\x1b[<{button};{x};{y}m".encode())


def main():
    global TERM
    world.safe_paths()
    manifest = world.up()
    binary = str(OUT / "bin/ncm")
    env = world.environment()
    command = [binary, "--socket", manifest["socket"], "--session", "demo"]
    before = sorted(p.name for p in (OUT / "pids").iterdir())
    try:
        world.up()
        check("up is idempotent", before == sorted(p.name for p in (OUT / "pids").iterdir()))
        config = world.tmux("list-windows", "-t", "demo", "-F",
                            "#{window_name}|#{window_width}x#{window_height}|#{window_panes}").stdout
        (OUT / "windows.txt").write_text(config)
        check("private server geometry", config.count("200x55") == 12 and "cloud-teapot|200x55|2" in config)
        check("mouse is on", world.tmux("show-options", "-gv", "mouse").stdout.strip() == "on")
        titles = world.tmux("list-panes", "-s", "-t", "demo", "-F", "#{pane_title}").stdout.splitlines()
        check("fictional pane titles", len(titles) == 13 and all(title in ["~/work/" + name for name in world.NAMES]
                                                                 for title in titles))
        for window in manifest["windows"]:
            panes = world.tmux("list-panes", "-t", window["id"], "-F", "#{pane_id}").stdout.splitlines()
            for index, pane in enumerate(panes):
                capture = world.tmux("capture-pane", "-p", "-t", pane).stdout
                (OUT / "screens" / f"pane-{window['name']}-{index}.txt").write_text(capture)
        diagnosis = subprocess.run(command + ["--diagnose-talk"], env=env, capture_output=True,
                                   text=True, timeout=12)
        (OUT / "diagnose-talk.txt").write_text(diagnosis.stdout + diagnosis.stderr)
        check("Talk resolves every agent", diagnosis.returncode == 0 and all(
            f"pane={w['pane']} session=demo" in diagnosis.stdout and
            f"sessionId=demo-{w['name']}" in diagnosis.stdout
            for w in manifest["windows"][:8]) and diagnosis.stdout.count("adapter=attached") == 8,
            "eight fake sessions, eight attached adapters")
        check("one unanswered fixture", diagnosis.stdout.count("unanswered_count=1 ") == 1)
        TERM = realuse.Terminal(command, env, "demo")
        check("grid contains 12 windows", wait(lambda: all(name in TERM.text() for name in world.NAMES)))
        check("dock state counts", wait(lambda: all(text in TERM.text() for text in
                                                   ["1 need you", "4 working", "2 done"])))
        check("pink unanswered card", wait(lambda: "unanswered" in TERM.text()))
        check("mixed harness badges", "claude" in TERM.text() and "lilac-engine  codex" in TERM.text())
        grid = save("01-grid")
        with TERM.lock:
            backgrounds = {}
            for window in manifest["windows"]:
                # The empty right part of the header retains the section tint even on selection.
                for y, row in enumerate(TERM.screen.display):
                    x = row.find(window["name"])
                    if x >= 0:
                        backgrounds.setdefault(window["group"], set()).add(TERM.screen.buffer[y][x + len(window["name"]) + 1].bg)
            colors = {group: values - {"f0f3f8", "0a0b0e"} for group, values in backgrounds.items()}
        (OUT / "section-colors.json").write_text(json.dumps({k: sorted(v) for k, v in backgrounds.items()}, indent=2))
        check("section tints", len(colors) == 4 and all(len(values) == 1 for values in colors.values())
              and len(set().union(*colors.values())) == 4, str(backgrounds))
        click(*point("otter-ledger"))
        check("Quick Look opens", wait(lambda: "Screen" in TERM.text() and "Reports" in TERM.text()))
        save("02-screen")
        TERM.feed(b"2")
        check("Talk messages and replies", wait(lambda: "The patch changes the commit guard" in TERM.text()
                                                 and "Please keep the lantern" in TERM.text()))
        check("collapsed report run", "3 reports" in TERM.text())
        check("reports by agent", all(name in TERM.text() for name in ["reports, by agent", *["fern-scout", "glaze-checker", "sprout-builder"]]))
        check("long-message fold", bool(re.search(r"\d+ more lines", TERM.text())))
        check("model/context/subagent facts", all(text in TERM.text() for text in ["claude-sonnet-4-6", "30.6k ctx", "subagents"]))
        save("03-talk")
        TERM.feed(b"3")
        check("Reports contents", wait(lambda: all(name in TERM.text() for name in ["fern-scout", "glaze-checker", "sprout-builder", "24 pretend checks"])))
        save("04-reports")
        TERM.feed(b"\x1b")
        check("back to grid", wait(lambda: all(name in TERM.text() for name in world.NAMES)))
        click(*point("velvet-radar"))
        check("waiting Quick Look", wait(lambda: "Screen" in TERM.text() and "Reports" in TERM.text()))
        TERM.feed(b"2")
        check("unanswered Talk banner", wait(lambda: "Your last message has no reply yet" in TERM.text()))
        save("05-unanswered")
        TERM.feed(b"\x1b")
        check("grid restored", wait(lambda: all(name in TERM.text() for name in world.NAMES)))
        click(*point("plum-forge"), button=2)
        check("right-click menu", wait(lambda: all(text in TERM.text() for text in ["Go to window", "Rename", "Close (confirm)"])))
        save("06-menu")
        TERM.feed(b"\x1b")
        check("menu cancels", wait(lambda: "Close (confirm)" not in TERM.text()))
        TERM.feed(b"q")
        check("ncm quits", wait(lambda: TERM.child.poll() is not None))
        # Prove down refuses an unowned private server, without involving another server.
        world.tmux("set-option", "-s", "@ncm_demo_owner", "foreign-fixture")
        try:
            world.down()
            check("foreign ownership protection", False)
        except RuntimeError as exc:
            check("foreign ownership protection", "not owned" in str(exc) and world.server_alive())
        finally:
            world.tmux("set-option", "-s", "@ncm_demo_owner", manifest["owner"])
    finally:
        if TERM:
            TERM.close()
        world.down()
        world.down()
        cleanup = json.loads((OUT / "cleanup.json").read_text())
        RESULTS.append({"name": "down is idempotent and removes owned processes/server", "pass":
                        cleanup["live_owned_pids"] == [] and not world.server_alive() and not Path(manifest["socket"]).exists()})
        if all(row["pass"] for row in RESULTS):
            for path in (OUT / "screens").glob("failure*"):
                path.unlink()
        (OUT / "verification.json").write_text(json.dumps(RESULTS, indent=2))
        (OUT / "verification.md").write_text("# Demo verification\n\n" + "\n".join(
            f"- {'PASS' if row['pass'] else 'FAIL'}: {row['name']}" for row in RESULTS) +
            "\n\nScreen dumps: screens/01-grid.txt through screens/06-menu.txt.\n"
            "VT wire capture: demo.vt; cell colors: screens/*-styles.json.\n"
            "pyte checks the text and style plane; Kitty raster pictures need the lead's Ghostty capture.\n")


if __name__ == "__main__":
    main()
