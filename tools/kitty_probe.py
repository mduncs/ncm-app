#!/usr/bin/env python3
"""Manual kitty/Unicode-placeholder probe. Run through the host heavy wrapper.

No tmux mutations: only diagnostics and display -p calls to the current server.
Protocol reference: https://sw.kovidgoyal.net/kitty/graphics-protocol/
"""

import argparse
import base64
import contextlib
import fcntl
import os
import random
import re
import select
import shutil
import struct
import subprocess
import sys
import termios
import threading
import time
import zlib

ESC = b"\x1b"
PLACEHOLDER = "\U0010eeee"
# First 64 entries of kitty's rowcolumn-diacritics.txt (NOT Unicode order).
DIACRITICS = tuple(chr(int(c, 16)) for c in """
0305 030D 030E 0310 0312 033D 033E 033F 0346 034A 034B 034C
0350 0351 0352 0357 035B 0363 0364 0365 0366 0367 0368 0369
036A 036B 036C 036D 036E 036F 0483 0484 0485 0486 0487 0592
0593 0594 0595 0597 0598 0599 059C 059D 059E 059F 05A0 05A1
05A8 05A9 05AB 05AC 05AF 05C4 0610 0611 0612 0613 0614 0615
0616 0617 0657 0658
""".split())
WIDTH, HEIGHT = 500, 272
COLORS = ((145, 45, 65), (25, 115, 90), (35, 80, 155), (140, 100, 25),
          (100, 45, 145))
QUERY_ID = 16777215


def tmux_wrap(inner):
    return ESC + b"Ptmux;" + inner.replace(ESC, ESC + ESC) + ESC + b"\\"


def graphics_command(controls, payload=b""):
    header = ",".join(f"{k}={v}" for k, v in controls.items()).encode("ascii")
    return ESC + b"_G" + header + b";" + payload + ESC + b"\\"


def query_command(image_id=QUERY_ID):
    return graphics_command(dict(a="q", i=image_id, t="d", f=32, s=1, v=1),
                            base64.b64encode(b"\0\0\0\xff"))


def image_chunks(compressed, image_id, cols, rows, width=WIDTH, height=HEIGHT,
                 chunk_size=4096):
    """Encode a zlib RGBA stream; each yielded APC needs its own DCS wrapper.

    a=T re-creates the virtual placement on EVERY replacement: retransmission
    deletes existing placements per the protocol. Placeholder text stays put.
    """
    if not 0 < chunk_size <= 4096 or chunk_size % 4:
        raise ValueError("chunk_size must be a multiple of 4, at most 4096")
    if not compressed:
        raise ValueError("empty compressed stream")
    encoded = base64.b64encode(compressed)
    for offset in range(0, len(encoded), chunk_size):
        more = int(offset + chunk_size < len(encoded))
        controls = (dict(a="T", t="d", f=32, o="z", s=width, v=height,
                         i=image_id, U=1, c=cols, r=rows, q=2, m=more)
                    if offset == 0 else dict(m=more, q=2))
        yield graphics_command(controls, encoded[offset:offset + chunk_size])


def placeholder_row(image_id, row, start_col, count):
    if not 0 < image_id < 1 << 24:
        raise ValueError("image id must fit nonzero 24-bit foreground")
    if not (0 <= row < len(DIACRITICS) and 0 <= start_col and
            0 <= count and start_col + count <= len(DIACRITICS)):
        raise ValueError("placeholder coordinates outside diacritic table")
    rgb = (image_id >> 16, (image_id >> 8) & 255, image_id & 255)
    color = ("\x1b[38;2;%d;%d;%dm" % rgb).encode("ascii")
    cells = "".join(PLACEHOLDER + DIACRITICS[row] + DIACRITICS[col]
                    for col in range(start_col, start_col + count))
    # Zero underline color: no accidental inherited placement id.
    return b"\x1b[0m\x1b[58;2;0;0;0m" + color + cells.encode("utf-8") + b"\x1b[0m"


def cursor(row, col):
    return f"\x1b[{row};{col}H".encode("ascii")


def sync_output(data):
    return b"\x1b[?2026h" + data + b"\x1b[?2026l"


def tmux_read(*args):
    result = subprocess.run(["tmux", *args], capture_output=True, text=True, timeout=2)
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "tmux command failed")
    return result.stdout.strip()


