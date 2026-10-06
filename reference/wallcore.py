"""Quick Look wall, gussied up. Writes <out>.png (clean) and <out>_notes.png (every addition boxed + numbered).
  python3 wall4.py out.png [window index to preview]
"""
import math, re, sys, time
from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont, ImageOps
sys.argv = [sys.argv[0], "x.png"] + sys.argv[1:]
import wall
deck = wall.deck
out, pick = (sys.argv[2] if len(sys.argv) > 2 else "x.png"), (sys.argv[3] if len(sys.argv) > 3 else "8")
SW, SH, CW, CH, GAP, DOCK = 2560, 1440, 8, 16, 14, 56
FD = "/System/Applications/Utilities/Terminal.app/Contents/Resources/Fonts/"
mono = ImageFont.truetype(FD + "SF-Mono-Regular.otf", 13)
chipf = ImageFont.truetype(FD + "SF-Mono-Semibold.otf", 11) if False else ImageFont.truetype(FD + "SF-Mono-Medium.otf", 11)
capf = ImageFont.truetype(FD + "SF-Mono-Regular.otf", 11)
capb = ImageFont.truetype(FD + "SF-Mono-Bold.otf", 11)
dockf = ImageFont.truetype(FD + "SF-Mono-Medium.otf", 13)
notef = ImageFont.truetype(FD + "SF-Mono-Bold.otf", 15)
notet = ImageFont.truetype(FD + "SF-Mono-Regular.otf", 15)
BG = (13, 14, 17)
NOW = time.time()


def oklch(L, C, h):
    a, b = C * math.cos(math.radians(h)), C * math.sin(math.radians(h))
    l_, m_, s_ = L + .3963377774 * a + .2158037573 * b, L - .1055613458 * a - .0638541728 * b, L - .0894841775 * a - 1.2914855480 * b
    l, m, s = l_ ** 3, m_ ** 3, s_ ** 3
    rgb = (4.0767416621 * l - 3.3077115913 * m + .2309699292 * s, -1.2684380046 * l + 2.6097574011 * m - .3413193965 * s,
           -.0041960863 * l - .7034186147 * m + 1.7076147010 * s)
    f = lambda c: 12.92 * c if c <= .0031308 else 1.055 * max(c, 0) ** (1 / 2.4) - .055
    return tuple(max(0, min(255, round(255 * f(c)))) for c in rgb)


POWERSHELL, AUBERGINE = (1, 36, 86), (48, 10, 36)
TYPE = {"pinned": ("PIN", 250), "build": ("BUILD", 150), "explore": ("EXPLORE", 300), "personal": ("PERSONAL", 70)}
HARNESS = {"CLAUDE": (217, 119, 87), "CODEX": (16, 163, 127), "GLM": (80, 120, 220), "KIMI": (110, 110, 240)}
STATE = {"needs": (255, 46, 136), "done": (126, 231, 135), "working": (76, 201, 240)}
YELLOW = (255, 212, 0)

# ---- data ---------------------------------------------------------------------------------
wins = [w for w in deck.list_windows() if w["name"] != "deck-demo"]
extra = {l.split("\t")[0]: l.split("\t")[1:] for l in
         deck.tmux("list-panes", "-s", "-F", "#{pane_id}\t#{pane_current_path}\t#{pane_title}").splitlines()}
activity = dict(l.split("\t") for l in deck.tmux("list-windows", "-F", "#{window_index}\t#{window_activity}").splitlines())
for w in wins:
    w["age"] = NOW - int(activity.get(w["index"], NOW) or NOW)
    for p in w["panes"]:
        p["path"], p["title"] = extra.get(p["pane"], ["", ""])
caps = dict(zip([p["pane"] for w in wins for p in w["panes"]],
                deck.capture_all([p["pane"] for w in wins for p in w["panes"]])))


def project(p):
    parts = p["path"].strip("/").split("/")
    for i in range(len(parts) - 1, -1, -1):
        if parts[i] in ("code", "code-storage"):
            rest = parts[i + 1:]
            return (f"research/{rest[1]}" if rest[:1] == ["research"] and len(rest) > 1 else rest[0]) if rest else ""
    return ""


