#!/usr/bin/env python3
"""Drive the owned terminal, never tmux send-keys or ncm's input handlers."""
import argparse
import base64
import codecs
import copy
from collections import Counter
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import shlex
import signal
import struct
import subprocess
import termios
import threading
import time
import traceback

import pyte
from wcwidth import wcwidth

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "target" / "realuse"
WIDTH, HEIGHT = 160, 48
KEYS = {"Left": b"\x1b[D", "Right": b"\x1b[C", "Up": b"\x1b[A", "Down": b"\x1b[B",
        "Tab": b"\t", "Enter": b"\r", "Esc": b"\x1b", "Space": b" ", "Ctrl-]": b"\x1d"}


def trace_clock():
    # CPython on macOS uses CLOCK_UPTIME_RAW for monotonic(); ncm uses CLOCK_MONOTONIC.
    return time.clock_gettime_ns(time.CLOCK_MONOTONIC)


class VTStream(pyte.Stream):
    # pyte 0.8.2 omits ECMA-48 REP, used for kitty placeholder runs.
    csi = dict(pyte.Stream.csi, b="repeat_character")
    events = pyte.Stream.events | {"repeat_character"}


class VTScreen(pyte.Screen):
    """pyte's cell engine with xterm alternate-screen save/restore."""
    def __init__(self, *args):
        self.saved_main = None
        self.in_alternate = False
        super().__init__(*args)

    def draw(self, data):
        super().draw(data)
        for char in data:
            if wcwidth(char) > 0:
                self.last_printable = char

    def repeat_character(self, count=1):
        char = getattr(self, "last_printable", "")
        if char:
            self.draw(char * (count or 1))

    def set_mode(self, *modes, **kwargs):
        if kwargs.get("private") and any(m in (47, 1047, 1049) for m in modes) and not self.in_alternate:
            self.saved_main = (copy.deepcopy(self.buffer), copy.deepcopy(self.cursor), self.margins)
            self.in_alternate = True
            self.erase_in_display(2)
            self.cursor_position()
        super().set_mode(*modes, **kwargs)

    def reset_mode(self, *modes, **kwargs):
        super().reset_mode(*modes, **kwargs)
        if kwargs.get("private") and any(m in (47, 1047, 1049) for m in modes) and self.in_alternate:
            if self.saved_main:
                self.buffer, self.cursor, self.margins = self.saved_main
            self.saved_main = None
            self.in_alternate = False
            self.dirty.update(range(self.lines))