def diagnostics():
    in_tmux = bool(os.environ.get("TMUX"))
    print(f"In tmux: {in_tmux}")
    print(f"TERM={os.environ.get('TERM', '(unset)')}")
    print(f"TERM_PROGRAM={os.environ.get('TERM_PROGRAM', '(unset)')}")
    if not shutil.which("tmux"):
        print("tmux -V: executable not found")
        print("allow-passthrough: unavailable; run inside a tmux pane")
        return False
    print(f"tmux -V: {tmux_read('-V')}")
    if not in_tmux:
        print("allow-passthrough: unavailable; run inside a tmux pane")
        return False
    target = os.environ.get("TMUX_PANE")
    args = ["show-options", "-A", "-v"] + (["-t", target] if target else [])
    value = tmux_read(*args, "allow-passthrough")
    print(f"allow-passthrough: {value}", flush=True)
    if value not in ("on", "all"):
        print("Passthrough is off. Fix it in this pane with exactly:\n"
              'tmux set-option -w -t "$TMUX_PANE" allow-passthrough on')
        return False
    return True


@contextlib.contextmanager
def cbreak(fd):
    saved = termios.tcgetattr(fd)
    mode = termios.tcgetattr(fd)
    mode[3] &= ~(termios.ICANON | termios.ECHO)
    mode[6][termios.VMIN] = 1
    mode[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, mode)
    try:
        yield
    finally:
        termios.tcsetattr(fd, termios.TCSANOW, saved)


class Terminal:
    def __init__(self, fd):
        self.fd = fd
        self.wire_bytes = 0

    def write(self, data):
        view = memoryview(data)
        while view:
            n = os.write(self.fd, view)
            view = view[n:]
            self.wire_bytes += n

    def graphics(self, data):
        self.write(tmux_wrap(data))

    def query(self, timeout):
        reply = bytearray()
        deadline = time.monotonic() + timeout
        pattern = rb"\x1b_Gi=" + str(QUERY_ID).encode() + rb"(?:,[^;]*)?;([^\x1b]*)\x1b\\"
        with cbreak(self.fd):
            self.graphics(query_command())
            while time.monotonic() < deadline:
                ready, _, _ = select.select([self.fd], [], [], max(0, deadline - time.monotonic()))
                if not ready:
                    break
                reply.extend(os.read(self.fd, 4096))
                match = re.search(pattern, reply)
                if match:
                    return bytes(reply), match.group(1) == b"OK"
        return bytes(reply), None

    def size(self):
        rows, cols, xp, yp = struct.unpack("HHHH", fcntl.ioctl(
            self.fd, termios.TIOCGWINSZ, b"\0" * 8))
        return rows, cols, xp / cols if xp and cols else 10, yp / rows if yp and rows else 20


def draw_frame(index, counter, noise=None):
    from PIL import Image, ImageDraw, ImageFont
    image = Image.new("RGBA", (WIDTH, HEIGHT), (*COLORS[index], 255))
    if noise is not None:
        # Fixed high-entropy strip gives ~100 KB real compressed pixels, not padding.
        image.paste(Image.frombytes("RGBA", (WIDTH, 50), noise), (0, HEIGHT - 50))
    draw = ImageDraw.Draw(image)
    for x in range(0, WIDTH, 32):
        draw.line((x, 0, x, HEIGHT), fill=(200, 210, 220, 255))
    for y in range(0, HEIGHT, 32):
        draw.line((0, y, WIDTH, y), fill=(200, 210, 220, 255))
    draw.rectangle((2, 2, WIDTH - 3, HEIGHT - 3), outline="white", width=4)
    font = ImageFont.load_default(size=90)
    draw.rectangle((50, 65, 450, 180), fill=(*COLORS[index], 255))
    draw.text((65, 65), f"{counter:03d}", font=font, fill="white")
    draw.text((10, 10), f"IMAGE {index + 1} / SAME ID", fill="white",
              font=ImageFont.load_default(size=22))
    return zlib.compress(image.tobytes(), 3)


