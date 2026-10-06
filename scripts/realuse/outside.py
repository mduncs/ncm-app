"""Primary direct-terminal workflow; the user's tmux stays in the background."""
import os
from pathlib import Path
import re
import shlex
import subprocess
import time

from harness import Harness, Terminal, ROOT, OUT, WIDTH, HEIGHT, KEYS, trace_clock


class DirectHarness(Harness):
    def __init__(self, binary):
        super().__init__(binary)
        self.variant = "direct"
        self.coverage = {v: {m: set() for m in ("grid", "quick")} for v in ("direct", "legacy")}
        self.direct_serial = 0
        self.socket = None
        self.primary = None

    def command(self):
        return [os.sys.executable, str(ROOT / "scripts/realuse/fake_agent.py"), str(OUT), "--own",
                str(self.binary), "--socket", self.socket, "--session", "realuse", "--log", str(self.trace)]

    def start(self):
        for d in ("screens", "pids", "home", "bin", "claude/sessions", "s"):
            (OUT / d).mkdir(parents=True, exist_ok=True)
        for p in (self.trace, OUT / "quiet", OUT / "alert", OUT / "clipboard.txt"):
            p.unlink(missing_ok=True)
        for d, pattern in (("screens", "*.txt"), ("pids", "*")):
            for p in (OUT / d).glob(pattern):
                p.unlink()
        shim = OUT / "bin/pbcopy"
        shim.write_text("#!/bin/sh\ncat > " + shlex.quote(str(OUT / "clipboard.txt")) + "\n")
        shim.chmod(0o755)
        def agent(label, interval):
            return shlex.join([os.sys.executable, str(ROOT / "scripts/realuse/fake_agent.py"),
                               str(OUT), label, str(interval)])
        self.tmux("new-session", "-d", "-s", "realuse", "-x", str(WIDTH), "-y", str(HEIGHT - 1),
                  "-c", str(ROOT), "-n", "agent-00", agent("agent-00", .2))
        self.tmux("set-option", "-g", "base-index", "1")
        self.tmux("move-window", "-s", "realuse:0", "-t", "realuse:1")
        for scope, key, value in (("-g", "mouse", "on"), ("-g", "allow-passthrough", "on"),
                                   ("-g", "window-size", "latest"), ("-s", "set-clipboard", "on"),
                                   ("-s", "copy-command", "pbcopy"), ("-g", "status", "on"),
                                   ("-g", "default-terminal", "tmux-256color"),
                                   ("-g", "automatic-rename", "off"), ("-g", "monitor-bell", "on"),
                                   ("-g", "bell-action", "none")):
            self.tmux("set-option", scope, key, value)
        self.tmux("set-option", "-as", "terminal-overrides", ",xterm-ghostty:Ms=\\E]52;%p1%s;%p2%s\\007")
        for i in range(1, 12):
            self.tmux("new-window", "-d", "-c", str(ROOT), "-t", "realuse", "-n", f"agent-{i:02d}",
                      agent(f"agent-{i:02d}", (.2, .7, 2, 0)[i % 4]))
        self.target = self.windows()[0]["id"]
        self.tmux("split-window", "-h", "-c", str(ROOT), "-t", self.target, agent("agent-split", 0))
        self.tmux("select-pane", "-t", self.target + ".0")
        for pane in self.tmux("list-panes", "-s", "-t", "realuse", "-F", "#{pane_id}").splitlines():
            self.tmux("set-option", "-p", "-t", pane, "@pane_harness", "claude")
        self.socket = self.tmux("display-message", "-p", "#{socket_path}")
        self.restart()
        self.check("direct startup with background server", "ncm --socket <private> --session realuse on owned PTY; no tmux client",
                   "grid visible; kitty query answered; direct images; zero passthrough; server has no interactive clients",
                   lambda: None,
                   lambda: "agent-00" in self.term.text() and self.term.stats["image_uploads"] > 0
                   and self.term.stats["queries"] == 1
                   and self.term.stats["passthrough_packets"] == 0 and not self.interactive_clients(), timeout=4)
        (OUT / "help.txt").write_text(subprocess.check_output([str(self.binary), "--help"], env=self.env).decode())
        (OUT / "configuration.txt").write_text(self.tmux("show-options", "-g") + "\n" + self.tmux("show-options", "-s")
                                               + "\nprimary: direct PTY, TERM=xterm-ghostty, TMUX unset\n")
        (OUT / "build.txt").write_text(subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT).decode()
                                      + subprocess.check_output(["tmux", "-V"], env=self.env).decode()
                                      + "binary=" + str(self.binary) + "\n")

    def restart(self):
        if self.primary:
            self.primary.close()
            if self.primary in self.terminals:
                self.terminals.remove(self.primary)
        self.direct_serial += 1
        self.primary = Terminal(self.command(), self.env, f"direct-{self.direct_serial}")
        self.term = self.primary
        self.terminals.append(self.term)
        self.wait(lambda: "agent-00" in self.term.text(), 4)
        time.sleep(.2)

    def interactive_clients(self):
        # ncm's read-only -C connection is size-neutral and has no user terminal.
        return self.tmux("list-clients", "-F", "#{?client_control_mode,,#{client_name}}")

    def helpers(self):
        return [s for s in self.tmux("list-sessions", "-F", "#{session_name}").splitlines()
                if s.startswith("_ncm_view_")]

    def viewer_alive(self):
        return self.primary.child.poll() is None

    def home(self):
        if self.helpers():
            self.key("Ctrl-]")
            if not self.wait(lambda: not self.helpers(), .8):
                for helper in self.helpers():
                    self.tmux("kill-session", "-t", helper)
        if not self.viewer_alive():
            self.restart()
        self.key("Esc")
        time.sleep(.07)
        for _ in range(13):
            self.key("Left")
        self.wait(lambda: self.selected_screen() == 1 or self.input_state().get("selected") == "0", .6)
        time.sleep(.1)

    def is_attached(self):
        return bool(self.helpers()) and "_ncm_view_" in self.tmux("list-clients", "-F", "#{session_name}")

    def live_preview(self):
        # A stale rendered header alone must never pass an open-preview assertion.
        return self.quick() and self.input_state().get("quick") == "true"

    def attach_cycle(self, mode, key):
        self.home()
        if mode == "quick":
            self.key("Space")
            self.wait(self.quick)
        alt = self.term.stats["alternate_exits"]
        self.check(f"{mode}: {key} attach", repr(KEYS.get(key, key.encode())),
                   "leave ncm alt screen; real attach client in _ncm_view_<pid>; source pane displayed",
                   lambda: self.key(key, mode), lambda: self.is_attached() and "CLIPBOARD_PROBE" in self.term.text()
                   and self.term.stats["alternate_exits"] > alt, timeout=1.5)
        was_attached = self.is_attached()
        images = self.term.stats["image_uploads"]
        self.check(f"{mode}: Ctrl-] return after {key}", "b'\\x1d'", "attach exits; grouped session removed; ncm preview returns with images",
                   lambda: self.key("Ctrl-]", mode), lambda: was_attached and not self.helpers() and not self.interactive_clients()
                   and self.live_preview() and self.viewer_alive() and self.term.stats["image_uploads"] > images, timeout=1.2,
                   actual=lambda: {"attach_existed_before_return": was_attached, "state": self.state()})

    def scenarios(self):
        for mode in ("grid", "quick"):
            self.home()
            if mode == "quick":
                self.key("Space")
                self.wait(self.quick)
            for key, delta in (("Right", 1), ("Left", -1)):
                before = int(self.input_state().get("selected", "0"))
                self.check(f"{mode}: {key}", repr(KEYS[key]), "selection moves one card and preview follows",
                           lambda k=key: self.key(k, mode),
                           lambda b=before, d=delta: self.input_state().get("selected") == str(b + d)
                           and (self.quick() if mode == "quick" else self.selected_screen() == b + d + 1))
            if mode == "quick":
                self.key("2")
                self.wait(lambda: "HISTORY-" in self.term.text(), 1.5)
                order = ("Up", "Down")
            else:
                order = ("Down", "Up")
            for key in order:
                before = self.term.text()
                self.check(f"{mode}: {key}", repr(KEYS[key]), "older/newer Talk history" if mode == "quick" else "move one grid row",
                           lambda k=key: self.key(k, mode), lambda b=before: self.term.text() != b)
            before = self.input_state().get("selected")
            self.check(f"{mode}: Tab", "b'\\t'", "select another fixture with an outstanding bell alert",
                       lambda: self.key("Tab", mode), lambda: self.input_state().get("selected") != before)
            self.home()
            if mode == "quick":
                self.key("Space")
                self.wait(self.quick)
            for key in ("1", "2", "3", "[", "]", ",", "x", "n"):
                if mode == "grid" and key in (",", "x", "n"):
                    continue
                before = self.term.text()
                ids = [(w["id"], w["name"]) for w in self.windows()]
                stamp = trace_clock()
                if mode == "quick" and key in ("1", "2", "3"):
                    label = {"1": "Screen", "2": "Talk", "3": "Reports"}[key]
                    predicate = lambda label=label: self.active_tab(label)
                    expected = f"{label} tab active in rendered preview"
                elif mode == "quick" and key in ("[", "]"):
                    predicate = lambda b=before: self.quick() and self.term.text() != b
                    expected = "follow another pane in the split fixture"
                else:
                    predicate = lambda s=stamp, ids=ids: any(r.get("event") == "input_painted" for r in self.logs(s)) and [(w["id"], w["name"]) for w in self.windows()] == ids
                    expected = "no action outside documented context"
                self.check(f"{mode}: {key}", repr(key.encode()), expected,
                           lambda k=key: self.key(k, mode), predicate)
            query = self.term.stats["queries"]
            images = self.term.stats["image_uploads"]
            self.check(f"{mode}: g", "b'g'", "single startup probe retained; direct graphics continue with no tmux passthrough",
                       lambda: self.key("g", mode), lambda: self.term.stats["queries"] == query and query == 1
                       and self.term.stats["image_uploads"] > images
                       and self.term.stats["passthrough_packets"] == 0, timeout=1.5)
            self.check(f"{mode}: Space", "b' '", "toggle floating preview", lambda: self.key("Space", mode),
                       lambda: self.live_preview() if mode == "grid" else not self.quick())
            self.check(f"{mode}: Esc", "b'\\x1b'", "return to grid and keep ncm running", lambda: self.key("Esc", mode),
                       lambda: not self.quick() and self.viewer_alive())
            self.attach_cycle(mode, "Enter")
            self.attach_cycle(mode, "i")
        self.grid_actions()
        self.mouse_actions()
        for mode in ("quick", "grid"):
            self.home()
            if mode == "quick":
                self.key("Space")
                self.wait(self.quick)
            self.check(f"{mode}: q", "b'q'", "ncm exits, leaves alt screen and restores terminal modes",
                       lambda: self.key("q", mode), lambda: not self.viewer_alive() and not self.term.screen.in_alternate
                       and self.term.modes() == self.term.initial_modes,
                       actual=lambda: {"ncm_alive": self.viewer_alive(), "alternate": self.term.screen.in_alternate,
                                       "terminal_modes_restored": self.term.modes() == self.term.initial_modes,
                                       "terminal": self.term.counters()})
            self.restart()

    def grid_actions(self):
        self.home()
        self.check("grid: , rename prompt", "b','", "rename prompt visible", lambda: self.key(",", "grid"),
                   lambda: "Rename:" in self.term.text())
        self.check("grid: rename save", "b'renamed-realuse\\r'", "target window and rendered card renamed",
                   lambda: self.term.feed(b"renamed-realuse\r"),
                   lambda: any(w["id"] == self.target and w["name"] == "renamed-realuse" for w in self.windows()) and "renamed-realuse" in self.term.text())
        self.tmux("rename-window", "-t", self.target, "agent-00")
        self.check("grid: x close prompt", "b'x'", "close asks y/n", lambda: self.key("x", "grid"),
                   lambda: "Close selected window? y/n" in self.term.text())
        self.check("grid: close cancel", "b'n'", "target preserved and prompt closes", lambda: self.key("n"),
                   lambda: any(w["id"] == self.target for w in self.windows()) and "Close selected window?" not in self.term.text())
        ids = {w["id"] for w in self.windows()}
        self.check("grid: n new window", "b'n'", "one persistent new tmux window and a rendered card",
                   lambda: self.key("n", "grid"), lambda: len(self.windows()) == len(ids) + 1 and "new" in self.term.text())
        new = next((w for w in self.windows() if w["id"] not in ids), None)
        if not new:
            self.tmux("new-window", "-d", "-t", "realuse", "-n", "new", "sleep 180")
        self.wait(lambda: "new" in self.term.text(), 1)
        for _ in range(16):
            self.key("Right")
        time.sleep(.15)
        self.check("grid: close confirm", "b'x', b'y'", "only disposable new window removed",
                   lambda: (self.key("x"), time.sleep(.1), self.key("y")), lambda: {w["id"] for w in self.windows()} == ids)

    def mouse_actions(self):
        self.home()
        point = self.card_point(2)
        self.check("mouse: click card", f"SGR left click at {point}", "clicked card selected and floating preview opens",
                   lambda: self.click(*point), lambda: self.live_preview() and self.input_state().get("selected") == "1")
        self.home()
        point = self.card_point(1)
        self.check("mouse: click selected card", f"SGR left click at {point}", "floating preview opens",
                   lambda: self.click(*point), self.live_preview)
        self.check("mouse: click outside preview", "SGR left click at (2,2)", "floating preview closes",
                   lambda: self.click(2, 2), lambda: not self.quick())
        self.home()
        self.check("mouse: double-click card/preview", "two SGR left clicks at (30,8), 90ms apart",
                   "first opens preview; second inside preview enters native attach",
                   lambda: (self.click(30, 8), time.sleep(.09), self.click(30, 8)), self.is_attached)
        self.home()
        self.key("Space")
        self.wait(self.quick)
        self.check("mouse: click inside preview", "SGR left click at (80,15)", "real attach on _ncm_view_<pid> grouped session",
                   lambda: self.click(80, 15), self.is_attached)
        self.native_mouse()
        self.home()
        self.key("Space")
        self.wait(self.quick)
        self.key("2")
        self.wait(lambda: "HISTORY-" in self.term.text(), 1.5)
        before = self.term.text()
        self.check("mouse: wheel up in Talk", "SGR 64;80;15M x4", "older Talk history displayed",
                   lambda: [self.mouse(64, 80, 15) for _ in range(4)],
                   lambda: self.term.text() != before and "HISTORY-" in self.term.text())
        before = self.term.text()
        self.check("mouse: wheel down in Talk", "SGR 65;80;15M x4", "newer Talk history displayed",
                   lambda: [self.mouse(65, 80, 15) for _ in range(4)],
                   lambda: self.term.text() != before and "HISTORY-" in self.term.text())
        self.home()
        point = self.card_point(1)
        self.check("mouse: right-click options", f"SGR right click at {point}", "usable Go to window, Rename and Close menu without existing tmux client",
                   lambda: self.click(*point, button=2), lambda: all(s in self.term.text() for s in ("Go to window", "Rename", "Close")))
        self.key("Esc")
        self.wait(lambda: "Go to window" not in self.term.text())
        self.check("menu: Go to window", "SGR right click then Enter", "menu opens a real grouped attach without any existing interactive client",
                   lambda: (self.click(*point, button=2), self.key("Enter")), self.is_attached)
        self.home()
        point = self.card_point(1)
        self.check("menu: Rename", "SGR right click, Down, Enter, menu-renamed + Enter", "menu rename changes source window and card",
                   lambda: (self.click(*point, button=2), self.key("Down"), self.key("Enter"), self.term.feed(b"menu-renamed\r")),
                   lambda: any(w["id"] == self.target and w["name"] == "menu-renamed" for w in self.windows()) and "menu-renamed" in self.term.text())
        self.tmux("rename-window", "-t", self.target, "agent-00")
        self.wait(lambda: "agent-00" in self.term.text())
        self.check("menu: Close cancel", "SGR right click, x, n", "menu Close requests confirmation; cancel preserves target",
                   lambda: (self.click(*point, button=2), self.key("x")),
                   lambda: "Close selected window? y/n" in self.term.text())
        self.key("n")
        self.wait(lambda: "Close selected window?" not in self.term.text())
        ids = {w["id"] for w in self.windows()}
        self.key("n")
        self.wait(lambda: len(self.windows()) == len(ids) + 1 and "new" in self.term.text())
        new = next(w for w in self.windows() if w["id"] not in ids)
        point = self.card_point(int(new["index"]))
        def menu_close():
            self.click(*point, button=2)
            self.wait(lambda: "Go to window" in self.term.text())
            # Shared painting/hit-test bounds: centered 34 x 6 menu, Close row.
            self.click(WIDTH // 2, (HEIGHT - 6) // 2 + 4)
            self.wait(lambda: "Close selected window? y/n" in self.term.text())
            self.key("y")
        self.check("menu: Close confirm by mouse", "SGR right click disposable card; click Close; y", "only the disposable source window is removed",
                   menu_close, lambda: {w["id"] for w in self.windows()} == ids)

    def native_mouse(self):
        cases = ("native: click split pane", "native: wheel scroll", "native: drag-select and copy",
                 "native: Ctrl-] return", "native: prefix-d return")
        if not self.is_attached():
            intended = ("SGR left click at (120,5)", "SGR 64;20;10M x3 then 65;20;10M x3",
                        "SGR 0;1;1M, 32;8/16/23;1M, 0;23;1m", "b'\\x1d'", "b'\\x02d'")
            for name, inputs in zip(cases, intended):
                self.record(name, inputs, "real attach client routes input and returns cleanly", False,
                            {"blocked_by": "click-in-preview did not create _ncm_view_ attach client", "state": self.state()})
            return
        initial = self.tmux("display-message", "-p", "-t", self.target, "#{window_width}x#{window_height}")
        panes = self.tmux("list-panes", "-t", self.target, "-F", "#{pane_id}").splitlines()
        self.check(cases[0], "SGR left click at (120,5)", "tmux selects the right-hand real pane",
                   lambda: self.click(120, 5),
                   lambda: self.tmux("display-message", "-p", "-t", self.target, "#{pane_id}") == panes[1])
        self.click(10, 5)
        before = self.term.text()
        self.check(cases[1], "SGR 64;20;10M x3", "native tmux copy mode and older pane history",
                   lambda: [self.mouse(64, 20, 10) for _ in range(3)],
                   lambda: self.tmux("display-message", "-p", "-t", panes[0], "#{pane_in_mode}") == "1" and self.term.text() != before)
        before = self.term.text()
        self.check("native: wheel down", "SGR 65;20;10M x3", "tmux scrolls toward newest real-pane output",
                   lambda: [self.mouse(65, 20, 10) for _ in range(3)], lambda: self.term.text() != before)
        self.term.feed(b"\x03")
        time.sleep(.12)
        def drag():
            self.mouse(0, 1, 1)
            time.sleep(.1)
            for x in (8, 16, 23):
                self.mouse(32, x, 1)
                time.sleep(.04)
            self.mouse(0, 23, 1, True)
        (OUT / "clipboard.txt").unlink(missing_ok=True)
        self.term.clipboard = b""
        self.check(cases[2], "SGR 0;1;1M, 32;8/16/23;1M, 0;23;1m",
                   "tmux buffer and isolated clipboard contain CLIPBOARD_PROBE; source size unchanged", drag,
                   lambda: "CLIPBOARD_PROBE" in self.tmux("show-buffer", check=False)
                   and "CLIPBOARD_PROBE" in ((OUT / "clipboard.txt").read_text() if (OUT / "clipboard.txt").exists() else self.term.clipboard.decode(errors="replace"))
                   and self.tmux("display-message", "-p", "-t", self.target, "#{window_width}x#{window_height}") == initial,
                   actual=lambda: {"buffer": self.tmux("show-buffer", check=False), "osc52": self.term.clipboard.decode(errors="replace"),
                                   "copy_sink": (OUT / "clipboard.txt").read_text() if (OUT / "clipboard.txt").exists() else None,
                                   "size_before": initial, "state": self.state()})
        images = self.term.stats["image_uploads"]
        self.check(cases[3], "b'\\x1d'", "detach, remove grouped helper, resume floating ncm preview",
                   lambda: self.key("Ctrl-]"), lambda: not self.helpers() and self.live_preview() and not self.interactive_clients()
                   and self.term.stats["image_uploads"] > images)
        self.home()
        self.key("i")
        self.wait(self.is_attached, 1.2)
        images = self.term.stats["image_uploads"]
        self.check(cases[4], "b'\\x02d'", "native tmux detach returns to ncm and removes helper",
                   lambda: self.term.feed(b"\x02d"), lambda: not self.helpers() and self.live_preview() and not self.interactive_clients()
                   and self.term.stats["image_uploads"] > images)

    def background_and_legacy(self):
        self.home()
        self.tmux("new-session", "-d", "-s", "laptop", "-x", str(WIDTH), "-y", str(HEIGHT), "sleep 180", outer=True)
        for key, value in (("default-terminal", "tmux-256color"), ("mouse", "on"), ("status", "off"), ("allow-passthrough", "on")):
            self.tmux("set-option", "-g", key, value, outer=True)
        nested = Terminal(["tmux", "-L", self.outer, "-f", "/dev/null", "attach-session", "-t", "laptop"], self.env, "background-nested")
        self.terminals.append(nested)
        attach = shlex.join(["env", "-u", "TMUX", "-u", "TMUX_PANE", "tmux", "-L", self.server,
                            "-f", "/dev/null", "attach-session", "-t", "realuse"])
        images = self.term.stats["image_uploads"]
        self.tmux("respawn-pane", "-k", "-t", "laptop:0", attach, outer=True)
        self.check("background nested client attach", "outer tmux -> background server; ncm remains direct",
                   "nested tmux client exists; direct ncm continues kitty graphics without passthrough",
                   lambda: None, lambda: "tmux " in self.state()["clients"] and self.term.stats["image_uploads"] > images
                   and self.term.stats["passthrough_packets"] == 0 and "agent-00" in self.term.text(), timeout=3)
        self.tmux("kill-server", outer=True, check=False)
        nested.close()
        self.terminals.remove(nested)
        self.check("background nested client detach", "detach owned outer client", "direct grid stays visible with no helpers or background interactive clients",
                   lambda: None, lambda: not self.interactive_clients() and "agent-00" in self.term.text())
        self.variant = "legacy"
        legacytrace = OUT / "legacy.log"
        legacytrace.unlink(missing_ok=True)
        primarytrace = self.trace
        self.trace = legacytrace
        # Deliberately omit --socket: inside tmux must be detected as the legacy viewer.
        cmd = shlex.join([os.sys.executable, str(ROOT / "scripts/realuse/fake_agent.py"), str(OUT), "--own",
                          str(self.binary), "--session", "realuse", "--log", str(legacytrace)])
        viewer = self.tmux("new-window", "-d", "-t", "realuse", "-n", "legacy-ncm", "-P", "-F", "#{window_id}", "sleep 180")
        legacy = Terminal(self.base + ["attach-session", "-t", "realuse"], self.env, "legacy")
        self.terminals.append(legacy)
        self.term = legacy
        self.tmux("respawn-pane", "-k", "-t", viewer, cmd)
        self.check("legacy: start hidden", "launch inside background window while attached client shows agent",
                   "no kitty query/images while legacy viewer is hidden", lambda: time.sleep(.6),
                   lambda: legacy.stats["graphics_bytes"] == 0, timeout=.3)
        self.check("legacy: show viewer", "select legacy-ncm window (setup)", "usable text/braille grid; zero graphics bytes including probes",
                   lambda: self.tmux("select-window", "-t", viewer),
                   lambda: "agent-00" in legacy.text() and legacy.stats["graphics_bytes"] == 0, timeout=2)
        self.check("legacy: preview", "b' '", "text preview and zero graphics bytes", lambda: self.key("Space", "quick"),
                   lambda: self.quick() and legacy.stats["graphics_bytes"] == 0)
        stamp = trace_clock()
        def legacy_reprobe():
            self.key("g", "quick")
            self.wait(lambda: any(r.get("event") == "input" and r.get("key") == "Char('g')" for r in self.logs(stamp)), .7)
            time.sleep(1.2)
        self.check("legacy: g", "b'g' plus 1.2 seconds observation", "zero graphics bytes even after a processed re-probe",
                   legacy_reprobe, lambda: legacy.stats["graphics_bytes"] == 0
                   and any(r.get("event") == "input" and r.get("key") == "Char('g')" for r in self.logs(stamp)), timeout=.3)
        self.key("q")
        time.sleep(.2)
        legacy.close()
        self.terminals.remove(legacy)
        self.tmux("kill-window", "-t", viewer, check=False)
        self.term = self.primary
        self.trace = primarytrace
        self.variant = "direct"
