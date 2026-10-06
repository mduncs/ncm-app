# Integrated ncm

ncm combines tmux discovery/capture, window composition, atlas pictures, Claude/Codex transcript
adapters and screen fallback. Cards show tmux colors, harness names, now/subagent lines,
unanswered badges and activity ages. Quick Look follows the chosen pane for Talk/model/now;
card subagents aggregate across the window.

## Collection, input and pictures

`collector.rs` owns discovery, capture, composition and transcript facts off the input thread.
`CaptureScheduler::tick` uses one persistent, read-only, size-neutral tmux control client. Pane
output notifications dirty the emitting pane without retaining output bytes. Cards share a one-second
capture cooldown across their panes; pending changes survive it. Metadata queries back off from
250ms to 1s, with structural notifications requesting fresh snapshots. Quick Look captures every pane
of its window at 250ms. Ordinary live panes have no timer-driven safety recaptures. Grid selection
does no extra capture work. Capture hashes avoid reparsing unchanged ANSI text, and picture-strip
hashes prevent unchanged image uploads. Reconnect on EOF refreshes metadata/content state; shutdown
joins the collector and reaps its control client.

Process discovery uses libproc (`proc_listallpids`, `PROC_PIDTBSDINFO`) in-process. PID start times
distinguish reused identities. Codex rollout lookup uses `PROC_PIDLISTFDS` and
`PROC_PIDFDVNODEPATHINFO`, caching descriptor results until a process identity changes or its rollout
disappears. Linux uses `/proc`; production discovery launches neither `ps` nor `lsof`.

Control replies use matching `%begin`/`%end`/`%error` guard blocks; raw pane output is discarded
outside those blocks. Capture and border reads reuse the persistent connection. There is one source
collector; `app.rs` consumes snapshots. Screen snapshots are published before transcript catch-up.
Legacy viewer visibility checks share the source connection for the same server and exclude
headless control clients; direct-terminal graphics need no viewer checks. See [tmux's control-mode protocol](https://github.com/tmux/tmux/wiki/Control-Mode).

Transcript discovery sets live adapter context, including the Claude process start time. Background
agents launched before that lifetime are dropped; unfinished agents need transcript activity within
three minutes to count as busy. Claude's visible agent list overrides inferred busy/idle and elapsed
time. Content-based report envelopes work even without origin metadata; envelope names and inner
JSON summaries are projected into Reports, with report reply ancestry preserved.

Catch-up streams older records without retaining the full transcript. Display history is bounded to
256 events/256 KiB per pane, individual bodies to 16 KiB and ancestry to 2,048 nodes. Original
transcripts remain read-only. Talk projections and native window composites use shared immutable
Arcs. Single-pane windows use the parsed grid directly; split panes are transient during geometry
rebuilds. Steady capture changes parse only differing ANSI lines into the composite. Border color
changes repaint the narrow frame; shrinking a border restores the covered source pixels. Grid
snapshots share immutable rows and copy only rows that change; flat cell and JSON APIs retain
their existing format. This removes whole-grid string allocation when another thread holds an old
composite. Font discovery reads bounded name tables without CoreText.

Input drains queued events before rendering and coalesces held arrows. Selection paints a light
terminal title bar and bottom markers in the same frame, without image traffic. All three text
overlay rows are clipped to the card's own columns, including wide glyphs. `pictures.rs` performs
atlas rasterization/compression; `output.rs` prioritizes coalesced cell frames between complete image
packets. Worker channels and output queues are bounded. Input waits wake immediately on keys;
short UI timers run only while pictures are in flight. The collector waits on a condition variable until its next
metadata deadline, with immediate notification for Quick Look target and use-mode changes.
Monotonic trace timestamps and single append writes allow timings across threads. Repeated bare Unicode placeholder cells use REP
sequences; their anchored first cell retains its row/column diacritics. This reduces Quick Look
panel open/close text traffic and tty backpressure.

Card pictures update at most once per second. Quick Look floats centered at 88% of each grid axis
(roughly 78% of its area), preserving the dock and surrounding grid. Screen fits the entire window
as a picture and updates changed strips without the card throttle (one row for panels up to 55 rows,
four rows for larger panels); Talk and Reports use
terminal text inside the panel. The dock reserves 30 pixels rounded up to terminal cells. Stable
indexed placeholders and explicit placements are used. Grid cards normally use four-row strips,
with larger tiles when needed to fit the 199-card-placement budget. Pixel hashes suppress unchanged
strips. Preview toggles invalidate transmission hashes while retaining glyph/card render caches;
layout changes clear those caches. Closing/changing a preview discards unsent strips after finishing
any started continuation. Grid uploads yield to Quick Look at strip boundaries. The shared image
budget is 500 KB/s for the grid and 1.4 MB/s with Quick Look open,
with 16 KiB of queued upload work per UI frame. Uploads resume at complete kitty packet boundaries
and never interleave continuation chunks. Initial allowance is 600 KB with a one-second refill pause.

The primary viewer runs directly in Ghostty, outside tmux, including via
`ssh -t host ~/.local/bin/ncm`. Agents remain in the background tmux server.
The default socket, `--socket PATH`, and `NCM_TMUX_SOCKET` select the source server;
`--session NAME` selects its session (default `main` outside tmux). The shared tmux command
constructor finds `~/.local/bin/tmux`, then `/opt/homebrew/bin/tmux`, then PATH,
so a non-interactive SSH command needs no interactive shell setup.

Outside tmux, one kitty query is sent at startup with a five-second reply timeout. The ordinary
input decoder receives the reply without blocking navigation. Kitty APC packets go straight to the
terminal; there is no tmux passthrough, visibility re-probe, or multi-client graphics negotiation.
A missing reply settles in text/braille; a late successful reply can still enable pictures. Mouse
events come directly from the terminal. Legacy `$TMUX` mode always uses text/braille and emits
zero graphics bytes, including probes and cleanup. Legacy visibility checks can suspend collection
when its tmux window is hidden; they do not affect graphics capability.

## Navigation and use mode

Clicking a card selects it; clicking the selected card or pressing Space opens the floating
Quick Look. Screen, Talk and Reports remain preview tabs. Esc or an outside click closes it.
Arrows navigate, `1`/`2`/`3` choose tabs, and `[`/`]` choose a pane.

Outside tmux, Enter, `i`, or a click inside Quick Look opens a full-screen real tmux client.
ncm pauses its input/output workers, leaves the alternate screen and restores termios and stdout
flags before attaching. It creates an owned grouped `_ncm_view_<pid>` session on the source socket,
selects the target window there, and runs `tmux attach-session` directly on the terminal. The user's
mouse, scrolling, copy mode, clipboard commands and colors apply. The source session's current
window and its existing clients are untouched. The helper's status-right reads `ncm · C-] back`.

The root binding is `bind-key -n C-] if -F '#{m:_ncm_view_*,#{session_name}}' detach-client 'send-keys C-]'`.
Prefix-d or C-] detaches; C-] passes through in ordinary sessions. `destroy-unattached` is enabled
only after attaching, so detach removes the helper. ncm also owns cleanup on attach failure, exit,
SIGTERM and SIGHUP, and reaps its tmux child. On return it re-enters the alternate screen and raw mode,
redraws every cell, and retransmits/re-places all grid and preview images without another probe.

