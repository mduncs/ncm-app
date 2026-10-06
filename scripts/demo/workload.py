#!/usr/bin/env python3
"""Fictional ANSI workloads. All commands and diffs below are printed, never run."""
import datetime
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

OUT, NAME, KIND, RATE = Path(sys.argv[1]), sys.argv[2], sys.argv[3], float(sys.argv[4])
STOP = False
for sig in (signal.SIGHUP, signal.SIGTERM, signal.SIGINT):
    signal.signal(sig, lambda *_: globals().__setitem__("STOP", True))

ISSUES = {
    "otter-ledger": "Fix the double-counted moonbeans in the basket ledger.",
    "plum-forge": "Make the paper-dragon recipe compiler preserve glaze order.",
    "velvet-radar": "Pause before changing the lantern migration. Which glow should we keep?",
    "moss-kettle": "Repair the fizz scheduler when two kettles ring together.",
    "amber-orbit": "Add regression coverage for the marmalade orbit wraparound.",
    "pebble-vault": "Make the pebble index rebuild deterministic after a nap.",
    "lilac-engine": "Track down the missing comet crumbs in the lilac queue.",
    "waffle-comet": "Review the waffle manifest and summarize the syrup cache.",
}
WAITING = NAME == "velvet-radar"
DONE = NAME in ("amber-orbit", "pebble-vault")
REPORTERS = ["fern-scout", "glaze-checker", "sprout-builder"]
# Per-agent pane details: (file, removed line, added line, test command, test names, spinner verb).
WORK = {
    "otter-ledger": ("src/otter/ledger.rs", "count += glow.pending();", "count += glow.committed_once();",
                     "cargo test moonbean_commit", "basket_once  sleepy_kettle  glaze_order", "Testing"),
    "plum-forge": ("src/forge/glaze.rs", "layers.sort();", "layers.sort_by_key(|l| l.applied_at);",
                   "cargo test glaze_order", "dragon_red  dragon_teal  kiln_reload", "Forging"),
    "velvet-radar": ("migrations/0042_lantern_glow.sql", "glow TEXT DEFAULT 'velvet'", "glow TEXT DEFAULT 'plum'",
                     "sqlx migrate info", "pending  0042_lantern_glow", "Waiting"),
    "moss-kettle": ("src/kettle/fizz.ts", "if (a.ringAt < b.ringAt)", "if (a.ringAt <= b.ringAt)",
                    "pnpm test fizz", "two_kettles  same_bell  cold_start", "Brewing"),
    "amber-orbit": ("tests/orbit_wrap.py", "assert pos < 360", "assert pos == (pos + step) % 360",
                    "pytest tests/orbit_wrap.py", "wrap_zero  wrap_full  marmalade_drift", "Done"),
    "pebble-vault": ("src/vault/index.go", "for k := range pebbles {", "for _, k := range sortedKeys(pebbles) {",
                     "go test ./vault/...", "rebuild_after_nap  stable_order", "Done"),
    "lilac-engine": ("lib/queue/crumbs.ex", "Enum.take(crumbs, 10)", "Enum.take(crumbs, limit)",
                     "mix test test/crumbs_test.exs", "comet_tail  empty_queue  lilac_burst", "Hunting"),
    "waffle-comet": ("syrup/manifest.toml", "cache = \"warm\"", "cache = \"maple-lru\"",
                     "waffle lint manifest", "syrup_keys  griddle_ttl", "Idle"),
}


def stamp(seconds_ago=0):
    return (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=seconds_ago)).isoformat()