def env(p):
    t = f"{p['label']} {p['title']}".lower()
    return "ps" if ("powershell" in t or "windows" in t) else "wsl" if ("ubuntu" in t or "wsl" in t) else ""


projects = list(dict.fromkeys(project(p) for w in wins for p in w["panes"] if project(p)))
hue = {name: (25 + k * 7 * 360 / max(1, len(projects))) % 360 for k, name in enumerate(projects)}


def tint(p):
    e = env(p)
    if e:
        return POWERSHELL if e == "ps" else AUBERGINE
    return oklch(0.285, 0.045, hue[project(p)]) if project(p) else (36, 39, 46)


def plain(line):
    return deck.ANSI.sub("", line).replace("\xa0", " ")


SPIN = re.compile(r"^\s*[^\w\s]\s+([A-Z][a-z]+…\s*\(.*)$")
TOOL = re.compile(r"^\s*\W?\s*((Bash|Read|Edit|Write|Update|Task|Agent|Grep|Glob|WebSearch|WebFetch|Search)\((.*))$")
CHROME = re.compile(r"^\s*(.*\|\s*[/~(]|.*Ctx:|Model:|Session ID|.*bypass permissions|.*\? for shortcuts|[─━]+|›|>|[○●◉◯◎]\s|\d+ idle agents|↓ \d+ more|current: )")


def facts(p):
    """What the screen says about this agent: (caption kind, caption, model, ctx, subagents, working)."""
    lines = [plain(l).rstrip() for l in caps.get(p["pane"], [])]
    text = "\n".join(lines)
    model = (re.search(r"Model: ([A-Za-z]+ [\d.]+)", text) or [None, ""])[1]
    ctx = (re.search(r"Ctx: ([\d.]+k)", text) or [None, ""])[1]
    subs = [l for l in lines if re.match(r"^\s*[○●◉◯◎⏺]\s+\S+\s{2,}", l)]
    more = sum(int(m) for m in re.findall(r"(\d+) idle agents|↓ (\d+) more", text) for m in m if m)
    working = sum(1 for l in subs if not l.rstrip().endswith("idle"))
    spin = next((m.group(1) for l in lines if (m := SPIN.match(l))), None)
    if spin:
        return "spin", spin, model, ctx, len(subs) + more, working
    for l in reversed(lines):
        m = TOOL.match(l)
        if m:
            return "tool", m.group(1), model, ctx, len(subs) + more, working
    first_sub = next((i for i, l in enumerate(lines) if re.match(r"^\s*[○●◉◯◎]\s+\S+\s{2,}", l)), None)
    body = lines[:first_sub - 1] if first_sub else lines          # the line above the agent list is its heading
    for l in reversed(body):
        if l.strip() and not CHROME.match(l):
            return "text", l.strip(), model, ctx, len(subs) + more, working
    return "text", "", model, ctx, len(subs) + more, working


_native = {}


def native(win):
    if win["index"] in _native:
        return _native[win["index"]]
    im = Image.new("RGB", (win["ww"] * CW, win["wh"] * CH), (22, 24, 28))
    d = ImageDraw.Draw(im)
    for p in win["panes"]:
        base = tint(p)
        d.rectangle([p["x"] * CW, p["y"] * CH, (p["x"] + p["w"]) * CW - 1, (p["y"] + p["h"]) * CH - 1], fill=base)
        lines = caps.get(p["pane"], [])
        for j in range(min(p["h"], win["wh"] - p["y"])):
            for i, (ch, f, b) in enumerate(wall.parse(lines[j] if j < len(lines) else "", p["w"], base)):
                x, y = (p["x"] + i) * CW, (p["y"] + j) * CH
                if b and b != base:
                    d.rectangle([x, y, x + CW - 1, y + CH - 1], fill=b)
                if ch and ch.strip():
                    d.text((x, y + 1), ch, font=mono, fill=f or deck.DEFAULT_FG)
    _native[win["index"]] = im
    return im




def ago(s):
    return f"{int(s)}s" if s < 60 else f"{int(s // 60)}m" if s < 3600 else f"{int(s // 3600)}h" if s < 86400 else f"{int(s // 86400)}d"
