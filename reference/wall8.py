"""Quick Look wall v5: flat, square, calm. Writes <out>_grid.png and <out>.png (preview open).
  python3 wall5.py out.png [window index to preview]
"""
import re, sys
from PIL import Image, ImageDraw, ImageFont
from wallcore import *          # wins, caps, deck, native, facts, plain, tint, env, oklch, STATE, TYPE, HARNESS, fonts, ago...

SUB = re.compile(r"^\s*[◯○●◉◎]\s+(\S+)\s{2,}(.*?)\s{2,}(\S.*?)\s*$")
EDGE, HAIR, PANEL_BG = (60, 64, 74), (40, 43, 51), (24, 26, 31)
TH, ROW = 22, 19


def soft(color, k=0.72):
    """Chip colors: a dark fill tinted by the color, and the color itself lifted for the text."""
    fill = tuple(int(c * (1 - k) + b * k) for c, b in zip(color, (18, 19, 23)))
    ink = tuple(min(255, int(c * 0.55 + 255 * 0.45)) for c in color)
    return fill, ink


def chip(d, x, y, text, color, f=chipf, raw=None):
    fill, ink = raw or soft(color)
    w = int(d.textlength(text, font=f)) + 10
    d.rectangle([x, y, x + w, y + 15], fill=fill)
    d.text((x + 5, y + 1), text, font=f, fill=ink)
    return x + w + 4


def subagents(p):
    """[(name, status, busy)] from the agent list Claude draws under its prompt, plus a count of hidden ones."""
    lines = [plain(l).rstrip() for l in caps.get(p["pane"], [])]
    found = [(m.group(1), m.group(3)) for l in lines if (m := SUB.match(l))]
    hidden = sum(int(a or b) for a, b in re.findall(r"(\d+) idle agents|↓ (\d+) more", "\n".join(lines)))
    out = [(n, s, s != "idle") for n, s in found]
    return sorted(out, key=lambda t: not t[2]), hidden


def short_status(s):
    m = re.match(r"((?:\d+[hms]\s*)+)", s)
    return m.group(1).strip() if m else s.split("·")[0].strip()


def title_chips(d, win, x, y):
    tf, tb, _ = win["tab"]
    x = chip(d, x, y, f"{win['index']}{win['mark']} {win['name']}", (0, 0, 0),
             raw=(tb or (52, 56, 64), tf or (232, 232, 232)))                 # the real status tab, as drawn in tmux
    words = [(h.lower(), HARNESS.get(h, (150, 156, 166))) for h in dict.fromkeys(p["harness"] for p in win["panes"] if p["harness"])]
    words += [("pwsh" if e == "ps" else "wsl", (110, 170, 240) if e == "ps" else (240, 130, 90))
              for e in dict.fromkeys(env(p) for p in win["panes"] if env(p))]
    for word, color in words:
        d.text((x + 3, y + 1), word, font=capf, fill=color)
        x += d.textlength(word, font=capf) + 10
    return x


def fit(d, text, room, f):
    while text and d.textlength(text, font=f) > room:
        text = text[:-2]
    return text


