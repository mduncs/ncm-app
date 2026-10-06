"""Quick Look wall v5: flat, square, calm. Writes <out>_grid.png and <out>.png (preview open).
  python3 wall5.py out.png [window index to preview]
"""
import re, sys
from PIL import Image, ImageDraw, ImageFont
from wallcore import *
import talkdata, textwrap, time as _t
SESSION = re.compile(r"Session ID:\s*([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})")
_ev = {}
def session_of(p):
    m = SESSION.search(" ".join(plain(l) for l in caps.get(p["pane"], [])))
    return m.group(1) if m else None
def events_of(p):
    sid = session_of(p)
    if sid and sid not in _ev:
        _ev[sid] = talkdata.load(sid) or []
    return _ev.get(sid, []) if sid else []
def waiting(win):
    """Longest-waiting unanswered question across the window's agents, in seconds."""
    ages = [talkdata.unanswered(events_of(p)) for p in win["panes"] if p["harness"]]
    ages = [a for a in ages if a is not None]
    return max(ages) if ages else None          # wins, caps, deck, native, facts, plain, tint, env, oklch, STATE, TYPE, HARNESS, fonts, ago...

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
    wait = waiting(win)
    if wait is not None and wait > 20:                            # you asked; nothing has answered yet
        flag = f"unanswered {ago(wait)}"
        fw = d.textlength(flag, font=chipf) + 12
        d.rectangle([cx, y + 2, cx + fw, y + 19], fill=STATE["needs"])
        d.text((cx + 6, y + 4), flag, font=chipf, fill=(255, 255, 255))
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

# ---- Quick Look on the Talk tab -------------------------------------------------------------
win = wins[sel]
agent = next((p for p in win["panes"] if session_of(p) == "07b4b194-9932-4356-b92f-bd54525bc362"), None) or \
        next(p for p in win["panes"] if p["pane"] == win["pane"])
ev = events_of(agent)
src_w, src_h = 255 * CW, 60 * CH
HEAD = 30
pw, ph = src_w + 16, src_h + HEAD + 8
px, py = (SW - pw) // 2, (SH - DOCK - ph) // 2
d.rectangle([px, py, px + pw - 1, py + ph - 1], fill=(22, 24, 29))
d.rectangle([px, py, px + pw - 1, py + HEAD - 1], fill=(32, 34, 41))
# header: status tab + harness, then the three views, then the facts
tf, tb, _ = win["tab"]
tab = f"{win['index']}{win['mark']} {win['name']}"
tw = d.textlength(tab, font=chipf) + 10
d.rectangle([px + 8, py + 7, px + 8 + tw, py + 23], fill=tb or (52, 56, 64))
d.text((px + 13, py + 8), tab, font=chipf, fill=tf or (232, 232, 232))
cx = px + 8 + tw + 10
d.text((cx, py + 8), "claude", font=capf, fill=HARNESS["CLAUDE"]); cx += d.textlength("claude", font=capf) + 22
n_reports = sum(1 for e in ev if e[0] == "report")
for name, active in (("Screen", False), ("Talk", True), (f"Reports {n_reports}", False)):
    wdt = d.textlength(name, font=dockf)
    if active:
        d.rectangle([cx - 8, py + 4, cx + wdt + 8, py + HEAD - 1], fill=(22, 24, 29))
        d.line([cx - 8, py + 4, cx + wdt + 8, py + 4], fill=(236, 238, 244), width=2)
    d.text((cx, py + 7), name, font=dockf, fill=(236, 238, 244) if active else (130, 136, 148))
    cx += wdt + 26
_, _, model, ctx, _, _ = facts(agent)
facts_line = "  ·  ".join(t for t in [model, f"{ctx} context" if ctx else "", f"active {ago(win['age'])} ago"] if t)
d.text((px + pw - 12 - d.textlength(facts_line, font=dockf), py + 7), facts_line, font=dockf, fill=(170, 176, 188))
# talk column: you and the replies to you; reports and replies-to-reports collapse into one quiet line
TALK_W = 1330
tx, ty0, ty1 = px + 18, py + HEAD + 12, py + ph - 14
cols_chars = (TALK_W - 120) // CW
blocks, skipped = [], {"report": 0, "busywork": 0}
for e in ev:
    if e[0] in ("report", "busywork"):
        skipped[e[0]] += 1
        continue
    if skipped["report"] or skipped["busywork"]:
        blocks.append(("gap", dict(skipped)))
        skipped = {"report": 0, "busywork": 0}
    blocks.append((e[0], e))