class Layout:
    def __init__(self, terminal, ids, bench=False):
        rows, cols, cw, ch = terminal.size()
        if rows < 18 or cols < 40:
            raise RuntimeError("Pane too small: need at least 40 columns x 18 rows")
        self.rows, self.cols = rows, cols
        self.w = min(64, max(8, round(WIDTH / cw)), (cols - 3) // 2)
        self.h = min(64, max(5, round(HEIGHT / ch)), (rows - 7) // 2)
        self.cards = [(ids[i], 2 + (i // 2) * (self.h + 1),
                       1 + (i % 2) * (self.w + 1)) for i in range(4)]
        if bench:
            # Fifth image shares the upper-left rectangle, with its own visible
            # inset placement. All five streams traverse passthrough at 2 Hz.
            self.cards.append((ids[4], 3, 2))
        self.panel_w = min(46, self.w * 2 + 1)
        self.panel_h = 7
        self.panel_x = 1 + (self.w * 2 + 1 - self.panel_w) // 2
        self.panel_y = 2 + (self.h * 2 + 1 - self.panel_h) // 2

    def placeholders(self, region=None):
        data = bytearray()
        for image_id, y, x in self.cards:
            for r in range(self.h):
                left, right = x, x + self.w
                if region:
                    ry, rx, rh, rw = region
                    if not ry <= y + r < ry + rh:
                        continue
                    left, right = max(left, rx), min(right, rx + rw)
                if right > left:
                    data.extend(cursor(y + r, left))
                    data.extend(placeholder_row(image_id, r, left - x, right - left))
        return bytes(data)

    def panel(self):
        lines = ["+" + "-" * (self.panel_w - 2) + "+"]
        for text in ("QUICK LOOK: plain terminal text", "Images keep updating underneath",
                     "No placeholder writes during updates", "Panel visible from 10 to 18 seconds", ""):
            lines.append("|" + text[:self.panel_w - 2].center(self.panel_w - 2) + "|")
        lines.append(lines[0])
        return b"".join(cursor(self.panel_y + i, self.panel_x) +
                        b"\x1b[0;37;40m" + line.encode("ascii") + b"\x1b[0m"
                        for i, line in enumerate(lines))

    def restore_panel(self):
        region = (self.panel_y, self.panel_x, self.panel_h, self.panel_w)
        # Restore gaps as well as precisely the covered placeholder fragments.
        blank = b"".join(cursor(self.panel_y + i, self.panel_x) + b"\x1b[0m" +
                         b" " * self.panel_w for i in range(self.panel_h))
        return blank + self.placeholders(region)

    def status(self, text):
        return cursor(self.rows - 2, 1) + b"\x1b[0m" + text[:self.cols - 1].encode() + b"\x1b[K"


class LatencySampler:
    def __init__(self):
        self.samples = []
        self.errors = 0
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)

    def run(self):
        deadline = time.monotonic()
        while not self.stop.is_set():
            start = time.monotonic()
            try:
                tmux_read("display", "-p", "#{pid}")
                self.samples.append((start, (time.monotonic() - start) * 1000))
            except (RuntimeError, subprocess.TimeoutExpired, OSError):
                self.errors += 1
            deadline += 0.1
            if deadline < time.monotonic():
                deadline = time.monotonic()
            self.stop.wait(max(0, deadline - time.monotonic()))

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.stop.set()
        self.thread.join(timeout=3)


def latency_summary(samples):
    values = sorted(samples)
    if not values:
        return "no successful samples"
    def percentile(p):
        return values[max(0, int(len(values) * p + 0.999999) - 1)]
    return f"n={len(values)} p50={percentile(.50):.2f} ms p95={percentile(.95):.2f} ms"


def animate(terminal, layout, bench):
    count, duration = (5, 15) if bench else (4, 30)
    noises = [random.Random(410 + i).randbytes(WIDTH * 50 * 4)
              for i in range(count)] if bench else [None] * count
    payload_bytes = 0
    panel_shown = panel_restored = False
    sampler_context = LatencySampler() if bench else contextlib.nullcontext()
    with sampler_context as sampler:
        if bench:
            terminal.write(sync_output(layout.status("Idle latency baseline: 3 seconds...")))
            time.sleep(3)
        start = time.monotonic()
        start_bytes = terminal.wire_bytes
        counter = 0
        deadline = start
        while time.monotonic() - start < duration:
            for i, (image_id, _, _) in enumerate(layout.cards):
                compressed = draw_frame(i, counter, noises[i])
                payload_bytes += len(compressed)
                for chunk in image_chunks(compressed, image_id, layout.w, layout.h):
                    terminal.graphics(chunk)
            elapsed = time.monotonic() - start
            text = bytearray()
            if counter == 0:
                text.extend(layout.placeholders())
            if not bench and elapsed >= 10 and not panel_shown:
                text.extend(layout.panel())
                panel_shown = True
            if not bench and elapsed >= 18 and not panel_restored:
                text.extend(layout.restore_panel())
                panel_restored = True
            rate = (terminal.wire_bytes - start_bytes) / max(elapsed, .001)
            text.extend(layout.status(f"{'BENCH' if bench else 'PROBE'} {elapsed:4.1f}/{duration}s "
                                      f"frame={counter:03d} wire={rate:,.0f} B/s"))
            terminal.write(sync_output(bytes(text)))
            counter += 1
            deadline += .5
            time.sleep(max(0, min(deadline, start + duration) - time.monotonic()))
        end = time.monotonic()
        wire = terminal.wire_bytes - start_bytes
    results = [f"{counter} ticks / {end - start:.2f}s; compressed={payload_bytes / (end-start):,.0f} B/s; "
               f"wire={wire / (end-start):,.0f} B/s"]
    if counter < duration * 2:
        results.append(f"WARNING: missed 2 Hz target ({duration * 2} ticks); output/rendering stalled.")
    if bench:
        results.extend([
            f"Mean compressed image: {payload_bytes / (counter * count):,.0f} bytes (target ~100 KB)",
            "Idle:  " + latency_summary([ms for t, ms in sampler.samples if t < start]),
            "Burst: " + latency_summary([ms for t, ms in sampler.samples if start <= t < end]),
            f"tmux command failures/timeouts: {sampler.errors}",
            "Includes process-spawn overhead; target wire load exceeds 600 KB/s by design."])
    return results


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bench", action="store_true", help="5 images at 2 Hz for 15s, plus 3s idle baseline")
    parser.add_argument("--query-timeout", type=float, default=1.0, help="reply timeout in seconds (default 1)")
    args = parser.parse_args()
    if not 0 < args.query_timeout <= 10:
        parser.error("query timeout must be >0 and <=10 seconds")
    try:
        if not diagnostics():
            return 1
        # Check dependency before changing terminal state.
        from PIL import Image  # noqa: F401
        with open("/dev/tty", "r+b", buffering=0) as tty_file:
            terminal = Terminal(tty_file.fileno())
            reply, supported = terminal.query(args.query_timeout)
            print(f"Kitty query reply (escaped): {reply!r}", flush=True)
            if supported is False:
                print("Query rejected; graphics unavailable.")
                return 1
            if supported is None:
                print("No matching reply before timeout: support UNCONFIRMED. Continuing visual probe.")
            print("Query does not establish Unicode placeholder support. Watch for flicker and panel restoration.",
                  flush=True)
            ids = random.SystemRandom().sample(range(1, QUERY_ID), 5)
            layout = Layout(terminal, ids, args.bench)
            terminal.write(b"\x1b[?1049h\x1b[?25l")
            try:
                results = animate(terminal, layout, args.bench)
            finally:
                for image_id in ids:
                    terminal.graphics(graphics_command(dict(a="d", d="I", i=image_id, q=2)))
                terminal.write(b"\x1b[?2026l\x1b[0m\x1b[?25h\x1b[?1049l")
            for result in results:
                print(result)
            expected = ("5 images updating, no flashing; compare idle/burst latency" if args.bench else
                        "4 squares counting up, no flashing, a text box over the middle at 10–18s, then the squares fully back")
            print("You should have seen: " + expected)
            print("Press Enter to finish.", flush=True)
            with cbreak(terminal.fd):
                while True:
                    key = os.read(terminal.fd, 1024)
                    if b"\r" in key or b"\n" in key:
                        break
        return 0
    except KeyboardInterrupt:
        print("\nProbe interrupted; terminal restored.")
        return 130
    except (OSError, RuntimeError, subprocess.TimeoutExpired, ImportError) as error:
        print(f"Probe failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