def card(img, d, win, x, y, w, h, chosen):
    st = STATE.get(win["state"])
    group = TYPE.get(win["group"], ("", 0))
    src = native(win)
    TH = 0                                                       # no title strip: the picture is the whole card
    body_h = h
    s = max(w / src.width, body_h / src.height)
    thumb = src.resize((max(1, round(src.width * s)), max(1, round(src.height * s))), Image.LANCZOS)
    left, top = (thumb.width - w) // 2, thumb.height - body_h      # trim the top, keep the bottom
    img.paste(thumb.crop((left, top, left + w, top + body_h)), (x, y))
    # title, on the picture: the real status tab, then quiet words on a dark strip tinted by the section
    strip = oklch(0.17, 0.03, group[1]) if group[0] else (12, 13, 16)
    tf, tb, _ = win["tab"]
    tab = f"{win['index']}{win['mark']} {win['name']}"
    tw = d.textlength(tab, font=chipf) + 10
    words = [(h_.lower(), HARNESS.get(h_, (150, 156, 166))) for h_ in dict.fromkeys(p["harness"] for p in win["panes"] if p["harness"])]
    words += [("pwsh" if e == "ps" else "wsl", (110, 170, 240) if e == "ps" else (240, 130, 90))
              for e in dict.fromkeys(env(p) for p in win["panes"] if env(p))]
    ww_ = sum(d.textlength(t, font=capf) + 10 for t, _ in words)
    d.rectangle([x + 2, y + 2, x + 2 + tw + ww_ + 4, y + 19], fill=strip)
    d.rectangle([x + 2, y + 2, x + 2 + tw, y + 19], fill=tb or (52, 56, 64))
    d.text((x + 7, y + 4), tab, font=chipf, fill=tf or (232, 232, 232))
    cx = x + 2 + tw + 7
    for t, col in words:
        d.text((cx, y + 4), t, font=capf, fill=col)
        cx += d.textlength(t, font=capf) + 10
    label = ago(win["age"])
    lw = d.textlength(label, font=chipf)
    tab_bg = win["tab"][1]
    age_ink = tab_bg if tab_bg and tab_bg != (48, 52, 59) and sum(tab_bg) > 200 else (160, 166, 178)
    d.rectangle([x + w - lw - 30, y + 2, x + w - 3, y + 19], fill=strip)
    d.rectangle([x + w - lw - 22, y + 8, x + w - lw - 16, y + 14], fill=age_ink)
    d.text((x + w - lw - 9, y + 4), label, font=chipf, fill=age_ink)
    # the two text lines sit on top of the picture's bottom edge, like subtitles
    active = next(p for p in win["panes"] if p["pane"] == win["pane"])
    kind, cap, *_ = facts(active)
    now = []
    if kind == "tool":
        tool, cap = cap.split("(", 1)
        now = [(tool, capb, (229, 192, 123)), (cap.rstrip(")"), capf, (226, 230, 238))]
    elif cap:
        now = [(cap, capf, (130, 205, 245) if kind == "spin" else (226, 230, 238))]
    subs, hidden = [], 0
    for p in win["panes"]:
        sp, hp = subagents(p)
        subs += sp
        hidden += hp
    sub = []
    if subs or hidden:
        sub = [(f"⤷ {len(subs) + hidden}", capb, (186, 162, 245))]
        sub += [(f"{n} {short_status(st_)}" if busy else n, capb if busy else capf,
                 (236, 238, 244) if busy else (150, 156, 168)) for n, st_, busy in subs]
        if hidden:
            sub.append((f"+{hidden}", capf, (150, 156, 168)))
    lines = [l for l in (now, sub) if l]
    layer = Image.new("RGBA", (w, body_h), (0, 0, 0, 0))
    ld = ImageDraw.Draw(layer)
    yy = body_h - 4 - ROW * len(lines)
    for runs in lines:
        xx, placed = 6, []
        for text, f, col in runs:
            text = fit(ld, text, w - 12 - xx, f)
            if not text:
                break
            placed.append((xx, text, f, col))
            xx += ld.textlength(text, font=f) + 9
        ld.rectangle([2, yy, min(w - 3, xx - 2), yy + ROW - 2], fill=(10, 11, 14, 222))
        for px_, text, f, col in placed:
            ld.text((px_, yy + 2), text, font=f, fill=col)
        yy += ROW
    region = img.crop((x, y, x + w, y + h)).convert("RGBA")
    img.paste(Image.alpha_composite(region, layer).convert("RGB"), (x, y))
    # edges: hairline normally, state color when there is a state, white when chosen
    d.rectangle([x, y, x + w - 1, y + h - 1], outline=HAIR, width=1)
    if st:
        d.rectangle([x, y, x + w - 1, y + h - 1], outline=st, width=2)
    if chosen:
        d.rectangle([x - 3, y - 3, x + w + 2, y + h + 2], outline=(240, 243, 248), width=2)


