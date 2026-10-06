# Real-use harness

Run `scripts/realuse/run.sh`. Requires tmux, Rust and Python 3. It builds this
worktree's ncm, installs pinned pyte/wcwidth in `target/realuse/venv` on first use,
checks fragmented VT/protocol decoding, and runs scenarios through the host
`heavy` wrapper. Warm execution targets <3 minutes (165-second scenario budget;
admission queues and cold setup are additional). Exit 1 means failures found.
Use `--binary target/path/to/ncm` to test a replacement build within the worktree.

**Primary:** an owned PTY runs ncm directly, TERM=xterm-ghostty and TMUX unset,
like a Ghostty terminal or `ssh -t host ncm`. A private
`tmux -L ncm-real-<pid> -f /dev/null` server stays in the background with 12 fake
agent windows, one split, and 0.2/0.7/2-second or idle output plus transcript history.
Expected workflow: Space/click opens a floating preview; Enter/i/click inside
preview leaves ncm's alt screen and runs real tmux attach on `_ncm_view_<pid>`;
Ctrl-] or prefix-d removes the helper and resumes ncm. Click, wheel, and drag-copy
**inside attach** must use native tmux routing. Missing attach support is reported
as a failure, and dependent native checks explicitly name that prerequisite.

**Secondary:** legacy ncm inside tmux must render a usable text preview and emit
zero graphics bytes, including probes. Hidden-start/show is exercised here.
A real outer tmux also attaches/detaches a nested client to the background server;
this must not interfere with the primary direct terminal's graphics.

Every scenario uses actual key/SGR mouse bytes through the owned PTY; no
`send-keys`. Assertions use the rendered VT text/styles and tmux state, with ncm's
existing timing trace as supplemental evidence. The fake terminal answers kitty
queries and records image IDs/payload hashes. It renders the text/placeholder
plane, not kitty raster pixels. Alternate-screen save/restore is emulated.
Thirty seconds without input are measured with busy agents, then with all agents
quiet. Full redraws are bursts touching ≥90% of rows, separated by 50ms silence;
explicit clears, image uploads, identical re-sends and wire/graphics B/s are separate.
The controlled comparison and per-second trace correlations give trigger evidence.

Private options: mouse on, allow-passthrough on, window-size latest,
set-clipboard on, copy-command pbcopy, status on. The literal `pbcopy` command
is shadowed **only in the private server PATH** to an artifact sink. OSC52 is
captured by the fake terminal. No desktop clipboard, GUI, audio or live agent data.

Artifacts in `target/realuse/`:

- `report.md`: pass/fail table, exact inputs, expected/actual and screen dumps
  for each failure, refresh rates and cleanup status.
- `results.json`, `ncm.log`, `legacy.log`, `help.txt`, `configuration.txt`,
  `build.txt`: machine-readable evidence and tested build/options.
- `screens/*.txt`, `*.vt`, `*-terminal.json`: rendered screens, raw client output,
  exact PTY writes, protocol counters and image hashes; `clipboard.txt`: copy sink.

Clients/servers close in `finally`; pane-local supervisors terminate and wait for
ncm children, followed by an owned-PID audit. SIGTERM/SIGHUP also trigger cleanup.
Restricted sandboxes may need permission for private tmux servers and PTYs.
The harness never accesses md's tmux server or modifies ncm source.

Assertions exclude size-neutral tmux control-mode clients when checking for attached
user clients. The terminal implements REP (CSI b), including fragmented sequences,
so kitty placeholder runs advance the cursor and erase old preview text accurately.
The `g` key preserves the single startup probe; retired re-probing is not expected.
Menu checks exercise Go, Rename, cancellation and confirmed Close through ncm's own UI.
Redraw bursts are separated by 50 ms without text bytes; graphics traffic does not
merge them. Both native detach bindings must restore a live preview with fresh
image uploads. `g` must leave the one probe intact while image uploads continue.
