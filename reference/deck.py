#!/usr/bin/env python3
"""
deck - every tmux window as a live card; spotlight one and type into it without switching.

  arrows/hjkl  move            Space   spotlight on/off
  Tab          next needs/done Enter   go to that window and close
  z            mini / tail     q       close (Esc never closes)

  mini: each card is the whole window shrunk down (every pane, real colors and
        project tints, braille dots for ink), with each pane's harness chip and
        label written on top. tail: the last lines of the active pane.

  Cards sit under the same PINNED / BUILD / EXPLORE / PERSONAL bars as the
  status rows (@dashboard_group), and each card's title is that window's
  status tab (@status_style_window + @status_marker_window). The border glows
  by @agent_state: pink needs you, mint done, blue working.

  spotlight: the chosen agent big and readable on the left, every card small on
  the right; moving keeps the spotlight following. [ ] picks another pane of a
  split window. i types into it (every key goes to the pane, Esc included)
  until Ctrl-]. Esc or Space goes back to the grid.

Read-only by construction: cards and the spotlight are drawn from `capture-pane`, keys
are forwarded with `send-keys`. No second client ever attaches, so no window is
resized. Run it in a popup:  bind D display-popup -w 95% -h 95% -E deck
DECK_MODE=tail starts in tail mode.
"""
import os
import re
import select
import shutil
import subprocess
import sys
import termios
import time
import tty
import unicodedata

SEP = "\x1f\x1fDECK-SEP\x1f\x1f"
STATE_COLOR = {"needs": "38;2;255;46;136", "done": "38;2;126;231;135", "working": "38;2;76;201;240"}
STATE_MARK = {"needs": "?", "done": "✓", "working": "●"}
ANSI = re.compile(r"\x1b\[[0-9;:?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-_]")
TOKEN = re.compile(f"({ANSI.pattern})")
SGR = re.compile(r"\x1b\[([0-9;:]*)m")

# One Half Dark, so default-colored text shrinks to the color it really has.
BASE16 = [(40, 44, 52), (224, 108, 117), (152, 195, 121), (229, 192, 123),
          (97, 175, 239), (198, 120, 221), (86, 182, 194), (220, 223, 228),
          (93, 103, 122), (224, 108, 117), (152, 195, 121), (229, 192, 123),
          (97, 175, 239), (198, 120, 221), (86, 182, 194), (220, 223, 228)]
DEFAULT_FG, DEFAULT_BG = BASE16[7], BASE16[0]
LABEL_FG, LABEL_BG = (236, 238, 242), (30, 33, 40)
BORDER = (True, (72, 78, 94), None)   # pane separators and title rows: dim ink
BRAILLE = ((0x01, 0x08), (0x02, 0x10), (0x04, 0x20), (0x40, 0x80))  # [dot row][dot col]
COLOR_NAMES = {"black": 0, "red": 1, "green": 2, "yellow": 3, "blue": 4, "magenta": 5, "cyan": 6, "white": 7}

# Same order and colors as the status rows in ~/.tmux-main-status.conf.
SECTIONS = {"pinned": ("PINNED", (219, 234, 254), (30, 58, 138)),
            "build": ("BUILD", (187, 247, 208), (20, 83, 45)),
            "explore": ("EXPLORE", (233, 213, 255), (88, 28, 135)),
            "personal": ("PERSONAL", (254, 243, 199), (120, 53, 15))}
SECTION_ORDER = list(SECTIONS)

FIELDS = ("window_index", "window_name", "pane_id", "@agent_state", "window_active", "pane_width",
          "pane_height", "cursor_x", "cursor_y", "pane_active", "pane_left", "pane_top",
          "window_width", "window_height", "pane_tty", "window-style", "@pane_harness",
          "@pane_harness_color", "?#{@pane_label},#{@pane_label},#{pane_title}", "@dashboard_group",
          "E:@status_style_window", "E:@status_marker_window", "host")


def tmux(*args):
    return subprocess.run(["tmux", *args], capture_output=True, text=True, errors="replace").stdout


def own_tty():
    try:
        return os.ttyname(sys.stdin.fileno())
    except OSError:
        return None


