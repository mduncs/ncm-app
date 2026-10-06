Run from a tmux pane in Ghostty 1.3.1, from the worktree root:

```sh
python3 tools/kitty_probe.py
python3 tools/kitty_probe.py --bench
```

Python 3 and Pillow are required. Use a pane at least 40 columns by 18 rows;
roughly 105 columns by 34 rows gives the 500x272 images room at typical cell
sizes. Keep the pane size unchanged during a run. Smaller panes scale the images.
The script only reads tmux options and runs `display -p`; it never changes your
server options. If passthrough is off, it exits with a command you can run to
enable it for that window, including when a local override masks the global value.

The visual run takes 30 seconds. Expect four colored grid images counting up
at 2 Hz, no flashing, a plain text panel across their center at 10–18 seconds,
and a complete grid again afterward. The placeholder text is written initially
and only the covered cells are restored after the panel. The status line alone
updates during normal image replacement. The alternate screen and cursor are
restored at the end, then the report stays visible until Enter. Ctrl-C also
restores the terminal and deletes only this run's random image ids.

`--bench` measures 3 seconds idle, then transmits five image ids at 2 Hz for
15 seconds. The fifth placement covers most of the first image, leaving its
edges visible. Fixed random RGBA pixels produce approximately 100 KB of actual
zlib data per image. It reports achieved ticks, compressed bytes/s, total wire
bytes/s (base64, protocol wrappers and text included), mean compressed image
size, and idle/burst tmux round-trip p50/p95 with sample counts and failures.
The sampler runs in a background thread every 100 ms; each read has a 2-second
timeout. These latencies include subprocess startup. The intentionally stressful
wire load is roughly 1.3 MB/s, above SPEC.md's 600 KB/s production budget.

A successful `a=q` reply establishes basic graphics support, not placeholders,
smooth image replacement, or text occlusion. A timeout prints the escaped bytes
received and continues with support marked unconfirmed, because the return path
itself may be the problem. An explicit error reply exits. `$TERM_PROGRAM` inside
tmux may identify tmux rather than the outer terminal: this probe cannot verify
the Ghostty version. Your eyes must settle the visual behavior, and typing or
switching panes during the benchmark should still feel responsive.

The [kitty protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/) says
retransmitting an id deletes its placements. Each update therefore combines
transmission with recreating the same virtual placement (`a=T,U=1,q=2`). It sends
RGBA with zlib (`f=32,o=z`), base64 chunks up to 4096 bytes, each in its own tmux
passthrough DCS. Placeholder text uses U+10EEEE, explicit row/column diacritics,
and the image id in 24-bit foreground color. Text updates use synchronized
output. No full-screen clears occur during animation.

Run byte-level tests from `tools/`:

```sh
cd tools
python3 -m unittest -v
```

Headless private-tmux checks establish that the script executes and emits the
expected traffic; they cannot certify Ghostty rendering. No GUI or live tmux
server is used for these checks.

Recorded headless check on md's tmux 3.7c (2026-10-03):

| Check | Result |
| --- | --- |
| Unit tests | 12 passed, including exact bytes and 10s/18s panel timing |
| Passthrough off | Exit 1 with the exact per-window fix command |
| Visual run, no query reply | Marked unconfirmed; 60 ticks / 30.00s; ~123 KB/s wire; Enter exits |
| Benchmark, synthetic OK reply | 23 ticks / 15.89s; ~96.8 KB compressed/image; ~942 KB/s wire |
| Idle command latency | n=28, p50 27.42 ms, p95 162.02 ms |
| Burst command latency | n=108, p50 44.95 ms, p95 402.85 ms; no command failures |

The benchmark missed its target 30 ticks. This is a performance concern, not a
rendering certification or a prediction of Ghostty latency. The synthetic query
reply only exercises parsing through tmux. Repeat in the real Ghostty pane and
judge flicker, panel occlusion/restoration, and responsiveness there. Check logs,
pane captures, and raw protocol streams are in `.workers/kitty-probe/` (ignored
by git). The headless harness is `check.py` there and must also run through the
heavy wrapper; it uses a private server with `TMUX_TMPDIR` in that directory.