if skipped["report"] or skipped["busywork"]:
    blocks.append(("gap", dict(skipped)))
def wrap(text, limit):
    out = []
    for para in text.split("\n"):
        out += textwrap.wrap(para, cols_chars) or [""]
    if len(out) > limit:
        out = out[:limit - 1] + [f"… {len(out) - limit + 1} more lines (enter opens the full reply)"]
    return out
rendered = []
for kind, e in blocks:
    if kind == "gap":
        bits = []
        if e["report"]:
            bits.append(f"{e['report']} report{'s' if e['report'] != 1 else ''}")
        if e["busywork"]:
            bits.append(f"{e['busywork']} repl{'ies' if e['busywork'] != 1 else 'y'} to them")
        rendered.append([("gap", "   ·· " + " and ".join(bits) + " (Reports tab) ··")])
    elif kind == "you":
        rendered.append([("whohead", f"you · {ago(_t.time() - (e[1] or _t.time()))} ago")] + [("you", l) for l in wrap(e[2], 6)])
    else:
        rendered.append([("whohead", f"{win['name']} · {ago(_t.time() - (e[1] or _t.time()))} ago")] + [("reply", l) for l in wrap(e[2], 9)])
lines = []
for blk in rendered:
    lines += blk + [("blank", "")]
rows_fit = (ty1 - ty0) // CH
lines = lines[-rows_fit:]
last_you = max((i for i, (k, _) in enumerate(lines) if k == "whohead" and _.startswith("you")), default=None)
yy = ty0
for i, (k, t) in enumerate(lines):
    if k == "you":
        d.rectangle([tx - 10, yy, tx - 7, yy + CH - 1], fill=(255, 120, 170))
        d.text((tx, yy + 1), t, font=mono, fill=(250, 214, 228))
    elif k == "reply":
        d.text((tx, yy + 1), t, font=mono, fill=(214, 218, 226))
    elif k == "whohead":
        col = (255, 120, 170) if t.startswith("you") else HARNESS["CLAUDE"]
        d.text((tx, yy + 1), t, font=capb, fill=col)
    elif k == "gap":
        d.text((tx, yy + 1), t, font=capf, fill=(110, 116, 128))
    yy += CH
wait = talkdata.unanswered(ev)
if wait is not None:
    msg = f"▲ no reply to your last message yet ({ago(wait)}). Replies to reports since then: {sum(1 for e in ev[[i for i, e in enumerate(ev) if e[0] == 'you'][-1]:] if e[0] == 'busywork')}"
    d.rectangle([tx - 10, ty1 - 20, tx + d.textlength(msg, font=capb) + 10, ty1], fill=STATE["needs"])
    d.text((tx, ty1 - 18), msg, font=capb, fill=(255, 255, 255))
# reports column: one row per agent, newest first
rx = px + 16 + TALK_W
d.line([rx, py + HEAD + 8, rx, py + ph - 10], fill=HAIR)
rx += 16
d.text((rx, ty0), "reports, by agent", font=capb, fill=(176, 150, 240))
latest = {}
for e in ev:
    if e[0] == "report":
        cnt = latest.get(e[2], (0,))[0]
        latest[e[2]] = (cnt + 1, e[1], e[3])
yy = ty0 + 26
room = pw - (rx - px) - 20
for name, (cnt, t, body) in sorted(latest.items(), key=lambda kv: -(kv[1][1] or 0)):
    if yy > ty1 - 40:
        break
    head = f"{name}"
    d.text((rx, yy), head, font=capb, fill=(226, 230, 238))
    meta = f"×{cnt} · {ago(_t.time() - (t or _t.time()))} ago"
    d.text((rx + room - d.textlength(meta, font=capf), yy), meta, font=capf, fill=(120, 126, 138))
    for l in textwrap.wrap(re.sub(r"\s+", " ", body), room // 7)[:2]:
        yy += 17
        d.text((rx, yy), l, font=capf, fill=(150, 156, 168))
    yy += 26
d.rectangle([px, py, px + pw - 1, py + ph - 1], outline=STATE.get(win["state"]) or EDGE, width=2)
img.save(out)
print(f"talk: {sum(1 for e in ev if e[0]=='you')} from you, {sum(1 for e in ev if e[0]=='reply')} replies, {n_reports} reports, {sum(1 for e in ev if e[0]=='busywork')} replies to reports; unanswered={wait}")