def list_windows():
    """One entry per window, in section order: its active pane (for tail), every pane's
    geometry, tint and label (for mini), and its status tab and section."""
    fmt = "\x1f".join("#{%s}" % f for f in FIELDS)
    wins, me = {}, own_tty()
    for line in tmux("list-panes", "-s", "-F", fmt).splitlines():
        p = line.split("\x1f")
        if len(p) != len(FIELDS) or p[14] == me:
            continue
        win = wins.setdefault(p[0], dict(
            index=p[0], name=p[1], state=p[3], active=p[4] == "1", ww=int(p[12]), wh=int(p[13]),
            group=p[19], tab=tmux_style(p[20]), mark=p[21] or STATE_MARK.get(p[3], ""), panes=[]))
        tint = tmux_style(p[15])[1]
        label = "" if p[18] in (p[22], p[22].split(".")[0]) else p[18]   # untitled panes show the host
        win["panes"].append(dict(pane=p[2], x=int(p[10]), y=int(p[11]), w=int(p[5]), h=int(p[6]),
                                 cx=int(p[7]), cy=int(p[8]), tint=tint, harness=p[16],
                                 hcolor=tmux_color(p[17]), label=label))
        if p[9] == "1":
            win.update(pane=p[2], w=int(p[5]), h=int(p[6]), cx=int(p[7]), cy=int(p[8]), tint=tint)
    # A window whose active pane is the deck itself is left out.
    order = {g: i for i, g in enumerate(SECTION_ORDER)}
    shown = [w for w in wins.values() if "pane" in w]
    return sorted(shown, key=lambda w: order.get(w["group"], len(order) + (not w["group"])))


def capture_all(panes):
    """One tmux invocation for every card: capture, separator, capture, ..."""
    args = []
    for pane in panes:
        args += ["capture-pane", "-p", "-e", "-N", "-t", pane, ";", "display", "-p", SEP, ";"]
    out = tmux(*args[:-1]) if args else ""
    chunks = out.split(SEP + "\n")
    return [c.split("\n") for c in chunks[: len(panes)]]


def cell_width(ch):
    return 2 if unicodedata.east_asian_width(ch) in "WF" else (0 if unicodedata.combining(ch) else 1)


def crop(line, width):
    """Cut a line to `width` cells, keeping SGR escapes, padding with spaces."""
    out, used, i = [], 0, 0
    while i < len(line) and used < width:
        m = ANSI.match(line, i)
        if m:
            if m.group().endswith("m"):
                out.append(m.group())
            i = m.end()
            continue
        w = cell_width(line[i])
        if used + w > width:
            break
        out.append(line[i])
        used += w
        i += 1
    return "".join(out) + "\x1b[0m" + " " * (width - used)


def tail(lines, n):
    while lines and not ANSI.sub("", lines[-1]).strip():
        lines = lines[:-1]
    return lines[-n:] if n > 0 else []


# ---- colors ----------------------------------------------------------------