# ---- grid -----------------------------------------------------------------------------------
GAP, DOCK = 8, 30
n = len(wins)
best = None
for cols in range(1, n + 1):
    rows = -(-n // cols)
    cw, ch = (SW - GAP * (cols + 1)) // cols, (SH - DOCK - GAP * (rows + 1)) // rows
    sc = min(cw / (255 * CW), ch / (60 * CH))
    if best is None or sc > best[0]:
        best = (sc, cols, rows, cw, ch)
_, cols, rows, cw, ch = best
sel = next(k for k, w in enumerate(wins) if w["index"] == pick)
img = Image.new("RGB", (SW, SH), BG)
d = ImageDraw.Draw(img)
for k, win in enumerate(wins):
    card(img, d, win, GAP + (k % cols) * (cw + GAP), GAP + (k // cols) * (ch + GAP), cw, ch, k == sel)
# dock: its own row at the bottom, never over a card
dy = SH - DOCK
d.rectangle([0, dy, SW, SH], fill=(20, 21, 25))
d.line([0, dy, SW, dy], fill=HAIR)
counts = {s: sum(1 for w in wins if w["state"] == s) for s in ("needs", "working", "done")}
x = 14
for s, label in (("needs", "need you"), ("working", "working"), ("done", "done")):
    d.rectangle([x, dy + 11, x + 8, dy + 19], fill=STATE[s])
    t = f"{counts[s]} {label}"
    d.text((x + 14, dy + 7), t, font=dockf, fill=(222, 226, 234))
    x += d.textlength(t, font=dockf) + 34
keys = "←↑↓→ move   space preview   tab next alert   enter go   q close"
d.text((SW - d.textlength(keys, font=dockf) - 14, dy + 7), keys, font=dockf, fill=(132, 138, 150))
img.save(out.replace(".png", "_grid.png"))

# ---- Quick Look: floats over the untouched grid ---------------------------------------------
win = wins[sel]
src = native(win)
HEAD = 30
pw, ph = src.width + 16, src.height + HEAD + 8
px, py = (SW - pw) // 2, (SH - DOCK - ph) // 2
d.rectangle([px, py, px + pw - 1, py + ph - 1], fill=PANEL_BG)
d.rectangle([px, py, px + pw - 1, py + HEAD - 1], fill=(32, 34, 41))
img.paste(src, (px + 8, py + HEAD))
st = STATE.get(win["state"])
d.rectangle([px - 1, py - 1, px + pw, py + ph], outline=(12, 12, 14), width=1)
d.rectangle([px, py, px + pw - 1, py + ph - 1], outline=st or EDGE, width=2)
cx = title_chips(d, win, px + 10, py + 7)
active = next(p for p in win["panes"] if p["pane"] == win["pane"])
_, _, model, ctx, _, _ = facts(active)
subs = [s for p in win["panes"] for s in subagents(p)[0]]
hidden = sum(subagents(p)[1] for p in win["panes"])
busy = sum(1 for s in subs if s[2])
total = len(subs) + hidden
d.line([cx + 4, py + 7, cx + 4, py + 22], fill=EDGE)
cx += 12
facts_line = "  ·  ".join(t for t in [model, f"{ctx} context" if ctx else "",
                                      f"{total} subagent{'s' if total != 1 else ''}, {busy} busy" if total else "",
                                      f"active {ago(win['age'])} ago"] if t)
d.text((cx, py + 7), facts_line, font=dockf, fill=(196, 200, 210))
hint = "i type   ←→ next   enter go there   space close"
d.text((px + pw - 12 - d.textlength(hint, font=dockf), py + 7), hint, font=dockf, fill=(132, 138, 150))
img.save(out)
print(f"grid {cols}x{rows}; card {cw}x{ch}; states {counts}")