def transcript():
    sid = "demo-" + NAME
    path = OUT / "claude/projects/fixture" / (sid + ".jsonl")
    records = []
    parent = None

    def add(role, content, ago, origin="human", **extra):
        nonlocal parent
        uid = f"{NAME}-{len(records)}"
        record = {"type": role, "uuid": uid, "timestamp": stamp(ago),
                  "message": {"role": role, "content": content}, **extra}
        if parent:
            record["parentUuid"] = parent
        if role == "user":
            record["origin"] = {"kind": origin}
        if role == "assistant":
            record["message"].update(model="claude-sonnet-4-6", usage={"input_tokens": 18400,
                "cache_read_input_tokens": 9200, "cache_creation_input_tokens": 3000})
        records.append(record)
        parent = uid

    add("user", ISSUES[NAME] + "\nThe product is Lantern Pantry, a pretend store for moonbeans and paper dragons.\n"
        "Keep the fixture small and give me a clear regression story.", 600)
    add("assistant", "I reproduced the bug with a tiny basket fixture.\n"
        "The shimmer counter was advanced before the basket was committed.\n"
        "I will keep the old glow names and move the increment into the commit path.\n"
        "The empty-basket case needs its own guard.\n"
        "The paper-dragon case must preserve glaze order.\n"
        "I have added a deterministic seed to the fixture.\n"
        "Next I am checking the sleepy-kettle boundary.\n"
        "The public behavior stays easy to explain: each basket counts once.\n"
        "The three fixture reviewers are checking separate pieces.", 590)
    # A consecutive run produces the collapsed reports line, Reports tab, and by-agent column.
    for i, reporter in enumerate(REPORTERS):
        add("user", f'<task-notification><task-id>{NAME}-task-{i}</task-id>'
            f'<status>completed</status><summary>Agent "{reporter}" finished</summary>'
            f'<result>{reporter}: checked the fictional {NAME} fixture.\n'
            'The sleepy basket now keeps exactly one moonbean.\n'
            'Added a regression for the paper-dragon edge case.\n'
            'All 24 pretend checks pass; no real tools were run.</result></task-notification>',
            430 - i * 10, origin="task-notification")
    # Start a human turn after the reports so the reply is attributed to the user.
    add("user", "Please keep the lantern labels stable and explain the small diff.", 380)
    add("assistant", "The patch changes the commit guard and adds two basket cases.\n"
        "```diff\n- count += glow.pending();\n+ count += glow.committed_once();\n```\n"
        "The glaze reviewer verified that the label order is stable.\n"
        "The sprout reviewer checked the empty basket and the sleepy basket.\n"
        "The fern reviewer confirmed the two lantern colors remain distinct.\n"
        "I am keeping all the test values imaginary.\n"
        "The remaining check is the same-bell kettle pair.\n"
        "After that, the moonbean fixture is ready to review.", 370)
    if RATE:
        add("assistant", [{"type": "tool_use", "id": "scout", "name": "Agent", "input": {
            "name": "petal-runner", "description": "Check sleepy baskets", "run_in_background": True}}], 350)
        add("user", [{"type": "tool_result", "tool_use_id": "scout", "content": "launched"}],
            349, toolUseResult={"agentId": "petal-runner"})
        add("assistant", [{"type": "tool_use", "id": "test", "name": "Bash",
                           "input": {"command": WORK[NAME][3]}}], 320)
    if DONE:
        add("assistant", "Done: the pretend basket regression passes and the tiny patch is ready.", 300)
    if WAITING:
        add("user", "Keep the velvet glow or switch to plum glow?\nPlease wait for my choice before editing the migration.", 185)
    path.write_text("".join(json.dumps(record) + "\n" for record in records))
    socket = os.environ["TMUX"].rsplit(",", 2)[0]
    window = subprocess.check_output([str(OUT / "bin/tmux"), "-S", socket, "display-message",
        "-p", "-t", os.environ["TMUX_PANE"], "#{window_id}"], text=True).strip()
    metadata = {"pid": os.getpid(), "sessionId": sid,
                "tmux": f"demo:{window}.{os.environ['TMUX_PANE']}"}
    (OUT / "claude/sessions" / f"{os.getpid()}.json").write_text(json.dumps(metadata))


def color(text, code):
    return f"\033[{code}m{text}\033[0m"


STEPS = [
    ("Grep", "pattern: \"{stem}\" · 14 matches in 6 files", "38;5;245"),
    ("Read", "{path} · {lines} lines", "38;5;245"),
    ("Read", "tests/fixtures/{stem}_cases.json · 48 lines", "38;5;245"),
    ("Edit", "{path}", "diff"),
    ("Bash", "{test}", "checks"),
    ("Task", "fern-scout · \"scan sibling modules for the same pattern\"", "38;5;141"),
    ("Read", "docs/{stem}.md · 112 lines", "38;5;245"),
    ("Edit", "tests/{stem}_test · add 2 cases", "diff2"),
    ("Bash", "{test} -- --nocapture", "checks"),
    ("TodoWrite", "3 done · 1 in progress · 2 pending", "38;5;245"),
]
PROSE = [
    "The counter moves before the commit lands, so a retry counts the same item twice.",
    "Two call sites share this path; only the second one needs the guard.",
    "The fixture is small on purpose; one case per boundary keeps the story readable.",
    "Ordering is stable now. I am checking the empty and single-item cases next.",
    "The reviewer agents are covering sibling modules while I finish this one.",
]