Inside tmux, Enter and `i` select the target window/pane in the user's session. There is no popup
or nested client. `--popup` is retired. `--read-only` disables all tmux actions in either mode.
Explicit sockets work for direct full-screen attach without an existing viewer client.

Clipboard: feel's private test found that a real drag filled the tmux buffer, but with
`set-clipboard external` and no `copy-command`, OSC 52 did not reach the outer pty and `pbpaste`
was unchanged. A private `copy-command pbcopy` probe repaired that drag path. No live server option
or user config was changed. Physical Ghostty display/clipboard verification belongs to the orchestrator.

## Validation and measurement

Run `scripts/check.sh` for fmt, clippy with `-D warnings`, workspace/doc tests and tty lifecycle
checks. The direct-terminal and legacy-mode checks run in private servers with owned, reaped viewers.
Live-source measurements use `--read-only --socket`; interactive attach tests target private servers. Emulated kitty/cell-size replies exercise pictures
and uploads without GUI windows. Timings measure stdout completion;
physical display/clipboard verification remains with the orchestrator.

CPU is percent of one core. Combined CPU includes ncm and completed spawned processes, excluding
the existing tmux server. Steady means are interval-weighted, excluding the first 15 seconds and
transcript catch-up; idle samples start after 65 seconds to avoid second-by-second age labels.
RSS uses MiB; the 80 decimal MB budget equals 76.294 MiB. Image KB/s is decimal protocol traffic.
Query timestamps count all viewer-owned tmux invocations, including visibility queries. No live
screen or transcript bodies are saved.

Historical observations (different live workloads and host load; not controlled replay):

| Tree | Combined busy CPU | Idle CPU | Spawns/s | Captures/s | Quick Look first-picture p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| lean alone | 1.99% | 0.33% | 0.98 | 5.07 | — |
| fixes + feel before lean | 8.93% (3.35 + 5.58) | — | — | 14.05 | 830.7 ms |
| this worktree before tuning | — | — | — | — | 417.2 ms |

