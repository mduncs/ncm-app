# ncm application

Run `ncm`, `ncm --session NAME`, or `ncm --demo` in a terminal. Demo contains six synthetic
windows (including a split), colors, tool/subagent facts, conversation, reports and an unanswered
message. It does not perform tmux actions. `--list`, `--version` and `--help` do not enter raw mode.

Arrows select cards; Space opens/closes Quick Look; Tab cycles alerts in needs/bell/done/activity
order. Enter, `i`, or a click inside Quick Look opens a full-screen real tmux client outside tmux.
The `_ncm_view_<pid>` grouped session selects the target independently of the source session;
native mouse, clipboard, copy mode and bindings apply. C-] or prefix-d returns to ncm. The helper
is destroyed on detach and cleaned on failure/exit. Inside tmux, Enter and `i` simply select the
chosen window/pane; legacy mode is text/braille only. Escape closes Quick Look and never quits;
`q` quits. In the grid, comma renames, `x` asks y/n before closing, and `n` creates a window.
Clicking any card selects it and opens Quick Look; clicking outside closes it. Right-click
opens ncm's Options menu, even with no interactive tmux client attached. Choose Go to window,
Rename or Close with arrows/Enter or a click; `r` and `x` choose Rename and Close. Close retains
its y/n confirmation and Escape dismisses the menu. Native detach returns to Quick Look with images.
Quick Look uses `1` Screen, `2` Talk, `3` Reports and `[`/`]` to choose the followed pane.

`layout.rs` scores column counts against native window geometry. Cell rectangles have no gaps and
exclude a 30px dock rounded up to whole terminal cells; the renderer owns the 4px inset. `terminal.rs` probes direct kitty once at startup with a five-second timeout,
checks `TIOCGWINSZ`, then asks CSI 16 t before falling back to 8x16. Raw mode, alternate screen,
SGR mouse and bracketed paste are guarded; normal exits, errors, panics and SIGTERM restore modes.
Signals set an atomic flag, with cleanup in the app loop. EOF/EIO/hangup is detected independently
of crossterm; reads and nonblocking writes run off the UI thread. Text frames coalesce and take
priority between image packets, and output waits check the stop flag every 5ms. The cell double-buffer compares rows
and writes changed runs inside synchronized output, including wide-glyph repairs. Identical
frames and cursors produce no output. Entry and return from native tmux clear/redraw the entire alternate screen; ordinary frames use diffs.

`collector.rs` uses `CaptureScheduler::tick` with one persistent read-only control client.
Pane output marks only its pane dirty; cards share a one-second capture limit. Metadata refreshes
back off from 250ms to 1s, while structural notifications invalidate it immediately on the next tick.
Quick Look retains its 250ms window captures. Content hashes skip identical captures before parsing
and composition; geometry/tint changes invalidate caches. Facts refresh every second. Image packets
use stable ids, one update/second/card, a 500KB/s token refill with a 100KB steady-state burst cap, and retained
pending packets when the traffic budget is exhausted. Quick Look has an independent four-row-strip picture refresh under a shared 1.4MB/s budget. Uploads span
ticks only at complete protocol chunk boundaries and never interleave image continuations. Placeholder grids are constructed on layout changes;
Quick Look temporarily hides/restores them through ordinary cell diffs. Cards over 64 rows are
split into stacked placements. Images contain only picture/edges; title, now and subagent overlays
are terminal text over solid `(10,11,14)`.

The app uses `ThumbnailRenderer::render_card` for per-window changed-row caches and
`TintResolver` for explicit/environment/project tints. Kitty images use stable ids and explicit
virtual placement dimensions, with indexed placeholder foreground and inferred columns.
Outside tmux, kitty packets go straight to the terminal. A failed startup query settles in
text/braille; a late success can still enable images. Inside tmux, probes, image transmissions and
deletes are disabled. Full-screen attach pauses both terminal I/O workers, restores termios and
leaves the alternate screen. Return re-enters raw/alternate-screen mode and invalidates all cell and
image transmission caches for a full redraw/retransmission. Popup/passthrough/re-probe paths are retired.

Panics append timestamp, thread, message, source location and a forced backtrace to
`~/.local/state/ncm/panic.log`. Records are bounded and logs over 256 KiB discard the oldest half.
The hook signals shutdown without changing terminal modes. Worker panics show a short dock line
once drawing has stopped; the main owner restores the terminal, prints the failure outside the
alternate screen and exits with status 1. Main-thread panics unwind through the same terminal
owner before printing their message.

`talk_source.rs` uses process/session discovery, `adapter_for`, streaming `poll_adapter_bounded`,
`merge_facts` and `screen_facts`. Cached projections share immutable Talk/Reports text; events are
bounded to the newest 256 and 256 KiB per pane. Discovery refreshes every three seconds and respects
tmux's harness name/color overrides. Missing sessions clear departed followers. `CODEX_HOME` is
honored by default discovery. Individual display bodies are capped at 16 KiB with a clipping note. Subagents aggregate across a window; Talk/model/now follow its chosen
pane. Initial loading is incremental and starts after the first visible frame.

`--socket PATH` / `NCM_TMUX_SOCKET` use an explicit `tmux -S` handle, ignoring the viewer's inherited
server, pane and session id. `--stats` shows own CPU/current RSS/image KB/s/captures/s;
`--log FILE` records timings, writes and running/reaped-child CPU as well. `--read-only` disables tmux
actions, including use mode and menu actions. See
[measurements](../../docs/INTEGRATION.md) for the release live-server run.

`cargo test -p ncm` includes layout, input and diff-writer tests plus a private-server fullscreen
check. All builds/tests on this host must use the host `heavy` admission wrapper. The tests use
`ncm_tmux::testing::PrivateServer`, capture the demo grid/Screen/Talk, exercise legacy select-window without an interactive client, and
compare complete `stty -g` modes before/after quit and SIGTERM. They use only owned private servers and never address
the user's live server. Captures and the demo idle `ps` sample are written under
`target/ncm-verification/`; the binary is `target/debug/ncm`. Run `scripts/check.sh` for workspace and doc tests,
owned tty lifecycle and worker-panic checks, and the direct-terminal integration test. The latter
emulates kitty replies in owned PTYs, checks mouse selection/preview/full-screen entry, startup
probe/fallback and resize, native grouped attach with another client present, C-]/prefix-d/SIGTERM
cleanup, termios restoration, complete image retransmission, root pass-through, one source control
connection at startup, and zero graphics in legacy mode. Artifacts are in
`target/ncm-outside-checks/` and `target/ncm-panic-checks/`; no GUI windows or audio are used.
The earlier `use_mode_check.py`, `graphics_reprobe_check.py` and `resize_clients_check.py` describe
the retired popup/in-tmux graphics behavior and are not part of the current check suite.

Hidden viewers suspend capture/transcript work and make one cheap viewer-server visibility query
per second, including when mirroring another socket. Return resets capture scheduling and catches
up transcripts immediately. `scripts/check.sh` includes owned-child pty-close and busy signal tests;
script-launched viewers have an owned parent (`run_owned.py` or `pty_owned.py`) that kills and reaps them on teardown.