def agent(tick):
    elapsed = 84 + int(tick * RATE)
    path, removed, added, test, names, verb = WORK[NAME]
    stem = Path(path).stem
    status = "Waiting for your choice" if WAITING else "Done · patch ready" if DONE else "Idle · ready for another task" if not RATE else verb
    checks = "  ".join("✓ " + n for n in names.split())
    body = []
    # Busy agents scroll through a rolling history of tool calls; quiet ones hold still.
    start = tick if RATE else 0
    for k in range(start, start + 9):
        tool, detail, style = STEPS[k % len(STEPS)]
        detail = detail.format(path=path, stem=stem, test=test, lines=60 + len(NAME) * 3 + k)
        body.append(color("⏺ ", "38;5;114" if tool != "Task" else "38;5;141") +
                    color(f"{tool}(", "1;38;5;252") + color(detail, "38;5;250") + color(")", "1;38;5;252"))
        if style == "diff":
            body += [color(f"  ⎿  Updated {path} with 1 addition and 1 removal", "38;5;245"),
                     color(f"     {40 + k:>3}   fn commit(&mut self, basket: &Basket) -> Result<()> {{", "38;5;245"),
                     color(f"     {41 + k:>3} - {removed}", "38;5;203;48;5;52"),
                     color(f"     {41 + k:>3} + {added}", "38;5;114;48;5;22")]
        elif style == "diff2":
            body += [color("  ⎿  Added 2 cases", "38;5;245"),
                     color(f"     + case(\"{stem}_empty\", expect=0)", "38;5;114;48;5;22"),
                     color(f"     + case(\"{stem}_twice\", expect=1)", "38;5;114;48;5;22")]
        elif style == "checks":
            body += [color(f"  ⎿  running {len(names.split()) + 21} tests", "38;5;245"),
                     color("     " + checks + "  · 0 failed", "38;5;114")]
        else:
            body.append(color("  ⎿  " + ("done" if tool != "Task" else "running in background"), "38;5;245"))
        body.append(PROSE[k % len(PROSE)])
        body.append("")
    head = [color("╭" + "─" * 78 + "╮", "38;5;173"),
            color("│ ", "38;5;173") + color("✻ Lantern Pantry", "1;38;5;173") +
            color(f"  ·  ~/work/{NAME}  ·  Sonnet 4.6  ·  ctx 30.6k / 200k".ljust(61), "38;5;245") + color("│", "38;5;173"),
            color("╰" + "─" * 78 + "╯", "38;5;173"), "",
            color("> " + ISSUES[NAME], "38;5;252;48;5;236"), ""]
    foot = ["",
            color("  fern-scout     idle · checked tiny fixtures      ", "38;5;245") +
            color("glaze-checker  idle · label order stable", "38;5;245"),
            color("  sprout-builder idle · reviewed the diff          ", "38;5;245") +
            (color("petal-runner   running · second pass", "38;5;141") if RATE else ""),
            "",
            color(("✳" if tick % 2 else "✶") + f" {status}… ", "38;5;211" if WAITING else "38;5;173") +
            color(f"({elapsed}s · ↓ {4200 + tick * 37:,} tokens · esc to interrupt)", "38;5;245"),
            color("─" * 196, "38;5;239"),
            color("> Choose velvet or plum" if WAITING else "> ", "1;38;5;252"),
            color("─" * 196, "38;5;239"),
            color("  ⏵⏵ accept edits on · ? for shortcuts", "38;5;245")]
    room = 55 - len(head) - len(foot)
    lines = head + body[-room:] + [""] * max(0, room - len(body)) + foot
    print("\033[2J\033[H" + "\n".join(lines[:55]), end="", flush=True)


def bar(value, width=30):
    full = int(value * width)
    return "█" * full + "░" * (width - full)