def xterm256(n):
    if n < 16:
        return BASE16[n]
    if n < 232:
        n -= 16
        steps = (0, 95, 135, 175, 215, 255)
        return steps[n // 36], steps[n // 6 % 6], steps[n % 6]
    v = 8 + 10 * (n - 232)
    return v, v, v


def tmux_color(v):
    v = v.strip().lower()
    if re.fullmatch(r"#[0-9a-f]{6}", v):
        return tuple(int(v[i:i + 2], 16) for i in (1, 3, 5))
    m = re.fullmatch(r"colou?r(\d+)", v)
    if m:
        return xterm256(int(m.group(1)) % 256)
    if v.startswith("bright") and v[6:] in COLOR_NAMES:
        return BASE16[COLOR_NAMES[v[6:]] + 8]
    return BASE16[COLOR_NAMES[v]] if v in COLOR_NAMES else None


def tmux_style(s):
    """A tmux style ('bg=#2b2438' or '#[fg=#111827]#[bg=#fff176]#[bold]') as (fg, bg, bold)."""
    fg = bg = None
    bold = False
    for item in re.sub(r"#\[|\]", ",", s).split(","):
        key, _, value = item.strip().partition("=")
        if key == "fg":
            fg = tmux_color(value)
        elif key == "bg":
            bg = tmux_color(value)
        elif key == "bold":
            bold = True
    return fg, bg, bold


def rgb_sgr(fg=None, bg=None, bold=False):
    parts = ["0"] + (["1"] if bold else [])
    parts.append("38;2;%d;%d;%d" % fg if fg else "39")
    parts.append("48;2;%d;%d;%d" % bg if bg else "49")
    return "\x1b[" + ";".join(parts) + "m"


def tinted(text, bg):
    """Give captured text a pane's tint wherever it would show the terminal's default background."""
    if not bg:
        return text
    on = "48;2;%d;%d;%d" % bg

    def fix(m):
        ps, out, i = m.group(1).split(";") if m.group(1) else [""], [], 0
        while i < len(ps):
            if ps[i] in ("38", "48") and i + 1 < len(ps):      # skip over extended color arguments
                n = {"5": 3, "2": 5}.get(ps[i + 1], 1)
                out += ps[i:i + n]
                i += n
                continue
            out.append(on if ps[i] == "49" else ps[i])
            if ps[i] in ("", "0"):
                out.append(on)
            i += 1
        return "\x1b[" + ";".join(out) + "m"
    return f"\x1b[{on}m" + SGR.sub(fix, text) + "\x1b[0m"


def apply_sgr(params, fg, bg, rev):
    """Fold one SGR sequence into the running (fg, bg, reverse) state."""
    parts = params.split(";") if params else ["0"]
    i = 0
    while i < len(parts):
        sub = parts[i].split(":")
        try:
            p = int(sub[0] or 0)
            if p in (38, 48):
                if len(sub) > 1:                       # 38:2::r:g:b  /  38:5:n
                    c = tuple(int(x or 0) for x in sub[-3:]) if sub[1] == "2" else xterm256(int(sub[2]))
                elif parts[i + 1] == "5":              # 38;5;n
                    c, i = xterm256(int(parts[i + 2])), i + 2
                else:                                  # 38;2;r;g;b
                    c, i = tuple(int(x or 0) for x in parts[i + 2:i + 5]), i + 4
                if p == 38:
                    fg = c
                else:
                    bg = c
            elif p == 0:
                fg = bg = None
                rev = False
            elif p == 7 or p == 27:
                rev = p == 7
            elif 30 <= p <= 37 or 90 <= p <= 97:
                fg = BASE16[p - 30 if p < 90 else p - 82]
            elif 40 <= p <= 47 or 100 <= p <= 107:
                bg = BASE16[p - 40 if p < 100 else p - 92]
            elif p == 39:
                fg = None
            elif p == 49:
                bg = None
        except (ValueError, IndexError):
            pass
        i += 1
    return fg, bg, rev


# ---- mini: shrink a whole window into a card -------------------------------

def cells(line, width, tint=None):
    """A captured line as exactly `width` cells of (ink, fg, bg); default background becomes `tint`."""
    out, fg, bg, rev = [], None, None, False
    for part in TOKEN.split(line):
        if not part:
            continue
        if part[0] == "\x1b":
            m = SGR.fullmatch(part)
            if m:
                fg, bg, rev = apply_sgr(m.group(1), fg, bg, rev)
            continue
        f, b = ((bg or tint or DEFAULT_BG), (fg or DEFAULT_FG)) if rev else (fg, bg or tint)
        for ch in part:
            w = 1 if ch < "̀" else cell_width(ch)
            if w:
                out.extend([(not ch.isspace(), f, b)] * w)
        if len(out) >= width:
            break
    return (out + [(False, None, tint)] * width)[:width]


def compose(win, caps):
    """The window as one grid of cells, each pane at its real position."""
    grid = [[BORDER] * win["ww"] for _ in range(win["wh"])]
    for p in win["panes"]:
        lines = caps.get(p["pane"], [])
        for j in range(min(p["h"], win["wh"] - p["y"])):
            grid[p["y"] + j][p["x"]:p["x"] + p["w"]] = cells(lines[j] if j < len(lines) else "", p["w"], p["tint"])
    for row in grid:                            # a pane at the right edge can overhang by a column
        del row[win["ww"]:]
    return grid


def spans(n_out, n_in, per):
    """Source range [a, b) behind each of n_out samples, `per` samples per source unit."""
    out = []
    for i in range(n_out):
        a = int(i / per)
        out.append((a, min(n_in, max(a + 1, int((i + 1) / per)))) if a < n_in else None)
    return out


def shrink(grid, iw, ih):
    """A cell grid as iw x ih braille cells (char, fg, bg): dot = any ink, color = most common ink
    color, background = the region's majority background. Also returns the mapping (scale, ox, oy)."""
    blank = [[(" ", None, None)] * iw for _ in range(ih)]
    H, W = len(grid), len(grid[0]) if grid else 0
    if not W or not H:
        return blank, (0, 0, 0)
    s = min(2 * iw / W, 4 * ih / (2 * H))      # dots per source column; a source row is two columns tall
    uw, uh = min(iw, -(-int(W * s) // 2)), min(ih, -(-int(2 * H * s) // 4))
    xs, ys = spans(2 * uw, W, s), spans(4 * uh, H, 2 * s)
    ox, oy = (iw - uw) // 2, (ih - uh) // 2
    for cy in range(uh):
        out = blank[oy + cy]
        for cx in range(uw):
            bits, n, fgs, bgs = 0, 0, {}, {}
            for dy in range(4):
                yr = ys[cy * 4 + dy]
                if yr is None:
                    continue
                for dx in range(2):
                    xr = xs[cx * 2 + dx]
                    if xr is None:
                        continue
                    for y in range(*yr):
                        row = grid[y]
                        for x in range(*xr):
                            ink, f, b = row[x]
                            n += 1
                            if b is not None:
                                bgs[b] = bgs.get(b, 0) + 1
                            if ink:
                                bits |= BRAILLE[dy][dx]
                                fgs[f] = fgs.get(f, 0) + 1
            fg = max(fgs, key=fgs.get) if fgs else None
            bg = max(bgs, key=bgs.get) if bgs else None
            if bg is not None and 2 * bgs[bg] < n:
                bg = None
            out[ox + cx] = (chr(0x2800 + bits) if bits else " ", fg or DEFAULT_FG, bg)
    return blank, (s, ox, oy)


def label_panes(rows, win, mapping):
    """Write each pane's harness chip and label over its title row, readable on top of the dots."""
    s, ox, oy = mapping
    iw = len(rows[0]) if rows else 0
    for p in win["panes"]:
        if not (p["harness"] or p["label"]) or not s:
            continue
        x0 = ox + int(p["x"] * s / 2)
        width = min(iw, ox + int((p["x"] + p["w"]) * s / 2)) - x0
        y = oy + int(2 * max(0, p["y"] - 1) * s / 4)
        if width < 2 or not 0 <= y < len(rows):
            continue
        text = []
        if p["harness"]:
            name = p["harness"] if width >= len(p["harness"]) + 8 else p["harness"][:1]
            text += [(ch, (17, 17, 17), p["hcolor"] or (68, 68, 68)) for ch in f" {name} "]
        for ch in " " + p["label"]:
            w = cell_width(ch)
            if w:
                text += [(ch, LABEL_FG, p["tint"] or LABEL_BG)] + [("", None, None)] * (w - 1)
        text = text[:width]
        if text and text[-1][0] == "" and len(text) > 1 and cell_width(text[-2][0]) == 2:
            text[-2] = (" ",) + text[-2][1:]            # never cut a wide character in half
        rows[y][x0:x0 + len(text)] = text


def frame(segs, W, H):
    """Positioned pieces {row: [(col, width, text)]} as exactly H full-row strings. Gaps are spaces
    and each row ends by erasing to the right, so writing a row replaces it completely: the main
    loop rewrites only rows that changed and never clears the screen (clearing is what flickers)."""
    rows = []
    for y in range(1, H + 1):
        out, col = [], 1
        for x, width, text in sorted(segs.get(y, ()), key=lambda s: s[0]):
            if x > col:
                out.append("\x1b[0m" + " " * (x - col))
            out.append(text + "\x1b[0m")
            col = x + width
        rows.append("".join(out) + "\x1b[0m\x1b[K")
    return rows


def paint(rows):
    out = []
    for row in rows:
        parts, last = [], None
        for ch, fg, bg in row:
            if ch == "":
                continue                                # second half of a wide character
            sgr = rgb_sgr(fg or DEFAULT_FG, bg)
            if sgr != last:
                parts.append(sgr)
                last = sgr
            parts.append(ch)
        out.append("".join(parts) + "\x1b[0m")
    return out


class Deck:
    def __init__(self):
        self.sel = 0
        self.top = 0       # first content row on screen (the grid scrolls when it can't fit)
        self.pos = {}      # window position in self.windows -> (content row, column)
        self.windows = []
        self.spot = False      # spotlight view (Space)
        self.typing = False    # keys go to the spotlight pane (i ... Ctrl-])
        self.pane_pick = None  # (window index, pane number) chosen with [ ]
        self.mini = os.environ.get("DECK_MODE", "mini") != "tail"
        self.thumbs = {}   # window index -> (inputs, rendered rows); idle windows are not re-rendered
        wins = list_windows()
        self.sel = next((i for i, w in enumerate(wins) if w["active"]), 0)

    # ---- layout -------------------------------------------------------
    def layout(self, W, H):
        """Sections in status-row order, cards flowing under each; picks the column count that
        gives the biggest thumbnails. Returns (card width, card height, items, content height)."""
        groups = []
        for i, w in enumerate(self.windows):
            if not groups or groups[-1][0] != w["group"]:
                groups.append((w["group"], []))
            groups[-1][1].append(i)
        headed = any(g for g, _ in groups)
        heads = len(groups) if headed else 0
        ww = max(w["ww"] for w in self.windows)
        wh = max(w["wh"] for w in self.windows)
        best = None
        for cols in range(1, max(1, W // 24) + 1):
            card_rows = sum(-(-len(ix) // cols) for _, ix in groups)
            cw, ch = W // cols, (H - 1 - heads) // card_rows
            if ch >= 5 and cw >= 24:
                scale = min(2 * (cw - 2) / ww, 2 * (ch - 2) / wh)
                if best is None or scale > best[0]:
                    best = (scale, cols, cw, ch)
        _, cols, cw, ch = best or (0, max(1, W // 30), W // max(1, W // 30), 7)
        items, y = [], 1
        for g, ix in groups:
            if headed:
                items.append(("head", y, g))
                y += 1
            for k, i in enumerate(ix):
                r, c = divmod(k, cols)
                items.append(("card", y + r * ch, 1 + c * cw, i))
            y += -(-len(ix) // cols) * ch
        return cw, ch, items, y

    # ---- drawing ------------------------------------------------------
    def thumb(self, win, caps, iw, ih):
        key = (iw, ih, win["ww"], win["wh"],
               tuple((p["x"], p["y"], p["w"], p["h"], p["tint"], p["harness"], p["hcolor"], p["label"],
                      "\n".join(caps.get(p["pane"], []))) for p in win["panes"]))
        hit = self.thumbs.get(win["index"])
        if hit and hit[0] == key:
            return hit[1]
        rows, mapping = shrink(compose(win, caps), iw, ih)
        label_panes(rows, win, mapping)
        rows = paint(rows)
        self.thumbs[win["index"]] = (key, rows)
        return rows

    def header(self, group, W):
        name, fg, bg = SECTIONS.get(group, ((group or "other").upper(), (229, 231, 235), (55, 65, 81)))
        return f"{rgb_sgr(fg, bg, True)} {name} \x1b[0m\x1b[38;2;{bg[0]};{bg[1]};{bg[2]}m{'─' * (W - len(name) - 3)}\x1b[0m"

    def refresh(self):
        self.windows = list_windows()
        self.sel = min(self.sel, max(0, len(self.windows) - 1))
        return bool(self.windows)

    def place(self, W, H, x0):
        """Lay the cards out in a W-wide area after column x0, scrolled so the chosen card shows.
        Returns (card width, card height, visible items, whether some cards are off screen)."""
        cw, ch, items, total = self.layout(W, H)
        items = [it if it[0] == "head" else ("card", it[1], it[2] + x0, it[3]) for it in items]
        self.pos = {it[3]: (it[1], it[2]) for it in items if it[0] == "card"}
        sel_y = self.pos[self.sel][0]
        self.top = max(0, min(self.top, sel_y - 2), sel_y + ch - H)   # keep the chosen card on screen
        visible = [it for it in items if 1 <= it[1] - self.top and it[1] - self.top + (ch if it[0] == "card" else 1) <= H]
        return cw, ch, visible, total - 1 > H - 1

    def card_panes(self, visible):
        shown = [self.windows[it[3]] for it in visible if it[0] == "card"]
        return [p["pane"] for w in shown for p in w["panes"]] if self.mini else [w["pane"] for w in shown]

    def draw_cards(self, put, visible, cw, ch, caps, W, x0):
        for it in visible:
            y = it[1] - self.top
            if it[0] == "head":
                put(y, x0 + 1, W - 1, self.header(it[2], W))
                continue
            x, win = it[2], self.windows[it[3]]
            chosen = it[3] == self.sel
            color = STATE_COLOR.get(win["state"], "38;5;244")
            edge = f"\x1b[{'1;' if chosen else ''}{color}m" if (chosen or win["state"]) else "\x1b[38;5;238m"
            h, v, tl, tr, bl, br = ("━", "┃", "┏", "┓", "┗", "┛") if chosen else ("─", "│", "╭", "╮", "╰", "╯")
            inner = cw - 2
            label = f" {win['index']}{win['mark']}:{win['name']} "
            used = min(len(label), inner - 2)
            chip = crop(self.tab_sgr(win) + label, used)
            put(y, x, cw, f"{edge}{tl}{h}\x1b[0m{chip}\x1b[0m{edge}{h * (inner - 1 - used)}{tr}")
            if self.mini:
                body = self.thumb(win, caps, inner, ch - 2)
            else:
                lines = tail(caps.get(win["pane"], []), ch - 2)
                body = [tinted(crop(lines[j], inner) if j < len(lines) else " " * inner, win["tint"])
                        for j in range(ch - 2)]
            for j in range(ch - 2):
                put(y + 1 + j, x, cw, f"{edge}{v}\x1b[0m{body[j]}\x1b[0m{edge}{v}")
            put(y + ch - 1, x, cw, f"{edge}{bl}{h * inner}{br}")

    def tab_sgr(self, win):
        fg, bg, bold = win["tab"]
        return rgb_sgr(fg, bg or (48, 52, 59), bold)

    def draw_grid(self):
        """The grid as one string per screen row (see frame())."""
        W, H = shutil.get_terminal_size()
        if not self.refresh():
            return frame({1: [(1, 10, "no windows")]}, W, H), None
        cw, ch, visible, more = self.place(W, H, 0)
        ids = self.card_panes(visible)
        caps = dict(zip(ids, capture_all(ids)))
        segs = {}

        def put(y, x, width, text):
            segs.setdefault(y, []).append((x, width, text))

        self.draw_cards(put, visible, cw, ch, caps, W, 0)
        mode = "mini" if self.mini else "tail"
        hint = f" ←↑↓→ move · space spotlight · tab next alert · z {mode} · enter go · q close{'  ↕ more' if more else ''} "[:W]
        put(H, 1, len(hint), f"\x1b[38;5;244m{hint}")
        return frame(segs, W, H), None

    def target(self):
        """The pane the spotlight shows and typing goes to: the window's active pane unless [ ] picked another."""
        win = self.windows[self.sel]
        if self.pane_pick and self.pane_pick[0] == win["index"]:
            return win["panes"][self.pane_pick[1] % len(win["panes"])]
        return next(p for p in win["panes"] if p["pane"] == win["pane"])

    def cycle_pane(self, step):
        win = self.windows[self.sel]
        self.pane_pick = (win["index"], (win["panes"].index(self.target()) + step) % len(win["panes"]))

    def draw_spot(self):
        """Spotlight: the chosen pane big and readable on the left, every card small on the right.
        Returns the rows and, while typing, where the pane's cursor goes."""
        W, H = shutil.get_terminal_size()
        if not self.refresh():
            return frame({1: [(1, 10, "no windows")]}, W, H), None
        L = max(40, min(W - 28, W * 3 // 5))
        win, pane = self.windows[self.sel], self.target()
        side = W - L - 1
        if side >= 26:
            cw, ch, visible, more = self.place(side, H, L + 1)
        else:
            visible, more, self.pos = [], False, {}
        ids = self.card_panes(visible) + [pane["pane"]]
        caps = dict(zip(ids, capture_all(ids)))
        segs = {}

        def put(y, x, width, text):
            segs.setdefault(y, []).append((x, width, text))

        if visible:
            self.draw_cards(put, visible, cw, ch, caps, side, L + 1)
        for y in range(1, H):
            put(y, L + 1, 1, "\x1b[38;5;238m│")
        n = len(win["panes"])
        if self.typing:
            head = f"\x1b[1;7;{STATE_COLOR['needs']}m TYPING → {win['index']}:{win['name']} · every key goes to this pane · Ctrl-] stop typing "
            hint = " TYPING into the spotlight pane · Ctrl-] stop typing "
        else:
            who = (rgb_sgr((17, 17, 17), pane["hcolor"] or (68, 68, 68), True) + f" {pane['harness']} " if pane["harness"] else "")
            which = f" pane {win['panes'].index(pane) + 1}/{n}" if n > 1 else ""
            head = (f"{self.tab_sgr(win)} {win['index']}{win['mark']}:{win['name']} \x1b[0m {who}\x1b[0m"
                    f"\x1b[1m {pane['label']}\x1b[0;38;5;244m{which} · i type into it")
            hint = (f" ←↑↓→/tab pick agent{' · [ ] pane' if n > 1 else ''} · i type · space/esc back to grid"
                    f" · z {'mini' if self.mini else 'tail'} · enter go · q close{'  ↕ more' if more else ''} ")
        put(1, 1, L, crop(head, L))
        lines = caps.get(pane["pane"], [])[: pane["h"]]
        body_h = H - 2
        top = max(0, min(pane["cy"] - body_h // 2, len(lines) - body_h))   # keep the pane's cursor row on screen
        for j in range(body_h):
            put(2 + j, 1, L, tinted(crop(lines[top + j] if top + j < len(lines) else "", L), pane["tint"]))
        put(H, 1, min(W, len(hint)), f"\x1b[38;5;244m{hint[:W]}")
        cy, cx = pane["cy"] - top + 2, pane["cx"] + 1
        return frame(segs, W, H), ((cy, cx) if self.typing and 2 <= cy < H and cx <= L else None)

    # ---- input --------------------------------------------------------
    def move(self, key):
        n = len(self.windows) or 1
        if key in ("left", "right"):
            self.sel = max(0, min(n - 1, self.sel + (1 if key == "right" else -1)))
            return
        if self.sel not in self.pos:                    # no card grid on screen: step through the list
            self.sel = max(0, min(n - 1, self.sel + (1 if key == "down" else -1)))
            return
        y0, x0 = self.pos[self.sel]
        rows = [y for y, _ in self.pos.values() if (y < y0 if key == "up" else y > y0)]
        if rows:
            ty = max(rows) if key == "up" else min(rows)
            self.sel = min((abs(x - x0), i) for i, (y, x) in self.pos.items() if y == ty)[1]

    def next_alert(self):
        n = len(self.windows)
        for off in range(1, n + 1):
            i = (self.sel + off) % n
            if self.windows[i]["state"] in ("needs", "done"):
                self.sel = i
                return

    def forward(self, data):
        pane = self.target()["pane"]
        specials = {"\r": "Enter", "\x7f": "BSpace", "\t": "Tab", "\x1b": "Escape",
                    "\x1b[A": "Up", "\x1b[B": "Down", "\x1b[C": "Right", "\x1b[D": "Left",
                    "\x1b[Z": "BTab", "\x1b[H": "Home", "\x1b[F": "End", "\x1b[3~": "DC"}
        run = []
        for tok in tokens(data) + [None]:
            if tok is not None and len(tok) == 1 and ord(tok) >= 32 and tok != "\x7f":
                run.append(tok)
                continue
            if run:
                tmux("send-keys", "-t", pane, "-l", "".join(run))
                run = []
            if tok is None:
                break
            if tok in specials:
                tmux("send-keys", "-t", pane, specials[tok])
            elif len(tok) == 1 and ord(tok) < 32:
                tmux("send-keys", "-t", pane, "C-" + chr(ord(tok) + 96))
            else:
                tmux("send-keys", "-t", pane, "-l", tok)


def tokens(data):
    """Split raw input into escape sequences and runs of plain text."""
    out, i = [], 0
    while i < len(data):
        if data[i] == "\x1b":
            m = re.match(r"\x1b(\[[0-9;]*[A-Za-z~]|O[A-Za-z])?", data[i:])
            out.append(m.group())
            i += len(m.group())
        elif ord(data[i]) < 32 or data[i] == "\x7f":
            out.append(data[i])
            i += 1
        else:
            j = i
            while j < len(data) and ord(data[j]) >= 32 and data[j] not in "\x7f\x1b":
                j += 1
            out.extend(data[i:j])
            i = j
    return out


KEYS = {"\x1b[A": "up", "\x1b[B": "down", "\x1b[C": "right", "\x1b[D": "left",
        "k": "up", "j": "down", "l": "right", "h": "left"}


def main():
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    out = sys.stdout
    deck = Deck()
    out.write("\x1b[?1049h\x1b[?25l")
    try:
        tty.setraw(fd)
        last = 0.0
        shown, shown_cursor, shown_key = [], None, None   # what is on screen right now
        while True:
            interval = 0.1 if deck.typing else 0.25 if deck.spot else 0.5
            if time.monotonic() - last >= interval:
                rows, cursor = deck.draw_spot() if deck.spot else deck.draw_grid()
                key = (deck.spot, shutil.get_terminal_size())
                if key != shown_key:                    # new view or new size: start from a blank screen
                    out.write("\x1b[?25l\x1b[H\x1b[2J")
                    shown, shown_cursor, shown_key = [], None, key
                changed = [i for i, r in enumerate(rows) if i >= len(shown) or shown[i] != r]
                if changed or cursor != shown_cursor:
                    parts = ["\x1b[?25l"] + [f"\x1b[{i + 1};1H{rows[i]}" for i in changed]
                    if cursor:
                        parts.append(f"\x1b[{cursor[0]};{cursor[1]}H\x1b[?25h")
                    out.write("".join(parts))
                    out.flush()
                shown, shown_cursor = rows, cursor
                last = time.monotonic()
            ready, _, _ = select.select([fd], [], [], interval)
            if not ready:
                continue
            data = os.read(fd, 4096).decode(errors="replace")
            last = 0.0
            pending = []
            for tok in tokens(data):
                if deck.typing:
                    if tok != "\x1d":
                        pending.append(tok)
                        continue
                    deck.forward("".join(pending))
                    pending = []
                    deck.typing = False
                elif tok in ("q", "\x03"):                  # not Esc: too easy to hit by accident
                    return
                elif tok in KEYS:
                    deck.move(KEYS[tok])
                elif tok == "\t":
                    deck.next_alert()
                elif tok == "z":
                    deck.mini = not deck.mini
                elif tok == " ":
                    deck.spot = not deck.spot
                elif tok == "\x1b":                          # Esc only ever backs out
                    deck.spot = False
                elif tok in ("[", "]") and deck.spot and deck.windows:
                    deck.cycle_pane(1 if tok == "]" else -1)
                elif tok == "i" and deck.windows:
                    deck.spot = deck.typing = True
                elif tok == "\r" and deck.windows:
                    pane = deck.target()["pane"]
                    tmux("select-window", "-t", pane, ";", "select-pane", "-t", pane)
                    return
            if pending:
                deck.forward("".join(pending))
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        out.write("\x1b[?25h\x1b[?1049l")
        out.flush()


if __name__ == "__main__":
    main()