class Terminal:
    """Streaming VT screen plus tmux DCS/kitty APC protocol termination.

    Kitty pictures are represented by placement IDs and payload hashes: pyte renders
    the text/placeholder plane, not raster pixels. Never feed base64 to the VT screen.
    """
    def __init__(self, command, env, name):
        self.name = name
        self.screen = VTScreen(WIDTH, HEIGHT)
        self.stream = VTStream(self.screen)
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")
        self.pending = b""
        self.text_at = time.monotonic()
        self.stats = Counter()
        self.images = {}
        self.upload = None
        self.clipboard = b""
        self.events = []
        self.inputs = []
        self.lock = threading.RLock()
        self.done = threading.Event()
        self.error = None
        self.raw = (OUT / f"{name}.vt").open("wb")
        self.master, slave = pty.openpty()
        self.initial_modes = termios.tcgetattr(slave)
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", HEIGHT, WIDTH, WIDTH * 8, HEIGHT * 16))
        wrapper = [os.sys.executable, str(ROOT / "scripts/realuse/fake_agent.py"), "--terminal"]
        self.child = subprocess.Popen(wrapper + command, stdin=slave, stdout=slave, stderr=slave,
                                      env=env, start_new_session=True)
        os.close(slave)
        self.thread = threading.Thread(target=self.drain, daemon=True)
        self.thread.start()

    def feed(self, data):
        self.inputs.append({"time": time.monotonic(), "hex": data.hex(), "escaped": repr(data)})
        os.write(self.master, data)

    def text(self):
        with self.lock:
            return "\n".join(line.rstrip() for line in self.screen.display)

    def vt(self, data):
        now = time.monotonic()
        if now - self.text_at > .05:
            self.flush_rows()
        self.text_at = now
        self.stats["text_bytes"] += len(data)
        self.stream.feed(self.decoder.decode(data))

    def protocol(self, data):
        self.pending += data
        while self.pending:
            esc = self.pending.find(b"\x1b")
            if esc < 0:
                self.vt(self.pending)
                self.pending = b""
                return
            if esc:
                self.vt(self.pending[:esc])
                self.pending = self.pending[esc:]
            if len(self.pending) < 2:
                return
            kind = self.pending[1:2]
            if kind in (b"P", b"_", b"]"):
                end = 2
                while end < len(self.pending):
                    if kind == b"]" and self.pending[end] == 7:
                        break
                    if self.pending[end:end + 2] == b"\x1b\x1b" and kind == b"P":
                        end += 2
                        continue
                    if self.pending[end:end + 2] == b"\x1b\\":
                        break
                    end += 1
                if end >= len(self.pending) or (self.pending[end] == 27 and end + 1 == len(self.pending)):
                    return
                stop = end + (1 if self.pending[end] == 7 else 2)
                packet, self.pending = self.pending[:stop], self.pending[stop:]
                if kind == b"P" and packet.startswith(b"\x1bPtmux;"):
                    self.stats["passthrough_packets"] += 1
                    tail = self.pending
                    self.pending = b""
                    self.protocol(packet[7:-2].replace(b"\x1b\x1b", b"\x1b"))
                    self.pending += tail
                elif kind == b"_" and packet.startswith(b"\x1b_G"):
                    self.kitty(packet)
                elif kind == b"]" and packet.startswith(b"\x1b]52;"):
                    encoded = packet[5:end].split(b";", 1)[-1]
                    try:
                        self.clipboard = base64.b64decode(encoded)
                        self.stats["osc52"] += 1
                    except ValueError:
                        pass
                elif kind == b"]":
                    self.vt(packet)
            elif kind == b"[":
                match = re.match(rb"\x1b\[[0-?]*[ -/]*[@-~]", self.pending)
                if not match:
                    return
                packet = match.group()
                self.pending = self.pending[len(packet):]
                if packet in (b"\x1b[c", b"\x1b[0c"):
                    os.write(self.master, b"\x1b[?62;4;22c")
                elif packet == b"\x1b[>c":
                    os.write(self.master, b"\x1b[>1;4000;0c")
                elif packet == b"\x1b[16t":
                    os.write(self.master, b"\x1b[6;16;8t")
                elif packet == b"\x1b[18t":
                    os.write(self.master, f"\x1b[8;{HEIGHT};{WIDTH}t".encode())
                elif packet == b"\x1b[6n":
                    os.write(self.master, b"\x1b[1;1R")
                elif packet in (b"\x1b[>q", b"\x1b[>0q"):
                    os.write(self.master, b"\x1bP>|ghostty 1.2.0\x1b\\")
                if packet in (b"\x1b[2J", b"\x1b[3J"):
                    self.stats["full_screen_clears"] += 1
                if re.fullmatch(rb"\x1b\[\?(47|1047|1049)h", packet):
                    self.stats["alternate_enters"] += 1
                if re.fullmatch(rb"\x1b\[\?(47|1047|1049)l", packet):
                    self.stats["alternate_exits"] += 1
                if packet[-1:] not in (b"c", b"n", b"t"):
                    self.vt(packet)
            else:
                count = 3 if kind in (b"(", b")", b"*", b"+") else 2
                if len(self.pending) < count:
                    return
                self.vt(self.pending[:count])
                self.pending = self.pending[count:]

    def kitty(self, packet):
        self.stats["graphics_bytes"] += len(packet)
        header, _, payload = packet[3:-2].partition(b";")
        fields = dict(pair.split(b"=", 1) for pair in header.split(b",") if b"=" in pair)
        action = fields.get(b"a")
        if action == b"q":
            self.stats["queries"] += 1
            ident = fields.get(b"i", b"255")
            os.write(self.master, b"\x1b_Gi=" + ident + b";OK\x1b\\\x1b[6;16;8t")
        if action in (b"T", b"t"):
            ident = int(fields.get(b"i", b"0"))
            self.upload = (ident, hashlib.sha256())
        if self.upload is not None and (b"m" in fields or action in (b"T", b"t")):
            ident, digest = self.upload
            digest.update(payload)
            if fields.get(b"m", b"0") == b"0":
                value = digest.hexdigest()
                self.stats["image_uploads"] += 1
                if self.images.get(ident) == value:
                    self.stats["identical_image_resends"] += 1
                self.images[ident] = value
                self.events.append({"time": time.monotonic(), "image": ident, "hash": value})
                self.upload = None

    def flush_rows(self):
        if self.screen.dirty:
            self.stats["text_bursts"] += 1
            if len(self.screen.dirty) >= (HEIGHT - 1) * .9:
                self.stats["full_screen_redraws"] += 1
            self.screen.dirty.clear()

    def drain(self):
        try:
            while not self.done.is_set():
                if not select.select([self.master], [], [], .025)[0]:
                    if time.monotonic() - self.text_at > .05:
                        with self.lock:
                            self.flush_rows()
                    continue
                data = os.read(self.master, 65536)
                if not data:
                    break
                with self.lock:
                    self.raw.write(data)
                    self.stats["wire_bytes"] += len(data)
                    self.protocol(data)
        except OSError:
            if self.child.poll() is None and not self.done.is_set():
                self.error = traceback.format_exc()
        except Exception:
            if not self.done.is_set():
                self.error = traceback.format_exc()

    def counters(self):
        with self.lock:
            if time.monotonic() - self.text_at > .05:
                self.flush_rows()
            return dict(self.stats)

    def modes(self):
        try:
            return termios.tcgetattr(self.master)
        except termios.error:
            return None

    def close(self):
        self.done.set()
        if self.child.poll() is None:
            self.child.terminate()
        try:
            self.child.wait(timeout=.5)
        except subprocess.TimeoutExpired:
            self.child.kill()
            # Darwin can keep a controlling-tty session leader in exit while the
            # parent retains the master. Release it before waiting for final reap.
            os.close(self.master)
            self.master = None
            self.child.wait(timeout=1.5)
        self.thread.join(timeout=.5)
        if self.master is not None:
            os.close(self.master)
            self.master = None
        self.raw.close()
        (OUT / f"{self.name}-terminal.json").write_text(json.dumps({"counters": dict(self.stats),
            "images": self.images, "events": self.events, "inputs": self.inputs, "error": self.error}, indent=2))