def monitor(tick):
    import math
    rows = [("moonbeans", "38;5;114"), ("paper dragons", "38;5;141"), ("sleepy kettles", "38;5;221"),
            ("glaze queue", "38;5;117"), ("comet crumbs", "38;5;211"), ("syrup cache", "38;5;180"),
            ("pebble index", "38;5;109"), ("lantern glow", "38;5;216")]
    lines = [color(" LANTERN PANTRY · fictional monitor ", "1;38;5;16;48;5;117") +
             color(f"   tick {tick:05d}   uptime {3 + tick // 60}h{tick % 60:02d}m   no host data", "38;5;245"), ""]
    for i, (name, c) in enumerate(rows):
        v = 0.5 + 0.45 * math.sin(tick / 3 + i)
        lines.append(f"  {name:<16}" + color(bar(v, 60), c) + f" {int(v * 100):>3}%")
    lines += ["", color("  throughput (baskets/s)", "1;38;5;252")]
    spark = "▁▂▃▄▅▆▇█"
    for i, (name, c) in enumerate(rows[:4]):
        series = "".join(spark[int(3.5 + 3.5 * math.sin((tick + j) / 4 + i))] for j in range(120))
        lines.append(f"  {name:<16}" + color(series, c))
    lines += ["", color("  recent", "1;38;5;252")]
    events = ["basket committed once", "glaze order stable", "kettle pair rang together",
              "crumb trail compacted", "pebble index rebuilt", "syrup cache warmed"]
    for j in range(22):
        k = tick + j
        lines.append(color(f"  {k:05d}  ", "38;5;245") + color("●", rows[k % 8][1]) +
                     f" {rows[k % 8][0]:<16} {events[k % 6]}")
    print("\033[2J\033[H" + "\n".join(lines[:55]), end="", flush=True)


def main():
    if KIND == "agent":
        transcript()
        agent(0)
    elif KIND == "shell":
        # A real sh with a fake HOME, no startup scripts, safe cwd and no host/user prompt.
        print("\033[2J\033[H" + color("CORAL PICNIC / scratch shell", "1;38;5;117") +
              "\n\nThis shell uses only the throwaway demo home.\n", flush=True)
        print("~/work/coral-picnic $ ls\n" + color("assets  docs  src  tests", "38;5;117") +
              "  Cargo.toml  README.md\n~/work/coral-picnic $ git log --oneline -5\n" +
              "\n".join(color(h, "38;5;221") + " " + m for h, m in [
                  ("a41c09e", "picnic: fold blanket rows"), ("9be2f13", "basket: count once"),
                  ("77d0a5c", "docs: sandwich glossary"), ("3f18b6e", "tests: ant-proof fixtures"),
                  ("0c2d9aa", "init")]), flush=True)
        os.environ["PS1"] = "~/work/coral-picnic $ "
        os.execve("/bin/sh", ["sh", "-i"], os.environ)
    elif KIND == "build":
        print("\033[2J\033[H" + color("NOODLE BEACON / fictional build log", "1;38;5;117"), flush=True)
        for i in range(38):
            print(color(f"  ok {i + 1:02d} · moonbean fixture / glaze order / sleepy basket", "38;5;114"), flush=True)
    else:
        monitor(0) if KIND == "monitor" else print("\033[2J\033[H" + color(
            "CLOUD TEAPOT / " + ("glaze watcher" if KIND == "split" else "basket watcher"), "1;38;5;141") +
            "\n\nTwo independent fictional streams in a split window.\n", flush=True)
    tick = 0
    last = time.monotonic()
    while not STOP:
        time.sleep(.1)
        if not RATE or time.monotonic() - last < RATE:
            continue
        last = time.monotonic()
        tick += 1
        if KIND == "agent":
            agent(tick)
        elif KIND == "monitor":
            monitor(tick)
        elif KIND == "build":
            print(color(f"  [{tick:04d}] test basket_commit_once ... ok · 24 pretend checks passed", "38;5;114"), flush=True)
        else:
            print(color(f"  [{tick:04d}] {'glaze stable' if KIND == 'split' else 'basket committed once'} · local fixture", "38;5;114"), flush=True)


if __name__ == "__main__":
    main()
