# Persistent tmux observer: implementation and live measurement

The source now owns one `tmux -N -C attach-session -f ignore-size` process, shared by its
queries, captures and same-server viewer checks. It uses pipes, leaves terminal sizes and
session/window selections alone, and creates no helper session. Commands use matched
`%begin`/`%end`/`%error` response guards. EOF reconnects on the next query; reconnect invalidates
content state. Shutdown joins the collector, kills/reaps its exact control client, and joins the
protocol reader.

Read-only behavior is enforced by an observer command allowlist: list/show queries,
`display-message -p`, and `capture-pane -p`. Keys and structural actions cannot reach its stdin.
Explicit actions use ordinary command clients, and `--read-only` continues to block them.
The tmux `read-only` client flag is omitted because tmux 3.7 also refuses external `send-keys`
when it chooses that observer as the command's implicit target client. This incompatibility was
caught by the existing graphics tests. Control clients are excluded from real viewer/graphics,
menu and use-mode client selection. tmux still accounts for an attached observer and can run
configured attach/detach hooks.

Pane output/mode notifications dirty only their stable pane id. Structural notifications request
metadata. Output payloads are discarded without decoding or retaining them; outside response
blocks the reader keeps at most 256 header bytes across arbitrary buffer splits. Raw output
notifications remain enabled: `no-output` removes the pane change signal. Measured client CPU
was negligible at this load, so pause/resume commands and output buffering were unnecessary.
Ordinary cards capture dirty panes under the existing shared one-second limit, without periodic
safety recaptures. Quick Look retains its 250ms window refresh. The pure polling scheduler retains
its signature comparison and 15s fallback for callers without notifications. Captured-text hashes
skip unchanged parsing/composition; existing pixel-strip hashes suppress unchanged image uploads.

Metadata still refreshes periodically (250ms, backing off to 1s) for options and external group
files that have no notification. Per-card capture limits, transcript facts, graphics negotiation,
explicit actions, linked-window selection and terminal restoration remain covered by the tests.

## Live measurements

Target: `--read-only --socket /private/tmp/tmux-501/default --session main --stats --log FILE`.
Server pid: **14233**. No real terminal client, GUI or audio device was started. A drained
160×50 PTY answered Kitty queries and cell-size requests. Builds, tests and measurement runs
used the host `heavy` wrapper. The baseline binary was saved before source edits.

Each sustained sample is **60 seconds after 30 seconds of transcript warm-up**. Neither run was
still catching up after warm-up. CPU is percent of one core, averaged using the stats intervals;
server CPU is the change in `ps -p 14233 -o time=` divided by the 60-second wall interval.
Child stats now include running children via native process queries, as well as reaped children.

| 60s sustained sample | ncm CPU % | child CPU % | tmux server CPU % | server delta over 0.85% reference | captures/s | image KB/s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Before | 1.414 | 0.416 | 3.400 | 2.550 | 7.050 | 42.231 |
| After, final implementation | 1.157 | 0.000 | 2.067 | 1.217 | 2.072 | 56.152 |

Two independent 60-second server-only samples measured **0.850%** before and **2.032%** after,
showing that the agents' background load varied. Relative to the lower reference, conservative
estimated combined observer CPU fell from **4.381% to 2.374%**, below the 3% sustained target.
Using the later server-only reference, final estimated combined CPU is **1.191%**. These are
background-subtracted estimates, not an isolated causal measurement of server cost. The
server's absolute CPU decreased **1.334 percentage points** between the sustained runs.

The final 90-second run (including warm-up) logged **one tmux spawn**, versus **55** for the
baseline run. Final peak sampled RSS was **45.7 MB**, versus **47.1 MB** before. Image traffic
was higher during the final sample despite fewer captures; live pane content differed between
runs. The unchanged-image regression test passed; traffic reflects transmitted protocol bytes,
including base64 overhead.

**Startup remains above the 3% target.** Over the first 60 seconds of those same runs, ncm CPU
alone averaged **5.576% before / 5.110% after**. Traces identify initial transcript parsing as the
largest cost. This change addresses sustained tmux monitoring; it does not optimize historical
transcript catch-up. The current baseline already batches command frames, so its child CPU did
not reproduce the task's earlier 22–52% observation.

## Validation and artifacts

`scripts/check.sh` passed on the final sources: formatting, clippy, all workspace tests, terminal
lifecycle, graphics re-probe, panic cleanup, and resize/client checks. Added in-module coverage
checks fragmented/non-UTF-8 megabyte output discard, response framing/errors, reconnect, query
reuse, observer action rejection, size preservation and running-child CPU accounting. Existing
Quick Look query-reuse coverage now expects zero additional spawns. No scripts or integration
fixture files were edited.

Artifacts under `target/control-bench/`:

- `ncm-before`, `ncm-after`: saved release binaries; current binary is `target/release/ncm`.
- `measure.py`: headless measurement runner.
- `before-steady.{log,json,queries}`, `after-final.{log,json,queries}`: sustained measurements.
- `server-only.json`, `server-only-final.json`: server background CPU references.
- `before.{log,json}`, `after-raw.{log,json}`, `after-steady.{log,json}`: exploratory samples;
  the latter two predate the final command-allowlist/child-accounting fixes.
- `check-final.log`: passing complete check output.

All launched ncm processes were reaped and their tmux clients exited; final process inspection
found no observer or ncm process from this worktree. No commit, push or branch change was made.