class Harness:
    def __init__(self, binary):
        self.binary = binary
        self.server = f"ncm-real-{os.getpid()}"
        self.outer = self.server + "-outer"
        self.base = ["tmux", "-L", self.server, "-f", "/dev/null"]
        self.env = {k: v for k, v in os.environ.items()
                    if k not in ("TMUX", "TMUX_PANE", "NCM_TMUX_SOCKET", "TMUX_TMPDIR")}
        self.env.update(TERM="xterm-ghostty", HOME=str(OUT / "home"),
                        TMUX_TMPDIR=str(OUT / "s"),
                        CODEX_HOME=str(OUT / "home" / "codex"),
                        NCM_CLAUDE_ROOT=str(OUT / "claude"))
        self.env["PATH"] = str(OUT / "bin") + ":" + self.env["PATH"]
        self.trace = OUT / "ncm.log"
        self.results = []
        self.term = None
        self.terminals = []
        self.variant = "ghostty"
        self.viewer = None
        self.idle = {}
        self.deadline = time.monotonic() + 165
        self.cleanup_result = None
        self.coverage = {v: {m: set() for m in ("grid", "quick")} for v in ("ghostty", "nested")}

    def tmux(self, *args, check=True, outer=False):
        base = ["tmux", "-L", self.outer, "-f", "/dev/null"] if outer else self.base
        p = subprocess.run(base + list(args), env=self.env, capture_output=True, text=True, timeout=3)
        if check and p.returncode:
            raise RuntimeError(shlex.join(base + list(args)) + ": " + p.stderr.strip())
        return p.stdout.strip()

    def windows(self):
        text = self.tmux("list-windows", "-t", "realuse", "-F", "#{window_id}|#{window_index}|#{window_name}|#{window_active}|#{window_panes}")
        return [dict(zip(("id", "index", "name", "active", "panes"), line.split("|"))) for line in text.splitlines()]

    def logs(self, since=0):
        rows = []
        if self.trace.exists():
            for line in self.trace.read_text().splitlines():
                row = dict(re.findall(r"(\w+)=([^\s]+)", line))
                if int(row.get("mono_ns", 0)) >= since:
                    rows.append(row)
        return rows

    def input_state(self):
        rows = [r for r in self.logs() if r.get("event") == "input_painted"]
        return rows[-1] if rows else {}

    def state(self):
        return {"windows": self.windows(), "clients": self.tmux("list-clients", "-F",
                "#{client_name}|#{client_termtype}|#{client_termname}|#{session_name}|#{window_id}"),
                "sessions": self.tmux("list-sessions", "-F", "#{session_name}"),
                "input_painted": self.input_state(), "terminal": self.term.counters() if self.term else {}}

    def wait(self, predicate, timeout=1.3):
        end = min(time.monotonic() + timeout, self.deadline)
        while time.monotonic() < end:
            if self.term and self.term.error:
                raise RuntimeError(self.term.error)
            if predicate():
                return True
            time.sleep(.035)
        return bool(predicate())

    def record(self, name, inputs, expected, passed, actual, exact_inputs=None):
        idx = len(self.results) + 1
        screen = self.term.text() if self.term else "No attached terminal"
        filename = f"{idx:03d}-{self.variant}-{re.sub('[^a-z0-9]+', '-', name.lower()).strip('-')}.txt"
        (OUT / "screens" / filename).write_text(screen + "\n")
        row = {"name": name, "variant": self.variant, "input": inputs, "expected": expected,
               "pass": bool(passed), "actual": actual, "screen": "screens/" + filename,
               "exact_inputs": exact_inputs or []}
        self.results.append(row)
        print(f"{'PASS' if passed else 'FAIL'} [{self.variant}] {name}", flush=True)
        self.report()

    def check(self, name, inputs, expected, action, predicate, actual=None, timeout=1.3):
        input_start = len(self.term.inputs) if self.term else 0
        try:
            action()
            ok = self.wait(predicate, timeout)
            time.sleep(.12)
            ok = ok and bool(predicate())
            self.record(name, inputs, expected, ok, actual() if actual else self.state(),
                        self.term.inputs[input_start:] if self.term else [])
        except Exception as exc:
            self.record(name, inputs, expected, False, {"error": str(exc), "trace": traceback.format_exc()},
                        self.term.inputs[input_start:] if self.term else [])
        if time.monotonic() > self.deadline:
            raise TimeoutError("165 second harness deadline reached")

    def key(self, key, mode=None):
        if mode:
            self.coverage[self.variant][mode].add(key)
        self.term.feed(KEYS.get(key, key.encode()))

    def mouse(self, button, x, y, release=False):
        self.term.feed(f"\x1b[<{button};{x};{y}{'m' if release else 'M'}".encode())

    def click(self, x, y, button=0):
        self.mouse(button, x, y)
        time.sleep(.045)
        self.mouse(button, x, y, True)


    def quick(self):
        return "Screen" in self.term.text() and "Reports" in self.term.text()


    def selected_screen(self):
        # Selected card has a bright border, an observable VT style, independent of logs.
        with self.term.lock:
            candidates = []
            for y, line in enumerate(self.term.screen.display[:-1]):
                for match in re.finditer(r" (\d+)[^\w\s]* (ncm|agent-[\w-]+|renamed-realuse|new) ", line):
                    cell = self.term.screen.buffer[y][match.start()]
                    if cell.bg == "f0f3f8" and cell.bold:
                        candidates.append(int(match.group(1)))
            return candidates[0] if candidates else None

    def card_point(self, index):
        with self.term.lock:
            for y, line in enumerate(self.term.screen.display[:-1]):
                match = re.search(rf" {index}[^\w\s]* (?:agent-|ncm|renamed|new)", line)
                if match:
                    return match.start() + 6, y + 3
        raise RuntimeError(f"card {index} not found in rendered screen")


    def active_tab(self, name):
        with self.term.lock:
            for y in range(HEIGHT):
                # display strings contain combining diacritics; indices are not cell columns.
                line = "".join(self.term.screen.buffer[y][x].data[:1] or " " for x in range(WIDTH))
                if "Screen" in line and "Reports" in line:
                    x = line.find(name)
                    return x >= 0 and self.term.screen.buffer[y][x].fg == "e2e6ee" and self.term.screen.buffer[y][x].bold
        return False


    def measure_idle(self, name, quiet):
        self.home()
        if quiet:
            (OUT / "quiet").touch()
            time.sleep(3.2)  # let every final quiet capture settle before sampling
        else:
            (OUT / "quiet").unlink(missing_ok=True)
            time.sleep(1)
        start = self.term.counters()
        stamp = trace_clock()
        first = time.monotonic()
        second_rates = []
        previous, previous_time = start, first
        for _ in range(30):
            time.sleep(1)
            current, current_time = self.term.counters(), time.monotonic()
            second_rates.append({"second": len(second_rates), "seconds": current_time - previous_time,
                                 "bytes_per_second": {k: (current.get(k, 0) - previous.get(k, 0)) / (current_time - previous_time)
                                                      for k in ("wire_bytes", "graphics_bytes", "text_bytes")},
                                 "counts": {k: current.get(k, 0) - previous.get(k, 0)
                                            for k in ("full_screen_redraws", "image_uploads", "identical_image_resends")}})
            previous, previous_time = current, current_time
        elapsed = time.monotonic() - first
        end = self.term.counters()
        delta = {k: end.get(k, 0) - start.get(k, 0) for k in end.keys() | start.keys()}
        rows = self.logs(stamp)
        events = Counter(r.get("event") for r in rows)
        bins = []
        for i in range(30):
            subset = [r for r in rows if stamp + i * 1_000_000_000 <= int(r.get("mono_ns", 0)) < stamp + (i + 1) * 1_000_000_000]
            bins.append({"second": i, "events": dict(Counter(r.get("event") for r in subset))})
        result = {"seconds": elapsed, "totals": delta, "per_second": {k: v / elapsed for k, v in delta.items()},
                  "trace_events": dict(events), "per_second_trace": bins, "per_second_terminal": second_rates,
                  "trigger_evidence": "fake pane output continues" if not quiet else "all fake pane output stopped before sample"}
        self.idle[name] = result
        passed = delta.get("full_screen_redraws", 0) == 0 and delta.get("full_screen_clears", 0) == 0
        if quiet:
            passed = passed and delta.get("identical_image_resends", 0) == 0 and delta.get("image_uploads", 0) == 0
        self.record(name, "no input for 30 seconds", "no full-screen redraws; quiet panes have no image uploads/resends",
                    passed, result)


    def cleanup(self):
        for t in list(self.terminals):
            t.close()
        self.terminals.clear()
        self.tmux("kill-server", check=False, outer=True)
        self.tmux("kill-server", check=False)
        alive = []
        for f in (OUT / "pids").glob("*"):
            pid = int(f.name)
            command = subprocess.run(["ps", "-p", str(pid), "-o", "command="], capture_output=True, text=True).stdout
            if str(ROOT) not in command:
                continue
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        end = time.monotonic() + 2
        while time.monotonic() < end:
            alive = []
            for f in (OUT / "pids").glob("*"):
                command = subprocess.run(["ps", "-p", f.name, "-o", "command="], capture_output=True, text=True).stdout
                if str(ROOT) in command:
                    alive.append(int(f.name))
            if not alive:
                break
            time.sleep(.05)
        for pid in alive:
            os.kill(pid, signal.SIGKILL)
        self.cleanup_result = {"surviving_owned_pids": alive,
                               "server_gone": not self.tmux("list-sessions", check=False),
                               "outer_server_gone": not self.tmux("list-sessions", check=False, outer=True)}
        self.report()

    def report(self):
        payload = {"server": self.server, "results": self.results, "idle": self.idle,
                   "coverage": {v: {m: sorted(keys) for m, keys in modes.items()} for v, modes in self.coverage.items()},
                   "cleanup": self.cleanup_result}
        (OUT / "results.json").write_text(json.dumps(payload, indent=2))
        fails = [r for r in self.results if not r["pass"]]
        lines = ["# ncm real-use report", "", f"{len(self.results) - len(fails)} passed; {len(fails)} failed.", "",
                 "PRIMARY: owned 160×48 PTY runs ncm directly with TERM=xterm-ghostty and TMUX unset, mirroring an owned private background server. Enter/i/click-in-preview are expected to leave the ncm alt screen and attach a real tmux client on _ncm_view_<pid>; Ctrl-] or native detach must return. SECONDARY: legacy in-tmux ncm must emit zero graphics bytes. Background nested attach/detach uses an actual outer tmux server.", "",
                 "The VT dumps render text, style-tested selection/tabs and kitty placeholders. Kitty pixels are not rasterized; uploads, stable IDs and payload hashes are recorded separately. The literal copy-command pbcopy resolves to an isolated sink in the private server PATH. No desktop clipboard or GUI is used.", "",
                 "| Variant | Scenario | Result | Screen |", "|---|---|---|---|"]
        for r in self.results:
            lines.append(f"| {r['variant']} | {r['name']} | {'PASS' if r['pass'] else 'FAIL'} | [dump]({r['screen']}) |")
        lines += ["", "## Refresh measurements", "",
                  "A full-screen redraw is a text output burst (50ms quiet boundary) touching ≥90% of terminal rows. Screen-clear commands are counted separately. Image re-sends mean an upload repeats the same payload hash for the same stable ID. Rates below exclude setup and inputs.", ""]
        for name, result in self.idle.items():
            rates = result["per_second"]
            lines += [f"### {name}", "", f"Measured {result['seconds']:.2f}s: full redraws {rates.get('full_screen_redraws', 0):.3f}/s; clears {rates.get('full_screen_clears', 0):.3f}/s; image uploads {rates.get('image_uploads', 0):.3f}/s; identical re-sends {rates.get('identical_image_resends', 0):.3f}/s; graphics {rates.get('graphics_bytes', 0):.1f} B/s; total {rates.get('wire_bytes', 0):.1f} B/s.", "",
                      f"Trigger context: {result['trigger_evidence']}. Trace event counts: `{json.dumps(result['trace_events'])}`. Per-second correlations are in results.json. Causation beyond this controlled busy/quiet comparison is not inferred.", ""]
        lines += ["## Failures", ""]
        for r in fails:
            screen = (OUT / r["screen"]).read_text()
            lines += [f"### {r['variant']}: {r['name']}", "", f"Input: `{r['input']}`", "",
                      "Exact PTY writes (hex and escaped bytes):", "```json", json.dumps(r["exact_inputs"], indent=2), "```", "",
                      f"Expected: {r['expected']}", "",
                      "Actual:", "```json", json.dumps(r["actual"], indent=2), "```", "", "Rendered terminal:", "```text", screen, "```", ""]
        lines += ["## Cleanup", "", "```json", json.dumps(self.cleanup_result, indent=2), "```", ""]
        (OUT / "report.md").write_text("\n".join(lines))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/ncm")
    args = parser.parse_args()
    binary = args.binary.resolve()
    if not binary.is_relative_to(ROOT):
        parser.error("binary must be inside this worktree")
    OUT.mkdir(parents=True, exist_ok=True)
    from outside import DirectHarness
    h = DirectHarness(binary)
    def interrupted(signum, _):
        raise KeyboardInterrupt(f"received signal {signum}")
    for sig in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(sig, interrupted)
    started = time.monotonic()
    try:
        h.start()
        h.measure_idle("idle with busy agents", False)
        h.measure_idle("idle with quiet agents", True)
        (OUT / "quiet").unlink(missing_ok=True)
        (OUT / "alert").touch()
        h.wait(lambda: "!" in h.tmux("list-windows", "-t", "realuse", "-F", "#{window_flags}"), 2)
        h.scenarios()
        h.background_and_legacy()
    except BaseException as exc:
        h.record("harness interrupted", "none", "complete all scenarios within 165 seconds", False,
                 {"error": str(exc), "trace": traceback.format_exc()})
    finally:
        h.cleanup()
    print(f"Report: {OUT / 'report.md'} ({time.monotonic() - started:.1f}s)", flush=True)
    for r in h.results:
        if not r["pass"]:
            print(f"FAIL {r['variant']}: {r['name']}")
    return 1 if any(not r["pass"] for r in h.results) or h.cleanup_result["surviving_owned_pids"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