Lean alone reported 61.547 MiB peak RSS. The earlier fixes + feel run reported 63.000 MiB;
its 40-sample latency run reported 2.304 ms arrow p95, 707.3 ms panel p95 and 830.7 ms first-picture
p95. The tuning baseline measured 8.345 ms arrow p95 and 327.3 ms panel p95. Its slow panel flushes
wrote approximately 48 KB and took up to 360 ms, while UI preparation took only a few milliseconds.
An attempted fixed eight-frame batch exposed `command too long` on the live 31-pane source; the final scheduler limits
packed argv to 16,000 bytes and shortens batches as needed. Panes with identical capture
conditions share exact ID-match groups, keeping cooldown-only frames small. The limit follows [tmux 3.7c imsg.h](https://github.com/tmux/tmux/blob/3.7c/compat/imsg.h) and its [client command packing](https://github.com/tmux/tmux/blob/3.7c/client.c).
The failed run is retained in `tune-final-busy.*` and `tune-diagnostic.*`; it is excluded from final
CPU results because captures were interrupted. Errors are now explicitly traced as `poll_error`.

## Final tuning results

The final pane-grouped build passed `scripts/check.sh` (fmt, clippy, 84 tests and tty lifecycle)
and `scripts/use_mode_check.py`. Tty exits were 1.4–62.4 ms for normal signal/tty cases and
697.8 ms with blocked output, all exit code zero. Use mode preserved 120×35 dimensions and
cleaned its helper after SIGTERM in 32.2 ms. Occupied-back-key checks also passed in the prior
pane-grouped validation. No physical clipboard verification was performed.

| Final run | ncm CPU | Subprocess CPU | Combined CPU | Spawns/s | Captures/s | Poll errors |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| live busy, 60 seconds | 1.444% | 0.614% | 2.058% | 0.707 | 4.223 | 0 |
| private idle, 90 seconds | 0.240% | 0.194% | 0.434% | 0.274 | 0.391 | 0 |

Busy load averages (1/5/15 minutes) were 11.59/14.36/13.92 before and 32.55/19.92/16.04
after; idle was 32.55/19.92/16.04 before and 12.43/16.34/15.04 after. Busy peak RSS was
69.516 MiB. The existing live tmux server measured 0.058% without the viewer and 0.111% with
it, reported separately from combined CPU. Neither CPU run discovered transcript followers,
so these numbers cover screen collection and pictures rather than active transcript catch-up.

The earlier row-sharing build measured 2.240% busy at load 23.62/14.88/12.97 →
17.24/14.43/12.93. The first pane-grouped run measured 3.352% busy and 0.497% idle at
18.58/14.73/13.23 → 15.40/13.83/12.97 → 17.60/15.40/13.71. Live workloads and host load
varied; these are observations, not a controlled comparison. The final run is the later loaded
repeat. It is slightly above the original 2.0% busy target; the orchestrator accepted 2.24% on
a loaded host provided idle stayed below 0.5%, then requested no further optimization.

Final `scripts/input_latency.py` (40 samples) measured Quick Look first-picture p95 **42.342 ms**
(median 10.908 ms, max 77.789 ms), panel p95 36.585 ms and arrow p95 **1.806 ms**
(max 2.026 ms). Hidden viewers performed zero source polls, facts refreshes or image flushes;
hidden combined CPU was 0.212%. The earlier row-sharing run measured 26.1 ms first-picture
p95 and 1.226 ms arrow p95. The first grouped run had a 242.457 ms first-picture p95 under
load; its JSON is retained alongside the final 42.342 ms repeat. Both final latency targets pass.

Final CPU summaries/traces are `target/ncm-measurements/tune-final-busy-3.*` and
`tune-final-idle-3.*`; check outputs are `final-check.log` and `final-live-check.log` in the same
directory. Latency JSON/trace, earlier `latency-row.json`/`latency-group-loaded.json`, use-mode
and tty results are in `target/ncm-feel-checks/`. All owned viewers were reaped; the final
`pgrep -x ncm` audit is `target/ncm-measurements/cleanup-audit.txt`.

## Reproduce

```sh
CARGO_BUILD_JOBS=1 scripts/check.sh
CARGO_HOME="$PWD/target/cargo-home" CARGO_BUILD_JOBS=1 cargo build --release --offline
python3 scripts/input_latency.py --socket /private/tmp/tmux-501/default --samples 40
python3 scripts/outside_mode_check.py --binary target/release/ncm
python3 scripts/measure.py --graphics --socket /private/tmp/tmux-501/default --session main --width 255 --height 60 --seconds 60 --name tune-busy
python3 scripts/measure.py --graphics --idle-fixture --width 255 --height 60 --seconds 90 --name tune-idle
```

Current direct-mode validation writes `target/ncm-outside-checks/outside.json`, timing logs, and
owned PTY captures. It covers a delayed SSH-like probe, single-timeout fallback, resizing, grouped
attach with another client present, C-]/prefix-d returns, image retransmission, native typing,
root-key pass-through, SIGTERM cleanup, restored termios, and zero graphics in legacy mode.
The earlier tuning/use-mode results above describe the retired in-tmux image/popup implementation.

The release binary is `target/release/ncm`. Timing logs, summaries, query timestamps and check
output are in `target/ncm-measurements/`; latency/use-mode/tty artifacts are in
`target/ncm-feel-checks/`. Builds and all heavy verification use the host admission wrapper, with
one cargo job and serial tests on the shared 16 GB Mac mini. No commits, pushes or branch switches.
